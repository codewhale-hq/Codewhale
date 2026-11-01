use crate::bridge::install_rustls_crypto_provider;
use crate::http::{app_router, resolve_auth_token};
use crate::stdio::{BoundedLines, run_stdio_loop};
use anyhow::Context as _;
use anyhow::{Result, anyhow};
use axum::http::{Method, StatusCode};
use codewhale_agent::ModelRegistry;
use codewhale_config::ConfigStore;
use codewhale_core::Runtime;
use codewhale_hooks::{HookDispatcher, JsonlHookSink, StdoutHookSink, UnixSocketHookSink};
use codewhale_protocol::EventFrame;
use codewhale_protocol::runtime::{MAX_RUNTIME_IMAGE_BODY_BYTES, RuntimeImageInput};
use codewhale_state::StateStore;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Child;
#[cfg(all(test, unix))]
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;
mod chat_completions;
mod thread_control;
pub use codewhale_protocol::{
    ThreadListParams, ThreadReadParams, ThreadRequest, ThreadResponse, ThreadSetNameParams,
};
pub use thread_control::{ThreadControlSelection, request_thread_control};
mod bridge;
mod config_rpc;
pub mod http;
pub mod stdio;
mod turns;

/// Capture once in a client before sending a durable control. Servers require
/// the caller's retained key and never manufacture a replacement on retry.
pub fn capture_thread_operation_key() -> String {
    Uuid::new_v4().to_string()
}

pub mod daemon_client;
pub mod daemon_socket;
#[cfg(windows)]
mod daemon_windows;

/// Legacy DeepSeek-era naming kept for external compatibility.
///
/// CodeWhale began life as DeepSeek-TUI; existing health probes, SDK
/// harnesses, and on-disk layouts still key off these names. Every remaining
/// legacy reference in this crate routes through this shim so a future
/// coordinated migration touches exactly one place (repo policy: preserve
/// legacy migration care).
mod legacy_deepseek_compat {
    /// Service name advertised by the HTTP and stdio health probes.
    pub(crate) const SERVICE_NAME: &str = "deepseek-app-server";
}

/// Upper bound on JSON request bodies accepted by the HTTP app-server.
const MAX_HTTP_BODY_BYTES: usize = 16 * 1024 * 1024;

const MAX_SSE_FRAME_BYTES: usize = 16 * 1024 * 1024;

const DEFAULT_CORS_ORIGINS: &[&str] = &[
    "http://localhost",
    "http://localhost:1420",
    "http://localhost:3000",
    "http://localhost:5173",
    "http://127.0.0.1",
    "http://127.0.0.1:1420",
    "tauri://localhost",
];

#[derive(Clone)]
pub struct AppServerOptions {
    pub listen: SocketAddr,
    pub config_path: Option<PathBuf>,
    pub auth_token: Option<String>,
    pub insecure_no_auth: bool,
    pub cors_origins: Vec<String>,
}

impl std::fmt::Debug for AppServerOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppServerOptions")
            .field("listen", &self.listen)
            .field("config_path", &self.config_path)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "<redacted>"),
            )
            .field("insecure_no_auth", &self.insecure_no_auth)
            .field("cors_origins", &self.cors_origins)
            .finish()
    }
}

/// Selected frontend facts passed by the canonical CLI in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeControlFrontend {
    Stdio,
    Socket { path: Option<PathBuf> },
    LegacyHttp,
    Acp { model: String },
}

/// A bounded listener request admitted only over the authenticated owner channel.
/// The bearer is explicit operator input, never discovery or response data.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeListenerSelection {
    pub workers: usize,
    pub workspace: PathBuf,
    pub config_profile: Option<String>,
    #[serde(default)]
    pub config_source: Option<PathBuf>,
    pub host: String,
    pub port: u16,
    pub cors_origins: Vec<String>,
    pub auth_token: Option<String>,
    pub insecure_no_auth: bool,
    pub mobile: bool,
    pub web: bool,
}

impl std::fmt::Debug for RuntimeListenerSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeListenerSelection")
            .field("host", &self.host)
            .field("port", &self.port)
            .field(
                "auth_token",
                &self.auth_token.as_ref().map(|_| "<redacted>"),
            )
            .field("mobile", &self.mobile)
            .field("web", &self.web)
            .finish_non_exhaustive()
    }
}

impl RuntimeListenerSelection {
    pub fn validate_bounds(&self) -> Result<()> {
        anyhow::ensure!(
            self.workspace.as_os_str().len() <= 32768
                && self
                    .config_source
                    .as_ref()
                    .is_none_or(|path| path.as_os_str().len() <= 32768)
                && self
                    .config_profile
                    .as_ref()
                    .is_none_or(|profile| profile.len() <= 1024)
                && self.host.len() <= 128
                && self.cors_origins.len() <= 64
                && self.cors_origins.iter().map(String::len).sum::<usize>() <= 32768
                && self
                    .auth_token
                    .as_ref()
                    .is_none_or(|token| token.len() <= 8192),
            "selected frontend input exceeds its bounds"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeFrontendScope {
    pub workers: usize,
    pub workspace: PathBuf,
    pub config_profile: Option<String>,
    #[serde(default)]
    pub config_source: Option<PathBuf>,
}

impl RuntimeFrontendScope {
    pub fn validate_bounds(&self) -> Result<()> {
        anyhow::ensure!(
            self.workspace.as_os_str().len() <= 32768
                && self
                    .config_source
                    .as_ref()
                    .is_none_or(|path| path.as_os_str().len() <= 32768)
                && self
                    .config_profile
                    .as_ref()
                    .is_none_or(|profile| profile.len() <= 1024),
            "selected frontend scope exceeds its bounds"
        );
        Ok(())
    }
}

#[derive(Clone)]
pub enum RuntimeOwnerFrontendSelection {
    Control(RuntimeFrontendScope),
    Acp {
        scope: Option<RuntimeFrontendScope>,
        model: Option<String>,
    },
    Listener(RuntimeListenerSelection),
}

/// Captured Runtime-owned IO projection. Implementations use the held manager
/// and existing routers; this port owns neither a store nor a turn controller.
pub trait RuntimeOwnerFrontend: Send + Sync {
    fn validate_selection<'a>(
        &'a self,
        selection: &'a RuntimeOwnerFrontendSelection,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>;
    fn serve(
        &self,
        selection: RuntimeOwnerFrontendSelection,
        compatibility: AppState,
        input: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>,
        output: Box<dyn AsyncWrite + Send + Unpin>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>;
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeOwnerRouting {
    pub endpoint: SocketAddr,
    /// The owning manager's acknowledged workspace. A legacy cold wrapper
    /// without one cannot supply a default mutation scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<PathBuf>,
    /// Actual held scheduler setting; absent on historical cold wrappers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workers: Option<usize>,
    pub mobile: bool,
    pub web: bool,
    pub acp: bool,
    #[serde(default)]
    pub acp_only: bool,
}

/// Cached app-server→runtime bridge handle.
///
/// The outer [`AppState::runtime_bridge`] mutex guards only the cache slot;
/// this inner mutex serializes traffic and event cursors for the captured
/// canonical owner. It never creates or replaces an execution process.
type SharedRuntimeBridge = Arc<Mutex<RuntimeBridge>>;

#[derive(Clone)]
pub struct AppState {
    captured_owner: Option<codewhale_protocol::RuntimeOwnerReceipt>,
    captured_routing: Option<RuntimeOwnerRouting>,
    frontend_workspace: Option<PathBuf>,
    owner_frontend: Option<Arc<dyn RuntimeOwnerFrontend>>,
    config_path: Option<PathBuf>,
    config: Arc<RwLock<codewhale_config::ConfigToml>>,
    /// Bookkeeping config and read-only historical archive access.
    /// Actual turns and transcript writes belong to the captured owner.
    runtime: Arc<RwLock<Runtime>>,
    registry: ModelRegistry,
    auth_token: Option<String>,
    /// Cached bridge to the real runtime API. Shared by every surface that
    /// executes a turn — stdio `thread/message`, HTTP `/thread` messages, and
    /// both `/prompt` transports — because there is exactly one turn engine.
    runtime_bridge: Arc<Mutex<Option<SharedRuntimeBridge>>>,
    runtime_catalog_transport: Option<RuntimeCatalogTransport>,
    /// Client-facing thread key → durable runtime thread id.
    ///
    /// Runtime threads are persisted by the captured owner's store. Durable
    /// aliases keep their exact target across frontend detach/reconnect;
    /// withdrawing a bridge never remints a replacement thread.
    /// Callers serialize traffic on the bridge mutex.
    runtime_thread_map: Arc<Mutex<HashMap<String, String>>>,
    stdio_thread_hints: Arc<Mutex<HashMap<String, RuntimeThreadHint>>>,
    /// Turns currently streaming over stdio, keyed by stdio thread id.
    ///
    /// Deliberately kept *outside* the bridge mutex: a streaming turn holds
    /// that mutex for its entire duration, so anything reachable only through
    /// it cannot be used to stop the turn. This holds its own copy of what an
    /// interrupt needs, so a cancel never waits on the turn it is cancelling.
    in_flight_turns: Arc<Mutex<HashMap<String, InFlightTurn>>>,
}

/// Everything needed to interrupt a running turn without the bridge lock.
#[derive(Debug, Clone)]
struct InFlightTurn {
    base_url: String,
    auth_token: Option<String>,
    /// Thread id as the *runtime* knows it, not the stdio-facing id.
    runtime_thread_id: String,
    turn_id: String,
}

type TurnRegistry = Arc<Mutex<HashMap<String, InFlightTurn>>>;

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// Server error: the app-server could not reach the runtime that executes
/// turns. Kept in the JSON-RPC implementation-defined server range
/// (-32000..-32099) alongside `thread_not_found` (-32004).
const RUNTIME_UNAVAILABLE_CODE: i64 = -32005;

/// Server error: the named thread does not exist.
const THREAD_NOT_FOUND_CODE: i64 = -32004;

/// Server error: a daemon-socket client tried to act before `daemon/attach`.
/// Only the unix listener raises it; gated so the Windows build (where the
/// listener is a typed-unsupported stub) does not fail `warnings = "deny"`
/// on dead code.
#[cfg(any(unix, windows))]
const ATTACH_REQUIRED_CODE: i64 = -32010;

/// Server error: a `daemon/attach` claim lost to a live owner.
#[cfg(any(unix, windows))]
const DAEMON_ALREADY_CLAIMED_CODE: i64 = -32011;

/// Server error: only the owning client may `shutdown` the daemon.
const NOT_DAEMON_OWNER_CODE: i64 = -32012;

/// Server error: the client refused the daemon's version at attach time.
#[cfg(any(unix, windows))]
const DAEMON_VERSION_SKEW_CODE: i64 = -32013;

/// Server error: `daemon/attach` sent twice on one connection.
const ALREADY_ATTACHED_CODE: i64 = -32014;

#[derive(Debug)]
struct JsonRpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

#[derive(Debug)]
struct StdioDispatchResult {
    result: Value,
    should_exit: bool,
}

#[derive(Clone)]
struct RuntimeCatalogTransport {
    bridge: std::sync::Weak<Mutex<RuntimeBridge>>,
    base_url: String,
    client: reqwest::Client,
    auth_token: Option<String>,
}

struct RuntimeBridge {
    base_url: String,
    client: reqwest::Client,
    auth_token: Option<String>,
    #[cfg(test)]
    child: Option<Child>,
    /// Captured owner SSE cursors, keyed by Runtime thread identity.
    last_seq_by_thread: HashMap<String, u64>,
}

impl std::fmt::Debug for RuntimeBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeBridge")
            .field("base_url", &self.base_url)
            .field("authenticated", &self.auth_token.is_some())
            .field("last_seq_by_thread", &self.last_seq_by_thread)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default)]
struct RuntimeThreadHint {
    model: Option<String>,
    workspace: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TurnTerminalStatus {
    Completed,
    Failed,
    Interrupted,
    Canceled,
}

/// Structured capture of one bridged turn, for callers that must *return*
/// the turn instead of streaming it (HTTP `/prompt`, HTTP `/thread` messages).
///
/// The stdio path streams the same events to its writer and needs none of
/// this, so it passes `None` and pays nothing.
#[derive(Debug, Default)]
struct TurnTranscript {
    /// Concatenated `agent_message` deltas — the model's actual output.
    text: String,
    /// The model the runtime reports for the thread that ran the turn.
    model: Option<String>,
    /// The same frames the stdio path writes, in order.
    events: Vec<EventFrame>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppTransport {
    Http,
    Stdio,
    /// Unix-domain-socket daemon transport (`daemon_socket`). Speaks the
    /// stdio JSON-RPC protocol verbatim after a `daemon/attach` handshake.
    Socket,
}

impl AppTransport {
    /// Wire label reported by `healthz` / `capabilities`.
    fn label(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Stdio => "stdio",
            Self::Socket => {
                #[cfg(windows)]
                {
                    "named-pipe"
                }
                #[cfg(not(windows))]
                {
                    "unix-socket"
                }
            }
        }
    }
}

/// Whether the peer driving a JSON-RPC loop may stop the whole server.
///
/// The process-owned stdio loop always may (its peer *is* the supervisor).
/// On the daemon socket only the client that claimed the daemon may; every
/// other attached client is refused with `not_daemon_owner` — the brief's
/// "never terminate a daemon the app did not spawn", enforced server-side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShutdownAuthority {
    Granted,
    Denied,
}

/// Per-connection policy for [`run_stdio_loop`].
#[derive(Debug, Clone, Copy)]
struct StdioLoopPolicy {
    transport: AppTransport,
    shutdown: ShutdownAuthority,
}

impl StdioLoopPolicy {
    /// Legacy stdio qualification comparator; production stdio attaches its owner.
    const fn process_stdio() -> Self {
        Self {
            transport: AppTransport::Stdio,
            shutdown: ShutdownAuthority::Granted,
        }
    }
}

/// Why [`run_stdio_loop`] returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StdioLoopExit {
    /// The peer closed its write side; nothing asked the server to stop.
    InputClosed,
    /// The peer sent an honoured `shutdown`.
    Shutdown,
}

#[derive(Debug, Deserialize)]
struct ConfigGetParams {
    key: String,
}

#[derive(Debug, Deserialize)]
struct ConfigSetParams {
    key: String,
    value: String,
}

#[derive(Debug, Deserialize)]
struct ThreadIdParams {
    thread_id: String,
}

#[derive(Debug, Deserialize)]
struct ThreadMessageParams {
    #[serde(default)]
    allowed_tools: Option<Vec<String>>,
    #[serde(default, rename = "maxOutputTokens", alias = "max_output_tokens")]
    max_output_tokens: Option<std::num::NonZeroU32>,
    thread_id: String,
    input: String,
    #[serde(default)]
    images: Vec<RuntimeImageInput>,
}

#[derive(Debug, Deserialize)]
struct ThreadInterruptParams {
    thread_id: String,
}

pub async fn run(options: AppServerOptions) -> Result<()> {
    let auth_token = resolve_auth_token(&options)?;
    let state =
        build_state_off_runtime(options.config_path.clone(), auth_token, AppTransport::Http)
            .await?;
    let app = app_router(state, &options.cors_origins);

    let listener = tokio::net::TcpListener::bind(options.listen).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// Attach the existing compatibility dispatcher to the actual held Runtime.
/// Auth remains process-private; it is never part of the owner receipt or IPC.
#[cfg(any(unix, windows))]
pub async fn bind_runtime_owner(
    config_path: Option<PathBuf>,
    endpoint: SocketAddr,
    auth_token: Option<String>,
    owner: codewhale_protocol::RuntimeOwnerReceipt,
) -> Result<daemon_socket::DaemonSocket> {
    let (daemon, _) = bind_runtime_frontends(
        config_path,
        auth_token,
        owner,
        RuntimeOwnerRouting {
            endpoint,
            workspace: None,
            workers: None,
            mobile: false,
            web: false,
            acp: false,
            acp_only: false,
        },
        None,
        None,
    )
    .await?;
    // A caller of this entry point has no HTTP frontend to fall back on: it
    // exists to hand back a socket. `bind_runtime_frontends` only degrades to
    // `None` when the filesystem cannot hold a private endpoint directory, and
    // there is nothing to serve if that happens here.
    daemon.ok_or_else(|| {
        anyhow::anyhow!(
            "this filesystem cannot hold the private directory a runtime owner endpoint needs"
        )
    })
}

/// Build the actual compatibility state once for all owner frontends.
#[cfg(any(unix, windows))]
pub async fn bind_runtime_frontends(
    config_path: Option<PathBuf>,
    auth_token: Option<String>,
    owner: codewhale_protocol::RuntimeOwnerReceipt,
    routing: RuntimeOwnerRouting,
    owner_frontend: Option<Arc<dyn RuntimeOwnerFrontend>>,
    recovery: Option<Arc<daemon_client::UnavailablePublication>>,
) -> Result<(Option<daemon_socket::DaemonSocket>, AppState)> {
    anyhow::ensure!(
        owner.version == 1 && owner.pid == std::process::id() && !owner.lease_generation.is_empty(),
        "invalid captured Runtime owner"
    );
    let mut state =
        build_state_off_runtime(config_path, auth_token.clone(), AppTransport::Socket).await?;
    install_rustls_crypto_provider();
    let endpoint = routing.endpoint;
    let address = match endpoint.ip() {
        std::net::IpAddr::V4(ip) if ip.is_unspecified() => {
            SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, endpoint.port()))
        }
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => {
            SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, endpoint.port()))
        }
        _ => endpoint,
    };
    let bridge = RuntimeBridge {
        base_url: format!("http://{address}"),
        client: codewhale_release::platform_http_client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        auth_token,
        #[cfg(test)]
        child: None,
        last_seq_by_thread: HashMap::new(),
    };
    state.captured_owner = Some(owner.clone());
    anyhow::ensure!(
        routing.acp == owner_frontend.is_some() && (!routing.acp_only || routing.acp),
        "ACP projection must belong to its actual captured owner"
    );
    state.frontend_workspace = routing.workspace.clone();
    state.captured_routing = Some(routing);
    state.owner_frontend = owner_frontend;
    let (base_url, client, auth_token) = (
        bridge.base_url.clone(),
        bridge.client.clone(),
        bridge.auth_token.clone(),
    );
    let bridge = Arc::new(Mutex::new(bridge));
    state.runtime_catalog_transport = Some(RuntimeCatalogTransport {
        bridge: Arc::downgrade(&bridge),
        base_url,
        client,
        auth_token,
    });
    *state.runtime_bridge.lock().await = Some(bridge);
    let daemon = daemon_socket::bind_captured_owner(state.clone(), owner, recovery).await?;
    Ok((daemon, state))
}

pub async fn run_owned_acp(state: AppState) -> Result<()> {
    anyhow::ensure!(
        state
            .captured_owner
            .as_ref()
            .is_some_and(|owner| owner.pid == std::process::id()),
        "ACP stdio must belong to its actual captured host"
    );
    state
        .owner_frontend
        .as_ref()
        .context("captured owner has no ACP projection")?
        .serve(
            RuntimeOwnerFrontendSelection::Acp {
                scope: None,
                model: None,
            },
            state.clone(),
            Box::new(BufReader::new(tokio::io::stdin())),
            Box::new(tokio::io::BufWriter::new(tokio::io::stdout())),
        )
        .await
}

/// The existing charged socket connection invokes the same priority-aware
/// dispatcher after its captured Runtime has admitted the selected scope.
pub async fn run_guest_control(
    mut state: AppState,
    workspace: PathBuf,
    input: Box<dyn AsyncBufRead + Send + Unpin>,
    output: Box<dyn AsyncWrite + Send + Unpin>,
) -> Result<()> {
    anyhow::ensure!(
        state
            .captured_owner
            .as_ref()
            .is_some_and(|owner| owner.pid == std::process::id()),
        "control projection requires its actual captured host"
    );
    state.frontend_workspace = Some(workspace);
    let policy = StdioLoopPolicy {
        transport: AppTransport::Socket,
        shutdown: ShutdownAuthority::Denied,
    };
    run_stdio_loop(&state, BoundedLines::new(input), output, policy, None::<()>).await?;
    Ok(())
}

/// Logical write EOF for duplex transports. This is only a per-connection
/// lifecycle notification; it carries no tool, turn or host-shutdown authority.
pub fn is_control_input_closed(message: &Value) -> bool {
    message["jsonrpc"] == "2.0"
        && message.get("id").is_none()
        && message["method"] == "daemon/input_closed"
        && message["params"]
            .as_object()
            .is_some_and(|params| params.is_empty())
}

pub async fn run_owned_stdio(state: AppState) -> Result<()> {
    anyhow::ensure!(
        state
            .captured_owner
            .as_ref()
            .is_some_and(|owner| owner.pid == std::process::id()),
        "stdio must belong to its actual captured Runtime host"
    );
    let lines = BoundedLines::new(BufReader::new(tokio::io::stdin()));
    let writer = tokio::io::BufWriter::new(tokio::io::stdout());
    run_stdio_loop(
        &state,
        lines,
        writer,
        StdioLoopPolicy::process_stdio(),
        None::<()>,
    )
    .await?;
    Ok(())
}

pub async fn run_stdio(config_path: Option<PathBuf>) -> Result<()> {
    daemon_client::forward_stdio(config_path).await
}

#[cfg(test)]
fn build_state(config_path: Option<PathBuf>, auth_token: Option<String>) -> Result<AppState> {
    build_state_with_transport(config_path, auth_token, AppTransport::Http)
}

/// [`build_state_with_transport`] on the blocking pool. Server startup is
/// async, but building state is not: it reads and parses the config file,
/// creates directories, and opens SQLite (a schema migration that may wait
/// out the 5s busy timeout behind another process). Run inline, that parked
/// a Tokio worker.
async fn build_state_off_runtime(
    config_path: Option<PathBuf>,
    auth_token: Option<String>,
    transport: AppTransport,
) -> Result<AppState> {
    daemon_socket::owner_work(move || {
        build_state_with_transport(config_path, auth_token, transport)
    })
    .await
    .context("app-server state setup task failed")
}

fn build_state_with_transport(
    config_path: Option<PathBuf>,
    auth_token: Option<String>,
    transport: AppTransport,
) -> Result<AppState> {
    let has_explicit_config_path = config_path.is_some();
    let store = ConfigStore::load(config_path)?;
    let config_path = has_explicit_config_path.then(|| store.path().to_path_buf());
    let config = store.config.clone();
    let registry = ModelRegistry::default();

    let state_db_path = config_path
        .as_ref()
        .and_then(|p| p.parent().map(|parent| parent.join("state.db")));
    let state_store = StateStore::open(state_db_path)?;

    let mut hooks = HookDispatcher::default();
    // Stdio carries JSON-RPC on stdout: printing raw hook events there
    // corrupts the protocol stream (#5165). HTTP mode keeps the stdout
    // sink for local development visibility.
    if transport == AppTransport::Http {
        hooks.add_sink(Arc::new(StdoutHookSink));
    }
    // The hook log sits beside the state database: next to an explicit
    // config, otherwise in the directory the state store resolved under the
    // Codewhale home. Never relative to the process cwd (#6513).
    let hook_log_path = state_store
        .db_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("events.jsonl");
    hooks.add_sink(Arc::new(JsonlHookSink::new(hook_log_path)));

    if let Some(socket_path) = config
        .hook_sinks
        .as_ref()
        .and_then(|sinks| sinks.unix_socket_path.as_ref())
        .filter(|path| !path.as_os_str().is_empty())
    {
        hooks.add_sink(Arc::new(UnixSocketHookSink::new(socket_path.clone())));
    }

    let runtime = Runtime::new(config.clone(), state_store, hooks);

    Ok(AppState {
        captured_owner: None,
        captured_routing: None,
        frontend_workspace: None,
        owner_frontend: None,
        config_path,
        config: Arc::new(RwLock::new(config)),
        runtime: Arc::new(RwLock::new(runtime)),
        registry,
        auth_token,
        runtime_bridge: Arc::new(Mutex::new(None)),
        runtime_catalog_transport: None,
        runtime_thread_map: Arc::new(Mutex::new(HashMap::new())),
        stdio_thread_hints: Arc::new(Mutex::new(HashMap::new())),
        in_flight_turns: Arc::new(Mutex::new(HashMap::new())),
    })
}

fn params_or_object(params: Value) -> Value {
    if params.is_null() { json!({}) } else { params }
}

fn parse_params<T: DeserializeOwned>(params: Value) -> std::result::Result<T, JsonRpcError> {
    serde_json::from_value(params).map_err(|err| JsonRpcError::invalid_params(err.to_string()))
}

fn jsonrpc_result(id: Option<Value>, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "result": result
    })
}

fn jsonrpc_error(id: Option<Value>, err: JsonRpcError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id.unwrap_or(Value::Null),
        "error": {
            "code": err.code,
            "message": err.message,
            "data": err.data
        }
    })
}

impl JsonRpcError {
    fn parse_error(message: impl Into<String>) -> Self {
        Self {
            code: -32700,
            message: message.into(),
            data: None,
        }
    }

    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            code: -32600,
            message: message.into(),
            data: None,
        }
    }

    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("unsupported method: {method}"),
            data: None,
        }
    }

    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
            data: None,
        }
    }

    fn unsupported_interactive_execution() -> Self {
        Self {
            code: -32602,
            message: "Compatibility transports cannot answer approvals or clarification requests. Use codewhale app-server --http and /v1/* for interactive turns and goal execution; tool-free turn requests require explicit allowed_tools: [].".to_string(),
            data: Some(json!({
                "error": "interactive_execution_unsupported",
                "entrypoint": "codewhale app-server --http",
                "allowed_tools_required": [],
                "goal_execution": false,
            })),
        }
    }

    /// Server error (-32000..-32099): the turn engine could not be reached,
    /// or refused to start the turn — either way nothing ran. Distinct from
    /// `internal` because the caller can retry this one once a runtime is up.
    fn runtime_unavailable(message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            code: RUNTIME_UNAVAILABLE_CODE,
            message: message.clone(),
            data: Some(json!({
                "error": "runtime_unavailable",
                "detail": message,
            })),
        }
    }

    /// Server error (-32000..-32099): the named thread does not exist.
    fn thread_not_found(thread_id: &str) -> Self {
        Self {
            code: THREAD_NOT_FOUND_CODE,
            message: format!("thread not found: {thread_id}"),
            data: Some(json!({
                "error": "thread_not_found",
                "thread_id": thread_id,
            })),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
            data: None,
        }
    }

    /// Server error (-32000..-32099): the daemon-socket connection has not
    /// completed `daemon/attach`, so nothing but `healthz` is allowed yet.
    #[cfg(any(unix, windows))]
    fn attach_required(method: &str) -> Self {
        Self {
            code: ATTACH_REQUIRED_CODE,
            message: format!("send daemon/attach before `{method}`"),
            data: Some(json!({
                "error": "attach_required",
                "method": method,
                "attach_method": daemon_socket::ATTACH_METHOD,
            })),
        }
    }

    /// Server error (-32000..-32099): a `claim` attach lost to a live owner.
    #[cfg(any(unix, windows))]
    fn daemon_already_claimed(owner: &Value) -> Self {
        Self {
            code: DAEMON_ALREADY_CLAIMED_CODE,
            message: "daemon already claimed by another client; attach with mode=attach"
                .to_string(),
            data: Some(json!({
                "error": "daemon_already_claimed",
                "owner": owner,
            })),
        }
    }

    /// Server error (-32000..-32099): only the owner may stop the daemon.
    fn not_daemon_owner() -> Self {
        Self {
            code: NOT_DAEMON_OWNER_CODE,
            message: "only the client that claimed this daemon may shut it down".to_string(),
            data: Some(json!({ "error": "not_daemon_owner" })),
        }
    }

    /// Server error (-32000..-32099): the client's expected daemon version
    /// does not match the running binary (bundle skew).
    #[cfg(any(unix, windows))]
    fn daemon_version_skew(expected: &str, actual: &str) -> Self {
        Self {
            code: DAEMON_VERSION_SKEW_CODE,
            message: format!(
                "daemon version {actual} does not match the client's expected {expected}"
            ),
            data: Some(json!({
                "error": "daemon_version_skew",
                "expected": expected,
                "actual": actual,
            })),
        }
    }

    /// Server error (-32000..-32099): `daemon/attach` after attaching.
    fn already_attached() -> Self {
        Self {
            code: ALREADY_ATTACHED_CODE,
            message: "this connection is already attached".to_string(),
            data: Some(json!({ "error": "already_attached" })),
        }
    }
}

/// Install the process-wide rustls crypto provider once for tests that build
/// an HTTP client. Production installs it at startup; each test must do the
/// same instead of relying on another test in the process having run first
/// (nextest runs every test in its own process).
#[cfg(test)]
pub(crate) fn install_test_crypto_provider() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests;

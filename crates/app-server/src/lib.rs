use codewhale_protocol::runtime::{MAX_RUNTIME_IMAGE_BODY_BYTES, RuntimeImageInput};
use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use codewhale_agent::ModelRegistry;
use codewhale_config::ConfigStore;
use codewhale_core::Runtime;
use codewhale_hooks::{HookDispatcher, JsonlHookSink, StdoutHookSink, UnixSocketHookSink};
use codewhale_protocol::{
    AppRequest, AppResponse, EventFrame, PromptRequest, PromptResponse, ResponseChannel,
    ThreadGoalClearParams, ThreadGoalGetParams, ThreadGoalSetParams,
};
use codewhale_state::StateStore;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;
use uuid::Uuid;

mod chat_completions;
mod thread_control;
pub use codewhale_protocol::{
    ThreadListParams, ThreadReadParams, ThreadRequest, ThreadResponse, ThreadSetNameParams,
};
pub use thread_control::{ThreadControlSelection, request_thread_control};

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
    /// Bookkeeping config/jobs and read-only historical archive access.
    /// Actual turns and transcript writes belong to the captured owner.
    runtime: Arc<RwLock<Runtime>>,
    registry: ModelRegistry,
    auth_token: Option<String>,
    /// Cached bridge to the real runtime API. Shared by every surface that
    /// executes a turn — stdio `thread/message`, HTTP `/thread` messages, and
    /// both `/prompt` transports — because there is exactly one turn engine.
    runtime_bridge: Arc<Mutex<Option<SharedRuntimeBridge>>>,
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

/// Protected routes the `Capabilities` response advertises. A test sends a
/// request to each one through [`app_router`], so an entry here without a
/// handler fails the tests instead of shipping a dead route.
///
/// There is no `/tool`: a direct tool call outside a turn would need its own
/// tool catalog and approval decision, and the Engine behind the runtime
/// bridge is the only tool and approval authority. Tools run inside turns
/// (`/prompt`, `/thread` messages). This server does not surface approvals:
/// `RuntimeBridge::stream_turn_events` forwards only `item.delta` and the
/// turn's completion, and there is no decision route, so approval-gated work
/// belongs on the Runtime API (`/v1/threads/*`, `POST /v1/approvals/{id}`).
const ADVERTISED_ROUTES: &[&str] = &["/thread", "/app", "/prompt"];

/// Existing compatibility routes adopted by the canonical host listener.
/// Both callers share the exact existing dispatcher/auth/body-limit routes.
pub fn runtime_compatibility_router(
    mut state: AppState,
    cors_origins: &[String],
    auth_token: Option<String>,
    workspace: Option<PathBuf>,
) -> Router {
    // Only the listener's gate changes. The captured bridge keeps the original
    // owner's in-memory credential and all shared dispatcher state.
    state.auth_token = auth_token;
    state.frontend_workspace = workspace;
    app_router(state, cors_origins)
}

fn app_router(state: AppState, cors_origins: &[String]) -> Router {
    let protected_routes = Router::new()
        .route(
            "/thread",
            post(thread_handler).layer(axum::extract::DefaultBodyLimit::max(
                MAX_RUNTIME_IMAGE_BODY_BYTES,
            )),
        )
        .route("/app", post(app_handler))
        .route(
            "/prompt",
            post(prompt_handler).layer(axum::extract::DefaultBodyLimit::max(
                MAX_RUNTIME_IMAGE_BODY_BYTES,
            )),
        )
        .route(
            "/v1/chat/completions",
            post(chat_completions::chat_completions_handler),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_app_server_token,
        ));

    Router::new()
        .route("/healthz", get(healthz))
        .merge(protected_routes)
        .layer(DefaultBodyLimit::max(MAX_HTTP_BODY_BYTES))
        .layer(cors_layer(cors_origins))
        .with_state(state)
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
    Ok(daemon)
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
) -> Result<(daemon_socket::DaemonSocket, AppState)> {
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
    *state.runtime_bridge.lock().await = Some(Arc::new(Mutex::new(bridge)));
    #[cfg(unix)]
    let daemon = daemon_socket::bind_captured_owner(state.clone(), owner, recovery).await?;
    #[cfg(windows)]
    let daemon = {
        anyhow::ensure!(
            recovery.is_none(),
            "Windows stale-owner recovery is not qualified; refusing fallback"
        );
        daemon_socket::bind_captured_owner(state.clone(), owner).await?
    };
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

/// Cancellation-safe, allocation-bounded newline framing shared by every
/// local control transport. Consumed partial bytes remain owned by the reader.
pub struct BoundedLines<R> {
    reader: R,
    pending: Vec<u8>,
}

impl<R: AsyncBufRead + Unpin> BoundedLines<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            pending: Vec::new(),
        }
    }

    pub fn into_inner(self) -> R {
        self.reader
    }
    pub async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                if self.pending.is_empty() {
                    return Ok(None);
                }
                if self.pending.len() > MAX_RUNTIME_IMAGE_BODY_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "control request exceeds the 8 MiB transport limit",
                    ));
                }
                let bytes = std::mem::take(&mut self.pending);
                return String::from_utf8(bytes).map(Some).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "control frame is not UTF-8",
                    )
                });
            }
            let newline = available.iter().position(|&byte| byte == b'\n');
            let count = newline.unwrap_or(available.len());
            if count > (MAX_RUNTIME_IMAGE_BODY_BYTES + 1).saturating_sub(self.pending.len()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "control request exceeds the 8 MiB transport limit",
                ));
            }
            // Do not let Vec's geometric growth double an almost-full frame.
            self.pending.reserve_exact(count);
            self.pending.extend_from_slice(&available[..count]);
            self.reader.consume(count + usize::from(newline.is_some()));
            if self.pending.len() > MAX_RUNTIME_IMAGE_BODY_BYTES
                && self.pending.last() != Some(&b'\r')
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "control request exceeds the 8 MiB transport limit",
                ));
            }
            if newline.is_some() {
                if self.pending.last() == Some(&b'\r') {
                    self.pending.pop();
                }
                let bytes = std::mem::take(&mut self.pending);
                return String::from_utf8(bytes).map(Some).map_err(|_| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "control frame is not UTF-8",
                    )
                });
            }
        }
    }
}

/// The stdio JSON-RPC loop, generic over its transport so it can be driven by
/// a duplex pipe in tests rather than the process's real stdin/stdout.
async fn run_stdio_loop<R, W, C>(
    state: &AppState,
    mut reader: BoundedLines<R>,
    mut writer: W,
    policy: StdioLoopPolicy,
    // Dropped the moment input closes, not when the in-flight turn ends. The
    // socket transport passes its owner claim here: a `thread/message` can run
    // for minutes, and an owner who disconnects mid-turn must not keep the
    // daemon claimed for the rest of it, or a relaunched client is locked out
    // with `daemon_already_claimed` and cannot even shut the daemon down.
    // Process stdio has no claim and passes `None`.
    mut input_claim: Option<C>,
) -> Result<StdioLoopExit>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    C: Send,
{
    // Work that arrived while a turn was streaming. The turn owns the writer
    // for its whole duration, so these wait for it rather than interleaving
    // into the middle of a response.
    let mut pending: VecDeque<(PendingStdioWork, usize)> = VecDeque::new();
    let mut pending_bytes = 0usize;
    let mut stdin_open = true;

    loop {
        let next = pending.pop_front().map(|(work, bytes)| {
            pending_bytes -= bytes;
            work
        });
        let request = match next {
            Some(PendingStdioWork::Response(response)) => {
                write_stdio_line(&mut writer, &response).await?;
                continue;
            }
            Some(PendingStdioWork::Request(request)) => request,
            None => {
                if !stdin_open {
                    return Ok(StdioLoopExit::InputClosed);
                }
                let Some(line) = reader.next_line().await? else {
                    return Ok(StdioLoopExit::InputClosed);
                };
                match parse_stdio_line(&line) {
                    ParsedStdioLine::Blank => continue,
                    ParsedStdioLine::Rejected(response) => {
                        write_stdio_line(&mut writer, &response).await?;
                        continue;
                    }
                    ParsedStdioLine::Request(request) => request,
                }
            }
        };

        if is_control_detach(&request) {
            drop(input_claim.take());
            return Ok(StdioLoopExit::InputClosed);
        }
        let id = request.id.clone();
        if request.method == "shutdown" && policy.shutdown == ShutdownAuthority::Denied {
            write_stdio_line(
                &mut writer,
                &jsonrpc_error(id, JsonRpcError::not_daemon_owner()),
            )
            .await?;
            continue;
        }
        let dispatched = if request.method == "thread/message" {
            // A turn can run for minutes. Keep reading stdin while it streams
            // so an interrupt (or a shutdown) can actually reach it — with a
            // plain `await` here, nothing could be read until it finished.
            let dispatch = dispatch_stdio_request_with_writer(
                state,
                &mut writer,
                &request.method,
                request.params,
                policy.transport,
            );
            tokio::pin!(dispatch);
            loop {
                tokio::select! {
                    outcome = &mut dispatch => break outcome,
                    line = reader.next_line(), if stdin_open => {
                        match line? {
                            None => {
                                stdin_open = false;
                                // Release the claim here, not after `dispatch`
                                // resolves.
                                drop(input_claim.take());
                            }
                            Some(line) => {
                                if matches!(parse_stdio_line(&line), ParsedStdioLine::Request(ref request) if is_control_detach(request)) {
                                    stdin_open=false; drop(input_claim.take());
                                } else {handle_line_during_turn(state, &line, &mut pending, &mut pending_bytes, policy).await?;}
                            }
                        }
                    }
                }
            }
        } else {
            dispatch_stdio_request_with_writer(
                state,
                &mut writer,
                &request.method,
                request.params,
                policy.transport,
            )
            .await
        };

        match dispatched {
            Ok(dispatch) => {
                write_stdio_line(&mut writer, &jsonrpc_result(id, dispatch.result)).await?;
                if dispatch.should_exit {
                    return Ok(StdioLoopExit::Shutdown);
                }
            }
            Err(err) => {
                write_stdio_line(&mut writer, &jsonrpc_error(id, err)).await?;
            }
        }
    }
}

/// Work deferred until a streaming turn releases the writer.
enum PendingStdioWork {
    /// Already answered (an interrupt acted immediately); just needs writing.
    Response(Value),
    /// Not started yet; runs normally once the turn is done.
    Request(JsonRpcRequest),
}

/// Fixed retained queue limits; exhaustion is a visible connection refusal
/// before deferred work is admitted. Already-running effects are never replayed.
fn queue_stdio_work(
    pending: &mut VecDeque<(PendingStdioWork, usize)>,
    bytes: &mut usize,
    work: PendingStdioWork,
    input_bytes: usize,
) -> Result<()> {
    let retained = input_bytes
        .checked_add(1024)
        .context("control queue size overflow")?;
    anyhow::ensure!(
        pending.len() < 64 && retained <= MAX_RUNTIME_IMAGE_BODY_BYTES.saturating_sub(*bytes),
        "control queue limit exceeded; deferred request not admitted, in-flight outcomes may be uncertain"
    );
    *bytes += retained;
    pending.push_back((work, retained));
    Ok(())
}

enum ParsedStdioLine {
    Blank,
    Request(JsonRpcRequest),
    Rejected(Value),
}

fn parse_stdio_line(line: &str) -> ParsedStdioLine {
    if line.len() > MAX_RUNTIME_IMAGE_BODY_BYTES {
        return ParsedStdioLine::Rejected(jsonrpc_error(
            None,
            JsonRpcError::invalid_params("request exceeds the 8 MiB transport limit"),
        ));
    }
    if line.trim().is_empty() {
        return ParsedStdioLine::Blank;
    }
    let request: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => {
            return ParsedStdioLine::Rejected(jsonrpc_error(
                None,
                JsonRpcError::parse_error(format!("invalid json: {err}")),
            ));
        }
    };
    if request
        .jsonrpc
        .as_deref()
        .is_some_and(|version| version != "2.0")
    {
        return ParsedStdioLine::Rejected(jsonrpc_error(
            request.id,
            JsonRpcError::invalid_request("jsonrpc version must be 2.0"),
        ));
    }
    ParsedStdioLine::Request(request)
}

/// Triage a request that arrived mid-turn.
///
/// Cancellation is the whole point of reading here, so `thread/interrupt`
/// runs immediately and only its reply waits for the writer. `shutdown` also
/// interrupts immediately — otherwise it would block on the bridge mutex the
/// turn is holding — and then queues so the turn can unwind first. Everything
/// else simply queues: it was never urgent, and running it now would race the
/// turn for the writer.
async fn handle_line_during_turn(
    state: &AppState,
    line: &str,
    pending: &mut VecDeque<(PendingStdioWork, usize)>,
    pending_bytes: &mut usize,
    policy: StdioLoopPolicy,
) -> Result<()> {
    let request = match parse_stdio_line(line) {
        ParsedStdioLine::Blank => return Ok(()),
        ParsedStdioLine::Rejected(response) => {
            return queue_stdio_work(
                pending,
                pending_bytes,
                PendingStdioWork::Response(response),
                line.len(),
            );
        }
        ParsedStdioLine::Request(request) => request,
    };

    match request.method.as_str() {
        "thread/interrupt" => {
            let id = request.id.clone();
            let response = match parse_params::<ThreadInterruptParams>(params_or_object(
                request.params.clone(),
            )) {
                Ok(parsed) => match interrupt_stdio_turn(state, &parsed.thread_id).await {
                    Ok(interrupted) => jsonrpc_result(
                        id,
                        json!({ "thread_id": parsed.thread_id, "interrupted": interrupted }),
                    ),
                    Err(err) => jsonrpc_error(id, err),
                },
                Err(err) => jsonrpc_error(id, err),
            };
            queue_stdio_work(
                pending,
                pending_bytes,
                PendingStdioWork::Response(response),
                line.len(),
            )?;
        }
        "shutdown" if policy.shutdown == ShutdownAuthority::Denied => {
            // A non-owner may not even interrupt the live turns: that is the
            // first half of what shutdown does.
            queue_stdio_work(
                pending,
                pending_bytes,
                PendingStdioWork::Response(jsonrpc_error(
                    request.id,
                    JsonRpcError::not_daemon_owner(),
                )),
                line.len(),
            )?;
        }
        "shutdown" => {
            let _ = interrupt_all_stdio_turns(state).await;
            queue_stdio_work(
                pending,
                pending_bytes,
                PendingStdioWork::Request(request),
                line.len(),
            )?;
        }
        _ => queue_stdio_work(
            pending,
            pending_bytes,
            PendingStdioWork::Request(request),
            line.len(),
        )?,
    }
    Ok(())
}

fn is_control_detach(request: &JsonRpcRequest) -> bool {
    request.jsonrpc.as_deref() == Some("2.0")
        && request.id.is_none()
        && request.method == "daemon/input_closed"
        && request
            .params
            .as_object()
            .is_some_and(serde_json::Map::is_empty)
}

async fn write_stdio_line<W: AsyncWrite + Unpin>(writer: &mut W, response: &Value) -> Result<()> {
    writer.write_all(&serde_json::to_vec(response)?).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

async fn healthz() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "protocol": "v2",
        "service": legacy_deepseek_compat::SERVICE_NAME
    }))
}

/// Render a routing failure as a typed HTTP error body.
///
/// Deliberately *not* a success-shaped payload with the error stuffed into a
/// content field: a client must be able to tell "the model said this" from
/// "nothing ran".
fn http_error_from_jsonrpc(err: JsonRpcError) -> (StatusCode, Json<Value>) {
    let (status, code) = match err.code {
        -32600 | -32602 => (StatusCode::BAD_REQUEST, "invalid_request"),
        THREAD_NOT_FOUND_CODE => (StatusCode::NOT_FOUND, "thread_not_found"),
        RUNTIME_UNAVAILABLE_CODE => (StatusCode::SERVICE_UNAVAILABLE, "runtime_unavailable"),
        _ => (StatusCode::INTERNAL_SERVER_ERROR, "internal_error"),
    };
    (
        status,
        Json(json!({
            "error": {
                "code": code,
                "jsonrpc_code": err.code,
                "message": err.message,
            }
        })),
    )
}

async fn thread_handler(State(state): State<AppState>, Json(req): Json<ThreadRequest>) -> Response {
    // A message is a turn, and turns belong to the runtime — not to the
    // bookkeeping `Runtime` behind the other thread operations. This mirrors
    // the interception stdio `thread/message` has always done.
    if let ThreadRequest::Message {
        thread_id,
        input,
        images,
        max_output_tokens,
    } = req
    {
        return match run_http_thread_message(&state, thread_id, input, images, max_output_tokens)
            .await
        {
            Ok(res) => (StatusCode::OK, Json(res)).into_response(),
            Err(err) => http_error_from_jsonrpc(err).into_response(),
        };
    }
    match handle_thread_request(&state, req).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(err) => http_error_from_jsonrpc(err).into_response(),
    }
}

/// `POST /prompt` — runs a genuine model turn through the runtime bridge.
///
/// Note what this handler does *not* do: it never takes the `Runtime` write
/// lock. The old implementation held it across the whole request while doing
/// no model work at all.
async fn prompt_handler(State(state): State<AppState>, Json(req): Json<PromptRequest>) -> Response {
    let mut sink = tokio::io::sink();
    match run_prompt_turn(&state, &mut sink, req).await {
        Ok(res) => (StatusCode::OK, Json(res)).into_response(),
        Err(err) => http_error_from_jsonrpc(err).into_response(),
    }
}

async fn app_handler(
    State(state): State<AppState>,
    Json(req): Json<AppRequest>,
) -> (StatusCode, Json<AppResponse>) {
    let response = process_app_request(&state, req, AppTransport::Http).await;
    (app_response_status(&response), Json(response))
}

fn app_response_status(response: &AppResponse) -> StatusCode {
    if response.ok {
        return StatusCode::OK;
    }
    if response.data.get("request_id").is_some() {
        StatusCode::CONFLICT
    } else if response
        .data
        .get("error")
        .and_then(Value::as_str)
        .is_some_and(|err| err.starts_with(CONFIG_LOAD_ERROR) || err.starts_with(CONFIG_SAVE_ERROR))
    {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::BAD_REQUEST
    }
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
        runtime_thread_map: Arc::new(Mutex::new(HashMap::new())),
        stdio_thread_hints: Arc::new(Mutex::new(HashMap::new())),
        in_flight_turns: Arc::new(Mutex::new(HashMap::new())),
    })
}

fn resolve_auth_token(options: &AppServerOptions) -> Result<Option<String>> {
    let configured = options.auth_token.as_ref().map(|token| token.trim());
    if let Some(token) = configured
        && token.is_empty()
    {
        bail!("app-server auth token cannot be empty");
    }
    let has_explicit_token = configured.is_some();

    if options.insecure_no_auth {
        if !options.listen.ip().is_loopback() {
            bail!("refusing unauthenticated app-server bind on non-loopback address");
        }
        eprintln!("warning: app-server HTTP auth disabled by --insecure-no-auth");
        return Ok(None);
    }

    if !has_explicit_token && !options.listen.ip().is_loopback() {
        bail!(
            "refusing non-loopback app-server bind without explicit auth token; pass --auth-token or set CODEWHALE_APP_SERVER_TOKEN"
        );
    }

    let token = configured
        .map(str::to_string)
        .unwrap_or_else(|| format!("cwapp_{}", Uuid::new_v4().simple()));
    for line in app_server_auth_status_lines(has_explicit_token) {
        eprintln!("{line}");
    }
    Ok(Some(token))
}

fn app_server_auth_status_lines(has_explicit_token: bool) -> Vec<&'static str> {
    if has_explicit_token {
        return vec!["app-server auth: bearer token required for HTTP routes."];
    }
    vec![
        "app-server auth: generated bearer token for this process (not printed).",
        "  Pass --auth-token or set CODEWHALE_APP_SERVER_TOKEN when another client needs to connect.",
    ]
}

fn cors_layer(extra_origins: &[String]) -> CorsLayer {
    let mut origins: Vec<HeaderValue> = DEFAULT_CORS_ORIGINS
        .iter()
        .filter_map(|origin| HeaderValue::from_str(origin).ok())
        .collect();
    for raw in extra_origins {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match HeaderValue::from_str(trimmed) {
            Ok(value) if !origins.contains(&value) => origins.push(value),
            Ok(_) => {}
            Err(err) => {
                eprintln!("warning: ignoring invalid app-server CORS origin `{trimmed}`: {err}")
            }
        }
    }

    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE])
}

async fn require_app_server_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.auth_token.as_deref() else {
        return next.run(req).await;
    };
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.strip_prefix("Bearer "))
        .is_some_and(|token| {
            codewhale_core::secret_eq::constant_time_eq(token.as_bytes(), expected.as_bytes())
        });

    if authorized {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": {
                    "message": "app-server bearer token required",
                    "status": StatusCode::UNAUTHORIZED.as_u16(),
                }
            })),
        )
            .into_response()
    }
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

async fn handle_thread_request(
    state: &AppState,
    req: ThreadRequest,
) -> std::result::Result<ThreadResponse, JsonRpcError> {
    thread_control::handle(state, req).await
}

/// One turn's worth of routing decisions, shared by every surface that runs
/// a turn through the bridge.
struct RuntimeTurnInput<'a> {
    input: &'a str,
    images: &'a [RuntimeImageInput],
    max_output_tokens: Option<std::num::NonZeroU32>,
    expected_workspace: Option<&'a Path>,
}

struct BridgedTurn<'a> {
    max_output_tokens: Option<std::num::NonZeroU32>,
    /// Client-facing thread id; the bridge maps it to a runtime thread.
    thread_key: &'a str,
    input: &'a str,
    images: &'a [RuntimeImageInput],
    /// Model for the runtime thread when this call is the one that creates
    /// it. An existing thread keeps the model it was created with.
    model_override: Option<String>,
    /// Publish the live turn so a concurrent `thread/interrupt` can cancel
    /// it. Only stdio has a mid-turn channel, so only stdio sets this.
    interruptible: bool,
    /// Forget the thread mapping once the turn ends. Set for one-shot
    /// prompts, whose synthetic thread key no client can name again.
    ephemeral: bool,
    /// Refuse a `thread_key` that is neither mapped, durably linked, nor a
    /// persisted thread, instead of minting an empty runtime thread for it.
    /// Set by `thread/message`, whose ids come from `thread/create`.
    require_known_thread: bool,
}

/// Execute exactly one turn on the real runtime.
///
/// This is the only way any app-server surface runs a model: `/prompt`,
/// `prompt/request`, `prompt/run`, stdio `thread/message`, and HTTP `/thread`
/// messages all land here. There is no local fallback that fabricates a
/// response — if the runtime cannot be reached the caller gets
/// [`JsonRpcError::runtime_unavailable`] and nothing is written to history.
async fn run_bridged_turn<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    turn: BridgedTurn<'_>,
    transcript: Option<&mut TurnTranscript>,
) -> std::result::Result<Value, JsonRpcError> {
    let mut hint = {
        let hints = state.stdio_thread_hints.lock().await;
        hints.get(turn.thread_key).cloned()
    };
    if let Some(model) = turn.model_override {
        hint.get_or_insert_with(RuntimeThreadHint::default).model = Some(model);
    }
    // Durable compatibility IDs resolve through full canonical history and
    // exact owner receipts before the existing turn transport can begin.
    let resolved = if turn.ephemeral {
        None
    } else {
        Some(
            thread_control::resolve(state, turn.thread_key, true)
                .await
                .map_err(|error| thread_control::rpc_error(error, Some(turn.thread_key)))?,
        )
    };
    if turn.require_known_thread && resolved.is_none() {
        return Err(JsonRpcError::thread_not_found(turn.thread_key));
    }
    if let Some((_, workspace)) = resolved.as_ref() {
        hint.get_or_insert_with(RuntimeThreadHint::default)
            .workspace = Some(workspace.clone());
    } else if let Some(workspace) = state.frontend_workspace.as_ref() {
        hint.get_or_insert_with(RuntimeThreadHint::default)
            .workspace
            .get_or_insert_with(|| workspace.clone());
    }
    // Retain the acknowledged/observed scope across all transport awaits.
    // The owning Runtime rechecks this assertion at its final turn admission.
    let expected_workspace = hint.as_ref().and_then(|hint| hint.workspace.clone());
    let mut bridge = acquire_live_runtime_bridge(state).await?;
    let mut thread_map = state.runtime_thread_map.clone().lock_owned().await;
    if let Some((id, _)) = resolved {
        thread_map.insert(turn.thread_key.to_string(), id);
    }
    if turn.max_output_tokens.is_some() {
        let info = bridge
            .request_json(
                bridge.authed(
                    bridge
                        .client
                        .get(format!("{}/v1/runtime/info", bridge.base_url)),
                ),
            )
            .await
            .map_err(|error| JsonRpcError::runtime_unavailable(error.to_string()))?;
        if info
            .pointer("/capabilities/turn_output_token_limit")
            .and_then(Value::as_bool)
            != Some(true)
        {
            return Err(JsonRpcError::invalid_params(
                "Runtime does not support maxOutputTokens",
            ));
        }
        if !thread_map.contains_key(turn.thread_key) {
            bridge
                .require_output_limited_model(hint.as_ref().and_then(|hint| hint.model.as_deref()))
                .await
                .map_err(|error| JsonRpcError::invalid_params(error.to_string()))?;
        }
    }
    let runtime_thread_id = bridge
        .ensure_runtime_thread(&mut thread_map, turn.thread_key, hint)
        .await
        .map_err(|error| JsonRpcError::runtime_unavailable(error.to_string()))?;
    drop(thread_map);
    let registration = turn
        .interruptible
        .then(|| (state.in_flight_turns.clone(), turn.thread_key.to_string()));
    let result = bridge
        .message_thread(
            &runtime_thread_id,
            RuntimeTurnInput {
                input: turn.input,
                images: turn.images,
                max_output_tokens: turn.max_output_tokens,
                expected_workspace: expected_workspace.as_deref(),
            },
            writer,
            registration,
            transcript,
        )
        .await;
    if turn.ephemeral {
        // Drop the mapping while we still hold the bridge lock, so a
        // long-lived app-server does not accumulate one entry per one-shot
        // prompt.
        let mut thread_map = state.runtime_thread_map.lock().await;
        bridge.forget_thread(&mut thread_map, turn.thread_key);
    }
    result.map_err(|err| JsonRpcError::internal(err.to_string()))
}

/// Run a prompt as a genuine model turn and return what the model actually
/// said.
///
/// `writer` receives the same streaming frames stdio `thread/message` emits;
/// HTTP callers pass a sink and read the frames back out of
/// [`PromptResponse::events`].
async fn run_prompt_turn<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    req: PromptRequest,
) -> std::result::Result<PromptResponse, JsonRpcError> {
    if req.prompt.trim().is_empty() {
        return Err(JsonRpcError::invalid_params("prompt must not be empty"));
    }
    // The turn engine has no threadless mode, so a prompt without a thread
    // gets a fresh one. Keying it on a uuid keeps a one-shot prompt out of
    // any caller's history and out of the way of concurrent prompts.
    let ephemeral = req.thread_id.is_none();
    let thread_key = req
        .thread_id
        .clone()
        .unwrap_or_else(|| format!("prompt-{}", Uuid::new_v4()));

    let mut transcript = TurnTranscript::default();
    run_bridged_turn(
        state,
        writer,
        BridgedTurn {
            max_output_tokens: req.max_output_tokens,
            thread_key: &thread_key,
            input: &req.prompt,
            images: &req.images,
            model_override: req.model.clone(),
            // `thread/interrupt` addresses client-facing thread ids. A
            // one-shot prompt has none to hand back, and a caller-supplied
            // thread id is already interruptible through `thread/message`.
            interruptible: false,
            ephemeral,
            // A caller-chosen `/prompt` thread key keeps its conversation
            // but need not name a `thread/create` thread.
            require_known_thread: false,
        },
        Some(&mut transcript),
    )
    .await?;

    // Report the model the runtime actually ran, never a locally resolved
    // guess. The fallbacks only matter for a runtime that omits the field.
    let model = match transcript.model {
        Some(model) => model,
        None => match req.model {
            Some(model) => model,
            None => state
                .config
                .read()
                .await
                .model
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        },
    };

    Ok(PromptResponse {
        output: transcript.text,
        model,
        events: transcript.events,
    })
}

async fn handle_prompt_request<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    req: PromptRequest,
) -> std::result::Result<PromptResponse, JsonRpcError> {
    run_prompt_turn(state, writer, req).await
}

/// HTTP `/thread` with a `Message` body: same engine as stdio
/// `thread/message`, but the turn is collected rather than streamed because
/// this transport is request/response.
async fn run_http_thread_message(
    state: &AppState,
    thread_id: String,
    input: String,
    images: Vec<RuntimeImageInput>,
    max_output_tokens: Option<std::num::NonZeroU32>,
) -> std::result::Result<ThreadResponse, JsonRpcError> {
    let mut transcript = TurnTranscript::default();
    let mut sink = tokio::io::sink();
    let result = run_bridged_turn(
        state,
        &mut sink,
        BridgedTurn {
            max_output_tokens,
            thread_key: &thread_id,
            input: &input,
            images: &images,
            model_override: None,
            interruptible: false,
            ephemeral: false,
            require_known_thread: true,
        },
        Some(&mut transcript),
    )
    .await?;

    Ok(ThreadResponse {
        thread_id,
        // The turn ran to a terminal state before this response was built,
        // which is exactly what the old `accepted` did not mean.
        status: "completed".to_string(),
        thread: None,
        threads: Vec::new(),
        goal: None,
        model: transcript.model,
        model_provider: None,
        cwd: None,
        approval_policy: None,
        sandbox: None,
        events: transcript.events,
        data: result.get("data").cloned().unwrap_or_else(|| json!({})),
    })
}

async fn handle_stdio_thread_message<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    parsed: ThreadMessageParams,
) -> std::result::Result<Value, JsonRpcError> {
    let mut result = run_bridged_turn(
        state,
        writer,
        BridgedTurn {
            max_output_tokens: parsed.max_output_tokens,
            thread_key: &parsed.thread_id,
            input: &parsed.input,
            images: &parsed.images,
            model_override: None,
            interruptible: true,
            ephemeral: false,
            require_known_thread: true,
        },
        None,
    )
    .await?;
    if let Some(object) = result.as_object_mut() {
        object.insert("thread_id".to_string(), Value::String(parsed.thread_id));
    }
    Ok(result)
}

/// Resuming, forking, archiving or unarchiving a thread the runtime reports
/// as `missing` must fail with a named not-found error. Recording the null model/workspace of that
/// response as a stdio hint would clobber any previously cached hint for
/// the same thread id (#5171).
fn ensure_thread_found(response: &ThreadResponse) -> std::result::Result<(), JsonRpcError> {
    if response.status == "missing" {
        return Err(JsonRpcError::thread_not_found(&response.thread_id));
    }
    Ok(())
}

async fn record_stdio_thread_hint(state: &AppState, response: &ThreadResponse) {
    let mut hints = state.stdio_thread_hints.lock().await;
    hints.insert(
        response.thread_id.clone(),
        RuntimeThreadHint {
            model: response.model.clone(),
            workspace: response.cwd.clone(),
        },
    );
}

/// Historical cold-child cache comparator. It has no production producer.
#[cfg(all(test, unix))]
async fn acquire_historical_runtime_bridge<F, Fut>(
    state: &AppState,
    start: F,
) -> std::result::Result<SharedRuntimeBridge, JsonRpcError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<RuntimeBridge>>,
{
    if let Some(bridge) = state.runtime_bridge.lock().await.as_ref() {
        return Ok(bridge.clone());
    }
    if state.captured_owner.is_some() {
        return Err(JsonRpcError::runtime_unavailable(
            "the captured Runtime owner is unavailable; refusing another owner",
        ));
    }
    let bridge =
        Arc::new(Mutex::new(start().await.map_err(|err| {
            JsonRpcError::runtime_unavailable(err.to_string())
        })?));
    let mut slot = state.runtime_bridge.lock().await;
    // Prefer a bridge cached by a concurrent caller while we were spawning;
    // dropping our unused one kills the extra child via `Drop`.
    Ok(slot.get_or_insert_with(|| bridge.clone()).clone())
}

/// Use the exact bridge installed by the held canonical owner. No process,
/// store or transcript can be created when this captured connection is absent.
async fn acquire_runtime_bridge(
    state: &AppState,
) -> std::result::Result<SharedRuntimeBridge, JsonRpcError> {
    state
        .runtime_bridge
        .lock()
        .await
        .as_ref()
        .cloned()
        .ok_or_else(|| {
            JsonRpcError::runtime_unavailable(
                "the captured Runtime owner is unavailable; refusing another owner",
            )
        })
}

async fn acquire_live_runtime_bridge(
    state: &AppState,
) -> std::result::Result<tokio::sync::OwnedMutexGuard<RuntimeBridge>, JsonRpcError> {
    Ok(acquire_runtime_bridge(state).await?.lock_owned().await)
}

/// Historical respawn comparison; all actual frontends use the held owner.
#[cfg(all(test, unix))]
async fn acquire_live_runtime_bridge_with<F, Fut>(
    state: &AppState,
    start: F,
) -> std::result::Result<tokio::sync::OwnedMutexGuard<RuntimeBridge>, JsonRpcError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<RuntimeBridge>>,
{
    for _ in 0..2 {
        let shared = acquire_historical_runtime_bridge(state, &start).await?;
        let mut bridge = shared.clone().lock_owned().await;
        if !bridge.child_exited() {
            return Ok(bridge);
        }
        drop(bridge);
        let mut slot = state.runtime_bridge.lock().await;
        // Evict only the dead bridge: a concurrent caller may already have
        // replaced it with a live one.
        if slot
            .as_ref()
            .is_some_and(|cached| Arc::ptr_eq(cached, &shared))
        {
            *slot = None;
        }
    }
    Err(JsonRpcError::runtime_unavailable(
        "runtime API bridge exited immediately after starting",
    ))
}

/// Ask the runtime to interrupt a turn that is streaming right now.
///
/// Everything this needs was copied out of the bridge when the turn started,
/// so it never touches the bridge mutex the turn is holding. Returns whether
/// a live turn was found for `thread_id`.
/// Interrupt one in-flight turn over HTTP, from an owned snapshot.
///
/// Split from [`interrupt_stdio_turn`] so teardown paths can run many
/// concurrently (#6211 R8b) — each future owns its snapshot and never holds
/// the turn registry across the request.
async fn interrupt_turn_request(turn: &InFlightTurn) -> std::result::Result<bool, JsonRpcError> {
    let mut request = codewhale_release::platform_http_client_builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| JsonRpcError::internal(err.to_string()))?
        .post(format!(
            "{}/v1/threads/{}/turns/{}/interrupt",
            turn.base_url, turn.runtime_thread_id, turn.turn_id
        ));
    if let Some(token) = turn.auth_token.as_deref() {
        request = request.bearer_auth(token);
    }
    request
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|err| JsonRpcError::internal(format!("interrupt failed: {err}")))?;
    Ok(true)
}

async fn interrupt_stdio_turn(
    state: &AppState,
    thread_id: &str,
) -> std::result::Result<bool, JsonRpcError> {
    let Some(turn) = state.in_flight_turns.lock().await.get(thread_id).cloned() else {
        return Ok(false);
    };
    interrupt_turn_request(&turn).await
}

/// Interrupt every in-flight turn concurrently, reporting how many were
/// reached (#6211 R8b).
///
/// The teardown paths used to await each turn's interrupt in sequence, so
/// their latency grew with the number of live turns — up to the 10s
/// per-request timeout apiece. Each interrupt now owns its snapshot and runs
/// as an independent task; individual failures are ignored exactly as the
/// sequential loop ignored them, and the registry lock is never held across
/// the requests.
async fn interrupt_all_stdio_turns(state: &AppState) -> usize {
    let turns: Vec<InFlightTurn> = {
        let map = state.in_flight_turns.lock().await;
        map.values().cloned().collect()
    };
    let mut set = tokio::task::JoinSet::new();
    for turn in turns {
        set.spawn(async move { interrupt_turn_request(&turn).await });
    }
    let mut interrupted = 0usize;
    while let Some(joined) = set.join_next().await {
        if matches!(joined, Ok(Ok(true))) {
            interrupted += 1;
        }
    }
    interrupted
}

/// Historical config/cache comparator. Captured owners retain their bridge.
#[cfg(test)]
async fn invalidate_runtime_bridge(state: &AppState) {
    if state.captured_owner.is_some() {
        return;
    }
    let mut bridge = state.runtime_bridge.lock().await;
    *bridge = None;
}

impl RuntimeBridge {
    /// The child binds an ephemeral loopback port itself (`--port 0`) and
    /// reports it on stdout; the parent never reserves a port for it to race.
    #[cfg(test)]
    fn runtime_command(config_path: Option<&Path>, auth_token: &str) -> Result<Command> {
        let current_exe = std::env::current_exe().ok();
        let mut command = if let Some(path) = current_exe {
            Command::new(path)
        } else {
            Command::new("codewhale")
        };
        // Pass the runtime auth token out-of-band via env (not argv) so local
        // `ps` cannot read credential material from the child command line.
        // The TUI/runtime server already accepts CODEWHALE_RUNTIME_TOKEN /
        // DEEPSEEK_RUNTIME_TOKEN when --auth-token is absent.
        command
            .arg("app-server")
            .arg("--http")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg("0")
            .env("CODEWHALE_RUNTIME_TOKEN", auth_token)
            .env("DEEPSEEK_RUNTIME_TOKEN", auth_token)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if let Some(config_path) = config_path {
            command.arg("--config").arg(config_path);
        }
        Ok(command)
    }

    fn authed(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.auth_token.as_deref() {
            Some(token) => builder.bearer_auth(token),
            None => builder,
        }
    }

    async fn request_json(&self, builder: reqwest::RequestBuilder) -> Result<Value> {
        thread_control::read_json_response(builder.send().await?).await
    }

    /// Read the existing Runtime catalog before a one-shot prompt can create
    /// its thread. Existing threads are checked by canonical turn admission.
    async fn require_output_limited_model(&self, requested_model: Option<&str>) -> Result<()> {
        let providers = self
            .request_json(self.authed(self.client.get(format!("{}/v1/providers", self.base_url))))
            .await?;
        let current = providers
            .get("current")
            .and_then(Value::as_str)
            .context("Runtime provider is unavailable")?;
        let provider = providers
            .get("providers")
            .and_then(Value::as_array)
            .and_then(|providers| {
                providers
                    .iter()
                    .find(|provider| provider.get("id").and_then(Value::as_str) == Some(current))
            })
            .context("Runtime provider is unavailable")?;
        let model = requested_model
            .or_else(|| provider.get("default_model").and_then(Value::as_str))
            .context("maxOutputTokens requires an exact model")?;
        if model.trim().is_empty() || model.eq_ignore_ascii_case("auto") {
            bail!("maxOutputTokens requires an exact model");
        }
        if !current
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            bail!("Runtime provider identity is invalid");
        }
        let mut cursor = None;
        let mut seen = std::collections::HashSet::new();
        loop {
            let mut url =
                reqwest::Url::parse(&format!("{}/v1/providers/{current}/models", self.base_url))?;
            url.query_pairs_mut().append_pair("limit", "250");
            if let Some(cursor) = cursor.as_deref() {
                url.query_pairs_mut().append_pair("cursor", cursor);
            }
            let catalog = self.request_json(self.authed(self.client.get(url))).await?;
            if let Some(entry) =
                catalog
                    .get("models")
                    .and_then(Value::as_array)
                    .and_then(|models| {
                        models
                            .iter()
                            .find(|entry| entry.get("id").and_then(Value::as_str) == Some(model))
                    })
            {
                if entry.get("output_token_limit").and_then(Value::as_str) == Some("supported") {
                    return Ok(());
                }
                bail!("The selected Runtime model does not support maxOutputTokens");
            }
            let next = catalog
                .get("nextCursor")
                .and_then(Value::as_str)
                .context("Output-limit support is unknown for the selected Runtime model")?;
            if !seen.insert(next.to_string()) {
                bail!("Runtime model catalog cursor repeated");
            }
            cursor = Some(next.to_string());
        }
    }

    /// Resolve `stdio_thread_id` to a runtime thread, minting one only when
    /// `thread_map` has no entry. The map lives on [`AppState`] and outlives
    /// this bridge, so a thread created under a previous child keeps its id
    /// here as long as the store it was persisted to is shared (#6246).
    async fn ensure_runtime_thread(
        &mut self,
        thread_map: &mut HashMap<String, String>,
        stdio_thread_id: &str,
        hint: Option<RuntimeThreadHint>,
    ) -> Result<String> {
        if let Some(runtime_thread_id) = thread_map.get(stdio_thread_id) {
            return Ok(runtime_thread_id.clone());
        }
        let hint = hint.unwrap_or_default();
        let runtime_thread_id = self
            .create_runtime_thread(hint.model, hint.workspace)
            .await?;
        thread_map.insert(stdio_thread_id.to_string(), runtime_thread_id.clone());
        Ok(runtime_thread_id)
    }

    /// Drop a thread mapping (and its seq cursor) once no caller can name
    /// the client-facing key again.
    fn forget_thread(&mut self, thread_map: &mut HashMap<String, String>, stdio_thread_id: &str) {
        if let Some(runtime_thread_id) = thread_map.remove(stdio_thread_id) {
            self.last_seq_by_thread.remove(&runtime_thread_id);
        }
    }

    async fn create_runtime_thread(
        &mut self,
        model: Option<String>,
        workspace: Option<PathBuf>,
    ) -> Result<String> {
        let record = self
            .request_json(
                self.authed(self.client.post(format!("{}/v1/threads", self.base_url)))
                    .json(&json!({
                        "model": model,
                        "workspace": workspace,
                        "mode": "agent",
                        "archived": false,
                    })),
            )
            .await?;
        let thread_id = extract_runtime_thread_id(&record)?.to_string();
        self.last_seq_by_thread
            .entry(thread_id.clone())
            .or_insert(0);
        Ok(thread_id)
    }

    /// Run one turn to completion, streaming its events to `writer`.
    ///
    /// `registration` is `Some` on the stdio path: it publishes the live turn
    /// so an `thread/interrupt` arriving mid-stream can reach the runtime
    /// without waiting on the bridge mutex this call holds.
    async fn message_thread<W: AsyncWrite + Unpin>(
        &mut self,
        thread_id: &str,
        input: RuntimeTurnInput<'_>,
        writer: &mut W,
        registration: Option<(TurnRegistry, String)>,
        mut transcript: Option<&mut TurnTranscript>,
    ) -> Result<Value> {
        let RuntimeTurnInput {
            input,
            images,
            max_output_tokens,
            expected_workspace,
        } = input;
        let mut request = json!({ "prompt": input });
        if let Some(workspace) = expected_workspace {
            request["expected_workspace"] = json!(workspace);
        }
        if !images.is_empty() {
            let info = self
                .request_json(
                    self.authed(
                        self.client
                            .get(format!("{}/v1/runtime/info", self.base_url)),
                    ),
                )
                .await?;
            if info
                .pointer("/capabilities/turn_image_inputs")
                .and_then(Value::as_bool)
                != Some(true)
            {
                bail!(
                    "Runtime image input is unavailable; update the Runtime before sending attachments"
                );
            }
            request["images"] = json!(images);
        }
        if let Some(limit) = max_output_tokens {
            request["maxOutputTokens"] = json!(limit);
        }
        let turn = self
            .request_json(
                self.authed(
                    self.client
                        .post(format!("{}/v1/threads/{thread_id}/turns", self.base_url)),
                )
                .json(&request),
            )
            .await?;
        let turn_id = turn
            .pointer("/turn/id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("runtime API turn response missing turn.id"))?
            .to_string();
        let response_id = format!("{thread_id}:{turn_id}");

        if let Some(transcript) = transcript.as_deref_mut() {
            transcript.model = turn
                .pointer("/thread/model")
                .and_then(Value::as_str)
                .map(str::to_string);
            transcript.events.push(EventFrame::ResponseStart {
                response_id: response_id.clone(),
            });
        }

        emit_stdio_event(
            writer,
            json!({
                "type": "response_start",
                "response_id": response_id,
            }),
        )
        .await?;

        // Publish the turn only for the streaming window, and take it back
        // before any `?` below: a turn that has already finished must never
        // look cancellable.
        if let Some((registry, key)) = registration.as_ref() {
            registry.lock().await.insert(
                key.clone(),
                InFlightTurn {
                    base_url: self.base_url.clone(),
                    auth_token: self.auth_token.clone(),
                    runtime_thread_id: thread_id.to_string(),
                    turn_id: turn_id.clone(),
                },
            );
        }

        let since_seq = self.last_seq_by_thread.get(thread_id).copied().unwrap_or(0);
        let stream_result = self
            .stream_turn_events(
                thread_id,
                &turn_id,
                &response_id,
                writer,
                since_seq,
                transcript.as_deref_mut(),
            )
            .await;

        if let Some((registry, key)) = registration.as_ref() {
            registry.lock().await.remove(key);
        }

        if stream_result.is_ok() {
            let _ = emit_stdio_event(
                writer,
                json!({
                    "type": "response_end",
                    "response_id": response_id,
                }),
            )
            .await;
            if let Some(transcript) = transcript {
                transcript.events.push(EventFrame::ResponseEnd {
                    response_id: response_id.clone(),
                });
            }
        } else {
            // The stream broke before `turn.completed` (transport error,
            // oversized or invalid frame, a writer that went away). Ending
            // the response here reported success ahead of the error, and the
            // runtime turn kept running with nothing left able to interrupt
            // it (the registry entry is gone), so the thread refused its next
            // message until the orphan finished. Stop it, best effort, and
            // let the error below be the only outcome the client sees.
            let orphan = InFlightTurn {
                base_url: self.base_url.clone(),
                auth_token: self.auth_token.clone(),
                runtime_thread_id: thread_id.to_string(),
                turn_id: turn_id.clone(),
            };
            if let Err(error) = interrupt_turn_request(&orphan).await {
                tracing::warn!(
                    thread_id,
                    turn_id = %turn_id,
                    "failed to interrupt a turn whose event stream broke: {}",
                    error.message
                );
            }
        }

        let (last_seq, status, error) = stream_result?;
        self.last_seq_by_thread
            .insert(thread_id.to_string(), last_seq);

        match status {
            TurnTerminalStatus::Completed => Ok(json!({
                "thread_id": thread_id,
                "status": "accepted",
                "thread": Value::Null,
                "threads": [],
                "model": Value::Null,
                "model_provider": Value::Null,
                "cwd": Value::Null,
                "approval_policy": Value::Null,
                "sandbox": Value::Null,
                "events": [],
                "data": { "turn_id": turn_id },
            })),
            TurnTerminalStatus::Failed => Err(anyhow!(
                "{}",
                error.unwrap_or_else(|| "turn failed".to_string())
            )),
            TurnTerminalStatus::Interrupted => Err(anyhow!(
                "{}",
                error.unwrap_or_else(|| "turn interrupted".to_string())
            )),
            TurnTerminalStatus::Canceled => Err(anyhow!(
                "{}",
                error.unwrap_or_else(|| "turn canceled".to_string())
            )),
        }
    }

    async fn stream_turn_events<W: AsyncWrite + Unpin>(
        &self,
        thread_id: &str,
        turn_id: &str,
        response_id: &str,
        writer: &mut W,
        since_seq: u64,
        mut transcript: Option<&mut TurnTranscript>,
    ) -> Result<(u64, TurnTerminalStatus, Option<String>)> {
        let mut response = self
            .authed(self.client.get(format!(
                "{}/v1/threads/{thread_id}/events?since_seq={since_seq}",
                self.base_url
            )))
            .send()
            .await?
            .error_for_status()?;

        let mut buffer = Vec::new();
        let mut last_seq = since_seq;

        while let Some(chunk) = response.chunk().await? {
            buffer.extend_from_slice(&chunk);
            if buffer.len() > MAX_SSE_FRAME_BYTES {
                bail!(
                    "runtime SSE frame exceeded {MAX_SSE_FRAME_BYTES} bytes without a frame delimiter"
                );
            }
            while let Some(frame_bytes) = take_sse_frame(&mut buffer) {
                let Some((event_name, frame_data)) = parse_sse_frame(&frame_bytes) else {
                    continue;
                };
                let envelope: Value = serde_json::from_str(&frame_data)
                    .with_context(|| format!("invalid SSE json for {event_name}: {frame_data}"))?;
                if let Some(seq) = envelope.get("seq").and_then(Value::as_u64) {
                    last_seq = last_seq.max(seq);
                }
                if envelope.get("turn_id").and_then(Value::as_str) != Some(turn_id) {
                    continue;
                }
                let payload = envelope.get("payload").cloned().unwrap_or(Value::Null);
                match event_name.as_str() {
                    "item.delta" => {
                        let kind = payload
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        if kind == "agent_message"
                            && let Some(delta) = payload.get("delta").and_then(Value::as_str)
                            && !delta.is_empty()
                        {
                            emit_stdio_event(
                                writer,
                                json!({
                                    "type": "response_delta",
                                    "response_id": response_id,
                                    "delta": delta,
                                }),
                            )
                            .await?;
                            if let Some(transcript) = transcript.as_deref_mut() {
                                transcript.text.push_str(delta);
                                transcript.events.push(EventFrame::ResponseDelta {
                                    response_id: response_id.to_string(),
                                    delta: delta.to_string(),
                                    channel: ResponseChannel::Text,
                                });
                            }
                        }
                    }
                    "turn.completed" => {
                        let status = turn_terminal_status(&payload);
                        let error = payload
                            .pointer("/turn/error")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        return Ok((last_seq, status, error));
                    }
                    _ => {}
                }
            }
        }

        bail!("runtime event stream ended before turn.completed")
    }

    #[cfg(test)]
    fn from_base_url_for_test(base_url: String) -> Self {
        install_rustls_crypto_provider();
        Self {
            base_url,
            client: codewhale_release::platform_http_client_builder()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("build reqwest test client"),
            auth_token: None,
            child: None,
            last_seq_by_thread: HashMap::new(),
        }
    }
}

#[cfg(test)]
impl RuntimeBridge {
    /// Whether the historical fixture child has exited. A bridge without a child (tests,
    /// or an externally managed runtime) never reports exited. A `try_wait`
    /// error counts as exited: with `WNOHANG` it only fails when the pid is no
    /// longer this process's child (already reaped elsewhere), and such a
    /// child can neither be tracked nor killed on drop.
    #[cfg(unix)]
    fn child_exited(&mut self) -> bool {
        self.child
            .as_mut()
            .is_some_and(|child| !matches!(child.try_wait(), Ok(None)))
    }

    /// Kills the managed runtime child and reaps it on a detached thread so
    /// neither an explicit shutdown nor Drop blocks a Tokio runtime thread.
    fn shutdown_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

#[cfg(test)]
impl Drop for RuntimeBridge {
    fn drop(&mut self) {
        self.shutdown_child();
    }
}

/// The line the Runtime prints once it holds its listener (the Runtime's
/// `RUNTIME_LISTENING_PREFIX`; this crate does not depend on that one).
#[cfg(test)]
const RUNTIME_LISTENING_PREFIX: &str = "Runtime API listening on http://";
/// The endpoint line is short and comes first; anything larger is not it.
#[cfg(test)]
const RUNTIME_READY_MAX_BYTES: usize = 1024;

/// The endpoint a Runtime child reports for itself: exactly a nonzero port on
/// `127.0.0.1`, the host the parent asked it to bind.
#[cfg(test)]
fn parse_runtime_endpoint(line: &str) -> Result<std::net::SocketAddr> {
    let address = line
        .trim_end()
        .strip_prefix(RUNTIME_LISTENING_PREFIX)
        .context("runtime API bridge did not report its endpoint")?;
    let endpoint: std::net::SocketAddr = address
        .parse()
        .context("runtime API bridge reported an invalid endpoint")?;
    if endpoint.ip() != std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST) || endpoint.port() == 0
    {
        bail!("runtime API bridge reported an endpoint that is not a loopback port");
    }
    Ok(endpoint)
}

/// Read the first stdout line, bounded.
#[cfg(test)]
fn read_runtime_ready_line(stdout: &mut impl std::io::Read) -> Result<String> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stdout.read(&mut byte)? == 0 {
            bail!("runtime API bridge closed stdout before reporting its endpoint");
        }
        if byte[0] == b'\n' {
            break;
        }
        if line.len() >= RUNTIME_READY_MAX_BYTES {
            bail!("runtime API bridge sent an oversized readiness line");
        }
        line.push(byte[0]);
    }
    String::from_utf8(line).context("runtime API bridge readiness line is not UTF-8")
}

fn install_rustls_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn extract_runtime_thread_id(record: &Value) -> Result<&str> {
    record
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("runtime API thread response missing id"))
}

fn turn_terminal_status(payload: &Value) -> TurnTerminalStatus {
    match payload
        .pointer("/turn/status")
        .and_then(Value::as_str)
        .unwrap_or("completed")
        .to_ascii_lowercase()
        .as_str()
    {
        "failed" => TurnTerminalStatus::Failed,
        "interrupted" => TurnTerminalStatus::Interrupted,
        "canceled" | "cancelled" => TurnTerminalStatus::Canceled,
        _ => TurnTerminalStatus::Completed,
    }
}

async fn emit_stdio_event<W: AsyncWrite + Unpin>(writer: &mut W, event: Value) -> Result<()> {
    writer.write_all(&serde_json::to_vec(&event)?).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn take_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    if let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
        return Some(buffer.drain(..pos + 4).collect());
    }
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|pos| buffer.drain(..pos + 2).collect())
}

fn parse_sse_frame(frame_bytes: &[u8]) -> Option<(String, String)> {
    let text = String::from_utf8(frame_bytes.to_vec()).ok()?;
    let mut event_name = None;
    let mut data_lines = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data_lines.push(value.trim_start().to_string());
        }
    }
    match (event_name, data_lines.is_empty()) {
        (Some(event), false) => Some((event, data_lines.join("\n"))),
        _ => None,
    }
}

#[cfg(test)]
async fn dispatch_stdio_request(
    state: &AppState,
    method: &str,
    params: Value,
) -> std::result::Result<StdioDispatchResult, JsonRpcError> {
    let mut sink = tokio::io::sink();
    dispatch_stdio_request_with_writer(state, &mut sink, method, params, AppTransport::Stdio).await
}

async fn dispatch_stdio_app_request(
    state: &AppState,
    request: AppRequest,
    transport: AppTransport,
) -> std::result::Result<StdioDispatchResult, JsonRpcError> {
    let response = Box::pin(process_app_request(state, request, transport)).await;
    Ok(StdioDispatchResult {
        result: serde_json::to_value(response)
            .map_err(|err| JsonRpcError::internal(err.to_string()))?,
        should_exit: false,
    })
}

async fn dispatch_stdio_request_with_writer<W: AsyncWrite + Unpin>(
    state: &AppState,
    writer: &mut W,
    method: &str,
    params: Value,
    transport: AppTransport,
) -> std::result::Result<StdioDispatchResult, JsonRpcError> {
    let outcome = match method {
        "healthz" | "app/healthz" => StdioDispatchResult {
            result: json!({
                "status": "ok",
                "service": legacy_deepseek_compat::SERVICE_NAME,
                "transport": transport.label()
            }),
            should_exit: false,
        },
        "capabilities" => {
            let mut methods = vec![
                "healthz",
                "thread/capabilities",
                "thread/request",
                "thread/create",
                "thread/start",
                "thread/resume",
                "thread/fork",
                "thread/list",
                "thread/read",
                "thread/set_name",
                "thread/goal/set",
                "thread/goal/get",
                "thread/goal/clear",
                "thread/archive",
                "thread/unarchive",
                "thread/message",
                "thread/interrupt",
                "app/capabilities",
                "app/request",
                "app/config/get",
                "app/config/set",
                "app/config/unset",
                "app/config/list",
                "app/config/reload",
                "app/models",
                "app/thread_loaded_list",
                "prompt/capabilities",
                "prompt/request",
                "prompt/run",
                "shutdown",
            ];
            if transport == AppTransport::Socket {
                // The daemon handshake exists only on the socket transport;
                // stdio/HTTP clients never see it, so the stdio pin is unchanged.
                methods.insert(1, daemon_socket::ATTACH_METHOD);
            }
            StdioDispatchResult {
                result: json!({
                    "transport": transport.label(),
                    "families": ["thread/*", "app/*", "prompt/*"],
                    "turn_image_inputs": true,
                    "methods": methods,
                }),
                should_exit: false,
            }
        }
        "thread/capabilities" => StdioDispatchResult {
            result: json!({
                "turn_image_inputs": true,
                "methods": [
                    "thread/request",
                    "thread/create",
                    "thread/start",
                    "thread/resume",
                    "thread/fork",
                    "thread/list",
                    "thread/read",
                    "thread/set_name",
                    "thread/goal/set",
                    "thread/goal/get",
                    "thread/goal/clear",
                    "thread/archive",
                    "thread/unarchive",
                    "thread/message",
                    "thread/interrupt"
                ]
            }),
            should_exit: false,
        },
        "thread/request" => {
            let request: ThreadRequest = parse_params(params)?;
            if let ThreadRequest::Message {
                thread_id,
                input,
                images,
                max_output_tokens,
            } = request
            {
                let response = handle_stdio_thread_message(
                    state,
                    writer,
                    ThreadMessageParams {
                        thread_id,
                        input,
                        images,
                        max_output_tokens,
                    },
                )
                .await?;
                return Ok(StdioDispatchResult {
                    result: response,
                    should_exit: false,
                });
            }
            let should_record_hint = matches!(
                &request,
                ThreadRequest::Create { .. }
                    | ThreadRequest::Start(_)
                    | ThreadRequest::Resume(_)
                    | ThreadRequest::Fork(_)
            );
            let response = handle_thread_request(state, request).await?;
            if should_record_hint {
                record_stdio_thread_hint(state, &response).await;
            }
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/create" => {
            #[derive(Debug, Deserialize)]
            struct CreateParams {
                #[serde(default)]
                metadata: Value,
            }
            let parsed: CreateParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Create {
                    metadata: parsed.metadata,
                },
            )
            .await?;
            record_stdio_thread_hint(state, &response).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/start" => {
            let request = ThreadRequest::Start(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            record_stdio_thread_hint(state, &response).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/resume" => {
            let request = ThreadRequest::Resume(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            ensure_thread_found(&response)?;
            record_stdio_thread_hint(state, &response).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/fork" => {
            let request = ThreadRequest::Fork(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            ensure_thread_found(&response)?;
            record_stdio_thread_hint(state, &response).await;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/list" => {
            let request = ThreadRequest::List(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/read" => {
            let request = ThreadRequest::Read(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/set_name" | "thread/set-name" => {
            let request = ThreadRequest::SetName(parse_params(params_or_object(params))?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/goal/set" | "thread/goal_set" | "thread/goal-set" => {
            let request = ThreadRequest::GoalSet(parse_params::<ThreadGoalSetParams>(
                params_or_object(params),
            )?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/goal/get" | "thread/goal_get" | "thread/goal-get" => {
            let request = ThreadRequest::GoalGet(parse_params::<ThreadGoalGetParams>(
                params_or_object(params),
            )?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/goal/clear" | "thread/goal_clear" | "thread/goal-clear" => {
            let request = ThreadRequest::GoalClear(parse_params::<ThreadGoalClearParams>(
                params_or_object(params),
            )?);
            let response = handle_thread_request(state, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/archive" => {
            let parsed: ThreadIdParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Archive {
                    thread_id: parsed.thread_id,
                },
            )
            .await?;
            ensure_thread_found(&response)?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/unarchive" => {
            let parsed: ThreadIdParams = parse_params(params_or_object(params))?;
            let response = handle_thread_request(
                state,
                ThreadRequest::Unarchive {
                    thread_id: parsed.thread_id,
                },
            )
            .await?;
            ensure_thread_found(&response)?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/message" => {
            let parsed: ThreadMessageParams = parse_params(params_or_object(params))?;
            let response = handle_stdio_thread_message(state, writer, parsed).await?;
            StdioDispatchResult {
                result: response,
                should_exit: false,
            }
        }
        "app/capabilities" => {
            dispatch_stdio_app_request(state, AppRequest::Capabilities, transport).await?
        }
        "app/request" => {
            let request: AppRequest = parse_params(params)?;
            dispatch_stdio_app_request(state, request, transport).await?
        }
        "app/config/get" => {
            let parsed: ConfigGetParams = parse_params(params_or_object(params))?;
            dispatch_stdio_app_request(state, AppRequest::ConfigGet { key: parsed.key }, transport)
                .await?
        }
        "app/config/set" => {
            let parsed: ConfigSetParams = parse_params(params_or_object(params))?;
            dispatch_stdio_app_request(
                state,
                AppRequest::ConfigSet {
                    key: parsed.key,
                    value: parsed.value,
                },
                transport,
            )
            .await?
        }
        "app/config/unset" => {
            let parsed: ConfigGetParams = parse_params(params_or_object(params))?;
            dispatch_stdio_app_request(
                state,
                AppRequest::ConfigUnset { key: parsed.key },
                transport,
            )
            .await?
        }
        "app/config/list" => {
            dispatch_stdio_app_request(state, AppRequest::ConfigList, transport).await?
        }
        "app/config/reload" => {
            dispatch_stdio_app_request(state, AppRequest::ConfigReload, transport).await?
        }
        "app/models" => dispatch_stdio_app_request(state, AppRequest::Models, transport).await?,
        "app/thread_loaded_list" | "app/thread-loaded-list" => {
            dispatch_stdio_app_request(state, AppRequest::ThreadLoadedList, transport).await?
        }
        "prompt/capabilities" => StdioDispatchResult {
            result: json!({
                "methods": ["prompt/request", "prompt/run"]
            }),
            should_exit: false,
        },
        "prompt/request" | "prompt/run" => {
            let request: PromptRequest = parse_params(params)?;
            let response = handle_prompt_request(state, writer, request).await?;
            StdioDispatchResult {
                result: serde_json::to_value(response)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?,
                should_exit: false,
            }
        }
        "thread/interrupt" => {
            let parsed: ThreadInterruptParams = parse_params(params_or_object(params))?;
            let interrupted = interrupt_stdio_turn(state, &parsed.thread_id).await?;
            StdioDispatchResult {
                result: json!({
                    "thread_id": parsed.thread_id,
                    "interrupted": interrupted,
                }),
                should_exit: false,
            }
        }
        "shutdown" => {
            // The transport checks shutdown authority before dispatch.
            // Interrupt live turns before returning the flushed shutdown
            // result to its captured host. Keep the shared bridge bound while
            // that owner drains its manager and listeners.
            let _ = interrupt_all_stdio_turns(state).await;
            StdioDispatchResult {
                result: json!({"ok": true, "status": "stopped"}),
                should_exit: true,
            }
        }
        daemon_socket::ATTACH_METHOD if transport == AppTransport::Socket => {
            return Err(JsonRpcError::already_attached());
        }
        _ => return Err(JsonRpcError::method_not_found(method)),
    };
    Ok(outcome)
}

async fn process_app_request(
    state: &AppState,
    req: AppRequest,
    _transport: AppTransport,
) -> AppResponse {
    match req {
        AppRequest::Capabilities => AppResponse {
            ok: true,
            data: json!({
                "routes": ADVERTISED_ROUTES,
                "config": ["get", "set", "unset", "list", "reload"],
                "events": ["response_start", "response_delta", "response_end", "tool_call_start", "tool_call_result"],
                "transport": "stdio+http",
                "config_path": state.config_path.as_ref().map(|p| p.display().to_string()),
            }),
            events: Vec::new(),
        },
        AppRequest::ConfigGet { key } => {
            let cfg = state.config.read().await;
            let value = cfg.get_display_value(&key);
            AppResponse {
                ok: true,
                data: json!({ "key": key, "value": value }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigSet { key, value } => {
            // Only propagate a mutation that actually happened. `set_value`
            // leaves the config untouched on an unknown key or invalid value,
            // so this is a no-op from the caller's point of view — but
            // `propagate_config` invalidates the cached stdio bridge
            // regardless, and dropping the last reference kills the running
            // child runtime along with its thread map. A single typo'd key
            // would orphan every in-flight thread on that bridge.
            let result = {
                let (key, value) = (key.clone(), value.clone());
                persist_config_mutation(state, move |cfg| cfg.set_value(&key, &value)).await
            };
            let ok = result.is_ok();
            let message = result.err().map(|e| e.to_string());
            let value = if ok {
                value
            } else {
                codewhale_config::persistence::redact_secrets(&value)
            };
            AppResponse {
                ok,
                data: json!({ "key": key, "value": value, "error": message }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigUnset { key } => {
            // See ConfigSet: a failed unset changed nothing and must not tear
            // down the runtime bridge.
            let result = {
                let key = key.clone();
                persist_config_mutation(state, move |cfg| cfg.unset_value(&key)).await
            };
            let ok = result.is_ok();
            let message = result.err().map(|e| e.to_string());
            AppResponse {
                ok,
                data: json!({ "key": key, "error": message }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigList => {
            let cfg = state.config.read().await;
            AppResponse {
                ok: true,
                data: json!({ "values": cfg.list_values() }),
                events: Vec::new(),
            }
        }
        AppRequest::ConfigReload => {
            // Re-read both `config.toml` and the sibling `permissions.toml`
            // from disk (the headless equivalent of the TUI
            // `reload_runtime_config` codepath) and push the fresh
            // snapshots into `state.config` and the live `Runtime`.
            //
            // `ConfigStore::load` resolves the same default config path
            // that `build_state` used at startup when `config_path` is
            // `None`, so a `None` here reloads from the same on-disk file
            // the server booted from.
            // Disk is already the source of truth here, so nothing to
            // persist. External `permissions.toml` edits reach the Engine
            // because the update invalidates the runtime bridge; the next
            // turn's child loads both files fresh.
            if let Err(error) = update_config_store(state, |_| Ok(())).await {
                return AppResponse {
                    ok: false,
                    data: json!({ "error": error.to_string() }),
                    events: Vec::new(),
                };
            }

            AppResponse {
                ok: true,
                data: json!({ "reloaded": true }),
                events: Vec::new(),
            }
        }
        AppRequest::Models => AppResponse {
            ok: true,
            data: json!({ "models": state.registry.list() }),
            events: Vec::new(),
        },
        AppRequest::ThreadLoadedList => {
            let response = handle_thread_request(
                state,
                ThreadRequest::List(ThreadListParams {
                    include_archived: false,
                    limit: Some(50),
                }),
            )
            .await;
            match response {
                Ok(thread_resp) => AppResponse {
                    ok: true,
                    data: json!({ "threads": thread_resp.threads }),
                    events: thread_resp.events,
                },
                Err(err) => AppResponse {
                    ok: false,
                    data: json!({ "error": err.message }),
                    events: Vec::new(),
                },
            }
        }
        AppRequest::SubmitUserInput { request_id, .. } => {
            // This transport cannot deliver a clarification answer, and
            // saying otherwise was the bug: the previous implementation
            // reported `resolved: true` and filed the answers in a map with
            // no reader anywhere in this crate.
            //
            // It cannot be made to work here. `handle_line_during_turn`
            // executes exactly one method while a turn is streaming —
            // `thread/interrupt`. Everything else, `app/request` included,
            // queues until the turn ends, so an answer sent over this
            // transport would wait on the very turn that is waiting for it.
            // The runtime API owns the pending request and can resume the
            // turn, so that is where the reply belongs.
            AppResponse {
                ok: false,
                data: json!({
                    "error": "user_input_reply_unsupported",
                    "request_id": request_id,
                    "message": concat!(
                        "the app-server control transport cannot deliver clarification answers: ",
                        "only `thread/interrupt` runs while a turn is streaming, so an answer sent ",
                        "here would queue behind the turn waiting for it. Reply on the runtime API ",
                        "instead: POST /v1/user-input/{thread_id}/{request_id}."
                    ),
                }),
                events: Vec::new(),
            }
        }
    }
}

/// Install the saved config in the bookkeeping Runtime, then apply it through
/// the already captured canonical owner. Unbound production frontends refuse
/// application while retaining the saved config for explicit recovery.
/// A saved config whose owner reload fails is reported as retained but unapplied.
async fn propagate_config(state: &AppState) -> Result<()> {
    {
        let mut runtime = state.runtime.write().await;
        let snapshot = state.config.read().await.clone();
        runtime.update_config(snapshot);
    }
    if state.captured_owner.is_some() {
        let shared = acquire_runtime_bridge(state)
            .await
            .map_err(|_| anyhow!("config saved but captured Runtime owner is unavailable"))?;
        let request = {
            let bridge = shared.lock().await;
            bridge
                .authed(
                    bridge
                        .client
                        .post(format!("{}/v1/config/reload", bridge.base_url)),
                )
                .timeout(Duration::from_secs(10))
        };
        let response = request
            .send()
            .await
            .context("config saved; captured owner reload outcome is uncertain, not replayed")?;
        anyhow::ensure!(
            response.status().is_success(),
            "config saved but captured owner rejected reload (status {})",
            response.status()
        );
    } else {
        #[cfg(test)]
        invalidate_runtime_bridge(state).await;
        #[cfg(not(test))]
        bail!("config saved but no captured Runtime owner can apply it");
    }
    Ok(())
}

/// Prefix of a config error that is the server's fault (the file could not
/// be read, parsed, or written), reported over HTTP `/app` as a 500 rather
/// than the 400 a rejected key or value gets.
const CONFIG_LOAD_ERROR: &str = "failed to load config";
/// See [`CONFIG_LOAD_ERROR`].
const CONFIG_SAVE_ERROR: &str = "failed to save config";

/// Apply `mutate` to the config on disk, then propagate the saved result.
///
/// The mutation runs against a freshly loaded store, not the in-memory
/// snapshot, so edits another process (TUI, `codewhale login`) saved since
/// startup survive. With no explicit `--config` the store resolves the same
/// default path the runtime child reads, so the change reaches turns and
/// survives a restart. Any load, mutation, or save failure is returned and
/// nothing is propagated, so the caller never reports `ok` for a change
/// that was not kept.
///
/// The work runs on its own task: once the file is written, a caller that
/// goes away (an HTTP client disconnect) must not leave disk ahead of
/// `state.config`, the live runtime, and the cached bridge.
async fn persist_config_mutation(
    state: &AppState,
    mutate: impl FnOnce(&mut codewhale_config::ConfigToml) -> Result<()> + Send + 'static,
) -> Result<()> {
    update_config_store(state, move |store| {
        mutate(&mut store.config)?;
        store
            .save()
            .map_err(|err| anyhow!("{CONFIG_SAVE_ERROR}: {err}"))
    })
    .await
}

/// Serialize reloads and writes before reading disk, and finish propagation
/// even if the requesting connection disappears. A reload mutates nothing.
async fn update_config_store(
    state: &AppState,
    update: impl FnOnce(&mut ConfigStore) -> Result<()> + Send + 'static,
) -> Result<()> {
    let state = state.clone();
    tokio::spawn(async move {
        // Own the write guard across load→mutate→save→install so two
        // concurrent mutations cannot overwrite one another. All disk work
        // runs off-runtime; the owned operation still finishes propagation
        // when the requesting connection goes away.
        let mut config = state.config.clone().write_owned().await;
        let config_path = state.config_path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut store = ConfigStore::load(config_path)
                .map_err(|err| anyhow!("{CONFIG_LOAD_ERROR}: {err}"))?;
            update(&mut store)?;
            *config = store.config;
            Ok(())
        })
        .await
        .map_err(|err| anyhow!("config store task failed: {err}"))??;
        propagate_config(&state).await?;
        Ok(())
    })
    .await
    .map_err(|err| anyhow!("config update task failed: {err}"))?
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
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::extract::{Path as AxumPath, Query};
    use axum::http::header;
    use codewhale_protocol::AppRequest;
    use std::collections::HashMap;
    use std::fs;
    use tokio::io::AsyncReadExt;
    use tower::ServiceExt;

    #[tokio::test]
    async fn control_framing_accepts_exact_limit_crlf_and_preserves_next_frame() {
        let mut input = vec![b'x'; MAX_RUNTIME_IMAGE_BODY_BYTES];
        input.extend_from_slice(b"\r\nnext\n");
        let mut reader = BoundedLines::new(BufReader::with_capacity(8192, input.as_slice()));
        assert_eq!(
            reader.next_line().await.unwrap().unwrap().len(),
            MAX_RUNTIME_IMAGE_BODY_BYTES
        );
        assert_eq!(reader.next_line().await.unwrap().as_deref(), Some("next"));
        assert!(reader.next_line().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn control_framing_refuses_oversize_before_retaining_extra_bytes() {
        let input = vec![b'x'; MAX_RUNTIME_IMAGE_BODY_BYTES + 4096];
        let mut reader = BoundedLines::new(BufReader::with_capacity(8192, input.as_slice()));
        assert_eq!(
            reader.next_line().await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        assert!(reader.pending.len() <= MAX_RUNTIME_IMAGE_BODY_BYTES + 1);
        assert!(reader.pending.capacity() <= MAX_RUNTIME_IMAGE_BODY_BYTES + 1);
    }

    #[tokio::test]
    async fn control_framing_keeps_consumed_prefix_when_read_future_is_cancelled() {
        let (mut input, output) = tokio::io::duplex(64);
        input.write_all(b"partial").await.unwrap();
        let mut reader = BoundedLines::new(BufReader::new(output));
        std::future::poll_fn(|cx| {
            let mut read = std::pin::pin!(reader.next_line());
            assert!(std::future::Future::poll(read.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(reader.pending, b"partial");
        input.write_all(b"-continued\n").await.unwrap();
        assert_eq!(
            reader.next_line().await.unwrap().as_deref(),
            Some("partial-continued")
        );
    }

    #[tokio::test]
    async fn control_framing_rejects_non_utf8_and_keeps_eof_line_contract() {
        let mut invalid = BoundedLines::new(BufReader::new(&b"\xff\n"[..]));
        assert_eq!(
            invalid.next_line().await.unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        let mut eof = BoundedLines::new(BufReader::new(&b"last\r"[..]));
        assert_eq!(eof.next_line().await.unwrap().as_deref(), Some("last\r"));
        assert!(eof.next_line().await.unwrap().is_none());
    }

    #[test]
    fn retained_control_queue_refuses_exhaustion_without_admitting_more_work() {
        let mut pending = VecDeque::new();
        let mut bytes = 0;
        for _ in 0..64 {
            queue_stdio_work(
                &mut pending,
                &mut bytes,
                PendingStdioWork::Response(json!({})),
                2,
            )
            .unwrap();
        }
        let before = bytes;
        assert!(
            queue_stdio_work(
                &mut pending,
                &mut bytes,
                PendingStdioWork::Response(json!({})),
                2
            )
            .is_err()
        );
        assert_eq!(pending.len(), 64);
        assert_eq!(bytes, before);
        let mut pending = VecDeque::new();
        let mut bytes = 0;
        assert!(
            queue_stdio_work(
                &mut pending,
                &mut bytes,
                PendingStdioWork::Response(json!({})),
                MAX_RUNTIME_IMAGE_BODY_BYTES
            )
            .is_err()
        );
        assert!(pending.is_empty());
        assert_eq!(bytes, 0);
    }

    #[tokio::test]
    async fn captured_owner_bridge_is_retained_and_never_spawns_a_replacement() {
        install_test_crypto_provider();
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config.toml");
        fs::write(&config, "").unwrap();
        let mut state = build_state(Some(config), None).unwrap();
        state.captured_owner = Some(codewhale_protocol::RuntimeOwnerReceipt {
            version: 1,
            data_dir: tmp.path().join("runtime"),
            execution_scope: "fixture".into(),
            lease_generation: "fixture".into(),
            pid: std::process::id(),
            process_start: "fixture".into(),
            principal: "fixture".into(),
            socket_path: tmp.path().join("owner.sock"),
            config_path: state.config_path.clone(),
        });
        let captured = Arc::new(Mutex::new(RuntimeBridge {
            base_url: "http://127.0.0.1:1".into(),
            client: codewhale_release::tls::reqwest_client(),
            auth_token: None,
            child: None,
            last_seq_by_thread: HashMap::new(),
        }));
        *state.runtime_bridge.lock().await = Some(captured.clone());
        invalidate_runtime_bridge(&state).await;
        let live = acquire_runtime_bridge(&state).await.unwrap();
        assert!(Arc::ptr_eq(&live, &captured));
        *state.runtime_bridge.lock().await = None;
        let refused = acquire_runtime_bridge(&state).await;
        assert!(refused.is_err());
    }

    #[tokio::test]
    async fn unbound_frontend_refuses_turn_without_creating_a_runtime_owner() {
        install_test_crypto_provider();
        let (state, _temporary) = capability_test_state();
        assert!(state.runtime_bridge.lock().await.is_none());
        let refused = dispatch_stdio_request(
            &state,
            "prompt/request",
            json!({"prompt":"must not dispatch"}),
        )
        .await
        .expect_err("no captured owner can execute this prompt");
        assert_eq!(refused.code, RUNTIME_UNAVAILABLE_CODE);
        assert!(refused.message.contains("refusing another owner"));
        assert!(state.runtime_bridge.lock().await.is_none());
        assert!(state.runtime_thread_map.lock().await.is_empty());
        assert!(state.in_flight_turns.lock().await.is_empty());
    }

    #[test]
    fn captured_bridge_diagnostics_redact_private_authentication() {
        let mut bridge = RuntimeBridge::from_base_url_for_test("http://127.0.0.1:1".into());
        bridge.auth_token = Some("private-bridge-token-must-stay-in-memory".into());
        let diagnostic = format!("{bridge:?}");
        assert!(!diagnostic.contains("private-bridge-token"));
        assert!(diagnostic.contains("authenticated: true"));
    }

    #[tokio::test]
    async fn full_control_queue_keeps_interrupt_priority_and_guest_shutdown_refusal() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        async fn interrupted(State(count): State<Arc<AtomicUsize>>) -> StatusCode {
            count.fetch_add(1, Ordering::SeqCst);
            StatusCode::OK
        }
        install_test_crypto_provider();
        let count = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/v1/threads/runtime/turns/turn/interrupt",
                post(interrupted),
            )
            .with_state(count.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (state, _root) = capability_test_state();
        state.in_flight_turns.lock().await.insert(
            "client".into(),
            InFlightTurn {
                base_url: format!("http://{address}"),
                auth_token: None,
                runtime_thread_id: "runtime".into(),
                turn_id: "turn".into(),
            },
        );
        let mut pending = VecDeque::new();
        let mut bytes = 0;
        for _ in 0..64 {
            queue_stdio_work(
                &mut pending,
                &mut bytes,
                PendingStdioWork::Response(json!({})),
                2,
            )
            .unwrap();
        }
        let guest = StdioLoopPolicy {
            transport: AppTransport::Socket,
            shutdown: ShutdownAuthority::Denied,
        };
        assert!(
            handle_line_during_turn(
                &state,
                r#"{"id":1,"method":"shutdown","params":{}}"#,
                &mut pending,
                &mut bytes,
                guest
            )
            .await
            .is_err()
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "guest must not interrupt before its denied shutdown"
        );
        assert!(
            handle_line_during_turn(
                &state,
                r#"{"id":2,"method":"thread/interrupt","params":{"thread_id":"client"}}"#,
                &mut pending,
                &mut bytes,
                guest
            )
            .await
            .is_err()
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "interrupt acts before its overloaded reply queue"
        );
        let owner = StdioLoopPolicy {
            transport: AppTransport::Socket,
            shutdown: ShutdownAuthority::Granted,
        };
        assert!(
            handle_line_during_turn(
                &state,
                r#"{"id":3,"method":"shutdown","params":{}}"#,
                &mut pending,
                &mut bytes,
                owner
            )
            .await
            .is_err()
        );
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "authorized shutdown retains immediate interrupt priority"
        );
        assert_eq!(pending.len(), 64);
        server.abort();
    }

    #[tokio::test]
    async fn bound_owner_config_reload_uses_private_auth_and_preserves_rejected_owner() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        #[derive(Clone)]
        struct ReloadFixture {
            count: Arc<AtomicUsize>,
            refuse: Arc<AtomicBool>,
        }
        async fn reload(
            State(fixture): State<ReloadFixture>,
            headers: axum::http::HeaderMap,
        ) -> StatusCode {
            assert_eq!(
                headers.get(header::AUTHORIZATION).unwrap(),
                "Bearer private-reload-fixture"
            );
            fixture.count.fetch_add(1, Ordering::SeqCst);
            if fixture.refuse.load(Ordering::SeqCst) {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::OK
            }
        }
        install_test_crypto_provider();
        let fixture = ReloadFixture {
            count: Arc::new(AtomicUsize::new(0)),
            refuse: Arc::new(AtomicBool::new(false)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = Router::new()
            .route("/v1/config/reload", post(reload))
            .with_state(fixture.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let (mut state, root) = capability_test_state();
        state.captured_owner = Some(codewhale_protocol::RuntimeOwnerReceipt {
            version: 1,
            data_dir: root.path().join("runtime"),
            execution_scope: "fixture".into(),
            lease_generation: "fixture".into(),
            pid: std::process::id(),
            process_start: "fixture".into(),
            principal: "fixture".into(),
            socket_path: root.path().join("owner.sock"),
            config_path: state.config_path.clone(),
        });
        let mut bridge = RuntimeBridge::from_base_url_for_test(format!("http://{address}"));
        bridge.auth_token = Some("private-reload-fixture".into());
        let captured = Arc::new(Mutex::new(bridge));
        *state.runtime_bridge.lock().await = Some(captured.clone());
        propagate_config(&state).await.unwrap();
        assert_eq!(fixture.count.load(Ordering::SeqCst), 1);
        fixture.refuse.store(true, Ordering::SeqCst);
        assert!(
            propagate_config(&state)
                .await
                .unwrap_err()
                .to_string()
                .contains("config saved")
        );
        assert_eq!(fixture.count.load(Ordering::SeqCst), 2);
        assert!(Arc::ptr_eq(
            state.runtime_bridge.lock().await.as_ref().unwrap(),
            &captured
        ));
        server.abort();
    }

    fn app_with_config(auth_token: Option<&str>) -> (Router, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "api_key = \"sk-deepseek-secret\"\n").expect("write config");
        let state = build_state(
            Some(config_path),
            auth_token.map(std::string::ToString::to_string),
        )
        .expect("state");
        (app_router(state, &[]), tmp)
    }

    #[test]
    fn build_state_keeps_resolved_explicit_config_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("config-dir");
        fs::create_dir_all(&config_dir).expect("config dir");
        let config_path = config_dir.join("config.toml");
        fs::write(&config_path, "api_key = \"sk-deepseek-secret\"\n").expect("write config");

        let state = build_state(Some(config_path.clone()), None).expect("state");

        assert_eq!(
            state.config_path.as_deref(),
            Some(
                config_path
                    .canonicalize()
                    .expect("canonical config")
                    .as_path()
            )
        );
    }

    #[tokio::test]
    async fn stdio_transport_never_registers_the_stdout_hook_sink() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "api_key = \"sk-deepseek-secret\"\n").expect("write config");

        let http_state =
            build_state_with_transport(Some(config_path.clone()), None, AppTransport::Http)
                .expect("http state");
        let stdio_state = build_state_with_transport(Some(config_path), None, AppTransport::Stdio)
            .expect("stdio state");

        let http_sinks = http_state.runtime.read().await.hooks.sink_count();
        let stdio_sinks = stdio_state.runtime.read().await.hooks.sink_count();
        assert_eq!(
            http_sinks,
            stdio_sinks + 1,
            "HTTP mode keeps StdoutHookSink + JsonlHookSink; stdio must drop the stdout sink (#5165)"
        );
    }

    // Points CODEWHALE_HOME at a temp dir for one test and restores it after.
    // The variable is process-global, so these tests hold a lock.
    static CODEWHALE_HOME_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct CodewhaleHomeGuard {
        prior: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl CodewhaleHomeGuard {
        fn set(value: &Path) -> Self {
            let lock = CODEWHALE_HOME_TEST_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let prior = std::env::var_os("CODEWHALE_HOME");
            // SAFETY: serialised by CODEWHALE_HOME_TEST_LOCK.
            unsafe { std::env::set_var("CODEWHALE_HOME", value) };
            Self { prior, _lock: lock }
        }
    }

    impl Drop for CodewhaleHomeGuard {
        fn drop(&mut self) {
            // SAFETY: serialised by CODEWHALE_HOME_TEST_LOCK.
            unsafe {
                match &self.prior {
                    Some(value) => std::env::set_var("CODEWHALE_HOME", value),
                    None => std::env::remove_var("CODEWHALE_HOME"),
                }
            }
        }
    }

    #[tokio::test]
    async fn hook_log_without_config_path_goes_beside_state_db_not_cwd() {
        let home = tempfile::tempdir().expect("tempdir");
        let _home = CodewhaleHomeGuard::set(home.path());

        let state = build_state_with_transport(None, None, AppTransport::Stdio).expect("state");
        state
            .runtime
            .read()
            .await
            .hooks
            .emit(codewhale_hooks::HookEvent::ResponseStart {
                response_id: "resp-6513".to_string(),
            })
            .await;

        let log = fs::read_to_string(home.path().join("events.jsonl"))
            .expect("hook log beside state.db in CODEWHALE_HOME");
        assert!(log.contains("resp-6513"), "{log}");
    }

    async fn response_body_json(response: Response) -> Value {
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body bytes");
        serde_json::from_slice(&bytes).expect("json response")
    }

    #[tokio::test]
    async fn http_app_routes_require_bearer_token_when_auth_enabled() {
        let (app, _tmp) = app_with_config(Some("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/app")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&AppRequest::ConfigGet {
                            key: "api_key".to_string(),
                        })
                        .expect("request json"),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn http_config_get_redacts_sensitive_values_after_auth() {
        let (app, _tmp) = app_with_config(Some("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/app")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&AppRequest::ConfigGet {
                            key: "api_key".to_string(),
                        })
                        .expect("request json"),
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        let body = response_body_json(response).await;
        assert_eq!(body["data"]["value"], "sk-d***cret");
    }

    /// Every route `Capabilities` advertises must reach a handler. A POST
    /// with an empty JSON body is enough: a registered handler answers with
    /// its own status (200, or 422 for a body it rejects), while a path with
    /// no handler answers 404 and a wrong method 405.
    #[tokio::test]
    async fn every_advertised_route_has_a_handler() {
        let (_app, tmp) = app_with_config(Some("test-token"));
        let state = build_state(Some(tmp.path().join("config.toml")), None).expect("state");
        let caps = process_app_request(&state, AppRequest::Capabilities, AppTransport::Http).await;
        let advertised: Vec<String> =
            serde_json::from_value(caps.data["routes"].clone()).expect("routes list");
        assert_eq!(advertised, ADVERTISED_ROUTES);

        for route in &advertised {
            let mut status = StatusCode::METHOD_NOT_ALLOWED;
            for method in [Method::POST, Method::GET] {
                let (app, _tmp) = app_with_config(Some("test-token"));
                let body = if method == Method::POST {
                    Body::from("{}")
                } else {
                    Body::empty()
                };
                status = app
                    .oneshot(
                        Request::builder()
                            .method(method)
                            .uri(route.as_str())
                            .header(header::AUTHORIZATION, "Bearer test-token")
                            .header(header::CONTENT_TYPE, "application/json")
                            .body(body)
                            .expect("request"),
                    )
                    .await
                    .expect("response")
                    .status();
                if status != StatusCode::METHOD_NOT_ALLOWED {
                    break;
                }
            }
            assert!(
                status != StatusCode::NOT_FOUND
                    && status != StatusCode::METHOD_NOT_ALLOWED
                    && !status.is_server_error(),
                "advertised route {route} has no working handler: {status}"
            );
        }
    }

    /// `/tool` ran calls against an empty tool registry under an approval
    /// mapping of its own. It is gone from both the router and the
    /// advertised list; tools run only inside Engine turns.
    #[tokio::test]
    async fn tool_route_is_not_served_or_advertised() {
        let (app, tmp) = app_with_config(Some("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/tool")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        r#"{"call":{"name":"exec_shell","payload":{"type":"local_shell","command":["true"]}}}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let state = build_state(Some(tmp.path().join("config.toml")), None).expect("state");
        let caps = process_app_request(&state, AppRequest::Capabilities, AppTransport::Http).await;
        let advertised = caps.data["routes"].as_array().expect("routes list");
        assert!(!advertised.iter().any(|route| route == "/tool"));
    }

    #[tokio::test]
    async fn mcp_startup_route_cannot_start_a_parallel_pool() {
        let (app, tmp) = app_with_config(Some("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/mcp/startup")
                    .header(header::AUTHORIZATION, "Bearer test-token")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let state = build_state(Some(tmp.path().join("config.toml")), None).expect("state");
        let caps = process_app_request(&state, AppRequest::Capabilities, AppTransport::Http).await;
        assert!(
            !caps.data["routes"]
                .as_array()
                .expect("routes")
                .iter()
                .any(|route| route == "/mcp/startup")
        );
        assert!(
            !caps.data["events"]
                .as_array()
                .expect("events")
                .iter()
                .any(|event| event == "mcp_startup_update" || event == "mcp_startup_complete")
        );
    }

    #[tokio::test]
    async fn cors_does_not_allow_arbitrary_origins() {
        let (app, _tmp) = app_with_config(Some("test-token"));
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/healthz")
                    .header(header::ORIGIN, "https://attacker.example")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response
                .headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
    }

    #[tokio::test]
    async fn config_reload_refreshes_runtime_config_from_disk() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            "api_key = \"sk-deepseek-secret\"\nmodel = \"deepseek-chat\"\n",
        )
        .expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");
        {
            let runtime = state.runtime.read().await;
            assert_eq!(runtime.config.model.as_deref(), Some("deepseek-chat"));
        }

        fs::write(
            &config_path,
            "api_key = \"sk-deepseek-secret\"\nmodel = \"deepseek-reasoner\"\n",
        )
        .expect("rewrite config");

        // ConfigReload must re-read the file and push it into the live
        // Runtime without a restart.
        let response =
            process_app_request(&state, AppRequest::ConfigReload, AppTransport::Stdio).await;
        assert!(response.ok, "reload should succeed");
        assert_eq!(response.data["reloaded"], true);

        // The shared config lock reflects the new model.
        {
            let cfg = state.config.read().await;
            assert_eq!(cfg.model.as_deref(), Some("deepseek-reasoner"));
        }
        // The live Runtime reflects the new model.
        {
            let runtime = state.runtime.read().await;
            assert_eq!(runtime.config.model.as_deref(), Some("deepseek-reasoner"));
        }
    }

    #[tokio::test]
    async fn config_set_propagates_to_runtime_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            "api_key = \"sk-deepseek-secret\"\nmodel = \"deepseek-chat\"\n",
        )
        .expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");

        // Set a new model via the API.
        let response = process_app_request(
            &state,
            AppRequest::ConfigSet {
                key: "model".to_string(),
                value: "deepseek-reasoner".to_string(),
            },
            AppTransport::Stdio,
        )
        .await;
        assert!(response.ok, "set should succeed");

        // Live runtime sees the new model.
        {
            let runtime = state.runtime.read().await;
            assert_eq!(runtime.config.model.as_deref(), Some("deepseek-reasoner"));
        }
        // The on-disk file was persisted.
        let persisted = fs::read_to_string(&config_path).expect("read config");
        assert!(persisted.contains("deepseek-reasoner"));
    }

    /// A bridge stand-in with no child process: this test only cares about
    /// whether the cache slot survives, not about talking to a runtime.
    fn sentinel_bridge() -> SharedRuntimeBridge {
        Arc::new(Mutex::new(RuntimeBridge {
            base_url: "http://127.0.0.1:0".to_string(),
            client: codewhale_release::tls::reqwest_client(),
            auth_token: None,
            child: None,
            last_seq_by_thread: HashMap::new(),
        }))
    }

    #[tokio::test]
    async fn failed_config_set_keeps_the_stdio_bridge() {
        crate::install_test_crypto_provider();
        // #4737: `set_value` rejects an invalid value before assigning, so the
        // request is a no-op — but `apply_config_update` ran anyway and
        // invalidated the cached bridge, dropping the child runtime along with
        // its thread map. A single bad value orphaned every in-flight stdio
        // thread, behind a response that correctly reported `ok: false`.
        //
        // Only `set_value` is exercised: an unknown key lands in `extras` and
        // succeeds, and `unset_value` has no failing input today, so its
        // identical guard has nothing to assert against.
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");
        let bridge = sentinel_bridge();
        *state.runtime_bridge.lock().await = Some(bridge.clone());
        state
            .runtime_thread_map
            .lock()
            .await
            .insert("stdio-1".to_string(), "runtime-1".to_string());

        let disk_before = fs::read(&config_path).unwrap();
        let config_before = serde_json::to_value(&*state.config.read().await).unwrap();
        let runtime_before = serde_json::to_value(&state.runtime.read().await.config).unwrap();
        let token = ["sk-live-", "Z7qX4mNb2Vc9Lk3PwR8t"].concat();
        for (key, value) in [
            ("telemetry", "not-a-bool"),
            ("approval_policy", "ask"),
            ("sandbox_mode", "full"),
            ("verbosity", "quiet"),
            ("approval_policy", token.as_str()),
            ("sandbox_mode", token.as_str()),
            ("verbosity", token.as_str()),
        ] {
            let response = process_app_request(
                &state,
                AppRequest::ConfigSet {
                    key: key.into(),
                    value: value.into(),
                },
                AppTransport::Stdio,
            )
            .await;
            assert!(!response.ok, "invalid {key} must fail");
            assert!(response.data["error"].is_string(), "refusal detail");
            let rendered = serde_json::to_string(&response).unwrap();
            assert!(
                !rendered.contains(&token),
                "credential must not enter diagnostics or the echoed value"
            );
            assert_eq!(
                response.data["value"].as_str(),
                Some(if value == token { "[redacted]" } else { value })
            );
            assert_eq!(fs::read(&config_path).unwrap(), disk_before);
            assert_eq!(
                serde_json::to_value(&*state.config.read().await).unwrap(),
                config_before
            );
            assert_eq!(
                serde_json::to_value(&state.runtime.read().await.config).unwrap(),
                runtime_before
            );
            assert!(
                state
                    .runtime_bridge
                    .lock()
                    .await
                    .as_ref()
                    .is_some_and(|cached| Arc::ptr_eq(cached, &bridge)),
                "the same bridge must survive a failed config/set",
            );
            assert_eq!(
                state
                    .runtime_thread_map
                    .lock()
                    .await
                    .get("stdio-1")
                    .map(String::as_str),
                Some("runtime-1"),
                "the live thread map must be intact",
            );
        }
    }

    #[tokio::test]
    async fn successful_config_set_still_invalidates_the_stdio_bridge() {
        crate::install_test_crypto_provider();
        // The other half of #4737: a mutation that *did* happen must still
        // rebuild the bridge, or the runtime keeps serving the old config.
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");
        *state.runtime_bridge.lock().await = Some(sentinel_bridge());

        let response = process_app_request(
            &state,
            AppRequest::ConfigSet {
                key: "model".to_string(),
                value: "deepseek-reasoner".to_string(),
            },
            AppTransport::Stdio,
        )
        .await;
        assert!(response.ok, "valid set should succeed: {response:?}");
        assert_eq!(response.data["value"], "deepseek-reasoner");
        assert!(
            state.runtime_bridge.lock().await.is_none(),
            "a successful config change must invalidate the cached bridge",
        );
    }

    #[tokio::test]
    async fn config_update_keeps_the_stdio_thread_mapping() {
        let (state, _tmp, server) = thread_control::compatibility_fixture().await;
        thread_control::resolve(&state, "canonical-1", true)
            .await
            .unwrap();
        let bridge = state.runtime_bridge.lock().await.clone().unwrap();
        let response =
            process_app_request(&state, AppRequest::ConfigReload, AppTransport::Stdio).await;
        assert!(response.ok, "captured owner reload failed: {response:?}");
        assert!(
            state
                .runtime_bridge
                .lock()
                .await
                .as_ref()
                .is_some_and(|captured| Arc::ptr_eq(captured, &bridge)),
            "reload keeps the already captured owner"
        );
        assert_eq!(
            state
                .runtime_thread_map
                .lock()
                .await
                .get("canonical-1")
                .map(String::as_str),
            Some("canonical-1")
        );
        let result = dispatch_stdio_request(
            &state,
            "thread/message",
            json!({"thread_id":"canonical-1","input":"next"}),
        )
        .await
        .unwrap();
        assert_eq!(result.result["status"], "accepted");
        server.abort();
    }

    #[tokio::test]
    async fn thread_message_on_an_unknown_thread_is_not_found() {
        let (state, _tmp, server) = thread_control::compatibility_fixture().await;
        let error = dispatch_stdio_request(
            &state,
            "thread/message",
            json!({"thread_id":"never-created","input":"hello"}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, THREAD_NOT_FOUND_CODE);
        let error = run_http_thread_message(
            &state,
            "never-created".into(),
            "hello".into(),
            Vec::new(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, THREAD_NOT_FOUND_CODE);
        assert!(state.runtime_thread_map.lock().await.is_empty());
        assert!(state.in_flight_turns.lock().await.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn thread_runtime_link_survives_an_app_server_restart() {
        let (state, tmp, server) = thread_control::compatibility_fixture().await;
        let workspace = state.frontend_workspace.clone().unwrap();
        let owner = state.captured_owner.clone();
        let store = state.runtime.read().await.state_store().clone();
        let mut metadata = codewhale_state::ThreadMetadata {
            cwd: workspace.clone(),
            ..test_client_metadata("client-1")
        };
        metadata.model_provider = "fixture-account".into();
        restore_test_thread_archive(&store, &metadata);
        dispatch_stdio_request(
            &state,
            "thread/message",
            json!({"thread_id":"client-1","input":"first"}),
        )
        .await
        .unwrap();
        let receipt = store
            .get_canonical_runtime_link("client-1", owner.as_ref().unwrap())
            .unwrap()
            .unwrap();
        let bridge = state.runtime_bridge.lock().await.clone();
        drop(state);
        let mut restarted = build_state(Some(tmp.path().join("config.toml")), None).unwrap();
        restarted.captured_owner = owner;
        restarted.frontend_workspace = Some(workspace);
        *restarted.runtime_bridge.lock().await = bridge;
        assert!(restarted.runtime_thread_map.lock().await.is_empty());
        dispatch_stdio_request(
            &restarted,
            "thread/message",
            json!({"thread_id":"client-1","input":"second"}),
        )
        .await
        .unwrap();
        assert_eq!(
            restarted
                .runtime_thread_map
                .lock()
                .await
                .get("client-1")
                .map(String::as_str),
            Some("canonical-1")
        );
        assert_eq!(
            store
                .get_canonical_runtime_link("client-1", restarted.captured_owner.as_ref().unwrap())
                .unwrap()
                .unwrap(),
            receipt
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_mapped_thread_without_a_saved_link_is_linked_on_its_next_turn() {
        let (state, _tmp, server) = thread_control::compatibility_fixture().await;
        let store = state.runtime.read().await.state_store().clone();
        restore_test_thread_archive(
            &store,
            &codewhale_state::ThreadMetadata {
                cwd: state.frontend_workspace.clone().unwrap(),
                ..test_client_metadata("client-1")
            },
        );
        state
            .runtime_thread_map
            .lock()
            .await
            .insert("client-1".into(), "unreceipted-cache".into());
        dispatch_stdio_request(
            &state,
            "thread/message",
            json!({"thread_id":"client-1","input":"hello"}),
        )
        .await
        .unwrap();
        let receipt = store
            .get_canonical_runtime_link("client-1", state.captured_owner.as_ref().unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(receipt.runtime_thread_id, "canonical-1");
        assert_eq!(
            store
                .get_runtime_thread_link("client-1")
                .unwrap()
                .as_deref(),
            Some("canonical-1")
        );
        assert_eq!(
            state
                .runtime_thread_map
                .lock()
                .await
                .get("client-1")
                .map(String::as_str),
            Some("canonical-1")
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_thread_messaged_after_a_restart_starts_in_its_stored_workspace() {
        let (state, _tmp, server) = thread_control::compatibility_fixture().await;
        let workspace = state.frontend_workspace.clone().unwrap();
        let store = state.runtime.read().await.state_store().clone();
        restore_test_thread_archive(
            &store,
            &codewhale_state::ThreadMetadata {
                cwd: workspace.clone(),
                ..test_client_metadata("client-1")
            },
        );
        assert!(state.stdio_thread_hints.lock().await.is_empty());
        dispatch_stdio_request(
            &state,
            "thread/message",
            json!({"thread_id":"client-1","input":"hello"}),
        )
        .await
        .unwrap();
        assert_eq!(
            thread_control::resolve(&state, "client-1", true)
                .await
                .unwrap()
                .1,
            workspace
        );
        assert_eq!(
            store.get_thread("client-1").unwrap().unwrap().cwd,
            workspace
        );
        server.abort();
    }

    #[tokio::test]
    async fn a_link_to_a_runtime_thread_that_is_gone_is_replaced() {
        // Keep the historical case identity. Missing owner checkpoints now
        // refuse; replacing them with an empty conversation loses history.
        let (state, _tmp, server) = thread_control::compatibility_fixture().await;
        let store = state.runtime.read().await.state_store().clone();
        restore_test_thread_archive(
            &store,
            &codewhale_state::ThreadMetadata {
                cwd: state.frontend_workspace.clone().unwrap(),
                ..test_client_metadata("client-1")
            },
        );
        rusqlite::Connection::open(store.db_path()).unwrap().execute(
            "INSERT INTO thread_runtime_links(thread_id,runtime_thread_id,created_at) VALUES('client-1','thr_gone',1)", []
        ).unwrap();
        let before = store.snapshot_legacy_thread_history("client-1").unwrap();
        let error = dispatch_stdio_request(
            &state,
            "thread/message",
            json!({"thread_id":"client-1","input":"hello"}),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, THREAD_NOT_FOUND_CODE);
        assert_eq!(
            store
                .get_runtime_thread_link("client-1")
                .unwrap()
                .as_deref(),
            Some("thr_gone")
        );
        assert!(
            store
                .get_canonical_runtime_link("client-1", state.captured_owner.as_ref().unwrap())
                .unwrap()
                .is_none()
        );
        assert_eq!(
            serde_json::to_value(store.snapshot_legacy_thread_history("client-1").unwrap())
                .unwrap(),
            serde_json::to_value(before).unwrap()
        );
        assert!(state.in_flight_turns.lock().await.is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn restored_thread_link_retries_validation_after_a_transient_failure() {
        let (state, _tmp, server) = thread_control::compatibility_fixture().await;
        let store = state.runtime.read().await.state_store().clone();
        restore_test_thread_archive(
            &store,
            &codewhale_state::ThreadMetadata {
                cwd: state.frontend_workspace.clone().unwrap(),
                ..test_client_metadata("client-1")
            },
        );
        thread_control::resolve(&state, "client-1", true)
            .await
            .unwrap();
        let receipt = store
            .get_canonical_runtime_link("client-1", state.captured_owner.as_ref().unwrap())
            .unwrap();
        let bridge = state.runtime_bridge.lock().await.take();
        let first = acquire_live_runtime_bridge(&state)
            .await
            .expect_err("transient connection refused");
        assert_eq!(first.code, RUNTIME_UNAVAILABLE_CODE);
        *state.runtime_bridge.lock().await = bridge;
        dispatch_stdio_request(
            &state,
            "thread/message",
            json!({"thread_id":"client-1","input":"retry"}),
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .get_canonical_runtime_link("client-1", state.captured_owner.as_ref().unwrap())
                .unwrap(),
            receipt
        );
        server.abort();
    }

    #[cfg(unix)]
    fn dead_bridge(base_url: &str) -> RuntimeBridge {
        let mut child = Command::new("true").spawn().expect("spawn fixture child");
        child.wait().expect("fixture child exits");
        let mut bridge = RuntimeBridge::from_base_url_for_test(base_url.to_string());
        bridge.child = Some(child);
        bridge
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dead_runtime_child_is_replaced_by_a_live_one() {
        crate::install_test_crypto_provider();
        // A crashed or killed child stayed cached, so every later turn failed
        // against it until the app-server restarted.
        let (state, _tmp) = capability_test_state();
        let dead = Arc::new(Mutex::new(dead_bridge("http://dead.invalid")));
        *state.runtime_bridge.lock().await = Some(dead.clone());

        let starts = std::sync::atomic::AtomicUsize::new(0);
        let bridge = acquire_live_runtime_bridge_with(&state, || {
            starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async {
                Ok(RuntimeBridge::from_base_url_for_test(
                    "http://live.invalid".to_string(),
                ))
            }
        })
        .await
        .expect("a live replacement is returned");
        assert_eq!(bridge.base_url, "http://live.invalid");
        drop(bridge);
        assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 1);
        let slot = state.runtime_bridge.lock().await;
        let cached = slot.as_ref().expect("the replacement is cached");
        assert!(!Arc::ptr_eq(cached, &dead), "the dead bridge is evicted");
        assert_eq!(cached.lock().await.base_url, "http://live.invalid");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_runtime_child_that_dies_on_respawn_is_reported_after_one_retry() {
        crate::install_test_crypto_provider();
        let (state, _tmp) = capability_test_state();
        *state.runtime_bridge.lock().await =
            Some(Arc::new(Mutex::new(dead_bridge("http://dead.invalid"))));

        let starts = std::sync::atomic::AtomicUsize::new(0);
        let err = acquire_live_runtime_bridge_with(&state, || {
            starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async { Ok(dead_bridge("http://dead-again.invalid")) }
        })
        .await
        .expect_err("a child that exits at once is not handed out");
        assert_eq!(err.code, RUNTIME_UNAVAILABLE_CODE);
        assert!(
            err.message.contains("exited immediately"),
            "{}",
            err.message
        );
        assert_eq!(
            starts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "respawned exactly once",
        );
        assert!(
            state.runtime_bridge.lock().await.is_none(),
            "no dead bridge stays cached",
        );
    }

    #[tokio::test]
    async fn config_set_keeps_edits_saved_by_other_processes() {
        // Persisting used to write the startup snapshot back over a freshly
        // loaded store, erasing edits another process (the TUI, `login`)
        // had saved since.
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");
        fs::write(
            &config_path,
            "model = \"deepseek-chat\"\ntelemetry = true\n",
        )
        .expect("external edit");

        let response = process_app_request(
            &state,
            AppRequest::ConfigSet {
                key: "model".to_string(),
                value: "deepseek-reasoner".to_string(),
            },
            AppTransport::Stdio,
        )
        .await;
        assert!(response.ok, "set should succeed: {response:?}");
        let persisted = fs::read_to_string(&config_path).expect("read config");
        assert!(persisted.contains("deepseek-reasoner"), "{persisted}");
        assert!(
            persisted.contains("telemetry = true"),
            "the external edit must survive: {persisted}"
        );
        assert_eq!(
            state.config.read().await.telemetry,
            Some(true),
            "the live config reflects what was saved",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn config_store_work_does_not_block_the_runtime() {
        let (state, _tmp) = capability_test_state();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let update_state = state.clone();
        let update = tokio::spawn(async move {
            update_config_store(&update_state, move |store| {
                started_tx
                    .send(())
                    .map_err(|_| anyhow!("configuration test caller disappeared"))?;
                // Simulate a stalled file/store operation. The only Tokio
                // thread must remain free to send the resume signal.
                resume_rx
                    .recv_timeout(Duration::from_secs(3))
                    .context("configuration store work blocked the runtime")?;
                store.config.model = Some("deepseek-reasoner".to_string());
                Ok(())
            })
            .await
        });
        tokio::time::timeout(Duration::from_secs(5), started_rx)
            .await
            .expect("the store operation started")
            .expect("start signal");
        resume_tx
            .send(())
            .expect("Tokio must run while the store operation is waiting");
        update
            .await
            .expect("configuration task joined")
            .expect("configuration update finished");
        assert_eq!(
            state.config.read().await.model.as_deref(),
            Some("deepseek-reasoner")
        );
    }

    #[tokio::test]
    async fn config_set_reports_a_failed_load() {
        crate::install_test_crypto_provider();
        // Persisting failures were logged and swallowed: the reply said ok
        // while disk (and so every future turn) kept the old value.
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");
        *state.runtime_bridge.lock().await = Some(sentinel_bridge());
        fs::write(&config_path, "model = [unterminated\n").expect("corrupt config");

        let response = process_app_request(
            &state,
            AppRequest::ConfigSet {
                key: "model".to_string(),
                value: "deepseek-reasoner".to_string(),
            },
            AppTransport::Stdio,
        )
        .await;
        assert!(!response.ok, "an unsaved change must not report ok");
        assert!(response.data["error"].is_string());
        assert_eq!(
            app_response_status(&response),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a config file the server cannot read is a server fault",
        );
        assert_eq!(
            state.config.read().await.model.as_deref(),
            Some("deepseek-chat"),
            "nothing is propagated when the load fails",
        );
        assert!(
            state.runtime_bridge.lock().await.is_some(),
            "a failed load must not tear down the bridge",
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn config_set_reports_a_failed_save() {
        use std::os::unix::fs::PermissionsExt as _;
        crate::install_test_crypto_provider();
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_dir = tmp.path().join("cfg");
        fs::create_dir(&config_dir).expect("config dir");
        let config_path = config_dir.join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");
        *state.runtime_bridge.lock().await = Some(sentinel_bridge());
        fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o500)).expect("read-only");
        if fs::write(config_dir.join("probe"), "").is_ok() {
            // Running as a user that ignores directory permissions (root);
            // the save cannot be made to fail this way.
            fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o700)).ok();
            return;
        }

        let set = |key: &str, value: &str| AppRequest::ConfigSet {
            key: key.to_string(),
            value: value.to_string(),
        };
        let response = process_app_request(
            &state,
            set("model", "deepseek-reasoner"),
            AppTransport::Stdio,
        )
        .await;
        let rejected =
            process_app_request(&state, set("telemetry", "not-a-bool"), AppTransport::Stdio).await;
        fs::set_permissions(&config_dir, fs::Permissions::from_mode(0o700)).expect("restore");

        assert!(!response.ok, "an unsaved change must not report ok");
        let error = response.data["error"].as_str().expect("error message");
        assert!(error.starts_with("failed to save config"), "{error}");
        assert_eq!(
            app_response_status(&response),
            StatusCode::INTERNAL_SERVER_ERROR,
            "a failed save is a server fault, not a bad request",
        );
        assert!(!rejected.ok);
        assert_eq!(
            app_response_status(&rejected),
            StatusCode::BAD_REQUEST,
            "an invalid value is still the caller's mistake",
        );
        assert_eq!(
            fs::read_to_string(&config_path).expect("read config"),
            "model = \"deepseek-chat\"\n",
        );
        assert_eq!(
            state.config.read().await.model.as_deref(),
            Some("deepseek-chat"),
            "nothing is propagated when the save fails",
        );
        assert!(
            state.runtime_bridge.lock().await.is_some(),
            "a failed save must not tear down the bridge",
        );
    }

    #[tokio::test]
    async fn a_saved_config_change_still_propagates_when_the_caller_goes_away() {
        crate::install_test_crypto_provider();
        // Once the file was written, a request future dropped before the
        // propagation (an HTTP client disconnect) left disk ahead of the live
        // runtime and the cached bridge until a reload or restart.
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");
        *state.runtime_bridge.lock().await = Some(sentinel_bridge());

        // A concurrent reader holds the runtime, so the set
        // saves to disk and then waits to propagate.
        let runtime_reader = state.runtime.read().await;
        let request = process_app_request(
            &state,
            AppRequest::ConfigSet {
                key: "model".to_string(),
                value: "deepseek-reasoner".to_string(),
            },
            AppTransport::Http,
        );
        // Long enough for the save to reach disk on a slow runner: 200 ms was
        // not, once, on hosted Windows.
        assert!(
            tokio::time::timeout(Duration::from_secs(2), request)
                .await
                .is_err(),
            "the set waits for the runtime",
        );
        assert!(
            fs::read_to_string(&config_path)
                .expect("read config")
                .contains("deepseek-reasoner"),
            "the change was saved before the caller went away",
        );
        drop(runtime_reader);

        tokio::time::timeout(Duration::from_secs(5), async {
            while state.runtime_bridge.lock().await.is_some() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the saved change still invalidates the bridge");
        assert_eq!(
            state.runtime.read().await.config.model.as_deref(),
            Some("deepseek-reasoner"),
        );
        assert_eq!(
            state.config.read().await.model.as_deref(),
            Some("deepseek-reasoner"),
        );
    }

    #[tokio::test]
    async fn config_reload_reads_disk_after_the_earlier_queued_write() {
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").unwrap();
        let state = build_state(Some(config_path.clone()), None).unwrap();
        let reader = state.config.read().await;
        let set_state = state.clone();
        let set = tokio::spawn(async move {
            process_app_request(
                &set_state,
                AppRequest::ConfigSet {
                    key: "model".into(),
                    value: "deepseek-reasoner".into(),
                },
                AppTransport::Http,
            )
            .await
        });
        // Tokio's writer-preferring lock rejects new readers only once the
        // first writer is queued. Keep the original reader until reload has
        // also been polled, deterministically reproducing the stale-load race.
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.config.try_read().is_ok() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut reload = Box::pin(process_app_request(
            &state,
            AppRequest::ConfigReload,
            AppTransport::Http,
        ));
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(reload.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(reader);
        let (set, reload) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(set, reload) })
                .await
                .expect("both queued operations finish");
        assert!(set.unwrap().ok);
        assert!(reload.ok);
        assert_eq!(
            state.config.read().await.model.as_deref(),
            Some("deepseek-reasoner")
        );
        assert_eq!(
            state.runtime.read().await.config.model.as_deref(),
            Some("deepseek-reasoner")
        );
        assert_eq!(
            ConfigStore::load(Some(config_path))
                .unwrap()
                .config
                .model
                .as_deref(),
            Some("deepseek-reasoner")
        );
    }

    #[tokio::test]
    async fn config_reload_finishes_propagation_when_the_caller_goes_away() {
        crate::install_test_crypto_provider();
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").unwrap();
        let state = build_state(Some(config_path.clone()), None).unwrap();
        *state.runtime_bridge.lock().await = Some(sentinel_bridge());
        fs::write(&config_path, "model = \"deepseek-reasoner\"\n").unwrap();
        let reader = state.runtime.read().await;
        let mut reload = Box::pin(process_app_request(
            &state,
            AppRequest::ConfigReload,
            AppTransport::Http,
        ));
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(reload.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.config.read().await.model.as_deref() != Some("deepseek-reasoner") {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(reload);
        drop(reader);
        tokio::time::timeout(Duration::from_secs(5), async {
            while state.runtime_bridge.lock().await.is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reload propagation outlives its caller");
        assert_eq!(
            state.runtime.read().await.config.model.as_deref(),
            Some("deepseek-reasoner")
        );
        assert_eq!(
            fs::read_to_string(config_path).unwrap(),
            "model = \"deepseek-reasoner\"\n",
            "reload must not rewrite the file"
        );
    }

    #[tokio::test]
    async fn config_unset_propagates_to_runtime_config() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            "api_key = \"sk-deepseek-secret\"\nmodel = \"deepseek-chat\"\n",
        )
        .expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");

        // Sanity: runtime starts with the on-disk model.
        {
            let runtime = state.runtime.read().await;
            assert_eq!(runtime.config.model.as_deref(), Some("deepseek-chat"));
        }

        // Unset the model via the API. This walks a separate code path
        // from ConfigSet (unset_value + update_config), so it needs its
        // own regression coverage.
        let response = process_app_request(
            &state,
            AppRequest::ConfigUnset {
                key: "model".to_string(),
            },
            AppTransport::Stdio,
        )
        .await;
        assert!(response.ok, "unset should succeed");

        // Live runtime sees the cleared model.
        {
            let runtime = state.runtime.read().await;
            assert!(runtime.config.model.is_none());
        }
        // Shared config lock agrees.
        {
            let cfg = state.config.read().await;
            assert!(cfg.model.is_none());
        }
        // The on-disk file no longer carries the model value.
        let persisted = fs::read_to_string(&config_path).expect("read config");
        assert!(!persisted.contains("deepseek-chat"));
    }

    #[tokio::test]
    async fn config_reload_returns_error_when_disk_config_is_invalid() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(
            &config_path,
            "api_key = \"sk-deepseek-secret\"\nmodel = \"deepseek-chat\"\n",
        )
        .expect("write config");
        let state = build_state(Some(config_path.clone()), None).expect("state");

        // Corrupt the on-disk config so ConfigStore::load fails to parse.
        fs::write(&config_path, "api_key = \"unterminated\n").expect("corrupt config");

        let response =
            process_app_request(&state, AppRequest::ConfigReload, AppTransport::Stdio).await;
        assert!(!response.ok, "reload of corrupt config must fail");
        let err = response.data["error"]
            .as_str()
            .expect("error message present")
            .to_string();
        assert!(
            err.contains("failed to load config"),
            "error should mention load failure, got: {err}"
        );

        // Live state is untouched: the early-return on load error must
        // not have clobbered runtime.config or state.config.
        {
            let runtime = state.runtime.read().await;
            assert_eq!(runtime.config.model.as_deref(), Some("deepseek-chat"));
        }
        {
            let cfg = state.config.read().await;
            assert_eq!(cfg.model.as_deref(), Some("deepseek-chat"));
        }
    }

    async fn seed_test_bridge(state: &AppState) -> SharedRuntimeBridge {
        let bridge = Arc::new(Mutex::new(RuntimeBridge::from_base_url_for_test(
            "http://127.0.0.1:9".to_string(),
        )));
        *state.runtime_bridge.lock().await = Some(bridge.clone());
        bridge
    }

    #[tokio::test]
    async fn config_set_invalidates_cached_stdio_bridge() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").expect("write config");
        let state = build_state(Some(config_path), None).expect("state");
        seed_test_bridge(&state).await;

        let response = process_app_request(
            &state,
            AppRequest::ConfigSet {
                key: "model".to_string(),
                value: "deepseek-reasoner".to_string(),
            },
            AppTransport::Stdio,
        )
        .await;
        assert!(response.ok, "set should succeed");

        // The cached bridge child must be dropped so the next stdio request
        // spawns a fresh runtime that reads the persisted config.
        assert!(state.runtime_bridge.lock().await.is_none());
    }

    #[tokio::test]
    async fn config_reload_invalidates_cached_stdio_bridge() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "model = \"deepseek-chat\"\n").expect("write config");
        let state = build_state(Some(config_path), None).expect("state");
        seed_test_bridge(&state).await;

        let response =
            process_app_request(&state, AppRequest::ConfigReload, AppTransport::Stdio).await;
        assert!(response.ok, "reload should succeed");

        assert!(state.runtime_bridge.lock().await.is_none());
    }

    #[tokio::test]
    async fn stdio_bridge_invalidation_not_blocked_by_in_flight_turn() {
        let (state, _tmp) = capability_test_state();
        let bridge = seed_test_bridge(&state).await;

        // Simulate a long streaming turn holding the inner bridge lock.
        let _in_flight = bridge.lock().await;

        // Invalidation only touches the cache slot, so it must complete
        // without waiting for the in-flight turn to release the bridge.
        tokio::time::timeout(Duration::from_secs(1), invalidate_runtime_bridge(&state))
            .await
            .expect("invalidation must not wait on bridge traffic");
        assert!(state.runtime_bridge.lock().await.is_none());
    }

    #[tokio::test]
    async fn runtime_read_paths_run_concurrently() {
        // Tool/status/mcp handlers take read guards; two must coexist so a
        // long-running tool call cannot serialize unrelated requests. With
        // the old `Mutex<Runtime>` this pattern would deadlock.
        let (state, _tmp) = capability_test_state();
        let first = state.runtime.read().await;
        let second = state.runtime.read().await;
        assert!(first.app_status().ok);
        assert!(second.app_status().ok);
    }

    #[tokio::test]
    async fn health_probes_advertise_legacy_deepseek_service_name() {
        // External probes still key off the DeepSeek-era service name; both
        // transports must serve it from the single compat shim.
        let (app, _tmp) = app_with_config(None);
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/healthz")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        let body = response_body_json(response).await;
        assert_eq!(body["service"], legacy_deepseek_compat::SERVICE_NAME);
        assert_eq!(body["service"], "deepseek-app-server");

        let (state, _tmp) = capability_test_state();
        let stdio = dispatch_stdio_request(&state, "healthz", json!({}))
            .await
            .expect("stdio healthz");
        assert_eq!(
            stdio.result["service"],
            legacy_deepseek_compat::SERVICE_NAME
        );
    }

    #[test]
    fn non_loopback_bind_without_auth_fails_fast() {
        let options = AppServerOptions {
            listen: "0.0.0.0:8787".parse().expect("socket addr"),
            config_path: None,
            auth_token: None,
            insecure_no_auth: false,
            cors_origins: Vec::new(),
        };

        let err =
            resolve_auth_token(&options).expect_err("non-loopback generated auth should fail");
        assert!(err.to_string().contains("without explicit auth token"));
    }

    #[tokio::test]
    async fn stdio_transport_redacts_config_get_secrets() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "").expect("write config");
        let state = build_state(Some(config_path), None).expect("state");
        {
            let mut cfg = state.config.write().await;
            cfg.providers.deepseek.api_key = Some("sk-deepseek-secret".to_string());
        }

        let response = process_app_request(
            &state,
            AppRequest::ConfigGet {
                key: "api_key".to_string(),
            },
            AppTransport::Stdio,
        )
        .await;

        assert_eq!(response.data["value"], "sk-d***cret");
    }

    #[tokio::test]
    async fn stdio_thread_goal_methods_round_trip_persisted_goal() {
        let (state, _tmp, server) = thread_control::compatibility_fixture().await;
        let capabilities = dispatch_stdio_request(&state, "thread/capabilities", json!({}))
            .await
            .unwrap();
        assert!(
            capabilities.result["methods"]
                .as_array()
                .unwrap()
                .iter()
                .any(|method| method == "thread/goal/set")
        );
        let started = dispatch_stdio_request(
            &state,
            "thread/start",
            json!({"operation_key":"goal-start"}),
        )
        .await
        .unwrap();
        let id = started.result["thread_id"].as_str().unwrap();
        let set = dispatch_stdio_request(
            &state,
            "thread/goal/set",
            json!({"thread_id":id,"objective":"Release 0.10.1","token_budget":59000}),
        )
        .await
        .unwrap();
        assert_eq!(set.result["goal"]["objective"], "Release 0.10.1");
        assert_eq!(set.result["goal"]["status"], "active");
        let got = dispatch_stdio_request(&state, "thread/goal/get", json!({"thread_id":id}))
            .await
            .unwrap();
        assert_eq!(got.result["goal"]["token_budget"], 59000);
        let cleared = dispatch_stdio_request(&state, "thread/goal/clear", json!({"thread_id":id}))
            .await
            .unwrap();
        assert_eq!(cleared.result["status"], "cleared");
        assert_eq!(cleared.result["data"]["cleared"], true);
        server.abort();
    }

    #[tokio::test]
    async fn stdio_resume_of_missing_thread_fails_without_clobbering_the_hint() {
        let (state, tmp, server) = thread_control::compatibility_fixture().await;
        let workspace = tmp.path().join("ws");
        state.stdio_thread_hints.lock().await.insert(
            "ghost-thread".into(),
            RuntimeThreadHint {
                model: Some("fixture-model".into()),
                workspace: Some(workspace.clone()),
            },
        );
        for method in ["thread/resume", "thread/fork"] {
            let error = dispatch_stdio_request(
                &state,
                method,
                json!({"thread_id":"ghost-thread","operation_key":format!("{method}-missing")}),
            )
            .await
            .unwrap_err();
            assert_eq!(error.code, THREAD_NOT_FOUND_CODE);
            assert!(error.message.contains("ghost-thread"));
        }
        let hints = state.stdio_thread_hints.lock().await;
        assert_eq!(
            hints["ghost-thread"].model.as_deref(),
            Some("fixture-model")
        );
        assert_eq!(hints["ghost-thread"].workspace.as_ref(), Some(&workspace));
        server.abort();
    }

    #[tokio::test]
    async fn stdio_archive_of_missing_thread_fails_instead_of_reporting_success() {
        let (state, _tmp, server) = thread_control::compatibility_fixture().await;
        for method in ["thread/archive", "thread/unarchive"] {
            let error = dispatch_stdio_request(&state, method, json!({"thread_id":"ghost-thread"}))
                .await
                .unwrap_err();
            assert_eq!(error.code, THREAD_NOT_FOUND_CODE);
            assert!(error.message.contains("ghost-thread"));
        }
        server.abort();
    }

    fn sse_frame(event: &str, payload: Value) -> String {
        format!("event: {event}\ndata: {payload}\n\n")
    }
    /// A runtime whose turn never ends on its own — only an interrupt stops
    /// it. That is the shape of the runaway turn this protects against.
    async fn spawn_uninterruptible_until_asked_runtime(
        owner_router: Router,
    ) -> (
        String,
        Arc<tokio::sync::Notify>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::body::Body;
        use axum::extract::Path as AxumPath;

        let interrupted = Arc::new(tokio::sync::Notify::new());

        async fn create_turn(AxumPath(_thread_id): AxumPath<String>) -> Json<Value> {
            Json(json!({ "turn": { "id": "turn_runaway" } }))
        }
        async fn create_thread() -> Json<Value> {
            Json(json!({ "id": "thr_runaway" }))
        }
        async fn interrupt(
            State(notify): State<Arc<tokio::sync::Notify>>,
            AxumPath((_thread_id, _turn_id)): AxumPath<(String, String)>,
        ) -> Json<Value> {
            notify.notify_waiters();
            Json(json!({ "ok": true }))
        }
        async fn thread_events(
            State(notify): State<Arc<tokio::sync::Notify>>,
            AxumPath(_thread_id): AxumPath<String>,
        ) -> ([(header::HeaderName, &'static str); 1], Body) {
            // Hold the event response open until something interrupts the
            // turn. Nothing else can end it, which is the point.
            notify.notified().await;
            let body = [
                sse_frame(
                    "item.delta",
                    json!({
                        "seq": 1,
                        "turn_id": "turn_runaway",
                        "payload": { "kind": "agent_message", "delta": "thinking" }
                    }),
                ),
                sse_frame(
                    "turn.completed",
                    json!({
                        "seq": 2,
                        "turn_id": "turn_runaway",
                        "payload": { "turn": { "status": "interrupted" } }
                    }),
                ),
            ]
            .concat();
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                Body::from(body),
            )
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let app = Router::new()
            .route("/v1/threads", post(create_thread))
            .route("/v1/threads/{thread_id}/turns", post(create_turn))
            .route(
                "/v1/threads/{thread_id}/turns/{turn_id}/interrupt",
                post(interrupt),
            )
            .route("/v1/threads/{thread_id}/events", get(thread_events))
            .with_state(interrupted.clone())
            .fallback_service(owner_router);
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test runtime");
        });
        (format!("http://{addr}"), interrupted, server)
    }

    #[tokio::test]
    async fn interrupt_stops_a_turn_that_would_otherwise_stream_forever() {
        let (state, _tmp, owner_router) = thread_control::compatibility_router();
        let (base_url, _notify, server) =
            spawn_uninterruptible_until_asked_runtime(owner_router).await;
        seed_client_thread(&state, "thr_a").await;
        *state.runtime_bridge.lock().await = Some(Arc::new(Mutex::new(
            RuntimeBridge::from_base_url_for_test(base_url),
        )));

        let (client, server_side) = tokio::io::duplex(16 * 1024);
        let (client_reader, mut client_writer) = tokio::io::split(client);

        let loop_state = state.clone();
        let loop_handle = tokio::spawn(async move {
            let (rx, tx) = tokio::io::split(server_side);
            run_stdio_loop(
                &loop_state,
                BoundedLines::new(BufReader::new(rx)),
                tx,
                StdioLoopPolicy::process_stdio(),
                None::<()>,
            )
            .await
        });

        // Start the runaway turn.
        client_writer
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"thread/message\",\
                  \"params\":{\"thread_id\":\"thr_a\",\"input\":\"go\"}}\n",
            )
            .await
            .expect("send thread/message");

        // Wait until the turn is genuinely in flight before cancelling, so the
        // test exercises mid-stream cancellation rather than a race.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if state.in_flight_turns.lock().await.contains_key("thr_a") {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("turn should register itself as in flight");

        // The read loop must accept this while the turn holds the bridge.
        client_writer
            .write_all(
                b"{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"thread/interrupt\",\
                  \"params\":{\"thread_id\":\"thr_a\"}}\n",
            )
            .await
            .expect("send thread/interrupt");
        client_writer
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"shutdown\"}\n")
            .await
            .expect("send shutdown");

        let finished = tokio::time::timeout(Duration::from_secs(20), loop_handle)
            .await
            .expect("the loop must exit rather than hang on the runaway turn");
        finished.expect("join loop").expect("loop result");

        let mut output = String::new();
        let mut lines = BufReader::new(client_reader);
        lines
            .read_to_string(&mut output)
            .await
            .expect("read stdio output");

        let responses: Vec<Value> = output
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .collect();
        let by_id = |id: u64| {
            responses
                .iter()
                .find(|value| value["id"] == json!(id))
                .unwrap_or_else(|| panic!("no response for id {id} in {output}"))
                .clone()
        };

        // The turn ended as interrupted rather than running to completion.
        assert!(
            by_id(1)["error"].is_object(),
            "the interrupted turn should report an error, got: {}",
            by_id(1)
        );
        assert_eq!(by_id(2)["result"]["interrupted"], json!(true));
        assert_eq!(by_id(3)["result"]["status"], json!("stopped"));

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn interrupting_an_idle_thread_is_not_an_error() {
        let (state, _tmp) = capability_test_state();
        let response = dispatch_stdio_request(
            &state,
            "thread/interrupt",
            json!({ "thread_id": "thr_nothing_running" }),
        )
        .await
        .expect("interrupt dispatch");
        assert_eq!(response.result["interrupted"], json!(false));
    }

    #[tokio::test]
    async fn output_cap_bridge_checks_support_before_creation_and_forwards_each_surface() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        #[derive(Clone)]
        struct Fixture {
            supported: Arc<AtomicBool>,
            created: Arc<AtomicUsize>,
            requests: Arc<Mutex<Vec<Value>>>,
        }
        async fn info(State(f): State<Fixture>, headers: axum::http::HeaderMap) -> Json<Value> {
            assert_eq!(
                headers.get(header::AUTHORIZATION).unwrap(),
                "Bearer fixture-output-cap"
            );
            Json(
                json!({"capabilities":{"turn_output_token_limit":f.supported.load(Ordering::SeqCst)}}),
            )
        }
        async fn providers() -> Json<Value> {
            Json(
                json!({"current":"custom","providers":[{"id":"custom","default_model":"fixture-model"}]}),
            )
        }
        async fn models() -> Json<Value> {
            Json(
                json!({"models":[{"id":"fixture-model","output_token_limit":"supported"},{"id":"uncapped-transport","output_token_limit":"unsupported"}]}),
            )
        }
        async fn create_thread(State(f): State<Fixture>) -> Json<Value> {
            let n = f.created.fetch_add(1, Ordering::SeqCst);
            Json(json!({"id":format!("thr_cap_{n}")}))
        }
        async fn create_turn(State(f): State<Fixture>, Json(body): Json<Value>) -> Json<Value> {
            f.requests.lock().await.push(body);
            Json(json!({"turn":{"id":"turn_cap"}}))
        }
        async fn events(State(f): State<Fixture>) -> impl IntoResponse {
            let seq = f.requests.lock().await.len();
            (
                [(header::CONTENT_TYPE, "text/event-stream")],
                sse_frame(
                    "turn.completed",
                    json!({
                        "seq":seq,"turn_id":"turn_cap","payload":{"turn":{"status":"completed"}}
                    }),
                ),
            )
        }
        let fixture = Fixture {
            supported: Arc::new(AtomicBool::new(false)),
            created: Arc::new(AtomicUsize::new(0)),
            requests: Arc::new(Mutex::new(Vec::new())),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (state, _tmp, owner_router) = thread_control::compatibility_router();
        let router = Router::new()
            .route("/v1/runtime/info", get(info))
            .route("/v1/providers", get(providers))
            .route("/v1/providers/custom/models", get(models))
            .route("/v1/threads", post(create_thread))
            .route("/v1/threads/{id}/turns", post(create_turn))
            .route("/v1/threads/{id}/events", get(events))
            .with_state(fixture.clone())
            .fallback_service(owner_router);
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut bridge = RuntimeBridge::from_base_url_for_test(format!("http://{addr}"));
        bridge.auth_token = Some("fixture-output-cap".into());
        *state.runtime_bridge.lock().await = Some(Arc::new(Mutex::new(bridge)));
        let result = dispatch_stdio_request(
            &state,
            "prompt/run",
            json!({"prompt":"review","maxOutputTokens":1500}),
        )
        .await;
        assert!(result.unwrap_err().message.contains("does not support"));
        assert_eq!(fixture.created.load(Ordering::SeqCst), 0);
        assert!(fixture.requests.lock().await.is_empty());
        fixture.supported.store(true, Ordering::SeqCst);
        for model in ["auto", "uncapped-transport", "unknown-model"] {
            assert!(
                dispatch_stdio_request(
                    &state,
                    "prompt/run",
                    json!({"prompt":"review","model":model,"maxOutputTokens":1500})
                )
                .await
                .is_err()
            );
        }
        assert_eq!(fixture.created.load(Ordering::SeqCst), 0);
        for thread_id in ["stdio-cap", "request-cap", "http-cap"] {
            seed_client_thread(&state, thread_id).await;
        }
        for (method, params) in [
            (
                "prompt/run",
                json!({"prompt":"review","maxOutputTokens":1500}),
            ),
            (
                "thread/message",
                json!({"thread_id":"stdio-cap","input":"review","maxOutputTokens":1500}),
            ),
            (
                "thread/request",
                json!({"kind":"message","thread_id":"request-cap","input":"review","maxOutputTokens":1500}),
            ),
        ] {
            dispatch_stdio_request(&state, method, params)
                .await
                .expect("existing app-server caller forwards allowance");
        }
        run_http_thread_message(
            &state,
            "http-cap".into(),
            "review".into(),
            Vec::new(),
            std::num::NonZeroU32::new(1500),
        )
        .await
        .unwrap();
        let requests = fixture.requests.lock().await.clone();
        assert_eq!(requests.len(), 4);
        assert!(
            requests
                .iter()
                .all(|request| request["maxOutputTokens"] == 1500)
        );
        let count = fixture.created.load(Ordering::SeqCst);
        for invalid in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!("1500"),
            json!(4_294_967_296u64),
        ] {
            assert!(
                dispatch_stdio_request(
                    &state,
                    "prompt/run",
                    json!({"prompt":"review","maxOutputTokens":invalid})
                )
                .await
                .is_err()
            );
        }
        assert_eq!(fixture.created.load(Ordering::SeqCst), count);
        assert_eq!(fixture.requests.lock().await.len(), 4);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn stdio_runtime_bridge_streams_response_delta_events() {
        async fn create_turn(AxumPath(thread_id): AxumPath<String>) -> Json<Value> {
            Json(json!({
                "thread": { "id": thread_id },
                "turn": { "id": "turn_test" },
            }))
        }

        async fn thread_events(
            AxumPath(thread_id): AxumPath<String>,
            Query(query): Query<HashMap<String, String>>,
        ) -> ([(header::HeaderName, &'static str); 1], String) {
            assert_eq!(thread_id, "thr_test");
            assert_eq!(query.get("since_seq").map(String::as_str), Some("0"));

            let body = [
                sse_frame(
                    "item.delta",
                    json!({
                        "seq": 1,
                        "turn_id": "turn_test",
                        "payload": {
                            "kind": "agent_message",
                            "delta": "hello"
                        }
                    }),
                ),
                sse_frame(
                    "turn.completed",
                    json!({
                        "seq": 2,
                        "turn_id": "turn_test",
                        "payload": {
                            "turn": {
                                "status": "completed"
                            }
                        }
                    }),
                ),
            ]
            .concat();

            ([(header::CONTENT_TYPE, "text/event-stream")], body)
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let app = Router::new()
            .route("/v1/threads/{thread_id}/turns", post(create_turn))
            .route("/v1/threads/{thread_id}/events", get(thread_events));

        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test runtime");
        });

        let mut bridge = RuntimeBridge::from_base_url_for_test(format!("http://{addr}"));
        let (mut reader, mut writer) = tokio::io::duplex(4096);

        let result = bridge
            .message_thread(
                "thr_test",
                RuntimeTurnInput {
                    input: "hello",
                    images: &[],
                    max_output_tokens: None,
                    expected_workspace: None,
                },
                &mut writer,
                None,
                None,
            )
            .await
            .expect("message_thread should succeed");
        drop(writer);

        let mut stdout = Vec::new();
        reader
            .read_to_end(&mut stdout)
            .await
            .expect("read stdio output");
        server.abort();
        let _ = server.await;

        let lines: Vec<Value> = String::from_utf8(stdout)
            .expect("utf8 output")
            .lines()
            .map(|line| serde_json::from_str(line).expect("json line"))
            .collect();

        assert_eq!(
            result.get("status").and_then(Value::as_str),
            Some("accepted")
        );
        assert_eq!(
            result.pointer("/data/turn_id").and_then(Value::as_str),
            Some("turn_test")
        );
        assert_eq!(bridge.last_seq_by_thread.get("thr_test"), Some(&2));

        let event_types: Vec<&str> = lines
            .iter()
            .map(|line| {
                line.get("type")
                    .and_then(Value::as_str)
                    .expect("event type")
            })
            .collect();
        assert_eq!(
            event_types,
            vec!["response_start", "response_delta", "response_end"]
        );
        assert_eq!(lines[1]["delta"], "hello");
    }

    /// Audit R03-05: a stream that breaks before `turn.completed` must not
    /// report `response_end` ahead of the error, and must not leave the
    /// runtime turn running with nothing able to interrupt it.
    #[tokio::test]
    async fn stdio_runtime_bridge_interrupts_a_turn_whose_stream_breaks() {
        static INTERRUPTED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);

        async fn create_turn(AxumPath(thread_id): AxumPath<String>) -> Json<Value> {
            Json(json!({
                "thread": { "id": thread_id },
                "turn": { "id": "turn_broken" },
            }))
        }

        async fn thread_events() -> ([(header::HeaderName, &'static str); 1], String) {
            // One delta, then the stream ends without `turn.completed`.
            let body = sse_frame(
                "item.delta",
                json!({
                    "seq": 1,
                    "turn_id": "turn_broken",
                    "payload": { "kind": "agent_message", "delta": "partial" }
                }),
            );
            ([(header::CONTENT_TYPE, "text/event-stream")], body)
        }

        async fn interrupt(
            AxumPath((thread_id, turn_id)): AxumPath<(String, String)>,
        ) -> Json<Value> {
            assert_eq!(thread_id, "thr_broken");
            assert_eq!(turn_id, "turn_broken");
            INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
            Json(json!({ "interrupted": true }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let app = Router::new()
            .route("/v1/threads/{thread_id}/turns", post(create_turn))
            .route("/v1/threads/{thread_id}/events", get(thread_events))
            .route(
                "/v1/threads/{thread_id}/turns/{turn_id}/interrupt",
                post(interrupt),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test runtime");
        });

        let mut bridge = RuntimeBridge::from_base_url_for_test(format!("http://{addr}"));
        let (mut reader, mut writer) = tokio::io::duplex(4096);
        let result = bridge
            .message_thread(
                "thr_broken",
                RuntimeTurnInput {
                    input: "hello",
                    images: &[],
                    max_output_tokens: None,
                    expected_workspace: None,
                },
                &mut writer,
                None,
                None,
            )
            .await;
        drop(writer);
        let mut stdout = Vec::new();
        reader
            .read_to_end(&mut stdout)
            .await
            .expect("read stdio output");
        server.abort();
        let _ = server.await;

        assert!(result.is_err(), "a broken stream is an error");
        let event_types: Vec<String> = String::from_utf8(stdout)
            .expect("utf8 output")
            .lines()
            .map(|line| {
                serde_json::from_str::<Value>(line).expect("json line")["type"]
                    .as_str()
                    .expect("event type")
                    .to_string()
            })
            .collect();
        assert_eq!(event_types, vec!["response_start", "response_delta"]);
        assert!(
            INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst),
            "the orphaned runtime turn was not interrupted"
        );
    }

    #[tokio::test]
    async fn stdio_runtime_bridge_applies_thread_start_hints() {
        async fn create_thread(Json(body): Json<Value>) -> Json<Value> {
            assert_eq!(body["model"], "deepseek-v4");
            assert_eq!(body["workspace"], "/tmp/codewhale-stdio");
            Json(json!({
                "id": "thr_runtime",
                "model": body["model"].clone(),
                "workspace": body["workspace"].clone(),
            }))
        }

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let addr = listener.local_addr().expect("listener addr");
        let app = Router::new().route("/v1/threads", post(create_thread));

        let server = tokio::spawn(async move {
            axum::serve(listener, app)
                .await
                .expect("serve test runtime");
        });

        let mut bridge = RuntimeBridge::from_base_url_for_test(format!("http://{addr}"));
        let mut thread_map = HashMap::new();
        let runtime_id = bridge
            .ensure_runtime_thread(
                &mut thread_map,
                "legacy_thread",
                Some(RuntimeThreadHint {
                    model: Some("deepseek-v4".to_string()),
                    workspace: Some(PathBuf::from("/tmp/codewhale-stdio")),
                }),
            )
            .await
            .expect("runtime thread");
        server.abort();
        let _ = server.await;

        assert_eq!(runtime_id, "thr_runtime");
        assert_eq!(
            thread_map.get("legacy_thread").map(String::as_str),
            Some("thr_runtime")
        );
    }

    // ── prompt routing runs a real turn ────────────────────────────────
    //
    // `/prompt`, `prompt/request` and `prompt/run` used to return HTTP 200
    // with a stringified echo of the caller's own routing metadata, having
    // called no model at all. These stand up the in-crate stub runtime and
    // assert the response is what the model streamed — not an echo — and
    // that an unreachable runtime is an explicit typed failure.

    /// Prompts the stub runtime was actually asked to run.
    type StubPrompts = Arc<Mutex<Vec<String>>>;

    /// A minimal but honest runtime: it creates threads, starts turns, and
    /// streams `agent_message` deltas followed by `turn.completed`.
    async fn spawn_stub_runtime(
        owner_router: Option<Router>,
    ) -> (String, StubPrompts, tokio::task::JoinHandle<()>) {
        async fn create_thread(Json(body): Json<Value>) -> Json<Value> {
            Json(json!({
                "id": "thr_stub",
                "model": body["model"].as_str().unwrap_or("stub-model-v1"),
            }))
        }

        async fn create_turn(
            State(prompts): State<StubPrompts>,
            AxumPath(thread_id): AxumPath<String>,
            Json(body): Json<Value>,
        ) -> Json<Value> {
            prompts
                .lock()
                .await
                .push(body["prompt"].as_str().unwrap_or_default().to_string());
            Json(json!({
                "thread": { "id": thread_id, "model": "stub-model-v1" },
                "turn": { "id": "turn_stub" },
            }))
        }

        async fn thread_events(
            AxumPath(_thread_id): AxumPath<String>,
        ) -> ([(header::HeaderName, &'static str); 1], String) {
            let body = [
                sse_frame(
                    "item.delta",
                    json!({
                        "seq": 1,
                        "turn_id": "turn_stub",
                        "payload": { "kind": "agent_message", "delta": "the answer" }
                    }),
                ),
                sse_frame(
                    "item.delta",
                    json!({
                        "seq": 2,
                        "turn_id": "turn_stub",
                        "payload": { "kind": "agent_message", "delta": " is 4" }
                    }),
                ),
                sse_frame(
                    "turn.completed",
                    json!({
                        "seq": 3,
                        "turn_id": "turn_stub",
                        "payload": { "turn": { "status": "completed" } }
                    }),
                ),
            ]
            .concat();
            ([(header::CONTENT_TYPE, "text/event-stream")], body)
        }

        let prompts: StubPrompts = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stub runtime");
        let addr = listener.local_addr().expect("listener addr");
        let app = Router::new()
            .route("/v1/threads", post(create_thread))
            .route("/v1/threads/{thread_id}/turns", post(create_turn))
            .route("/v1/threads/{thread_id}/events", get(thread_events))
            .with_state(prompts.clone());
        let app = match owner_router {
            Some(owner) => app.fallback_service(owner),
            None => app,
        };
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), prompts, server)
    }

    async fn seed_bridge_at(state: &AppState, base_url: String) -> SharedRuntimeBridge {
        let bridge = Arc::new(Mutex::new(RuntimeBridge::from_base_url_for_test(base_url)));
        *state.runtime_bridge.lock().await = Some(bridge.clone());
        bridge
    }

    #[tokio::test]
    async fn prompt_request_executes_a_genuine_model_turn() {
        let (state, _tmp) = capability_test_state();
        let (base_url, prompts, server) = spawn_stub_runtime(None).await;
        seed_bridge_at(&state, base_url).await;

        let (mut reader, mut writer) = tokio::io::duplex(4096);
        let dispatched = dispatch_stdio_request_with_writer(
            &state,
            &mut writer,
            "prompt/request",
            json!({ "prompt": "what is 2+2" }),
            AppTransport::Stdio,
        )
        .await
        .expect("prompt/request dispatch");
        drop(writer);

        let response: PromptResponse =
            serde_json::from_value(dispatched.result).expect("prompt response");

        // The model's words, not a restatement of the request.
        assert_eq!(response.output, "the answer is 4");
        assert!(
            !response.output.contains("what is 2+2"),
            "prompt echo leaked into the output: {}",
            response.output
        );
        assert_eq!(response.model, "stub-model-v1");
        assert_eq!(
            prompts.lock().await.as_slice(),
            ["what is 2+2".to_string()],
            "the prompt must reach the runtime's turn endpoint"
        );

        // Real streaming frames, not three canned ones.
        let deltas: Vec<String> = response
            .events
            .iter()
            .filter_map(|event| match event {
                EventFrame::ResponseDelta { delta, .. } => Some(delta.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(deltas, vec!["the answer".to_string(), " is 4".to_string()]);
        assert!(matches!(
            response.events.first(),
            Some(EventFrame::ResponseStart { .. })
        ));
        assert!(matches!(
            response.events.last(),
            Some(EventFrame::ResponseEnd { .. })
        ));

        // The stdio transport sees the same turn stream `thread/message` emits.
        let mut stdout = Vec::new();
        reader.read_to_end(&mut stdout).await.expect("read stdout");
        let stdout = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(
            stdout.contains("\"type\":\"response_delta\"") && stdout.contains("the answer"),
            "stdio prompt turn must stream its deltas, got: {stdout}"
        );

        // A prompt without a thread_id must not leave a mapping behind.
        assert!(
            state.runtime_thread_map.lock().await.is_empty(),
            "one-shot prompt threads must not accumulate in the map"
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn prompt_without_a_reachable_runtime_fails_explicitly() {
        let (state, _tmp) = capability_test_state();
        // Port 9 (discard) refuses immediately: no runtime is listening.
        seed_bridge_at(&state, "http://127.0.0.1:9".to_string()).await;

        let err = dispatch_stdio_request(&state, "prompt/run", json!({ "prompt": "hello" }))
            .await
            .expect_err("a prompt with no reachable runtime must fail, not echo");
        assert_eq!(err.code, RUNTIME_UNAVAILABLE_CODE);

        let (status, Json(body)) = http_error_from_jsonrpc(err);
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "runtime_unavailable");
        assert!(
            body.get("output").is_none(),
            "a failure must not be shaped like a PromptResponse: {body}"
        );
    }

    #[tokio::test]
    async fn empty_prompt_is_rejected_before_any_runtime_work() {
        let (state, _tmp) = capability_test_state();
        let err = dispatch_stdio_request(&state, "prompt/request", json!({ "prompt": "   " }))
            .await
            .expect_err("an empty prompt must be rejected");
        assert_eq!(err.code, -32602);
        assert!(
            state.runtime_bridge.lock().await.is_none(),
            "a rejected prompt must not start a runtime"
        );
    }

    #[tokio::test]
    async fn http_thread_message_runs_the_turn_instead_of_queueing_it() {
        let (state, _tmp, owner_router) = thread_control::compatibility_router();
        seed_client_thread(&state, "thr_http").await;
        let (base_url, prompts, server) = spawn_stub_runtime(Some(owner_router)).await;
        seed_bridge_at(&state, base_url).await;

        let response = run_http_thread_message(
            &state,
            "thr_http".to_string(),
            "go".to_string(),
            Vec::new(),
            None,
        )
        .await
        .expect("http thread message");

        assert_eq!(response.status, "completed");
        assert_eq!(response.thread_id, "thr_http");
        assert_eq!(response.data["turn_id"], "turn_stub");
        assert_eq!(prompts.lock().await.as_slice(), ["go".to_string()]);
        assert!(
            response
                .events
                .iter()
                .any(|event| matches!(event, EventFrame::ResponseDelta { .. })),
            "a completed turn must carry the deltas it streamed"
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn http_thread_message_without_a_runtime_is_a_typed_error() {
        let (state, _tmp, _owner_router) = thread_control::compatibility_router();
        seed_client_thread(&state, "thr_http").await;
        seed_bridge_at(&state, "http://127.0.0.1:9".to_string()).await;

        let err = run_http_thread_message(
            &state,
            "thr_http".to_string(),
            "go".to_string(),
            Vec::new(),
            None,
        )
        .await
        .expect_err("no runtime means no turn");
        assert_eq!(err.code, RUNTIME_UNAVAILABLE_CODE);
    }

    #[tokio::test]
    async fn submit_user_input_refuses_instead_of_claiming_resolution() {
        let (state, _tmp) = capability_test_state();
        let response = process_app_request(
            &state,
            AppRequest::SubmitUserInput {
                request_id: "user-input-1".to_string(),
                answers: Vec::new(),
            },
            AppTransport::Stdio,
        )
        .await;

        assert!(!response.ok, "this transport cannot deliver the answer");
        assert_eq!(response.data["error"], "user_input_reply_unsupported");
        assert!(
            response.data.get("resolved").is_none(),
            "nothing was resolved: {}",
            response.data
        );
        assert!(
            response.data["message"]
                .as_str()
                .expect("message")
                .contains("/v1/user-input/"),
            "the refusal must name the transport that can accept the answer"
        );
        assert!(
            !response.data["message"]
                .as_str()
                .expect("message")
                .contains("  "),
            "the refusal must not expose source-formatting whitespace"
        );
    }

    // ── capability drift guard ─────────────────────────────────────────
    //
    // The stdio `capabilities` method is the benchmark/SDK contract: external
    // harnesses probe it (without spending model tokens) to learn what the
    // app-server can do. Pin the advertised method set so any change forces a
    // deliberate update here, in the dispatcher, and in docs/RUNTIME_API.md.

    /// Methods advertised by the top-level `capabilities` probe, in order.
    const EXPECTED_CAPABILITY_METHODS: &[&str] = &[
        "healthz",
        "thread/capabilities",
        "thread/request",
        "thread/create",
        "thread/start",
        "thread/resume",
        "thread/fork",
        "thread/list",
        "thread/read",
        "thread/set_name",
        "thread/goal/set",
        "thread/goal/get",
        "thread/goal/clear",
        "thread/archive",
        "thread/unarchive",
        "thread/message",
        "thread/interrupt",
        "app/capabilities",
        "app/request",
        "app/config/get",
        "app/config/set",
        "app/config/unset",
        "app/config/list",
        "app/config/reload",
        "app/models",
        "app/thread_loaded_list",
        "prompt/capabilities",
        "prompt/request",
        "prompt/run",
        "shutdown",
    ];

    fn capability_test_state() -> (AppState, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let config_path = tmp.path().join("config.toml");
        fs::write(&config_path, "").expect("write config");
        let state = build_state(Some(config_path), None).expect("state");
        (state, tmp)
    }

    /// Persist a client thread under a fixed id, as `thread/create` would,
    /// so `thread/message` accepts it.
    fn test_client_metadata(thread_id: &str) -> codewhale_state::ThreadMetadata {
        codewhale_state::ThreadMetadata {
            id: thread_id.to_string(),
            rollout_path: None,
            preview: String::new(),
            ephemeral: false,
            model_provider: "deepseek".to_string(),
            created_at: 1,
            updated_at: 1,
            status: codewhale_state::ThreadStatus::Idle,
            path: None,
            cwd: PathBuf::from("/tmp/codewhale"),
            cli_version: "0.0.0-test".to_string(),
            source: codewhale_state::SessionSource::Api,
            name: None,
            sandbox_policy: None,
            approval_mode: None,
            archived: false,
            archived_at: None,
            git_sha: None,
            git_branch: None,
            git_origin_url: None,
            memory_mode: None,
            current_leaf_id: None,
        }
    }

    fn restore_test_thread_archive(store: &StateStore, metadata: &codewhale_state::ThreadMetadata) {
        store
            .restore_legacy_thread_archive(&codewhale_state::LegacyThreadArchive {
                thread: metadata.clone(),
                messages: Vec::new(),
                goal: None,
                checkpoints: Vec::new(),
            })
            .expect("restore absent historical fixture");
    }

    async fn seed_client_thread(state: &AppState, thread_id: &str) {
        let mut metadata = test_client_metadata(thread_id);
        if let Some(workspace) = state.frontend_workspace.as_ref() {
            metadata.cwd = workspace.clone();
        }
        let store = state.runtime.read().await.state_store().clone();
        restore_test_thread_archive(&store, &metadata);
    }

    #[tokio::test]
    async fn capabilities_method_set_is_stable() {
        let (state, _tmp) = capability_test_state();
        let caps = dispatch_stdio_request(&state, "capabilities", json!({}))
            .await
            .expect("capabilities dispatch");
        let methods: Vec<String> = caps.result["methods"]
            .as_array()
            .expect("methods array")
            .iter()
            .map(|m| m.as_str().expect("method string").to_string())
            .collect();
        assert_eq!(
            methods, EXPECTED_CAPABILITY_METHODS,
            "app-server stdio capability set drifted; update the dispatcher, this \
             snapshot, and docs/RUNTIME_API.md together"
        );
    }

    /// The socket transport advertises the `daemon/attach` handshake right
    /// after `healthz`; the stdio pin above must stay untouched by it.
    #[tokio::test]
    async fn socket_transport_advertises_daemon_attach() {
        let (state, _tmp) = capability_test_state();
        let mut sink = tokio::io::sink();
        let caps = dispatch_stdio_request_with_writer(
            &state,
            &mut sink,
            "capabilities",
            json!({}),
            AppTransport::Socket,
        )
        .await
        .expect("capabilities dispatch");
        let expected_transport = if cfg!(windows) {
            "named-pipe"
        } else {
            "unix-socket"
        };
        assert_eq!(caps.result["transport"], json!(expected_transport));
        let methods: Vec<String> = caps.result["methods"]
            .as_array()
            .expect("methods array")
            .iter()
            .map(|m| m.as_str().expect("method string").to_string())
            .collect();
        let mut expected: Vec<String> = EXPECTED_CAPABILITY_METHODS
            .iter()
            .map(|m| m.to_string())
            .collect();
        expected.insert(1, daemon_socket::ATTACH_METHOD.to_string());
        assert_eq!(methods, expected);
    }

    #[tokio::test]
    async fn every_advertised_capability_is_dispatchable() {
        let (state, _tmp) = capability_test_state();
        // Empty params: methods may fail validation (-32602), but none may report
        // method-not-found (-32601). Required fields (e.g. PromptRequest.prompt)
        // make the prompt routes fail at parse time, so no model tokens are spent.
        for method in EXPECTED_CAPABILITY_METHODS {
            if let Err(err) = dispatch_stdio_request(&state, method, json!({})).await {
                assert_ne!(
                    err.code,
                    JsonRpcError::method_not_found(method).code,
                    "advertised capability `{method}` is not dispatchable"
                );
            }
        }
    }

    // ── resolve_auth_token ─────────────────────────────────────────────

    #[test]
    fn auth_token_empty_string_fails() {
        let options = AppServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            config_path: None,
            auth_token: Some("  ".to_string()),
            insecure_no_auth: false,
            cors_origins: Vec::new(),
        };
        let err = resolve_auth_token(&options).expect_err("empty token should fail");
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn auth_token_generated_when_none_provided() {
        let options = AppServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            config_path: None,
            auth_token: None,
            insecure_no_auth: false,
            cors_origins: Vec::new(),
        };
        let token = resolve_auth_token(&options).unwrap();
        assert!(token.is_some());
        assert!(token.unwrap().starts_with("cwapp_"));
    }

    #[test]
    fn runtime_child_endpoint_must_be_a_reported_loopback_port() {
        let ok = parse_runtime_endpoint("Runtime API listening on http://127.0.0.1:49152\r\n")
            .expect("loopback endpoint");
        assert_eq!(ok.port(), 49152);
        for bad in [
            "Runtime API listening on http://127.0.0.1:0",
            "Runtime API listening on http://10.0.0.5:7878",
            "Runtime API listening on http://[::1]:7878",
            "Runtime API listening on http://example.com:80",
            "Runtime API listening on http://127.0.0.1:80/redirect",
            "listening on http://127.0.0.1:7878",
            "",
        ] {
            assert!(parse_runtime_endpoint(bad).is_err(), "{bad:?}");
        }

        let mut first_line_only: &[u8] =
            b"Runtime API listening on http://127.0.0.1:5000\nRuntime API listening on http://127.0.0.1:6000\n";
        assert_eq!(
            read_runtime_ready_line(&mut first_line_only).unwrap(),
            "Runtime API listening on http://127.0.0.1:5000"
        );
        let mut closed: &[u8] = b"Runtime API listening on http://127.0.0.1:5000";
        assert!(read_runtime_ready_line(&mut closed).is_err(), "no newline");
        let oversized = vec![b'a'; RUNTIME_READY_MAX_BYTES + 1];
        assert!(read_runtime_ready_line(&mut oversized.as_slice()).is_err());
    }

    #[test]
    fn runtime_bridge_command_keeps_auth_token_out_of_argv() {
        // FR001-C001: runtime auth token must not appear on the child argv
        // (visible via local `ps`); pass it via env instead.
        let token = "cwrt_unit_test_secret_token_not_for_argv";
        let cmd = RuntimeBridge::runtime_command(None, token).expect("command");
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            argv.windows(2).any(|pair| pair == ["--port", "0"]),
            "the child picks and reports its own port: {argv:?}"
        );
        assert!(
            !argv
                .iter()
                .any(|a| a.contains(token) || a == "--auth-token"),
            "auth token must not be present in child argv: {argv:?}"
        );
        let envs: Vec<(String, String)> = cmd
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert!(
            envs.iter()
                .any(|(k, v)| k == "CODEWHALE_RUNTIME_TOKEN" && v == token),
            "token must be carried via CODEWHALE_RUNTIME_TOKEN: {envs:?}"
        );
        assert!(
            envs.iter()
                .any(|(k, v)| k == "DEEPSEEK_RUNTIME_TOKEN" && v == token),
            "legacy alias DEEPSEEK_RUNTIME_TOKEN must also carry the token: {envs:?}"
        );
    }

    #[test]
    fn generated_auth_status_does_not_render_token() {
        let rendered = app_server_auth_status_lines(false).join("\n");

        assert!(!rendered.contains("Authorization: Bearer"));
        assert!(rendered.contains("not printed"));
        assert!(rendered.contains("CODEWHALE_APP_SERVER_TOKEN"));
    }

    #[test]
    fn auth_token_explicit_is_preserved() {
        let options = AppServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            config_path: None,
            auth_token: Some("my-secret".to_string()),
            insecure_no_auth: false,
            cors_origins: Vec::new(),
        };
        let token = resolve_auth_token(&options).unwrap();
        assert_eq!(token.as_deref(), Some("my-secret"));
    }

    #[test]
    fn auth_token_explicit_allows_non_loopback_bind() {
        let options = AppServerOptions {
            listen: "0.0.0.0:8787".parse().expect("socket addr"),
            config_path: None,
            auth_token: Some("my-secret".to_string()),
            insecure_no_auth: false,
            cors_origins: Vec::new(),
        };
        let token = resolve_auth_token(&options).unwrap();
        assert_eq!(token.as_deref(), Some("my-secret"));
    }

    #[test]
    fn insecure_no_auth_on_loopback_returns_none() {
        let options = AppServerOptions {
            listen: "127.0.0.1:0".parse().expect("addr"),
            config_path: None,
            auth_token: None,
            insecure_no_auth: true,
            cors_origins: Vec::new(),
        };
        let token = resolve_auth_token(&options).unwrap();
        assert!(token.is_none());
    }

    #[test]
    fn insecure_no_auth_on_non_loopback_fails_fast() {
        let options = AppServerOptions {
            listen: "0.0.0.0:8787".parse().expect("socket addr"),
            config_path: None,
            auth_token: None,
            insecure_no_auth: true,
            cors_origins: Vec::new(),
        };

        let err = resolve_auth_token(&options).expect_err("non-loopback unauth should fail");
        assert!(
            err.to_string()
                .contains("refusing unauthenticated app-server bind")
        );
    }

    // ── cors_layer ─────────────────────────────────────────────────────

    #[test]
    fn cors_layer_includes_default_origins() {
        let layer = cors_layer(&[]);
        // Just verify it doesn't panic and creates successfully
        let _ = layer;
    }

    #[test]
    fn cors_layer_adds_extra_origins() {
        let extras = vec!["https://example.com".to_string()];
        let layer = cors_layer(&extras);
        let _ = layer;
    }

    #[test]
    fn cors_layer_skips_empty_origins() {
        let extras = vec!["".to_string(), "  ".to_string()];
        let layer = cors_layer(&extras);
        let _ = layer;
    }

    // ── JsonRpc helpers ────────────────────────────────────────────────

    #[test]
    fn params_or_object_returns_object_for_null() {
        let result = params_or_object(Value::Null);
        assert_eq!(result, json!({}));
    }

    #[test]
    fn params_or_object_passthrough_for_non_null() {
        let input = json!({"key": "value"});
        let result = params_or_object(input.clone());
        assert_eq!(result, input);
    }

    #[test]
    fn jsonrpc_result_format() {
        let result = jsonrpc_result(Some(json!(1)), json!({"ok": true}));
        assert_eq!(result["jsonrpc"], "2.0");
        assert_eq!(result["id"], 1);
        assert_eq!(result["result"]["ok"], true);
    }

    #[test]
    fn jsonrpc_result_null_id() {
        let result = jsonrpc_result(None, json!(null));
        assert_eq!(result["id"], Value::Null);
    }

    #[test]
    fn jsonrpc_error_format() {
        let err = jsonrpc_error(Some(json!(2)), JsonRpcError::internal("oops"));
        assert_eq!(err["jsonrpc"], "2.0");
        assert_eq!(err["id"], 2);
        assert_eq!(err["error"]["code"], -32603);
        assert_eq!(err["error"]["message"], "oops");
    }

    #[test]
    fn jsonrpc_error_codes() {
        assert_eq!(JsonRpcError::parse_error("").code, -32700);
        assert_eq!(JsonRpcError::invalid_request("").code, -32600);
        assert_eq!(JsonRpcError::method_not_found("x").code, -32601);
        assert_eq!(JsonRpcError::invalid_params("").code, -32602);
        assert_eq!(JsonRpcError::internal("").code, -32603);
    }

    // ── AppServerOptions ───────────────────────────────────────────────

    #[test]
    fn app_server_options_debug_does_not_leak_token() {
        let options = AppServerOptions {
            listen: "127.0.0.1:8080".parse().expect("addr"),
            config_path: None,
            auth_token: Some("secret-token".to_string()),
            insecure_no_auth: false,
            cors_origins: vec!["https://example.com".to_string()],
        };
        let debug = format!("{options:?}");
        assert!(!debug.contains("secret-token"));
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains("8080"));
    }

    // ── Default CORS origins ──────────────────────────────────────────

    #[test]
    fn default_cors_origins_include_common_dev_ports() {
        assert!(DEFAULT_CORS_ORIGINS.contains(&"http://localhost:3000"));
        assert!(DEFAULT_CORS_ORIGINS.contains(&"http://localhost:5173"));
        assert!(DEFAULT_CORS_ORIGINS.contains(&"tauri://localhost"));
    }
    #[tokio::test]
    async fn runtime_image_daemon_bridge_checks_transport_and_forwards_exact_wire() {
        async fn capture(
            State(seen): State<Arc<Mutex<Vec<Value>>>>,
            Json(body): Json<Value>,
        ) -> (StatusCode, Json<Value>) {
            seen.lock().await.push(body);
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"fixture stops before an Engine"})),
            )
        }
        for supported in [false, true] {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let app = Router::new()
                .route(
                    "/v1/runtime/info",
                    get(move || async move {
                        Json(json!({"capabilities":{"turn_image_inputs":supported}}))
                    }),
                )
                .route("/v1/threads/{id}/turns", post(capture))
                .with_state(seen.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            let mut bridge = RuntimeBridge::from_base_url_for_test(format!("http://{addr}"));
            let images = vec![RuntimeImageInput {
                mime: "image/png".into(),
                data_base64: "fixture-bytes-validated-by-Core".into(),
            }];
            let mut writer = tokio::io::sink();
            assert!(
                bridge
                    .message_thread(
                        "thr_fixture",
                        RuntimeTurnInput {
                            input: "look",
                            images: &images,
                            max_output_tokens: None,
                            expected_workspace: None
                        },
                        &mut writer,
                        None,
                        None
                    )
                    .await
                    .is_err()
            );
            let requests = seen.lock().await;
            assert_eq!(requests.len(), usize::from(supported));
            if supported {
                assert_eq!(requests[0], json!({"prompt":"look","images":images}));
            }
            server.abort();
        }
    }

    #[test]
    fn runtime_image_daemon_all_input_families_preserve_images() {
        let image = json!({"mime":"image/png","dataBase64":"AQ=="});
        let thread: ThreadMessageParams = serde_json::from_value(
            json!({"thread_id":"thr_fixture","input":"look","images":[image.clone()]}),
        )
        .unwrap();
        let prompt: PromptRequest =
            serde_json::from_value(json!({"prompt":"look","images":[image.clone()]})).unwrap();
        let generic: ThreadRequest = serde_json::from_value(
            json!({"kind":"message","thread_id":"thr_fixture","input":"look","images":[image]}),
        )
        .unwrap();
        assert_eq!(thread.images, prompt.images);
        let ThreadRequest::Message { images, .. } = generic else {
            panic!("message");
        };
        assert_eq!(thread.images, images);
        assert!(matches!(
            parse_stdio_line(&" ".repeat(MAX_RUNTIME_IMAGE_BODY_BYTES + 1)),
            ParsedStdioLine::Rejected(_)
        ));
    }
}

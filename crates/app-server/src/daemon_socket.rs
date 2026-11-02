//! Authenticated local daemon transport: Unix socket and Windows named pipe.
//!
//! The desktop shell attaches to a long-lived `codewhale app-server --socket`
//! daemon over a local socket instead of a TCP port: local multi-client,
//! peer-credential auth, nothing to firewall (CORE-PROTOCOL spec §5). The
//! wire is *identical* to the `--stdio` transport — newline-delimited
//! JSON-RPC 2.0 driven by the same `crate::stdio::run_stdio_loop` — with exactly
//! one addition in front of it: a `daemon/attach` handshake that establishes
//! who this client is and whether it owns the daemon.
//!
//! Windows derives a local named-pipe endpoint from the selected home and
//! current principal. It shares the same dispatcher and ownership handshake;
//! no supported platform falls back to TCP. Windows runtime proof is separate.
//!
//! # Ownership
//!
//! Hermes' claim model, server-side: a client attaches with `mode: "claim"`
//! (it spawned the daemon and will manage its lifetime) or `mode: "attach"`
//! (it found a healthy daemon and is a guest). Only the current owner may
//! `shutdown` the daemon; guests get `not_daemon_owner`. When the owner
//! disconnects the slot frees, so a relaunched shell can re-claim the daemon
//! it left running — sessions survive UI restarts because the daemon does.

use std::path::{Path, PathBuf};

use codewhale_protocol::RuntimeOwnerReceipt;
use serde::{Deserialize, Serialize};

pub use codewhale_paths::daemon_socket::{
    DAEMON_SOCKET_FILE_NAME, DaemonSocketError, MAX_SOCKET_PATH_BYTES, SocketPathInputs,
    WINDOWS_NAMED_PIPE, resolve_socket_path,
};
#[cfg(unix)]
pub use codewhale_paths::daemon_socket::{default_socket_path, store_selected_socket_path};

/// JSON-RPC method a client must send first on a daemon-socket connection.
pub const ATTACH_METHOD: &str = "daemon/attach";

/// How to start the daemon socket transport.
#[derive(Debug, Clone, Default)]
pub struct DaemonSocketOptions {
    /// Explicit socket path; `None` resolves the platform default.
    pub socket_path: Option<PathBuf>,
    /// Explicit config file, like `app-server --config`.
    pub config_path: Option<PathBuf>,
}

/// The socket path this host would use with no explicit override.
#[cfg(not(any(unix, windows)))]
pub fn default_socket_path() -> Result<PathBuf, DaemonSocketError> {
    Err(unsupported_platform())
}

/// Who is on the other end of a daemon-socket connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientIdentity {
    /// Product name of the client, e.g. `codewhale-desktop`.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
}

/// Ownership intent carried by `daemon/attach`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachMode {
    /// A guest: use the daemon, never stop it.
    #[default]
    Attach,
    /// The daemon's owner: may `shutdown`. Fails if a live owner exists.
    Claim,
}

/// Role granted by a successful attach.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachRole {
    Owner,
    Attached,
}

/// `daemon/attach` params.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttachFrontend {
    #[default]
    Control,
    Acp,
    Listener,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachParams {
    #[serde(default)]
    pub frontend: AttachFrontend,
    #[serde(default)]
    pub listener: Option<crate::RuntimeListenerSelection>,
    #[serde(default)]
    pub scope: Option<crate::RuntimeFrontendScope>,
    #[serde(default)]
    pub acp_model: Option<String>,
    pub client: ClientIdentity,
    #[serde(default)]
    pub mode: AttachMode,
    /// Bundle-skew guard: when set, the daemon refuses the attach unless its
    /// own version string matches exactly.
    #[serde(default)]
    pub expect_daemon_version: Option<String>,
    #[serde(default)]
    pub expect_owner: Option<RuntimeOwnerReceipt>,
}

/// The typed refusal every non-unix entry point returns. Unused in the unix
/// library build by construction; the tests pin its wording on every host.
#[cfg(any(test, not(any(unix, windows))))]
fn unsupported_platform() -> DaemonSocketError {
    DaemonSocketError::UnsupportedPlatform {
        platform: std::env::consts::OS,
        planned_endpoint: WINDOWS_NAMED_PIPE,
    }
}

#[cfg(any(unix, windows))]
pub use codewhale_runtime::runtime_control::owner_work;

// Blocking endpoint mutations retain the actual captured manager through
// its existing frontend, even after cancellation of the async caller.
#[cfg(any(unix, windows))]
pub(crate) async fn socket_owner_work<T, F>(owner_state: Option<AppState>, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    owner_work(move || {
        let _held_owner = owner_state;
        work()
    })
    .await
}

use crate::stdio::{
    ParsedStdioLine, dispatch_stdio_request_with_writer, parse_stdio_line, run_stdio_loop,
    write_stdio_line,
};
use crate::{
    AppState, AppTransport, JsonRpcError, ShutdownAuthority, StdioLoopExit, StdioLoopPolicy,
    jsonrpc_error, jsonrpc_result, params_or_object, parse_params,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufRead, AsyncWrite, BufReader};
use tokio::sync::watch;
use tokio::task::JoinSet;

/// Facts about this daemon, reported in every attach reply.
#[derive(Debug)]
struct DaemonInfo {
    pid: u32,
    version: &'static str,
    /// Independent effective principal captured from this process.
    #[cfg(unix)]
    uid: u32,
    socket_path: PathBuf,
    started_at: Instant,
}

#[derive(Debug, Default)]
struct ConnectionRegistry {
    next_id: u64,
    connections: HashMap<u64, ClientIdentity>,
    owner: Option<u64>,
}

impl ConnectionRegistry {
    fn register(&mut self, client: ClientIdentity) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.connections.insert(id, client);
        id
    }

    /// Take the owner slot, or report who holds it.
    fn claim(&mut self, id: u64) -> Result<(), ClientIdentity> {
        if let Some(owner_id) = self.owner
            && owner_id != id
            && let Some(owner) = self.connections.get(&owner_id)
        {
            return Err(owner.clone());
        }
        self.owner = Some(id);
        Ok(())
    }

    fn owner(&self) -> Option<ClientIdentity> {
        self.owner.and_then(|id| self.connections.get(&id)).cloned()
    }

    fn remove(&mut self, id: u64) {
        self.connections.remove(&id);
        if self.owner == Some(id) {
            self.owner = None;
        }
    }
}

/// Releases the registry slot (and the owner claim) on drop, whichever
/// way the connection ends.
struct ConnectionGuard {
    frontend: AttachFrontend,
    listener: Option<crate::RuntimeListenerSelection>,
    scope: Option<crate::RuntimeFrontendScope>,
    acp_model: Option<String>,
    registry: Arc<Mutex<ConnectionRegistry>>,
    id: u64,
    role: AttachRole,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Ok(mut registry) = self.registry.lock() {
            registry.remove(self.id);
        }
    }
}

/// Asks a running [`DaemonSocket::serve`] to stop.
#[derive(Debug, Clone)]
pub struct DaemonShutdownHandle(pub(crate) Arc<watch::Sender<bool>>);

impl DaemonShutdownHandle {
    /// Idempotent; safe to call from any task or signal handler.
    pub fn trigger(&self) {
        self.0.send_replace(true);
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionContext {
    state: AppState,
    registry: Arc<Mutex<ConnectionRegistry>>,
    info: Arc<DaemonInfo>,
    shutdown: DaemonShutdownHandle,
    owner: Option<RuntimeOwnerReceipt>,
}

/// The daemon's connection tasks. A `JoinSet` keeps every finished task
/// until it is joined, so a long-lived daemon used to retain one task per
/// past client (health polls, re-attaches) for its whole lifetime.
/// [`Self::spawn`] is the only way in and reaps finished tasks first, so
/// the set is bounded by the live connections plus those that ended since
/// the last accept. A panicked connection is logged, not fatal.
#[derive(Default)]
pub(crate) struct ConnectionTasks(JoinSet<()>);

impl ConnectionTasks {
    pub(crate) async fn shutdown(&mut self) {
        self.0.shutdown().await;
    }
    pub(crate) fn at_capacity(&mut self) -> bool {
        while let Some(joined) = self.0.try_join_next() {
            if let Err(err) = joined
                && err.is_panic()
            {
                tracing::warn!(error = %err, "daemon connection task panicked");
            }
        }
        self.0.len() >= 64
    }
    pub(crate) fn spawn(&mut self, task: impl std::future::Future<Output = ()> + Send + 'static) {
        if !self.at_capacity() {
            self.0.spawn(task);
        } else {
            tracing::debug!("local control connection ceiling reached; refusing before admission");
        }
    }
}

/// Serve `healthz` and wait for `daemon/attach`; everything else is
/// refused with `attach_required` until the client attaches.
async fn handshake<R, W>(
    context: &ConnectionContext,
    lines: &mut crate::stdio::BoundedLines<R>,
    writer: &mut W,
) -> Result<Option<ConnectionGuard>>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let Some(line) = lines.next_line().await? else {
            return Ok(None);
        };
        let request = match parse_stdio_line(&line) {
            ParsedStdioLine::Blank => continue,
            ParsedStdioLine::Rejected(response) => {
                write_stdio_line(writer, &response).await?;
                continue;
            }
            ParsedStdioLine::Request(request) => request,
        };
        let id = request.id.clone();
        match request.method.as_str() {
            "healthz" | "app/healthz" => {
                let response = match dispatch_stdio_request_with_writer(
                    &context.state,
                    writer,
                    &request.method,
                    request.params,
                    AppTransport::Socket,
                )
                .await
                {
                    Ok(dispatch) => jsonrpc_result(id, dispatch.result),
                    Err(err) => jsonrpc_error(id, err),
                };
                write_stdio_line(writer, &response).await?;
            }
            ATTACH_METHOD => match attach(context, request.params).await {
                Ok((result, guard)) => {
                    write_stdio_line(writer, &jsonrpc_result(id, result)).await?;
                    return Ok(Some(guard));
                }
                Err(err) => write_stdio_line(writer, &jsonrpc_error(id, err)).await?,
            },
            other => {
                write_stdio_line(
                    writer,
                    &jsonrpc_error(id, JsonRpcError::attach_required(other)),
                )
                .await?;
            }
        }
    }
}

async fn attach(
    context: &ConnectionContext,
    params: Value,
) -> Result<(Value, ConnectionGuard), JsonRpcError> {
    let params: AttachParams = parse_params(params_or_object(params))?;
    if params.frontend == AttachFrontend::Acp
        && (context.state.owner_frontend.is_none()
            || !context
                .state
                .captured_routing
                .as_ref()
                .is_some_and(|routing| routing.acp))
    {
        return Err(JsonRpcError::invalid_params(
            "held owner has no narrowed ACP projection; cross-profile attachment refused",
        ));
    }
    if (params.frontend == AttachFrontend::Listener) != params.listener.is_some() {
        return Err(JsonRpcError::invalid_params(
            "listener selection does not match frontend",
        ));
    }
    if let Some(listener) = params.listener.as_ref() {
        listener.validate_bounds().map_err(|_| {
            JsonRpcError::invalid_params("selected frontend input exceeds its bounds")
        })?;
        if context.state.owner_frontend.is_none() {
            return Err(JsonRpcError::invalid_params(
                "held owner has no selected listener projection",
            ));
        }
    }
    if params.scope.is_some()
        && (params.frontend == AttachFrontend::Listener || context.state.owner_frontend.is_none())
        || params.acp_model.is_some() && params.frontend != AttachFrontend::Acp
    {
        return Err(JsonRpcError::invalid_params(
            "selected scope does not match captured frontend",
        ));
    }
    if let Some(scope) = params.scope.as_ref() {
        scope.validate_bounds().map_err(|_| {
            JsonRpcError::invalid_params("selected frontend scope exceeds its bounds")
        })?;
    }
    if params
        .acp_model
        .as_ref()
        .is_some_and(|model| model.trim().is_empty() || model.len() > 1024)
    {
        return Err(JsonRpcError::invalid_params("invalid selected ACP model"));
    }
    if let Some(owner) = context.owner.as_ref() {
        if params.expect_owner.as_ref() != Some(owner) {
            return Err(JsonRpcError::invalid_params(
                "captured owner generation or store does not match",
            ));
        }
        if params.mode == AttachMode::Claim {
            return Err(JsonRpcError::not_daemon_owner());
        }
    }
    if params.client.name.trim().is_empty() {
        return Err(JsonRpcError::invalid_params(
            "client.name must not be empty",
        ));
    }
    if let Some(expected) = params.expect_daemon_version.as_deref()
        && expected != context.info.version
    {
        return Err(JsonRpcError::daemon_version_skew(
            expected,
            context.info.version,
        ));
    }

    if let Some(frontend) = context.state.owner_frontend.as_ref() {
        let selection = match params.frontend {
            AttachFrontend::Listener => Some(crate::RuntimeOwnerFrontendSelection::Listener(
                params.listener.clone().expect("validated listener"),
            )),
            AttachFrontend::Acp => Some(crate::RuntimeOwnerFrontendSelection::Acp {
                scope: params.scope.clone(),
                model: params.acp_model.clone(),
            }),
            AttachFrontend::Control => params
                .scope
                .clone()
                .map(crate::RuntimeOwnerFrontendSelection::Control),
        };
        if let Some(selection) = selection {
            frontend
                .validate_selection(&selection)
                .await
                .map_err(|error| JsonRpcError::invalid_params(error.to_string()))?;
        }
    }
    let mut registry = context
        .registry
        .lock()
        .map_err(|_| JsonRpcError::internal("daemon connection registry poisoned"))?;
    let id = registry.register(params.client.clone());
    let role = match params.mode {
        AttachMode::Attach => AttachRole::Attached,
        AttachMode::Claim => match registry.claim(id) {
            Ok(()) => AttachRole::Owner,
            Err(owner) => {
                registry.remove(id);
                let owner = serde_json::to_value(owner)
                    .map_err(|err| JsonRpcError::internal(err.to_string()))?;
                return Err(JsonRpcError::daemon_already_claimed(&owner));
            }
        },
    };
    let owner = registry.owner();
    let connections = registry.connections.len();
    drop(registry);

    let info = &context.info;
    let result = json!({
        "attached": true,
        "connection_id": id,
        "role": role,
        "transport": AppTransport::Socket.label(),
        "daemon": {
            "service": crate::legacy_deepseek_compat::SERVICE_NAME,
            "pid": info.pid,
            "version": info.version,
            "socket_path": info.socket_path.display().to_string(),
            "uptime_ms": u64::try_from(info.started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        },
        "owner": owner,
        "connections": connections,
        "owner_receipt": context.owner,
        "runtime_routing": context.state.captured_routing,
    });
    Ok((
        result,
        ConnectionGuard {
            frontend: params.frontend,
            listener: params.listener,
            scope: params.scope,
            acp_model: params.acp_model,
            registry: Arc::clone(&context.registry),
            id,
            role,
        },
    ))
}

pub(crate) struct AuthorizedPeer {
    pub(crate) pid: u32,
    pub(crate) start: String,
    #[cfg(windows)]
    pub(crate) process: codewhale_config::windows_identity::WindowsPeerProcess,
}

impl AuthorizedPeer {
    pub(crate) fn check(&self) -> Result<()> {
        #[cfg(unix)]
        anyhow::ensure!(
            codewhale_config::private_directory::unix_process_start(self.pid)? == self.start,
            "local control peer generation changed"
        );
        #[cfg(windows)]
        {
            self.process.check_current_user()?;
            anyhow::ensure!(
                self.process.pid() == self.pid,
                "Windows kernel peer PID changed"
            );
            anyhow::ensure!(
                self.process.start() == self.start,
                "local control peer generation changed"
            );
        }
        Ok(())
    }
}

pub(crate) async fn connection_context(
    state: AppState,
    path: PathBuf,
    shutdown: DaemonShutdownHandle,
    owner: Option<RuntimeOwnerReceipt>,
) -> Result<ConnectionContext> {
    Ok(ConnectionContext {
        state,
        registry: Arc::new(Mutex::new(ConnectionRegistry::default())),
        info: Arc::new(DaemonInfo {
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION"),
            #[cfg(unix)]
            uid: codewhale_config::private_directory::PrivateDirectory::current_user_id(),
            socket_path: path,
            started_at: Instant::now(),
        }),
        shutdown,
        owner,
    })
}

pub(crate) async fn run_authorized_connection<R, W>(
    context: ConnectionContext,
    read: R,
    mut writer: W,
    peer: AuthorizedPeer,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let peer = Arc::new(peer);
    let check = peer.clone();
    if owner_work(move || check.check()).await.is_err() {
        return;
    }
    let mut lines = crate::stdio::BoundedLines::new(BufReader::new(read));
    let guard = match tokio::time::timeout(
        Duration::from_secs(5),
        handshake(&context, &mut lines, &mut writer),
    )
    .await
    {
        Err(_) => return,
        Ok(result) => match result {
            Ok(Some(guard)) => guard,
            Ok(None) => return,
            Err(error) => {
                tracing::debug!(%error,"local control handshake ended");
                return;
            }
        },
    };
    let check = peer.clone();
    if owner_work(move || check.check()).await.is_err() {
        return;
    }
    if guard.frontend != AttachFrontend::Control || guard.scope.is_some() {
        let Some(frontend) = context.state.owner_frontend.as_ref() else {
            return;
        };
        let selection = match guard.frontend {
            AttachFrontend::Listener => crate::RuntimeOwnerFrontendSelection::Listener(
                guard.listener.clone().expect("admitted listener"),
            ),
            AttachFrontend::Acp => crate::RuntimeOwnerFrontendSelection::Acp {
                scope: guard.scope.clone(),
                model: guard.acp_model.clone(),
            },
            AttachFrontend::Control => crate::RuntimeOwnerFrontendSelection::Control(
                guard.scope.clone().expect("admitted scope"),
            ),
        };
        let _claim = (guard, peer);
        if let Err(error) = frontend
            .serve(
                selection,
                context.state.clone(),
                Box::new(lines.into_inner()),
                Box::new(writer),
            )
            .await
        {
            tracing::debug!(%error,"captured frontend connection ended");
        }
        return;
    }
    let policy = StdioLoopPolicy {
        transport: AppTransport::Socket,
        shutdown: match guard.role {
            AttachRole::Owner => ShutdownAuthority::Granted,
            AttachRole::Attached => ShutdownAuthority::Denied,
        },
    };
    match run_stdio_loop(&context.state, lines, writer, policy, Some((guard, peer))).await {
        Ok(StdioLoopExit::Shutdown) => context.shutdown.trigger(),
        Ok(StdioLoopExit::InputClosed) => {}
        Err(error) => tracing::debug!(%error,"local control connection ended"),
    }
}

#[cfg(unix)]
mod platform {
    use std::fs::File;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Result;
    use codewhale_config::private_directory::{PrivateDirectory, PrivateSocketIdentity};
    use codewhale_paths::daemon_socket::{SocketPathInputs, resolve_socket_path};
    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::watch;

    #[cfg(test)]
    use super::{ClientIdentity, ConnectionRegistry};
    use super::{
        ConnectionContext, ConnectionTasks, DaemonShutdownHandle, DaemonSocketError,
        DaemonSocketOptions, socket_owner_work,
    };
    use crate::{AppState, AppTransport, build_state_off_runtime};

    /// How long the stale-socket probe waits for a connect to resolve.
    const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

    #[cfg(test)]
    type BeforeOwnerRebind = Box<dyn FnOnce(&Path)>;
    #[cfg(test)]
    thread_local! {
        static BEFORE_OWNER_REBIND: std::cell::RefCell<Option<BeforeOwnerRebind>> = const { std::cell::RefCell::new(None) };
    }

    /// Removes the socket file when the server stops, however it stops.
    struct SocketFileGuard {
        parent: Arc<PrivateDirectory>,
        name: String,
        identity: PrivateSocketIdentity,
        receipt: Option<(String, File)>,
        owner_state: Option<AppState>,
        retired: bool,
    }

    impl SocketFileGuard {
        fn retirement(&mut self) -> Option<impl FnOnce() -> Result<()> + Send + 'static> {
            if self.retired {
                return None;
            }
            self.retired = true;
            let parent = self.parent.clone();
            let name = self.name.clone();
            let identity = self.identity;
            let receipt = self.receipt.take();
            let owner_state = self.owner_state.take();
            Some(move || {
                let _held_owner = owner_state;
                if let Some((name, file)) = receipt {
                    parent.retire_private_receipt(&name, &file)?;
                }
                parent.retire_socket(&name, identity)?;
                Ok(())
            })
        }

        async fn retire(&mut self) -> Result<()> {
            if let Some(retire) = self.retirement() {
                tokio::spawn(async move { super::owner_work(retire).await })
                    .await
                    .map_err(anyhow::Error::from)??;
            }
            Ok(())
        }
    }

    impl Drop for SocketFileGuard {
        fn drop(&mut self) {
            let Some(retire) = self.retirement() else {
                return;
            };
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Err(error) = super::owner_work(retire).await {
                        tracing::warn!(%error, "private endpoint retirement is uncertain; retaining it");
                    }
                });
            } else if let Err(error) = retire() {
                tracing::warn!(%error, "private endpoint retirement is uncertain; retaining it");
            }
        }
    }

    /// A bound, not yet serving, daemon socket.
    pub struct DaemonSocket {
        listener: UnixListener,
        path: PathBuf,
        state: AppState,
        shutdown: Arc<watch::Sender<bool>>,
        socket_file: SocketFileGuard,
        owner: Option<super::RuntimeOwnerReceipt>,
    }

    impl DaemonSocket {
        /// Where clients connect.
        #[must_use]
        pub fn local_path(&self) -> &Path {
            &self.path
        }

        /// A handle that stops [`Self::serve`] from outside (signals, tests).
        #[must_use]
        pub fn shutdown_handle(&self) -> DaemonShutdownHandle {
            DaemonShutdownHandle(Arc::clone(&self.shutdown))
        }

        /// Accept clients until the owner sends `shutdown` or the handle is
        /// triggered. Removes the socket file on the way out.
        pub async fn serve(self) -> Result<(), DaemonSocketError> {
            let Self {
                listener,
                path,
                state,
                shutdown,
                socket_file,
                owner,
            } = self;
            let mut socket_file = socket_file;
            let context = super::connection_context(
                state,
                path.clone(),
                DaemonShutdownHandle(Arc::clone(&shutdown)),
                owner,
            )
            .await
            .map_err(DaemonSocketError::State)?;
            let mut shutdown_rx = shutdown.subscribe();
            let mut connections = ConnectionTasks::default();

            loop {
                if *shutdown_rx.borrow() {
                    break;
                }
                tokio::select! {
                    accepted = listener.accept() => match accepted {
                        Ok((stream, _)) => {
                            connections.spawn(handle_connection(context.clone(), stream));
                        }
                        Err(err) => {
                            tracing::warn!(error = %err, "daemon socket accept failed");
                            tokio::time::sleep(Duration::from_millis(50)).await;
                        }
                    },
                    changed = shutdown_rx.changed() => {
                        if changed.is_err() || *shutdown_rx.borrow() {
                            break;
                        }
                    }
                }
            }

            // The owner's `shutdown` reply was flushed before its loop
            // returned, so aborting what is left loses nothing a client
            // still needs.
            connections.shutdown().await;
            drop(listener);
            socket_file
                .retire()
                .await
                .map_err(DaemonSocketError::State)?;
            Ok(())
        }
    }

    /// Resolve the path, clear a stale socket, bind with `0600`, and build
    /// the shared app state. Does not accept anything until
    /// [`DaemonSocket::serve`].
    pub async fn bind_daemon_socket(
        options: DaemonSocketOptions,
    ) -> Result<DaemonSocket, DaemonSocketError> {
        // A daemon-only run has nothing else to serve with, so a mount that
        // cannot carry the endpoint is a failure here rather than a degraded
        // start. `bind_with_owner` reports it as such when `degrade` is false.
        match bind_with_owner(options, None, None, false).await? {
            Some(socket) => Ok(socket),
            // A non-degrading bind reports a mount it cannot use as an error,
            // so this is an internal invariant rather than a runtime state.
            None => Err(DaemonSocketError::State(anyhow::anyhow!(
                "a bind that cannot degrade returned no endpoint"
            ))),
        }
    }

    /// Bind the control endpoint for an owner that also has an HTTP frontend.
    ///
    /// `None` means this filesystem cannot hold a private endpoint directory.
    /// The owner still serves HTTP, which is what its clients connect to; what
    /// it loses is the socket a second client would use to discover it. The
    /// store owner lock still keeps a second engine out of the same store, so
    /// this degrades discovery, not mutual exclusion.
    pub(crate) async fn bind_captured_owner(
        state: AppState,
        owner: super::RuntimeOwnerReceipt,
        recovery: Option<Arc<crate::daemon_client::UnavailablePublication>>,
    ) -> Result<Option<DaemonSocket>, DaemonSocketError> {
        let options = DaemonSocketOptions {
            socket_path: Some(owner.socket_path.clone()),
            config_path: None,
        };
        bind_with_owner(options, Some((state, owner)), recovery, true).await
    }

    async fn bind_with_owner(
        options: DaemonSocketOptions,
        captured: Option<(AppState, super::RuntimeOwnerReceipt)>,
        recovery: Option<Arc<crate::daemon_client::UnavailablePublication>>,
        degrade: bool,
    ) -> Result<Option<DaemonSocket>, DaemonSocketError> {
        let owner_state = captured.as_ref().map(|(state, _)| state.clone());
        let path = resolve_socket_path(&SocketPathInputs::from_environment(options.socket_path)?)?;
        let parent_path = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or(DaemonSocketError::RuntimeDirUnavailable)?
            .to_path_buf();
        let parent = if let Some(previous) = recovery.as_ref() {
            let owner = &captured
                .as_ref()
                .ok_or_else(|| {
                    DaemonSocketError::State(anyhow::anyhow!(
                        "stale publication requires the actual captured Runtime owner"
                    ))
                })?
                .1;
            previous
                .require_same_selection(owner, &path)
                .map_err(DaemonSocketError::State)?;
            if !previous
                .revalidate()
                .await
                .map_err(DaemonSocketError::State)?
            {
                return Err(DaemonSocketError::State(anyhow::anyhow!(
                    "captured stale owner changed before bind"
                )));
            }
            previous.publication.parent.clone()
        } else {
            // The strict admission first: on a filesystem that stores owner and
            // mode it succeeds and nothing here costs anything extra. Only a
            // failure asks the mount what it can express, which is what turns
            // the opaque "failed to build daemon state" into either a reason to
            // continue without the endpoint or a named refusal.
            let admitted = socket_owner_work(owner_state.clone(), {
                let parent_path = parent_path.clone();
                move || PrivateDirectory::admit(&parent_path)
            })
            .await;
            match admitted {
                Ok(directory) => Arc::new(directory),
                Err(error) => {
                    if let codewhale_config::private_directory::PrivateFooting::Synthesised {
                        observed_uid,
                        observed_mode,
                    } = socket_owner_work(owner_state.clone(), {
                        let parent_path = parent_path.clone();
                        move || {
                            Ok(codewhale_config::private_directory::mount_footing(
                                &parent_path,
                            ))
                        }
                    })
                    .await
                    .map_err(DaemonSocketError::State)?
                    {
                        if degrade {
                            tracing::warn!(
                                endpoint = %path.display(),
                                "{} cannot provide workspace control-socket discovery: the \
                                 filesystem reports uid {observed_uid} and mode \
                                 {observed_mode:o} for objects created on it, so a private \
                                 endpoint directory cannot be verified here; serving the control \
                                 API over HTTP only",
                                parent_path.display()
                            );
                            return Ok(None);
                        }
                        return Err(DaemonSocketError::EndpointDirectoryUnsupported {
                            path: parent_path.clone(),
                            reason: format!(
                                "the filesystem reports uid {observed_uid} and mode \
                                 {observed_mode:o} for objects created on it, so a private \
                                 endpoint directory cannot be verified there"
                            ),
                        });
                    }
                    return Err(DaemonSocketError::State(error));
                }
            }
        };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(DaemonSocketError::RuntimeDirUnavailable)?
            .to_string();
        clear_stale_socket(
            &path,
            &parent,
            &name,
            recovery.clone(),
            captured.is_some() && recovery.is_none(),
            owner_state.clone(),
        )
        .await?;
        let check_parent = parent.clone();
        if !super::owner_work(move || check_parent.is_at_selected_path())
            .await
            .map_err(DaemonSocketError::State)?
        {
            return Err(DaemonSocketError::State(anyhow::anyhow!(
                "private endpoint parent changed"
            )));
        }

        // Withdraw the exact dead publication before a new PID can answer on
        // its endpoint. Contenders then re-observe absence and the held lease.
        if let Some(previous) = recovery.as_ref() {
            let previous = previous.clone();
            let parent = parent.clone();
            socket_owner_work(owner_state.clone(), move || {
                anyhow::ensure!(
                    previous.revalidate_receipt()?,
                    "captured stale owner receipt changed before retirement"
                );
                anyhow::ensure!(
                    parent.retire_private_receipt(
                        &previous.publication.receipt_name,
                        &previous.publication.file
                    )?,
                    "captured stale owner receipt changed during retirement"
                );
                Ok(())
            })
            .await
            .map_err(DaemonSocketError::State)?;
        }

        #[cfg(test)]
        BEFORE_OWNER_REBIND.with(|slot| {
            if let Some(observe) = slot.borrow_mut().take() {
                observe(&path);
            }
        });
        let listener = UnixListener::bind(&path).map_err(|source| DaemonSocketError::Io {
            context: "failed to bind the daemon socket",
            path: path.clone(),
            source,
        })?;
        let inspect_parent = parent.clone();
        let inspect_name = name.clone();
        let identity = super::owner_work(move || inspect_parent.socket_identity(&inspect_name))
            .await
            .map_err(DaemonSocketError::State)?
            .ok_or_else(|| {
                DaemonSocketError::State(anyhow::anyhow!("bound socket identity is unavailable"))
            })?;
        let socket_file = SocketFileGuard {
            parent: parent.clone(),
            name,
            identity,
            receipt: None,
            owner_state: owner_state.clone(),
            retired: false,
        };
        let protect_parent = parent.clone();
        let protect_name = socket_file.name.clone();
        socket_owner_work(owner_state.clone(), move || {
            anyhow::ensure!(
                protect_parent.is_at_selected_path()?,
                "private endpoint parent changed during bind"
            );
            protect_parent.protect_socket(&protect_name, identity)
        })
        .await
        .map_err(DaemonSocketError::State)?;

        let (state, owner) = match captured {
            Some((state, owner)) => (state, Some(owner)),
            None => (
                build_state_off_runtime(options.config_path, None, AppTransport::Socket)
                    .await
                    .map_err(DaemonSocketError::State)?,
                None,
            ),
        };
        let mut socket_file = socket_file;
        if let Some(owner) = owner.as_ref() {
            let receipt_name = format!("{}.owner.json", socket_file.name);
            let bytes = serde_json::to_vec(owner)
                .map_err(|error| DaemonSocketError::State(error.into()))?;
            if bytes.len() > 16384 {
                return Err(DaemonSocketError::State(anyhow::anyhow!(
                    "owner receipt exceeds private publication limit"
                )));
            }
            let parent = parent.clone();
            let name = receipt_name.clone();
            let guard = socket_file;
            socket_file = socket_owner_work(owner_state, move || {
                let mut guard = guard;
                anyhow::ensure!(
                    parent.is_at_selected_path()?,
                    "private owner parent changed before publication"
                );
                anyhow::ensure!(
                    parent.read_private_receipt(&name, 16384)?.is_none(),
                    "owner publication appeared during bootstrap; refusing replacement"
                );
                parent.write_owned_file(&name, &bytes, false)?;
                let (_, file) = parent
                    .read_private_receipt(&name, 16384)?
                    .ok_or_else(|| anyhow::anyhow!("published owner receipt unavailable"))?;
                guard.receipt = Some((name, file));
                anyhow::ensure!(
                    parent.is_at_selected_path()?,
                    "private owner parent changed during publication; retaining uncertain receipt"
                );
                Ok(guard)
            })
            .await
            .map_err(DaemonSocketError::State)?;
        }
        let (shutdown, _) = watch::channel(false);
        Ok(Some(DaemonSocket {
            listener,
            path,
            state,
            shutdown: Arc::new(shutdown),
            socket_file,
            owner,
        }))
    }

    /// Only a definite refusal permits exact captured endpoint retirement.
    async fn clear_stale_socket(
        path: &Path,
        parent: &Arc<PrivateDirectory>,
        name: &str,
        recovery: Option<Arc<crate::daemon_client::UnavailablePublication>>,
        expect_no_receipt: bool,
        owner_state: Option<AppState>,
    ) -> Result<(), DaemonSocketError> {
        let inspect_parent = parent.clone();
        let inspect_name = name.to_string();
        let expected = recovery.clone();
        let Some(identity) = super::owner_work(move || {
            if let Some(previous) = expected.as_ref() {
                anyhow::ensure!(
                    previous.revalidate_all()?,
                    "captured stale owner/socket changed before retirement"
                );
                return Ok(previous.publication.socket_identity);
            }
            if expect_no_receipt {
                anyhow::ensure!(
                    inspect_parent
                        .read_private_receipt(&format!("{inspect_name}.owner.json"), 16384)?
                        .is_none(),
                    "owner publication appeared before fresh endpoint bind; refusing replacement"
                );
            }
            inspect_parent.socket_identity(&inspect_name)
        })
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::InvalidInput)
            {
                DaemonSocketError::NotASocket {
                    path: path.to_path_buf(),
                }
            } else {
                DaemonSocketError::State(error)
            }
        })?
        else {
            return Ok(());
        };
        match tokio::time::timeout(PROBE_TIMEOUT, UnixStream::connect(path)).await {
            Ok(Ok(_live)) => Err(DaemonSocketError::AlreadyRunning {
                path: path.to_path_buf(),
            }),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                let retire_parent = parent.clone();
                let retire_name = name.to_string();
                if socket_owner_work(owner_state, move || {
                    if let Some(previous) = recovery.as_ref() {
                        anyhow::ensure!(previous.revalidate_all()?, "captured stale owner/socket changed before retirement");
                    } else if expect_no_receipt {
                        anyhow::ensure!(retire_parent.read_private_receipt(&format!("{retire_name}.owner.json"), 16384)?.is_none(),
                            "owner publication appeared before fresh endpoint retirement; refusing replacement");
                    }
                    retire_parent.retire_socket(&retire_name, identity)
                })
                    .await
                    .map_err(DaemonSocketError::State)?
                {
                    Ok(())
                } else {
                    Err(DaemonSocketError::State(anyhow::anyhow!(
                        "stale endpoint changed; refusing replacement"
                    )))
                }
            }
            Ok(Err(source)) => Err(DaemonSocketError::Io {
                context: "uncertain daemon endpoint probe; refusing replacement",
                path: path.to_path_buf(),
                source,
            }),
            Err(_) => Err(DaemonSocketError::ProbeTimedOut {
                path: path.to_path_buf(),
            }),
        }
    }

    async fn handle_connection(context: ConnectionContext, stream: UnixStream) {
        let credential = match stream.peer_cred() {
            Ok(peer) if peer.uid() == context.info.uid => peer,
            _ => return,
        };
        let Some(pid) = credential.pid().and_then(|pid| u32::try_from(pid).ok()) else {
            return;
        };
        let start = match super::owner_work(move || {
            codewhale_config::private_directory::unix_process_start(pid)
        })
        .await
        {
            Ok(start) => start,
            Err(_) => return,
        };
        let (read, write) = stream.into_split();
        super::run_authorized_connection(
            context,
            read,
            write,
            super::AuthorizedPeer { pid, start },
        )
        .await;
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use anyhow::Context as _;

        struct FrontendLifetime {
            _held: Arc<()>,
        }
        impl crate::RuntimeOwnerFrontend for FrontendLifetime {
            fn validate_selection<'a>(
                &'a self,
                _selection: &'a crate::RuntimeOwnerFrontendSelection,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>>
            {
                Box::pin(async { anyhow::bail!("lifetime-only frontend cannot admit a command") })
            }
            fn serve(
                &self,
                _selection: crate::RuntimeOwnerFrontendSelection,
                _compatibility: AppState,
                _input: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>,
                _output: Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>>
            {
                Box::pin(async { anyhow::bail!("lifetime-only frontend cannot serve a command") })
            }
        }

        #[tokio::test]
        async fn owner_bootstrap_cancelled_mutations_and_retirement_retain_captured_frontend()
        -> Result<()> {
            // Proves transport custody. The TUI bootstrap tests separately
            // prove the captured manager retains its real store lease.
            let temporary = tempfile::Builder::new()
                .prefix("cw-ob-cancel-")
                .tempdir_in("/tmp")?;
            let root = temporary.path().canonicalize()?;
            let config = root.join("config.toml");
            std::fs::write(&config, "")?;
            for retirement in [false, true] {
                let mut state =
                    build_state_off_runtime(Some(config.clone()), None, AppTransport::Socket)
                        .await?;
                let held = Arc::new(());
                let observed_owner = Arc::downgrade(&held);
                state.owner_frontend = Some(Arc::new(FrontendLifetime { _held: held }));
                let parent_path = root.join(if retirement { "retirement" } else { "mutation" });
                let parent = Arc::new(PrivateDirectory::admit(&parent_path)?);
                let path = parent_path.join("daemon.sock");
                let socket = std::os::unix::net::UnixListener::bind(&path)?;
                let identity = parent
                    .socket_identity("daemon.sock")?
                    .context("socket identity")?;
                parent.protect_socket("daemon.sock", identity)?;
                drop(socket);
                parent.write_owned_file("daemon.sock.owner.json", b"{}", false)?;
                let (_, receipt_file) = parent
                    .read_private_receipt("daemon.sock.owner.json", 16384)?
                    .context("receipt")?;
                let (release, released) = std::sync::mpsc::channel();
                let (started, observed_start) = tokio::sync::oneshot::channel();
                let (finished, observed_finish) = tokio::sync::oneshot::channel();
                let worker = if retirement {
                    let mut guard = SocketFileGuard {
                        parent,
                        name: "daemon.sock".into(),
                        identity,
                        receipt: Some(("daemon.sock.owner.json".into(), receipt_file)),
                        owner_state: Some(state.clone()),
                        retired: false,
                    };
                    let retire = guard.retirement().context("first retirement")?;
                    drop(guard);
                    tokio::spawn(super::super::owner_work(move || {
                        let _ = started.send(());
                        let _ = released.recv();
                        let result = retire();
                        let _ = finished.send(());
                        result
                    }))
                } else {
                    tokio::spawn(socket_owner_work(Some(state.clone()), move || {
                        let _ = started.send(());
                        let _ = released.recv();
                        parent.protect_socket("daemon.sock", identity)?;
                        let _ = finished.send(());
                        Ok(())
                    }))
                };
                drop(state);
                tokio::time::timeout(Duration::from_secs(5), observed_start).await??;
                worker.abort();
                let _ = worker.await;
                assert!(
                    observed_owner.upgrade().is_some(),
                    "running filesystem mutation retains captured owner after waiter cancellation"
                );
                release.send(())?;
                tokio::time::timeout(Duration::from_secs(5), observed_finish).await??;
                tokio::time::timeout(Duration::from_secs(5), async {
                    while observed_owner.upgrade().is_some() {
                        tokio::task::yield_now().await;
                    }
                })
                .await?;
                if retirement {
                    assert!(!path.exists());
                    assert!(!path.with_file_name("daemon.sock.owner.json").exists());
                } else {
                    assert!(path.exists(), "protection alone preserves publication");
                    assert!(path.with_file_name("daemon.sock.owner.json").exists());
                }
            }
            Ok(())
        }

        #[tokio::test(flavor = "current_thread")]
        async fn owner_bootstrap_withdraws_dead_receipt_before_replacement_listener_can_answer()
        -> Result<()> {
            struct OwnedChild(std::process::Child);
            impl Drop for OwnedChild {
                fn drop(&mut self) {
                    if self.0.try_wait().ok().flatten().is_none() {
                        let _ = self.0.kill();
                    }
                    let _ = self.0.wait();
                }
            }
            let temporary = tempfile::Builder::new()
                .prefix("cw-ob-order-")
                .tempdir_in("/tmp")?;
            let root = temporary.path().canonicalize()?;
            let path = root.join("run/daemon.sock");
            let config = root.join("config.toml");
            std::fs::write(&config, "")?;
            let parent = PrivateDirectory::admit(path.parent().unwrap())?;
            let stale = std::os::unix::net::UnixListener::bind(&path)?;
            let identity = parent.socket_identity("daemon.sock")?.unwrap();
            parent.protect_socket("daemon.sock", identity)?;
            drop(stale);
            let mut child = OwnedChild(
                std::process::Command::new("/bin/sleep")
                    .arg("60")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()?,
            );
            let mut owner = codewhale_protocol::RuntimeOwnerReceipt {
                version: 1,
                data_dir: root.join("runtime"),
                execution_scope: "ordering-store".into(),
                lease_generation: "ordering-old".into(),
                pid: child.0.id(),
                process_start: super::super::capture_process_start(child.0.id()).await?,
                principal: PrivateDirectory::current_user_id().to_string(),
                socket_path: path.clone(),
                config_path: Some(config.clone()),
            };
            child.0.kill()?;
            child.0.wait()?;
            parent.write_owned_file(
                "daemon.sock.owner.json",
                &serde_json::to_vec(&owner)?,
                false,
            )?;
            let crate::daemon_client::HostOwnerProbe::Unavailable(captured) =
                crate::daemon_client::probe_owner_for_host_startup(
                    Some(config.clone()),
                    Some(path.clone()),
                )
                .await?
            else {
                anyhow::bail!("dead owner must be captured")
            };
            let observed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let observed_hook = observed.clone();
            let selected = path.clone();
            let selected_config = config.clone();
            let stale_capture = captured.clone();
            BEFORE_OWNER_REBIND.with(|slot| {
                *slot.borrow_mut() = Some(Box::new(move |endpoint| {
                    assert_eq!(endpoint, selected);
                    assert!(!selected.with_file_name("daemon.sock.owner.json").exists());
                    let endpoint = selected.clone();
                    std::thread::spawn(move || {
                        tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap()
                            .block_on(async {
                                assert!(
                                    !stale_capture.revalidate().await.unwrap(),
                                    "withdrawn capture must never authorize a replacement"
                                );
                                assert!(matches!(
                                    crate::daemon_client::probe_owner_for_host_startup(
                                        Some(selected_config),
                                        Some(endpoint),
                                    )
                                    .await
                                    .unwrap(),
                                    crate::daemon_client::HostOwnerProbe::Absent
                                ));
                            });
                    })
                    .join()
                    .unwrap();
                    // Force bind failure after the exact observation point;
                    // the product must preserve this task-owned replacement.
                    drop(std::os::unix::net::UnixListener::bind(&selected).unwrap());
                    observed_hook.store(true, std::sync::atomic::Ordering::SeqCst);
                }));
            });
            owner.pid = std::process::id();
            owner.process_start = super::super::capture_process_start(owner.pid).await?;
            owner.lease_generation = "ordering-new".into();
            let result = crate::bind_runtime_frontends(
                Some(config),
                None,
                owner,
                crate::RuntimeOwnerRouting {
                    endpoint: "127.0.0.1:1".parse()?,
                    workspace: None,
                    workers: None,
                    mobile: false,
                    web: false,
                    acp: false,
                    acp_only: false,
                },
                None,
                Some(captured),
            )
            .await;
            assert!(result.is_err(), "injected replacement prevents new bind");
            assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
            assert!(
                parent.socket_identity("daemon.sock")?.is_some(),
                "replacement endpoint preserved"
            );
            assert!(!path.with_file_name("daemon.sock.owner.json").exists());
            Ok(())
        }

        fn client(name: &str) -> ClientIdentity {
            ClientIdentity {
                name: name.to_string(),
                version: None,
                pid: None,
            }
        }

        #[tokio::test]
        async fn finished_connection_tasks_are_reaped() {
            let mut connections = ConnectionTasks::default();
            let mut handles = Vec::new();
            for _ in 0..3 {
                handles.push(connections.0.spawn(async {}));
            }
            handles.push(
                connections
                    .0
                    .spawn(async { panic!("fixture connection panic") }),
            );
            tokio::time::timeout(Duration::from_secs(5), async {
                while !handles.iter().all(tokio::task::AbortHandle::is_finished) {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("fixture connections finish");

            // Accepting the next client reaps every finished connection,
            // including the panicked one, and keeps only the new live one.
            connections.spawn(std::future::pending::<()>());
            assert_eq!(connections.0.len(), 1, "only the live connection is kept");
            connections.0.abort_all();
        }

        #[tokio::test]
        async fn cancelled_owner_waiter_retains_worker_capacity_until_real_completion() {
            let (started, mut observed) = tokio::sync::mpsc::unbounded_channel();
            let mut releases = Vec::new();
            let mut waiters = Vec::new();
            for _ in 0..4 {
                let (release, released) = std::sync::mpsc::channel();
                releases.push(release);
                let started = started.clone();
                waiters.push(tokio::spawn(super::super::owner_work(move || {
                    started.send(()).unwrap();
                    let _ = released.recv();
                    Ok(())
                })));
            }
            for _ in 0..4 {
                tokio::time::timeout(Duration::from_secs(5), observed.recv())
                    .await
                    .unwrap()
                    .unwrap();
            }
            waiters[0].abort();
            let _ = (&mut waiters[0]).await;
            let (next_started, mut next_observed) = tokio::sync::mpsc::unbounded_channel();
            let next = tokio::spawn(super::super::owner_work(move || {
                next_started.send(()).unwrap();
                Ok(())
            }));
            tokio::task::yield_now().await;
            assert!(matches!(
                next_observed.try_recv(),
                Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            ));
            releases.remove(0).send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(5), next_observed.recv())
                .await
                .unwrap()
                .unwrap();
            for release in releases {
                release.send(()).unwrap();
            }
            for waiter in waiters.into_iter().skip(1) {
                waiter.await.unwrap().unwrap();
            }
            next.await.unwrap().unwrap();
        }

        #[test]
        fn registry_claim_is_exclusive_until_the_owner_leaves() {
            let mut registry = ConnectionRegistry::default();
            let first = registry.register(client("desktop-a"));
            let second = registry.register(client("desktop-b"));

            assert!(registry.claim(first).is_ok());
            assert_eq!(registry.claim(second), Err(client("desktop-a")));
            assert!(
                registry.claim(first).is_ok(),
                "re-claim by the owner is idempotent"
            );

            registry.remove(first);
            assert_eq!(registry.owner(), None);
            assert!(registry.claim(second).is_ok());
            assert_eq!(registry.owner(), Some(client("desktop-b")));
        }

        #[test]
        fn removing_a_guest_keeps_the_owner() {
            let mut registry = ConnectionRegistry::default();
            let owner = registry.register(client("owner"));
            let guest = registry.register(client("guest"));
            registry.claim(owner).expect("claim");
            registry.remove(guest);
            assert_eq!(registry.owner(), Some(client("owner")));
            assert_eq!(registry.connections.len(), 1);
        }
    }
}

#[cfg(not(any(unix, windows)))]
mod platform {
    use std::path::Path;

    use super::{
        DaemonShutdownHandle, DaemonSocketError, DaemonSocketOptions, unsupported_platform,
    };

    /// Placeholder until the Windows named pipe lands; cannot be constructed.
    pub struct DaemonSocket {
        never: std::convert::Infallible,
    }

    impl DaemonSocket {
        #[must_use]
        pub fn local_path(&self) -> &Path {
            match self.never {}
        }

        #[must_use]
        pub fn shutdown_handle(&self) -> DaemonShutdownHandle {
            match self.never {}
        }

        pub async fn serve(self) -> Result<(), DaemonSocketError> {
            match self.never {}
        }
    }

    /// Always [`DaemonSocketError::UnsupportedPlatform`] here.
    pub async fn bind_daemon_socket(
        _options: DaemonSocketOptions,
    ) -> Result<DaemonSocket, DaemonSocketError> {
        Err(unsupported_platform())
    }
}

#[cfg(windows)]
use crate::daemon_windows as platform;
pub use platform::{DaemonSocket, bind_daemon_socket};

/// `codewhale app-server --socket`: bind, announce, serve until the owner's
/// `shutdown` or a termination signal.
pub async fn run_daemon_socket(options: DaemonSocketOptions) -> anyhow::Result<()> {
    let daemon = bind_daemon_socket(options).await?;
    let path: &Path = daemon.local_path();
    tracing::info!(path = %path.display(), "codewhale daemon listening on unix socket");
    eprintln!("codewhale daemon: listening on {}", path.display());

    let handle = daemon.shutdown_handle();
    tokio::spawn(async move {
        crate::shutdown_signal().await;
        handle.trigger();
    });
    daemon.serve().await?;
    Ok(())
}

#[cfg(any(unix, windows))]
pub(crate) use platform::bind_captured_owner;

#[cfg(unix)]
pub async fn capture_process_start(pid: u32) -> anyhow::Result<String> {
    owner_work(move || codewhale_config::private_directory::unix_process_start(pid)).await
}

#[cfg(windows)]
pub fn default_socket_path() -> Result<PathBuf, DaemonSocketError> {
    crate::daemon_windows::selected_pipe_path(None).map_err(DaemonSocketError::State)
}

#[cfg(windows)]
pub async fn capture_process_start(pid: u32) -> anyhow::Result<String> {
    owner_work(move || {
        Ok(
            codewhale_config::windows_identity::WindowsPeerProcess::open_current_user(pid)?
                .start()
                .to_string(),
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> SocketPathInputs {
        SocketPathInputs {
            explicit: None,
            codewhale_home_override: None,
            xdg_runtime_dir: None,
            user_home: Some(PathBuf::from("/home/whale")),
            macos: false,
        }
    }

    #[test]
    fn explicit_path_wins() {
        let resolved = resolve_socket_path(&SocketPathInputs {
            explicit: Some(PathBuf::from("/tmp/x.sock")),
            codewhale_home_override: Some(PathBuf::from("/iso")),
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
            ..inputs()
        })
        .expect("resolve");
        assert_eq!(resolved, PathBuf::from("/tmp/x.sock"));
    }

    #[test]
    fn explicit_codewhale_home_isolates_the_daemon() {
        let resolved = resolve_socket_path(&SocketPathInputs {
            codewhale_home_override: Some(PathBuf::from("/iso/home")),
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
            ..inputs()
        })
        .expect("resolve");
        assert_eq!(resolved, PathBuf::from("/iso/home/run/daemon.sock"));
    }

    #[test]
    fn xdg_runtime_dir_beats_home_layouts() {
        let resolved = resolve_socket_path(&SocketPathInputs {
            xdg_runtime_dir: Some(PathBuf::from("/run/user/1000")),
            macos: true,
            ..inputs()
        })
        .expect("resolve");
        assert_eq!(
            resolved,
            PathBuf::from("/run/user/1000/codewhale/daemon.sock")
        );
    }

    #[test]
    fn macos_defaults_to_application_support() {
        let resolved = resolve_socket_path(&SocketPathInputs {
            macos: true,
            user_home: Some(PathBuf::from("/Users/whale")),
            ..inputs()
        })
        .expect("resolve");
        assert_eq!(
            resolved,
            PathBuf::from("/Users/whale/Library/Application Support/codewhale/daemon.sock")
        );
    }

    #[test]
    fn linux_defaults_to_dot_codewhale_run() {
        let resolved = resolve_socket_path(&inputs()).expect("resolve");
        assert_eq!(
            resolved,
            PathBuf::from("/home/whale/.codewhale/run/daemon.sock")
        );
    }

    #[cfg(unix)]
    #[test]
    fn default_store_keeps_the_well_known_socket() {
        let resolved = store_selected_socket_path(None, &inputs()).expect("resolve");
        assert_eq!(
            resolved,
            PathBuf::from("/home/whale/.codewhale/run/daemon.sock")
        );
    }

    #[cfg(unix)]
    #[test]
    fn distinct_stores_derive_distinct_sibling_sockets() {
        let one = store_selected_socket_path(
            Some(Path::new("/home/whale/.local/state/one/runtime")),
            &inputs(),
        )
        .expect("resolve");
        let two = store_selected_socket_path(
            Some(Path::new("/home/whale/.local/state/two/runtime")),
            &inputs(),
        )
        .expect("resolve");
        assert_eq!(
            one.parent().unwrap(),
            Path::new("/home/whale/.codewhale/run")
        );
        assert_eq!(one.parent().unwrap(), two.parent().unwrap());
        assert_ne!(one, two);
        let name = one.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("store-"), "derived name: {name}");
        assert!(name.ends_with(".sock"), "derived name: {name}");
        assert_eq!(name.len(), 23, "derived name: {name}");
    }

    #[cfg(unix)]
    #[test]
    fn the_derived_socket_is_a_pure_function_of_the_store_root() {
        let store = Path::new("/home/whale/.local/state/one/runtime");
        let first = store_selected_socket_path(Some(store), &inputs()).expect("resolve");
        let second = store_selected_socket_path(Some(store), &inputs()).expect("resolve");
        assert_eq!(first, second);
    }

    #[cfg(unix)]
    #[test]
    fn store_sockets_stay_within_the_kernel_path_budget() {
        // The longest realistic parent (macOS app support) plus a long store
        // path must still resolve: the derived name is a fixed 23 bytes.
        let macos = SocketPathInputs {
            user_home: Some(PathBuf::from("/Users/whale")),
            macos: true,
            ..inputs()
        };
        let store = PathBuf::from(format!(
            "/Users/whale/Library/Application Support/Code/User/globalStorage/ext/runtime/ws-{}",
            "a".repeat(16)
        ));
        let resolved = store_selected_socket_path(Some(&store), &macos).expect("resolve");
        assert!(resolved.as_os_str().len() <= MAX_SOCKET_PATH_BYTES);
    }

    #[test]
    fn no_home_is_a_typed_error() {
        let err = resolve_socket_path(&SocketPathInputs {
            user_home: None,
            ..inputs()
        })
        .expect_err("must fail");
        assert!(
            matches!(err, DaemonSocketError::RuntimeDirUnavailable),
            "{err}"
        );
    }

    #[test]
    fn over_long_paths_are_refused_before_bind() {
        let long = PathBuf::from(format!(
            "/{}/daemon.sock",
            "d".repeat(MAX_SOCKET_PATH_BYTES)
        ));
        let err = resolve_socket_path(&SocketPathInputs {
            explicit: Some(long.clone()),
            ..inputs()
        })
        .expect_err("must fail");
        match err {
            DaemonSocketError::PathTooLong { path, len, max } => {
                assert_eq!(path, long);
                assert!(len > max);
                assert_eq!(max, MAX_SOCKET_PATH_BYTES);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn unsupported_platform_error_names_the_named_pipe() {
        let err = unsupported_platform();
        let text = err.to_string();
        assert!(text.contains(WINDOWS_NAMED_PIPE), "{text}");
        assert!(text.contains("not implemented"), "{text}");
    }

    #[test]
    fn attach_mode_defaults_to_guest() {
        let params: AttachParams =
            serde_json::from_value(serde_json::json!({ "client": { "name": "x" } }))
                .expect("parse");
        assert_eq!(params.mode, AttachMode::Attach);
        assert_eq!(params.expect_daemon_version, None);
    }
}

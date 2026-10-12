//! Runtime HTTP/SSE API for local Codewhale automation.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::convert::Infallible;
use std::fs;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use async_stream::stream;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State};
use axum::http::header;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::middleware;
use axum::response::Html;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use codewhale_protocol::agent_mail::{
    AgentMailDeliveryMode, AgentMailEnvelope, AgentMailMessageId, AgentMailSendRequest,
    AgentMailSendResponse,
};
use codewhale_protocol::runtime::{
    DynamicToolCallResult, RUNTIME_API_VERSION, RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION,
    RuntimeCapabilities, RuntimeEventEnvelope, RuntimeExperimentalCapabilities,
};
use codewhale_secrets::account::{
    ACCOUNT_API_BASE_ENV, DEFAULT_ACCOUNT_API_BASE, RuntimeAccountInfo,
};
#[cfg(not(test))]
use codewhale_secrets::account::{AccountSessionStore, secure_account_session_secrets};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower_http::cors::CorsLayer;

mod constitution;
mod notification_delivery;

#[cfg(test)]
use crate::dependencies::ExternalTool;

use crate::automation_manager::{
    AutomationManager, AutomationRecord, AutomationRunRecord, AutomationSchedulerConfig,
    CreateAutomationRequest, SharedAutomationManager, UpdateAutomationRequest, spawn_scheduler,
};
#[cfg(test)]
use crate::config::DEFAULT_TEXT_MODEL;
use crate::config::{Config, ProviderKind, normalize_model_name_for_provider, validate_route};
use crate::fleet::executor::{FleetExecutor, configured_codewhale_binary};
use crate::fleet::ledger::{
    FleetEventReplayError, FleetLedgerState, FleetTaskLedgerStatus, fleet_ledger_path,
    subscribe_fleet_ledger_appends,
};
use crate::fleet::manager::{
    FleetManager, FleetStatusSnapshot, FleetWorkerInspection, FleetWorkerRuntimeProjection,
    ManagedFleetRunDescriptor,
};
use crate::fleet::profile::canonical_public_role_name;
use crate::fleet::task_spec::FleetTaskSpecDocument;
use crate::fleet::worker_runtime::fleet_write_roots;
use crate::mcp::McpPool;
use crate::runtime_threads::{
    CompactThreadRequest, CreateThreadRequest, ExternalApprovalDecision,
    MAX_RUNTIME_EVENT_REPLAY_TAIL, RuntimeThreadManager, RuntimeThreadManagerConfig,
    SharedRuntimeThreadManager, StartTurnRequest, SteerTurnRequest, ThreadDetail, ThreadListFilter,
    ThreadRecord, TurnRecord, UpdateThreadRequest, UsageGroupBy, UsageTotals,
};
// `TurnItemKind` is read only by the summary tests now that the route builds
// its rows from `ThreadListFacts` instead of walking item records here.
#[cfg(test)]
pub(super) use crate::runtime_threads::{RuntimeTurnStatus, TurnItemKind, TurnItemLifecycleStatus};
use crate::session_manager::default_sessions_dir;
#[cfg(test)]
pub(super) use crate::session_manager::{SavedSession, SessionMetadata};
use crate::skill_state::SkillStateStore;
use crate::task_manager::{
    NewTaskRequest, SharedTaskManager, TaskManager, TaskManagerConfig, TaskRecord, TaskSummary,
};
use crate::tools::subagent::{
    AgentWorkerRecord, AgentWorkerStatus, SharedSubAgentManager, SubAgentStatus,
    new_shared_subagent_manager_with_timeout,
};
#[cfg(test)]
pub(super) use codewhale_models::{ContentBlock, Message};
use codewhale_protocol::fleet::{
    FleetArtifactKind, FleetEventReplay, FleetRun, FleetRunId, FleetRuntimeEvent,
    FleetRuntimeTarget, FleetSecurityPolicy, FleetTaskSpec, FleetWorkerEventPayload,
    FleetWorkerSpec, FleetWorkerStatus, FleetWorkflowDescriptor, FleetWorkflowKind,
};

mod auth;
mod avatars;
mod computer_display;
mod context;
mod diagnostics;
mod git;
mod jobs;
mod lsp;
mod mcp_import;
mod memory_lens;
mod mobile;
mod plans;
mod plugins;
mod secrets;
pub(crate) mod sessions;
mod targets;
mod terminal;
pub(crate) mod thread_history;
mod turn_artifacts;
mod voice;
mod web;
mod workspace;
#[cfg(test)]
use self::auth::ResolvedRuntimeAuth;
use self::auth::{
    require_runtime_token, resolve_runtime_auth, runtime_auth_status_lines,
    runtime_request_is_authorized,
};
use self::sessions::{
    create_session_from_thread, delete_session, get_session, get_session_repair,
    list_session_artifacts, list_sessions, list_sessions_summary, patch_session,
    read_session_artifact, resume_session_thread, save_current_session,
};
#[cfg(test)]
use self::sessions::{messages_from_thread_detail, session_to_detail};
#[cfg(test)]
use self::workspace::collect_workspace_status;
use self::workspace::{
    WorkspaceGitMetadata, collect_workspace_git_metadata, workspace_file_read,
    workspace_file_search, workspace_file_write, workspace_files_list, workspace_instructions,
    workspace_status,
};

const RUNTIME_TOKEN_ENV: &str = "CODEWHALE_RUNTIME_TOKEN";
const LEGACY_RUNTIME_TOKEN_ENV: &str = "DEEPSEEK_RUNTIME_TOKEN";
const LEGACY_RUNTIME_TOKEN_WARNING: &str = "Warning: DEEPSEEK_RUNTIME_TOKEN is deprecated; use \
CODEWHALE_RUNTIME_TOKEN (the legacy alias is removed in 0.10.0).";

struct RuntimeTokenEnvironment {
    token: Option<String>,
    legacy_alias_used: bool,
}

fn runtime_token_environment(lookup: &dyn Fn(&str) -> Option<String>) -> RuntimeTokenEnvironment {
    let nonblank = |name| {
        lookup(name)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    if let Some(token) = nonblank(RUNTIME_TOKEN_ENV) {
        return RuntimeTokenEnvironment {
            token: Some(token),
            legacy_alias_used: false,
        };
    }

    let token = nonblank(LEGACY_RUNTIME_TOKEN_ENV);
    RuntimeTokenEnvironment {
        legacy_alias_used: token.is_some(),
        token,
    }
}

fn runtime_token_alias_warning(
    cli_token: Option<&str>,
    environment: &RuntimeTokenEnvironment,
) -> Option<&'static str> {
    let cli_token_is_used = cli_token.is_some_and(|token| !token.trim().is_empty());
    (!cli_token_is_used && environment.legacy_alias_used).then_some(LEGACY_RUNTIME_TOKEN_WARNING)
}

#[derive(Clone)]
pub struct RuntimeApiState {
    config: Arc<parking_lot::RwLock<Config>>,
    workspace: PathBuf,
    plugin_discovery: Arc<crate::plugins::PluginDiscoveryContext>,
    task_manager: SharedTaskManager,
    runtime_threads: SharedRuntimeThreadManager,
    cors_origins: Vec<String>,
    sessions_dir: PathBuf,
    /// Original `--config` path (if any) used to load the initial config.
    /// Passed to `Config::load` on reload and to persistence helpers so
    /// GUI-driven config changes target the same file the server was
    /// started with, instead of falling back to the default discovery.
    config_path: Option<PathBuf>,
    /// Effective initial profile (`--profile` or `DEEPSEEK_PROFILE`).
    /// Reload must retain this overlay so profile-scoped routes do not vanish.
    config_profile: Option<String>,
    automations: SharedAutomationManager,
    sub_agent_manager: SharedSubAgentManager,
    runtime_token: Option<String>,
    skill_state: Arc<Mutex<SkillStateStore>>,
    auth_required: bool,
    bind_host: String,
    bind_port: u16,
    mobile_enabled: bool,
    mobile: Option<mobile::RuntimeMobileState>,
    web: Option<web::RuntimeWebState>,
    /// Executable used by Runtime API-owned Fleet manager loops. Stored on
    /// state so tests and embedded callers can provide a hermetic worker.
    fleet_codewhale_binary: String,
    /// Held by the actual owner, shared by all matching listener scopes.
    workspace_scopes: Arc<RuntimeWorkspaceScopes>,
    workspace_scope: Arc<RuntimeWorkspaceScope>,
    /// The computer this Engine runs on: display socket, human control
    /// lease, device client tokens and `computer.*` events (§3.3).
    computer: computer_display::ComputerState,
    /// Fires when the server stops on purpose, so open thread event streams
    /// end with a typed `stream.end` rather than a bare EOF.
    shutdown: RuntimeServerShutdown,
    /// Serializes this runtime's git writes (stage/unstage/discard/commit/
    /// branch) so a precondition check and its write are atomic with respect
    /// to other windows on the same server (#6647).
    git_writes: Arc<tokio::sync::Mutex<()>>,
    /// Serializes provider switches: each one saves, applies and, when the
    /// apply is refused, takes back its own save before the next one starts.
    provider_switches: Arc<tokio::sync::Mutex<()>>,
    #[cfg(test)]
    compat_stream_test_hook: Option<tokio::sync::mpsc::UnboundedSender<CompatStreamTestPoint>>,
}

// This is a cache bound inside the existing owner, matching the daemon's
// maximum of 64 live frontend connections. Scopes remain retained until owner
// retirement; opening another listener never creates a parallel cache.
const MAX_RUNTIME_WORKSPACE_SCOPES: usize = 64;

struct RuntimeWorkspaceScope {
    lexical: PathBuf,
    canonical: PathBuf,
    directory: Arc<std::fs::File>,
    mcp: Mutex<Option<(u64, Arc<Mutex<McpPool>>)>>,
    lsp: std::sync::OnceLock<Arc<crate::lsp::LspManager>>,
    owner: SharedRuntimeThreadManager,
    cleanup_runtime: tokio::runtime::Handle,
}

pub(crate) fn open_workspace_directory(workspace: &FsPath) -> Result<(PathBuf, std::fs::File)> {
    let canonical = workspace
        .canonicalize()
        .context("workspace is unavailable")?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0200_0000 | 0x0020_0000); // BACKUP_SEMANTICS, OPEN_REPARSE_POINT
    }
    let directory = options.open(&canonical)?;
    let metadata = directory.metadata()?;
    anyhow::ensure!(
        metadata.is_dir() && !crate::plugins::metadata_is_link_or_reparse(&metadata),
        "workspace is not an ordinary directory"
    );
    anyhow::ensure!(
        workspace.canonicalize()? == canonical,
        "selected workspace changed"
    );
    Ok((canonical, directory))
}

impl RuntimeWorkspaceScope {
    fn validate_sync(&self) -> Result<()> {
        let (canonical, directory) = open_workspace_directory(&self.lexical)?;
        anyhow::ensure!(
            canonical == self.canonical
                && crate::fleet::files::same_file(&self.directory, &directory)?,
            "selected workspace identity changed"
        );
        Ok(())
    }
    async fn validate(self: &Arc<Self>) -> Result<()> {
        let scope = self.clone();
        codewhale_app_server::daemon_socket::owner_work(move || scope.validate_sync()).await
    }
}

impl Drop for RuntimeWorkspaceScope {
    fn drop(&mut self) {
        let mcp = self.mcp.get_mut().take().map(|(_, pool)| pool);
        let lsp = self.lsp.take();
        // The canonical writer lease and held directory survive the actual
        // transport cleanup, including waiter cancellation and guest detach.
        let owner = self.owner.clone();
        let directory = self.directory.clone();
        self.cleanup_runtime.spawn(async move {
            let _owner = owner;
            let _directory = directory;
            if let Some(pool) = mcp {
                pool.lock().await.shutdown_all().await;
            }
            if let Some(manager) = lsp {
                manager.shutdown_all().await;
            }
        });
    }
}

struct RuntimeWorkspaceScopes {
    scopes: parking_lot::Mutex<BTreeMap<PathBuf, Arc<RuntimeWorkspaceScope>>>,
    mcp_generation: std::sync::atomic::AtomicU64,
    dynamic_servers: Arc<parking_lot::RwLock<HashMap<String, crate::mcp::McpServerConfig>>>,
    owner: SharedRuntimeThreadManager,
    workers: SharedSubAgentManager,
    cleanup_runtime: tokio::runtime::Handle,
}

impl RuntimeWorkspaceScopes {
    fn new(owner: SharedRuntimeThreadManager, workers: SharedSubAgentManager) -> Arc<Self> {
        Arc::new(Self {
            scopes: parking_lot::Mutex::new(BTreeMap::new()),
            mcp_generation: std::sync::atomic::AtomicU64::new(0),
            dynamic_servers: Arc::new(parking_lot::RwLock::new(HashMap::new())),
            owner,
            workers,
            cleanup_runtime: tokio::runtime::Handle::current(),
        })
    }

    async fn admit(self: &Arc<Self>, lexical: PathBuf) -> Result<Arc<RuntimeWorkspaceScope>> {
        let owner = self.clone();
        codewhale_app_server::daemon_socket::owner_work(move || {
            let (canonical, directory) = open_workspace_directory(&lexical)?;
            let directory = Arc::new(directory);
            let mut scopes = owner.scopes.lock();
            if let Some(scope) = scopes.get(&lexical) {
                anyhow::ensure!(
                    canonical == scope.canonical
                        && crate::fleet::files::same_file(&scope.directory, &directory)?,
                    "selected workspace identity changed"
                );
                return Ok(scope.clone());
            }
            anyhow::ensure!(
                scopes.len() < MAX_RUNTIME_WORKSPACE_SCOPES,
                "Runtime workspace scope limit reached; restart the owner to retire unused scopes"
            );
            owner
                .workers
                .blocking_write()
                .admit_coordination_workspace(lexical.clone(), canonical.clone(), directory.clone())
                .map_err(anyhow::Error::msg)?;
            let scope = Arc::new(RuntimeWorkspaceScope {
                lexical: lexical.clone(),
                canonical,
                directory,
                mcp: Mutex::new(None),
                lsp: std::sync::OnceLock::new(),
                owner: owner.owner.clone(),
                cleanup_runtime: owner.cleanup_runtime.clone(),
            });
            scopes.insert(lexical, scope.clone());
            Ok(scope)
        })
        .await
    }
}

async fn require_workspace_scope(
    State(state): State<RuntimeApiState>,
    request: Request,
    next: middleware::Next,
) -> Response {
    if state.workspace_scope.validate().await.is_err() {
        return ApiError::conflict("selected workspace identity changed").into_response();
    }
    next.run(request).await
}

/// How the Runtime API server stops on purpose.
///
/// `requested` fires once the server decides to stop: the listener stops
/// accepting, idle connections close, and every open thread event stream sends
/// `stream.end {reason: "runtime_shutdown"}` and finishes. `stopped` fires once
/// `serve_runtime_api` has drained every connection.
#[derive(Clone, Default)]
pub(crate) struct RuntimeServerShutdown {
    requested: CancellationToken,
    stopped: CancellationToken,
}

impl RuntimeServerShutdown {
    /// Ask the server to stop and wait at most `deadline` for it to drain.
    /// Returns whether it drained in time; a long-lived response that does not
    /// watch `requested` (a turn or Fleet stream) can hold it to the deadline.
    pub(crate) async fn drain(&self, deadline: Duration) -> bool {
        self.requested.cancel();
        tokio::time::timeout(deadline, self.stopped.cancelled())
            .await
            .is_ok()
    }
}

/// Serve `app` until `shutdown` is requested, then drain gracefully so the
/// final frames of open streams reach their clients before connections close.
async fn serve_runtime_api(
    listener: TcpListener,
    app: Router,
    shutdown: RuntimeServerShutdown,
) -> std::io::Result<()> {
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown.requested.clone().cancelled_owned())
    .await;
    shutdown.stopped.cancel();
    result
}

/// Listener state is local to the selected authenticated frontend. All service
/// handles remain the original owner's captured handles.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeFrontendReady {
    endpoint: SocketAddr,
    auth_required: bool,
    generated_auth: bool,
    reused_owner_listener: bool,
    mobile_bootstrap_url: Option<String>,
    web_bootstrap_url: Option<String>,
}
fn canonical_runtime_config_source(source: Option<PathBuf>) -> Result<Option<PathBuf>> {
    let Some(source) = source else {
        return Ok(None);
    };
    anyhow::ensure!(
        source.as_os_str().len() <= 32768,
        "operator config source exceeds its bounds"
    );
    let source = std::path::absolute(source)?;
    if source.try_exists()? {
        anyhow::ensure!(
            source.is_file(),
            "operator config source is not a regular file"
        );
        return Ok(Some(source.canonicalize()?));
    }
    // The captured loader permits a missing config document and missing
    // parents. Anchor its exact uncreated suffix under the nearest existing
    // directory without resolving another default or creating those paths.
    let mut ancestor = source.as_path();
    while !ancestor.try_exists()? {
        ancestor = ancestor
            .parent()
            .context("operator config source has no existing ancestor")?;
    }
    anyhow::ensure!(
        ancestor.is_dir(),
        "operator config source ancestor is not a directory"
    );
    let suffix = source.strip_prefix(ancestor)?;
    Ok(Some(ancestor.canonicalize()?.join(suffix)))
}

struct CapturedRuntimeFrontend {
    worker_setting: usize,
    generated_auth: bool,
    config_source: Option<PathBuf>,
    state: RuntimeApiState,
    default_model: String,
}
impl CapturedRuntimeFrontend {
    async fn capture(
        state: RuntimeApiState,
        model: String,
        worker_setting: usize,
        generated_auth: bool,
    ) -> Result<Arc<Self>> {
        let source = state
            .config
            .read()
            .loaded_config_path
            .clone()
            .or_else(|| state.config_path.clone());
        let config_source = codewhale_app_server::daemon_socket::owner_work(move || {
            canonical_runtime_config_source(source)
        })
        .await?;
        Ok(Arc::new(Self {
            state,
            default_model: model,
            worker_setting,
            generated_auth,
            config_source,
        }))
    }
    async fn validate_scope(
        &self,
        scope: &codewhale_app_server::RuntimeFrontendScope,
    ) -> Result<()> {
        scope.validate_bounds()?;
        anyhow::ensure!(
            scope.workers == self.worker_setting,
            "selected worker setting differs from the held scheduler"
        );
        anyhow::ensure!(
            scope.config_profile == self.state.config_profile,
            "selected config profile differs from the captured owner scope"
        );
        if let Some(source) = scope.config_source.clone() {
            let source = codewhale_app_server::daemon_socket::owner_work(move || {
                canonical_runtime_config_source(Some(source))
            })
            .await?;
            anyhow::ensure!(
                source == self.config_source,
                "selected operator config differs from the captured owner source"
            );
        }
        Ok(())
    }
}
impl codewhale_app_server::RuntimeOwnerFrontend for CapturedRuntimeFrontend {
    fn validate_selection<'a>(
        &'a self,
        selection: &'a codewhale_app_server::RuntimeOwnerFrontendSelection,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            match selection {
                codewhale_app_server::RuntimeOwnerFrontendSelection::Control(scope) => {
                    self.validate_scope(scope).await?;
                    anyhow::ensure!(
                        !self.state.runtime_threads.is_acp_host(),
                        "immutable ACP-base owner cannot admit ordinary control turns"
                    );
                }
                codewhale_app_server::RuntimeOwnerFrontendSelection::Acp { scope, model } => {
                    if let Some(scope) = scope {
                        self.validate_scope(scope).await?;
                    }
                    anyhow::ensure!(
                        model
                            .as_ref()
                            .is_none_or(|model| !model.trim().is_empty() && model.len() <= 1024),
                        "invalid selected ACP model"
                    );
                }
                codewhale_app_server::RuntimeOwnerFrontendSelection::Listener(selection) => {
                    selection.validate_bounds()?;
                    self.validate_scope(&codewhale_app_server::RuntimeFrontendScope {
                        workers: selection.workers,
                        workspace: selection.workspace.clone(),
                        config_profile: selection.config_profile.clone(),
                        config_source: selection.config_source.clone(),
                    })
                    .await?;
                    anyhow::ensure!(
                        !self.state.runtime_threads.is_acp_host(),
                        "immutable ACP-base owner cannot admit ordinary listener turns"
                    );
                    validate_runtime_listener_security(&RuntimeApiOptions {
                        host: selection.host.clone(),
                        port: selection.port,
                        cors_origins: selection.cors_origins.clone(),
                        auth_token: selection.auth_token.clone(),
                        insecure_no_auth: selection.insecure_no_auth,
                        mobile: selection.mobile,
                        web: selection.web,
                        control_frontend: Some(
                            codewhale_app_server::RuntimeControlFrontend::LegacyHttp,
                        ),
                        ..Default::default()
                    })?;
                }
            }
            Ok(())
        })
    }
    fn serve(
        &self,
        selection: codewhale_app_server::RuntimeOwnerFrontendSelection,
        compatibility: codewhale_app_server::AppState,
        input: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>,
        mut output: Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.validate_selection(&selection).await?;
            let selection = match selection {
                codewhale_app_server::RuntimeOwnerFrontendSelection::Control(scope) => {
                    self.validate_scope(&scope).await?;
                    anyhow::ensure!(
                        !self.state.runtime_threads.is_acp_host(),
                        "immutable ACP-base owner cannot admit ordinary control turns"
                    );
                    return codewhale_app_server::run_guest_control(
                        compatibility,
                        scope.workspace,
                        input,
                        output,
                    )
                    .await;
                }
                codewhale_app_server::RuntimeOwnerFrontendSelection::Acp { scope, model } => {
                    let workspace = if let Some(scope) = scope {
                        self.validate_scope(&scope).await?;
                        scope.workspace
                    } else {
                        self.state.workspace.clone()
                    };
                    let config = self.state.config.read().clone();
                    let acp = crate::acp_server::capture_frontend(
                        config,
                        model.unwrap_or_else(|| self.default_model.clone()),
                        workspace,
                        self.state.runtime_threads.clone(),
                        self.state.sessions_dir.clone(),
                        self.state.config_path.clone(),
                        self.state.config_profile.clone(),
                    )?;
                    return acp.serve(input, output).await;
                }
                codewhale_app_server::RuntimeOwnerFrontendSelection::Listener(selection) => {
                    selection
                }
            };
            selection.validate_bounds()?;
            anyhow::ensure!(
                selection.workers == self.worker_setting,
                "selected worker setting differs from the held scheduler"
            );
            anyhow::ensure!(
                selection.config_profile == self.state.config_profile,
                "selected config profile differs from the captured owner scope"
            );
            anyhow::ensure!(
                !self.state.runtime_threads.is_acp_host(),
                "immutable ACP-base owner cannot admit ordinary listener turns"
            );
            let workspace = selection.workspace;
            let options = RuntimeApiOptions {
                host: selection.host,
                port: selection.port,
                cors_origins: selection.cors_origins,
                auth_token: selection.auth_token,
                insecure_no_auth: selection.insecure_no_auth,
                mobile: selection.mobile,
                web: selection.web,
                control_frontend: Some(codewhale_app_server::RuntimeControlFrontend::LegacyHttp),
                ..Default::default()
            };
            validate_runtime_listener_security(&options)?;
            let selected_addr = runtime_bind_address(&options.host, options.port)?;
            let original_addr = runtime_bind_address(&self.state.bind_host, self.state.bind_port)?;
            let same_auth = match (
                options
                    .auth_token
                    .as_deref()
                    .map(str::trim)
                    .filter(|token| !token.is_empty()),
                self.state.runtime_token.as_deref(),
            ) {
                (Some(selected), Some(original)) => codewhale_core::secret_eq::constant_time_eq(
                    selected.as_bytes(),
                    original.as_bytes(),
                ),
                (None, Some(_)) => self.generated_auth && !options.insecure_no_auth,
                (None, None) => options.insecure_no_auth,
                _ => false,
            };
            if selected_addr == original_addr
                && !options.web
                && !options.mobile
                && self.state.web.is_none()
                && self.state.mobile.is_none()
                && options.cors_origins == self.state.cors_origins
                && workspace == self.state.workspace
                && same_auth
            {
                let ready = RuntimeFrontendReady {
                    endpoint: original_addr,
                    auth_required: self.state.auth_required,
                    generated_auth: self.generated_auth,
                    reused_owner_listener: true,
                    mobile_bootstrap_url: None,
                    web_bootstrap_url: None,
                };
                write_frontend_ready(&mut output, ready).await?;
                return tokio::select! {
                    result = wait_frontend_input_close(input) => result,
                    _ = self.state.shutdown.requested.cancelled() => Ok(()),
                };
            }
            let resolved =
                resolve_runtime_auth(options.auth_token.clone(), None, options.insecure_no_auth);
            let listener =
                TcpListener::bind(runtime_bind_address(&options.host, options.port)?).await?;
            let endpoint = listener.local_addr()?;
            let workspace_scope = self.state.workspace_scopes.admit(workspace.clone()).await?;
            let mut state = self.state.clone();
            state.workspace = workspace.clone();
            state.workspace_scope = workspace_scope;
            state.runtime_token = resolved.token.clone();
            state.auth_required = resolved.token.is_some();
            state.bind_host = options.host;
            state.bind_port = endpoint.port();
            state.cors_origins = options.cors_origins.clone();
            state.mobile_enabled = options.mobile;
            let (web, web_bootstrap_url) = if options.web {
                anyhow::ensure!(
                    state.auth_required,
                    "Codewhale web requires Runtime authentication"
                );
                let (web, nonce) = web::RuntimeWebState::new();
                (Some(web), Some(web::bootstrap_url(endpoint, &nonce)))
            } else {
                (None, None)
            };
            let (mobile, mobile_bootstrap_url) = if options.mobile && state.auth_required {
                let (mobile, nonce) = mobile::RuntimeMobileState::new();
                (Some(mobile), Some(mobile::bootstrap_url(endpoint, &nonce)))
            } else {
                (None, None)
            };
            state.web = web;
            state.mobile = mobile;
            // The guest may drain its own streams/listener. It cannot replace
            // the global signal registration or stop shared schedulers/managers.
            let shutdown = RuntimeServerShutdown::default();
            state.shutdown = shutdown.clone();
            let app =
                build_router(state).merge(codewhale_app_server::runtime_compatibility_router(
                    compatibility,
                    &options.cors_origins,
                    resolved.token.clone(),
                    Some(workspace),
                ));
            let ready = RuntimeFrontendReady {
                endpoint,
                auth_required: resolved.token.is_some(),
                generated_auth: resolved.generated,
                reused_owner_listener: false,
                mobile_bootstrap_url,
                web_bootstrap_url,
            };
            write_frontend_ready(&mut output, ready).await?;
            let serving = serve_runtime_api(listener, app, shutdown.clone());
            tokio::pin!(serving);
            let input_result = tokio::select! {
                result = &mut serving => { result?; return Ok(()); }
                _ = self.state.shutdown.requested.cancelled() => Ok(()),
                result = wait_frontend_input_close(input) => result,
            };
            shutdown.requested.cancel();
            // Accepted turns live in the held manager, independently of this
            // response or listener drain. Never retry their uncertain writes.
            tokio::time::timeout(Duration::from_secs(5), &mut serving)
                .await
                .context("selected listener drain deadline expired")??;
            input_result
        })
    }
}

async fn write_frontend_ready(
    output: &mut (dyn tokio::io::AsyncWrite + Send + Unpin),
    ready: RuntimeFrontendReady,
) -> Result<()> {
    use tokio::io::AsyncWriteExt as _;
    let frame = serde_json::to_vec(
        &json!({"jsonrpc":"2.0","method":"daemon/frontend_ready","params":ready}),
    )?;
    tokio::time::timeout(Duration::from_secs(5), async {
        output.write_all(&frame).await?;
        output.write_all(b"\n").await?;
        output.flush().await
    })
    .await
    .context("selected listener readiness write timed out; outcome uncertain")??;
    Ok(())
}
async fn wait_frontend_input_close(
    input: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>,
) -> Result<()> {
    let mut input = codewhale_app_server::BoundedLines::new(input);
    if let Some(line) = input.next_line().await? {
        let value: Value = serde_json::from_str(&line)?;
        anyhow::ensure!(
            codewhale_app_server::is_control_input_closed(&value),
            "selected listener accepts only its logical input close"
        );
    }
    Ok(())
}

/// The serving Runtime API, for the process signal handler (`lib.rs`), which
/// exits the process on a terminating signal. Without this it would cut every
/// open stream mid-connection, indistinguishable from a network drop.
static SIGNAL_SHUTDOWN: std::sync::Mutex<Option<RuntimeServerShutdown>> =
    std::sync::Mutex::new(None);

/// How long a terminating signal waits for open streams to say goodbye. A
/// second signal skips the wait.
const SIGNAL_SHUTDOWN_DRAIN: Duration = Duration::from_secs(2);

/// Clears `SIGNAL_SHUTDOWN` when the server that registered it returns.
struct SignalShutdownRegistration;

impl SignalShutdownRegistration {
    fn register(shutdown: &RuntimeServerShutdown) -> Self {
        if let Ok(mut slot) = SIGNAL_SHUTDOWN.lock() {
            *slot = Some(shutdown.clone());
        }
        Self
    }
}

impl Drop for SignalShutdownRegistration {
    fn drop(&mut self) {
        if let Ok(mut slot) = SIGNAL_SHUTDOWN.lock() {
            *slot = None;
        }
    }
}

/// Called by the process signal handler before it exits: stop the serving
/// Runtime API (if any) and give its open streams a bounded window to send
/// their final `stream.end` frame.
pub(crate) async fn drain_for_signal_exit() {
    let shutdown = SIGNAL_SHUTDOWN.lock().ok().and_then(|slot| slot.clone());
    if let Some(shutdown) = shutdown
        && !shutdown.drain(SIGNAL_SHUTDOWN_DRAIN).await
    {
        tracing::warn!("Runtime API did not drain within the signal shutdown window");
    }
}

#[cfg(test)]
enum CompatStreamTestPoint {
    ThreadCreated {
        thread_id: String,
        resume: tokio::sync::oneshot::Sender<()>,
    },
    SubscribedBeforeReplay {
        thread_id: String,
        turn_id: String,
        resume: tokio::sync::oneshot::Sender<()>,
    },
    ReplayLoaded {
        thread_id: String,
        turn_id: String,
        resume: tokio::sync::oneshot::Sender<()>,
    },
}

#[derive(Debug, Clone)]
pub struct RuntimeApiOptions {
    pub host: String,
    pub port: u16,
    pub workers: usize,
    /// Additional CORS origins to allow on top of the built-in defaults
    /// (`http://localhost:{3000,1420}`, `http://127.0.0.1:{3000,1420}`,
    /// `tauri://localhost`). Populated by `--cors-origin` (repeatable),
    /// `CODEWHALE_CORS_ORIGINS` (comma-separated, `DEEPSEEK_CORS_ORIGINS`
    /// as alias), and `[runtime_api] cors_origins` in `config.toml`.
    /// Whalescale#255 / #561.
    pub cors_origins: Vec<String>,
    /// Optional bearer token required for `/v1/*` routes. If omitted here,
    /// `run_http_server` checks `CODEWHALE_RUNTIME_TOKEN`, then
    /// `DEEPSEEK_RUNTIME_TOKEN` as an alias.
    pub auth_token: Option<String>,
    /// Allow `/v1/*` routes without auth when no token is configured.
    pub insecure_no_auth: bool,
    /// Enables the built-in mobile control page at `/mobile`.
    pub mobile: bool,
    /// Enables the embedded local browser client and opens it after binding.
    /// Web mode is always loopback-only and uses a one-time bootstrap cookie
    /// exchange rather than exposing the Runtime token to the browser URL.
    pub web: bool,
    /// Show a QR code for the mobile URL in the terminal.
    pub show_qr: bool,
    /// Original `--config` path used to load the initial config. When
    /// `Some`, GUI-driven config reloads and persistence target this file
    /// instead of the default discovery path.
    pub config_path: Option<PathBuf>,
    /// Effective profile used to load the server's initial Config.
    pub config_profile: Option<String>,
    pub control_frontend: Option<codewhale_app_server::RuntimeControlFrontend>,
}

impl Default for RuntimeApiOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 7878,
            workers: 2,
            cors_origins: Vec::new(),
            auth_token: None,
            insecure_no_auth: false,
            mobile: false,
            web: false,
            show_qr: false,
            config_path: None,
            config_profile: None,
            control_frontend: None,
        }
    }
}

#[derive(Debug, Deserialize)]
struct StreamTurnRequest {
    #[serde(default, rename = "maxOutputTokens", alias = "max_output_tokens")]
    max_output_tokens: Option<std::num::NonZeroU32>,
    prompt: String,
    #[serde(default)]
    images: Vec<codewhale_protocol::runtime::RuntimeImageInput>,
    model: Option<String>,
    mode: Option<String>,
    permission_posture: Option<String>,
    workspace: Option<PathBuf>,
    allow_shell: Option<bool>,
    trust_mode: Option<bool>,
    auto_approve: Option<bool>,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    mode: &'static str,
}

#[derive(Debug, Serialize)]
struct TasksResponse {
    tasks: Vec<TaskSummary>,
    counts: crate::task_manager::TaskCounts,
}

#[derive(Debug, Deserialize)]
struct TasksQuery {
    limit: Option<usize>,
    workspace: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct ThreadsQuery {
    limit: Option<usize>,
    include_archived: Option<bool>,
    /// When `true`, returns archived threads only (overrides `include_archived`).
    /// Whalescale#260 / #563.
    archived_only: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ThreadSummaryQuery {
    limit: Option<usize>,
    search: Option<String>,
    include_archived: Option<bool>,
    /// Comma-separated exact IDs let callers filter before applying the result
    /// limit. One parameter, because axum's `Query` cannot collect repeated keys.
    thread_ids: Option<String>,
    /// When `true`, returns archived threads only (overrides `include_archived`).
    /// Whalescale#260 / #563.
    archived_only: Option<bool>,
}

fn resolve_thread_filter(
    include_archived: Option<bool>,
    archived_only: Option<bool>,
) -> ThreadListFilter {
    if archived_only.unwrap_or(false) {
        ThreadListFilter::ArchivedOnly
    } else if include_archived.unwrap_or(false) {
        ThreadListFilter::IncludeArchived
    } else {
        ThreadListFilter::ActiveOnly
    }
}

#[derive(Debug, Serialize)]
struct ThreadSummary {
    id: String,
    title: String,
    preview: String,
    model: String,
    mode: String,
    workspace: PathBuf,
    branch: Option<String>,
    head: Option<String>,
    dirty: bool,
    archived: bool,
    updated_at: chrono::DateTime<Utc>,
    latest_turn_id: Option<String>,
    latest_turn_status: Option<String>,
    /// Pending approvals plus pending user-input requests in the canonical
    /// thread snapshot. Clients use this typed fact for attention grouping;
    /// lifecycle prose and turn-status strings are not an authority signal.
    pending_attention_count: usize,
}

/// `GET /v1/skills` row. Routing metadata (`invocation`, `aliases`,
/// `bundled_tier`) rides along so a client can build a picker or autocomplete
/// without a second request per row; the body itself stays behind
/// `GET /v1/skills/{name}`.
#[derive(Debug, Serialize)]
struct SkillEntry {
    name: String,
    description: String,
    /// Native Skill locator. Reviewed plugin paths are deliberately omitted;
    /// their bodies are available only through the authority-bound snapshot.
    path: Option<PathBuf>,
    source: String,
    plugin_id: Option<String>,
    plugin_generation: Option<u64>,
    plugin_content_hash: Option<String>,
    enabled: bool,
    is_bundled: bool,
    /// `model+user` | `explicit-only` | `model-only` | `disabled`.
    invocation: &'static str,
    /// Alternate lookup names for the same body; never separate entries.
    aliases: Vec<String>,
    /// `core` | `tools` for bundled skills, absent for custom ones.
    bundled_tier: Option<&'static str>,
}

#[derive(Debug, Serialize)]
struct SkillsResponse {
    directory: PathBuf,
    directories: Vec<PathBuf>,
    warnings: Vec<String>,
    skills: Vec<SkillEntry>,
}

/// `GET /v1/skills/{name}` — one skill's body plus the routing metadata a
/// client needs to offer activation: the `/v1/skills` row, plus the full
/// SKILL.md body. Flattened rather than repeated field-by-field so the two
/// shapes cannot drift apart.
#[derive(Debug, Serialize)]
struct SkillDetailResponse {
    #[serde(flatten)]
    skill: SkillEntry,
    /// Full SKILL.md body (frontmatter stripped) for client-side activation.
    body: String,
}

#[derive(Debug, Serialize)]
struct AgentRunsResponse {
    runs: Vec<AgentWorkerRecord>,
    /// Live launch-governor state for agents this runtime launches (Fleet
    /// runs), so a client can say why a queued run waits (addendum F5).
    governor: AgentRunsGovernor,
}

/// The rate-limit governor behind agent launches, as of this response.
#[derive(Debug, Serialize)]
struct AgentRunsGovernor {
    /// Launch slots currently granted, after any rate-limit shrink.
    launch_slots: usize,
    /// Configured launch concurrency.
    max_launch_slots: usize,
    /// New launches are held entirely after sustained provider rate limits.
    paused: bool,
    /// Provider rate limits seen inside the governor's sliding window.
    recent_rate_limits: usize,
    /// One human line while launches are held back; absent at full speed.
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetSkillEnabledRequest {
    enabled: bool,
}

#[derive(Debug, Serialize)]
struct SetSkillEnabledResponse {
    name: String,
    enabled: bool,
}

// ─── Skill lifecycle request/response types ────────────────────────────────

#[derive(Debug, Deserialize)]
struct InstallSkillRequest {
    /// Remote source spec: `github:owner/repo`, `https://…`, or a registry name.
    source: String,
    /// `"project"` or `"global"` (default: `"global"`).
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateSkillRequest {
    /// `"project"`, `"global"`, or `null` (auto-detect).
    #[serde(default)]
    scope: Option<String>,
    /// Digest the caller observed before requesting the update. The mutation
    /// will fail if the on-disk digest has changed since.
    #[serde(default)]
    expected_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UninstallSkillQuery {
    /// `"project"`, `"global"`, or `null` (auto-detect).
    #[serde(default)]
    scope: Option<String>,
    /// Digest the caller observed. The mutation will fail if it has drifted.
    #[serde(default)]
    expected_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TrustSkillRequest {
    /// `"project"`, `"global"`, or `null` (auto-detect).
    #[serde(default)]
    scope: Option<String>,
    /// Digest the caller reviewed. The mutation will fail if it has drifted.
    #[serde(default)]
    expected_digest: Option<String>,
}

/// Scope query parameter used by the audit endpoint.
#[derive(Debug, Deserialize, Default)]
struct SkillScopeQuery {
    /// `"project"` or `"global"` to restrict to one root.
    scope: Option<String>,
}

#[derive(Debug, Serialize)]
struct SkillMutationReceiptResponse {
    /// Skill name as recorded by the mutation.
    name: String,
    /// Human-readable action performed: `"installed"`, `"updated"`, `"removed"`,
    /// `"trusted"`, `"no_change"`, etc.
    outcome: &'static str,
    /// Resolved install scope: `"project"` or `"global"`.
    scope: String,
    /// Display path of the skill package (may be redacted for plugin snapshots).
    safe_target_path: String,
    /// Trust advisory note, present only for `"trusted"` outcomes.
    #[serde(skip_serializing_if = "Option::is_none")]
    trust_note: Option<&'static str>,
}

/// Read-only audit receipt for a single installed skill.
#[derive(Debug, Serialize)]
struct SkillAuditEntry {
    name: String,
    safe_display_path: String,
    source_kind: String,
    scope: String,
    digest: SkillAuditDigest,
    trust: String,
    integrity: String,
    available_actions: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SkillAuditDigest {
    state: String,
    /// Hex digest value; absent when the digest is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
}

#[derive(Debug, Serialize)]
struct SkillAuditResponse {
    /// `true` when multiple owned copies with the same name exist. The
    /// caller should re-request with an explicit `scope` parameter.
    ambiguous: bool,
    skills: Vec<SkillAuditEntry>,
}

#[derive(Debug, Deserialize)]
struct DecideApprovalBody {
    decision: String,
    #[serde(default)]
    remember: bool,
}

#[derive(Debug, Serialize)]
struct DecideApprovalResponse {
    ok: bool,
    approval_id: String,
    decision: String,
    delivered: bool,
}

#[derive(Debug, Deserialize)]
struct SubmitUserInputBody {
    answers: Vec<UserInputAnswerBody>,
}

#[derive(Debug, Deserialize)]
struct UserInputAnswerBody {
    id: String,
    label: String,
    value: String,
}

#[derive(Debug, Serialize)]
struct SubmitUserInputResponse {
    ok: bool,
    input_id: String,
    delivered: bool,
}

#[derive(Debug, Serialize)]
struct RuntimeInfoResponse {
    service: &'static str,
    runtime_api_version: &'static str,
    codewhale_version: &'static str,
    /// Full 40-character source commit embedded by the shared build script.
    /// Desktop compatibility intentionally rejects `unknown` and abbreviated
    /// values, so source archives without build provenance fail closed.
    codewhale_commit: &'static str,
    bind_host: String,
    port: u16,
    auth_required: bool,
    transports: Vec<&'static str>,
    capabilities: RuntimeCapabilities,
    account: RuntimeAccountInfo,
    experimental: RuntimeExperimentalCapabilities,
    // Backward-compatible alias kept for existing clients.
    version: &'static str,
}

fn default_runtime_capabilities() -> RuntimeCapabilities {
    RuntimeCapabilities {
        client_token_intents: true,
        account_session: true,
        account_model_owner: true,
        threads: true,
        thread_shell_consent: true,
        turns: true,
        turn_operation_idempotency: true,
        turn_operation_lookup: true,
        automation_mutation_preconditions: true,
        turn_image_inputs: true,
        turn_output_token_limit: true,
        profile_constitution: true,
        turn_steer: true,
        turn_interrupt: true,
        event_replay: true,
        external_tools: true,
        environments: false,
        worker_runtime: true,
        fleet_run_create: true,
        fleet_run_start: true,
        fleet_event_replay: true,
        fleet_event_stream: true,
        fleet_local_target: true,
        thread_goals: true,
        memory: true,
        mcp_server_management: true,
        skill_lifecycle: true,
        skill_detail: true,
        plugin_management: true,
        agent_mail: true,
        // SSE journal frames carry their durable `seq` as the event id, and the
        // thread event stream resumes from `Last-Event-ID`.
        event_stream_resume: true,
        // The terminal family follows the routes' own gate: the owner is
        // `#[cfg(unix)]` end to end, and the Windows and OpenHarmony builds
        // answer 501. A client must be able to feature-detect that before it
        // offers a pane, so the flag must never outrun the handler.
        terminal_stream: cfg!(all(unix, not(target_env = "ohos"))),
        terminal_input: cfg!(all(unix, not(target_env = "ohos"))),
        terminal_resize: cfg!(all(unix, not(target_env = "ohos"))),
        terminal_kill: cfg!(all(unix, not(target_env = "ohos"))),
    }
}

fn runtime_api_sub_agent_manager(workspace: &FsPath, workers: usize) -> SharedSubAgentManager {
    let max_agents = workers.max(1);
    new_shared_subagent_manager_with_timeout(
        workspace.to_path_buf(),
        max_agents,
        max_agents,
        Duration::from_secs(crate::config::DEFAULT_SUBAGENT_HEARTBEAT_TIMEOUT_SECS),
        max_agents,
    )
}

#[derive(Debug, Serialize)]
struct McpServerEntry {
    name: String,
    origin: &'static str,
    writable: bool,
    auth_required: bool,
    enabled: bool,
    required: bool,
    command: Option<String>,
    url: Option<String>,
    connected: bool,
    enabled_tools: Vec<String>,
    disabled_tools: Vec<String>,
}

#[derive(Debug, Serialize)]
struct McpServersResponse {
    revision: String,
    servers: Vec<McpServerEntry>,
}

#[derive(Debug, Deserialize)]
struct McpToolsQuery {
    server: Option<String>,
    #[serde(default)]
    connect: bool,
}

#[derive(Debug, Serialize)]
struct McpToolEntry {
    server: String,
    name: String,
    prefixed_name: String,
    description: Option<String>,
    input_schema: Value,
}

#[derive(Debug, Serialize)]
struct McpToolsResponse {
    tools: Vec<McpToolEntry>,
    connections: Vec<McpConnectionOutcome>,
}

#[derive(Debug, Serialize)]
struct McpConnectionOutcome {
    server: String,
    connected: bool,
    auth_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Request body for `POST /v1/apps/mcp/servers` (create) and
/// `PATCH /v1/apps/mcp/servers/{name}` (update).
///
/// Either `command` **or** `url` must be set on create. On update, only
/// supplied fields are applied; absent fields leave the existing value in
/// place.
#[derive(Debug, Deserialize)]
struct McpServerWriteRequest {
    /// stdio command binary (e.g. `"npx"`).
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    command: Option<Option<String>>,
    /// Arguments for the stdio command.
    args: Option<Vec<String>>,
    /// Environment variables injected into the stdio child process.
    /// Values are stored as-is; use `${VAR}` syntax to reference environment
    /// variables at runtime instead of embedding secrets here.
    env: Option<std::collections::HashMap<String, String>>,
    /// HTTP(S) endpoint for streamable-HTTP or SSE MCP servers.
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    url: Option<Option<String>>,
    /// Explicit transport override (`"sse"` or `"streamable_http"`).
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    transport: Option<Option<String>>,
    /// Override the server-level connect timeout in seconds.
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    connect_timeout: Option<Option<u64>>,
    /// Override the server-level execute timeout in seconds.
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    execute_timeout: Option<Option<u64>>,
    /// Override the server-level read timeout in seconds.
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    read_timeout: Option<Option<u64>>,
    /// Whether the server is enabled. Defaults to `true` on create.
    enabled: Option<bool>,
    /// Whether a connection failure for this server is fatal.
    required: Option<bool>,
    /// Allowlist of tool names to expose (empty = expose all).
    enabled_tools: Option<Vec<String>>,
    /// Denylist of tool names to hide.
    disabled_tools: Option<Vec<String>>,
    /// Variable names whose runtime values are injected as HTTP headers.
    /// The key in this map is the HTTP header name; the value is the
    /// environment variable whose value supplies the header value at
    /// request time. Credentials remain in the environment, not on disk.
    env_headers: Option<std::collections::HashMap<String, String>>,
    /// Environment variable that contains a bearer token for URL-based servers.
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    bearer_token_env_var: Option<Option<String>>,
    /// OAuth scopes requested during `codewhale mcp login`.
    scopes: Option<Vec<String>>,
    /// RFC 8707 resource parameter for the OAuth authorization URL.
    #[serde(default, deserialize_with = "deserialize_present_nullable")]
    oauth_resource: Option<Option<String>>,
}

/// Preserve the difference between an omitted PATCH field and an explicit
/// `null`: serde only calls this decoder when the field is present.
fn deserialize_present_nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Response returned by MCP server management endpoints.
///
/// Sensitive fields (`headers`, `env_headers`, `bearer_token_env_var`,
/// `env`, OAuth client secrets) are intentionally omitted or redacted so
/// the API never echoes credentials back to callers.
#[derive(Debug, Serialize)]
struct McpServerDetail {
    revision: String,
    name: String,
    credential_configured: bool,
    origin: &'static str,
    writable: bool,
    auth_required: bool,
    enabled: bool,
    required: bool,
    command: Option<String>,
    args: Vec<String>,
    /// Environment variable names injected into the process.
    /// Values are **not** returned — callers see only the keys.
    env_keys: Vec<String>,
    url: Option<String>,
    transport: Option<String>,
    connect_timeout: Option<u64>,
    execute_timeout: Option<u64>,
    read_timeout: Option<u64>,
    enabled_tools: Vec<String>,
    disabled_tools: Vec<String>,
    /// HTTP header names that are read from environment variables.
    /// The corresponding environment variable values are **not** returned.
    env_header_keys: Vec<String>,
    /// Whether a `bearer_token_env_var` is configured (value not returned).
    has_bearer_token_env_var: bool,
    scopes: Vec<String>,
    oauth_resource: Option<String>,
    /// Live connection state from the in-memory pool (if the pool is active).
    connected: bool,
}

impl McpServerDetail {
    fn from_config(
        name: &str,
        cfg: &crate::mcp::McpServerConfig,
        connected: bool,
        revision: String,
    ) -> Self {
        let mut env_keys: Vec<String> = cfg.env.keys().cloned().collect();
        env_keys.sort();
        let mut env_header_keys: Vec<String> = cfg.env_headers.keys().cloned().collect();
        env_header_keys.sort();
        Self {
            revision,
            name: name.to_string(),
            credential_configured: mcp_credential_configured(cfg),
            origin: "global",
            writable: true,
            auth_required: false,
            enabled: cfg.is_enabled(),
            required: cfg.required,
            command: cfg.command.clone(),
            args: cfg.args.clone(),
            env_keys,
            url: cfg.url.clone(),
            transport: cfg.transport.clone(),
            connect_timeout: cfg.connect_timeout,
            execute_timeout: cfg.execute_timeout,
            read_timeout: cfg.read_timeout,
            enabled_tools: cfg.enabled_tools.clone(),
            disabled_tools: cfg.disabled_tools.clone(),
            env_header_keys,
            has_bearer_token_env_var: cfg.bearer_token_env_var.is_some(),
            scopes: cfg.scopes.clone(),
            oauth_resource: cfg.oauth_resource.clone(),
            connected,
        }
    }
}

#[derive(Debug, Serialize)]
struct McpServerActionReceipt {
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    name: String,
    action: &'static str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection: Option<McpConnectionOutcome>,
}

#[derive(Debug, Deserialize)]
struct AutomationRunsQuery {
    limit: Option<usize>,
    /// Serve the terminal-run archive kept for deleted automations instead of
    /// the live run history.
    #[serde(default)]
    archived: bool,
}

#[derive(Debug, Deserialize)]
struct ThreadEventsQuery {
    since_seq: Option<u64>,
    replay_limit: Option<usize>,
    #[serde(default)]
    progress: bool,
}

const DEFAULT_FLEET_EVENT_REPLAY_LIMIT: usize = 250;
const MAX_FLEET_EVENT_REPLAY_LIMIT: usize = 1_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateFleetRunRequest {
    #[serde(default)]
    name: Option<String>,
    target: FleetRuntimeTarget,
    roles: Vec<ManagedFleetRoleRequest>,
    workflow: ManagedFleetWorkflowRequest,
    #[serde(default, alias = "workers")]
    worker_specs: Vec<FleetWorkerSpec>,
    #[serde(default)]
    labels: BTreeMap<String, String>,
    #[serde(default)]
    security_policy: Option<FleetSecurityPolicy>,
    #[serde(default)]
    max_workers: Option<usize>,
    /// Optional run-wide usage ceiling (R6, #5567).
    #[serde(default)]
    usage_ceiling: Option<codewhale_protocol::fleet::FleetUsageCeiling>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedFleetRoleRequest {
    name: String,
    #[serde(default)]
    agent_profile: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedFleetWorkflowRequest {
    id: String,
    kind: FleetWorkflowKind,
    #[serde(alias = "task_specs")]
    tasks: Vec<FleetTaskSpec>,
}

#[derive(Debug, Deserialize)]
struct FleetEventsQuery {
    after: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct StartTurnResponse {
    thread: ThreadRecord,
    turn: TurnRecord,
    /// Present only when the durable `operation_key` made this submission a
    /// replay of one already accepted: the turn is the original and nothing
    /// new was admitted. Omitted otherwise so every existing response stays
    /// byte-identical — a client that never sends a key sees no change.
    #[serde(skip_serializing_if = "replay_flag_is_absent")]
    idempotent_replay: bool,
}

fn replay_flag_is_absent(replayed: &bool) -> bool {
    !*replayed
}

fn install_runtime_server_workshop_budgets(
    config: &Config,
) -> crate::tools::large_output_router::WorkshopConfig {
    crate::tools::large_output_router::WorkshopConfig::install_active(config.workshop.as_ref())
}

#[cfg(test)]
fn open_runtime_threads_for_server(
    config: &Config,
    workspace: PathBuf,
    manager_config: RuntimeThreadManagerConfig,
    plugin_registry: Arc<crate::plugins::PluginRegistry>,
) -> Result<(
    SharedRuntimeThreadManager,
    crate::tools::large_output_router::WorkshopConfig,
)> {
    open_runtime_threads_for_host(
        config,
        workspace,
        manager_config,
        plugin_registry,
        false,
        None,
        None,
    )
}

pub(crate) fn open_runtime_threads_for_host(
    config: &Config,
    workspace: PathBuf,
    manager_config: RuntimeThreadManagerConfig,
    plugin_registry: Arc<crate::plugins::PluginRegistry>,
    acp: bool,
    preheld_lock: Option<crate::runtime_threads::RuntimeProcessOwnerLock>,
    binding: Option<&crate::runtime_threads::RuntimeStoreBinding>,
) -> Result<(
    SharedRuntimeThreadManager,
    crate::tools::large_output_router::WorkshopConfig,
)> {
    // The Runtime API lazily creates engines after the HTTP/Web server starts.
    // Install the resolved process-wide read/tool byte limits before the
    // thread manager can spawn any of those engines, matching interactive and
    // headless exec startup.
    let workshop_activation = install_runtime_server_workshop_budgets(config);
    let profile = if acp {
        crate::core::engine::EngineHostProfile::Acp
    } else {
        crate::core::engine::EngineHostProfile::Normal
    };
    let manager = Arc::new(RuntimeThreadManager::open_host(
        config.clone(),
        workspace,
        manager_config,
        plugin_registry,
        profile,
        preheld_lock,
        binding,
    )?);
    // Publish the same exact endpoint-scoped catalog as interactive startup
    // before the server admits turns. A cached model list alone does not make
    // its capabilities available to route resolution.
    crate::provider_catalog_live::maybe_load_persisted_cache_for_config(config);
    Ok((manager, workshop_activation))
}

#[cfg(unix)]
enum RuntimeHostAdmission {
    Attached(Box<codewhale_app_server::daemon_client::OwnerClient>),
    Bootstrap {
        lock: crate::runtime_threads::RuntimeProcessOwnerLock,
        binding: Option<crate::runtime_threads::RuntimeStoreBinding>,
        recovery: Option<Arc<codewhale_app_server::daemon_client::UnavailablePublication>>,
    },
}

/// Observe first, then hold the existing store lease through all recovery.
/// Only pre-write publication changes or lease contention can re-observe.
#[cfg(unix)]
async fn admit_runtime_host(
    config_path: Option<PathBuf>,
    selected_socket: Option<PathBuf>,
    selected_store: PathBuf,
) -> Result<RuntimeHostAdmission> {
    use codewhale_app_server::daemon_client::{HostOwnerProbe, PublicationChangedBeforeAttach};
    use codewhale_app_server::daemon_socket::owner_work;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let probe = match codewhale_app_server::daemon_client::probe_owner_for_host_startup(
                config_path.clone(), selected_socket.clone(),
            ).await {
                Err(error) if error.is::<PublicationChangedBeforeAttach>() => {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                    continue;
                }
                result => result?,
            };
            let (binding, recovery) = match probe {
                HostOwnerProbe::Attached(client) => return Ok(RuntimeHostAdmission::Attached(client)),
                HostOwnerProbe::Absent => (None, None),
                HostOwnerProbe::Unavailable(publication) => {
                    let binding = validate_selected_owner_receipt(publication.receipt().clone(), selected_store.clone()).await?;
                    (Some(binding), Some(publication))
                }
            };
            let store = selected_store.clone();
            let existing = binding.clone();
            let create = recovery.is_none();
            let lock = owner_work(move || {
                if let Some(binding) = existing.as_ref() { binding.validate_existing_store()?; }
                crate::runtime_threads::RuntimeProcessOwnerLock::try_acquire_for_host(&store, create)
            }).await?;
            let Some(lock) = lock else {
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            };
            let store = selected_store.clone();
            let existing = binding.clone();
            let lock = owner_work(move || {
                lock.validate_at_root(&store)?;
                if let Some(binding) = existing.as_ref() { binding.validate_existing_store()?; }
                Ok(lock)
            }).await?;
            let current = match recovery.as_ref() {
                Some(publication) => publication.revalidate().await?,
                None => codewhale_app_server::daemon_client::publication_is_absent(
                    config_path.clone(), selected_socket.clone(),
                ).await?,
            };
            if !current {
                drop(lock);
                tokio::time::sleep(Duration::from_millis(25)).await;
                continue;
            }
            return Ok(RuntimeHostAdmission::Bootstrap { lock, binding, recovery });
        }
    }).await.context("Runtime host admission deadline expired; attachment outcome may be uncertain and is not replayed")?
}

/// Prefix of the first line the Runtime prints once it holds its listener.
pub const RUNTIME_LISTENING_PREFIX: &str = "Runtime API listening on http://";

/// Start the runtime API server.
pub async fn run_http_server(
    config: Config,
    workspace: PathBuf,
    plugin_discovery: Arc<crate::plugins::PluginDiscoveryContext>,
    options: RuntimeApiOptions,
) -> Result<()> {
    validate_runtime_listener_security(&options)?;
    let acp_selected = matches!(
        options.control_frontend.as_ref(),
        Some(codewhale_app_server::RuntimeControlFrontend::Acp { .. })
    );

    let task_default_model = runtime_request_model(&config, None).unwrap_or_else(|_| "auto".into());
    let task_cfg = TaskManagerConfig::from_runtime(
        &config,
        workspace.clone(),
        Some(task_default_model.clone()),
        Some(options.workers),
    );
    #[cfg(unix)]
    let admission = admit_runtime_host(
        options.config_path.clone(),
        selected_control_socket(&options),
        RuntimeThreadManagerConfig::from_task_data_dir(task_cfg.data_dir.clone()).data_dir,
    )
    .await?;
    #[cfg(unix)]
    let (published, preheld_lock, existing_binding, recovery) = match admission {
        RuntimeHostAdmission::Attached(client) => (Some(*client), None, None, None),
        RuntimeHostAdmission::Bootstrap {
            lock,
            binding,
            recovery,
        } => (None, Some(lock), binding, recovery),
    };
    #[cfg(windows)]
    let published = codewhale_app_server::daemon_client::connect_if_published(
        options.config_path.clone(),
        selected_control_socket(&options),
    )
    .await?;
    #[cfg(not(unix))]
    let preheld_lock = None;
    #[cfg(not(unix))]
    let existing_binding: Option<crate::runtime_threads::RuntimeStoreBinding> = None;
    #[cfg(windows)]
    let recovery = None;
    #[cfg(any(unix, windows))]
    if let Some(control) = published {
        let selected_store =
            RuntimeThreadManagerConfig::from_task_data_dir(task_cfg.data_dir.clone()).data_dir;
        validate_selected_owner(&control, selected_store).await?;
        let observed_owner = control.receipt().clone();
        let client = match options.control_frontend.as_ref() {
            Some(codewhale_app_server::RuntimeControlFrontend::Acp { model }) => {
                drop(control);
                let client =
                    codewhale_app_server::daemon_client::connect_selected_acp_if_published(
                        options.config_path.clone(),
                        selected_control_socket(&options),
                        codewhale_app_server::RuntimeFrontendScope {
                            workers: options.workers,
                            workspace: workspace.clone(),
                            config_profile: options.config_profile.clone(),
                            config_source: config
                                .loaded_config_path
                                .clone()
                                .or_else(|| options.config_path.clone()),
                        },
                        model.clone(),
                        observed_owner.clone(),
                    )
                    .await?
                    .context("authenticated owner disappeared before ACP attachment")?;
                anyhow::ensure!(
                    client.receipt() == &observed_owner,
                    "selected owner changed before ACP attachment"
                );
                client
            }
            Some(
                codewhale_app_server::RuntimeControlFrontend::Stdio
                | codewhale_app_server::RuntimeControlFrontend::Socket { .. },
            ) => {
                drop(control);
                codewhale_app_server::daemon_client::connect_scoped_control_if_published(
                    options.config_path.clone(),
                    selected_control_socket(&options),
                    codewhale_app_server::RuntimeFrontendScope {
                        workers: options.workers,
                        workspace: workspace.clone(),
                        config_profile: options.config_profile.clone(),
                        config_source: config
                            .loaded_config_path
                            .clone()
                            .or_else(|| options.config_path.clone()),
                    },
                    observed_owner,
                )
                .await?
                .context("authenticated owner disappeared before control attachment")?
            }
            _ => {
                let environment = runtime_token_environment(&|name| std::env::var(name).ok());
                let selection = codewhale_app_server::RuntimeListenerSelection {
                    workers: options.workers,
                    workspace: workspace.clone(),
                    config_profile: options.config_profile.clone(),
                    config_source: config
                        .loaded_config_path
                        .clone()
                        .or_else(|| options.config_path.clone()),
                    host: options.host.clone(),
                    port: options.port,
                    cors_origins: options.cors_origins.clone(),
                    auth_token: options
                        .auth_token
                        .clone()
                        .filter(|token| !token.trim().is_empty())
                        .or(environment.token),
                    insecure_no_auth: options.insecure_no_auth,
                    mobile: options.mobile,
                    web: options.web,
                };
                drop(control);
                codewhale_app_server::daemon_client::connect_listener_if_published(
                    options.config_path.clone(),
                    selected_control_socket(&options),
                    selection,
                    observed_owner,
                )
                .await?
                .context("authenticated owner disappeared before listener attachment")?
            }
        };
        return run_attached_frontend(client, &options).await;
    }
    // No publication permits guessing an owner or bypassing its lease. The
    // real manager open below is still the exclusive bootstrap authority.
    let addr = runtime_bind_address(&options.host, options.port)?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("Failed to bind {addr}"))?;
    let bound_addr = listener
        .local_addr()
        .context("Failed to read Runtime API listener address")?;
    let sessions_dir = default_sessions_dir().unwrap_or_else(|_| fallback_sessions_dir());
    let mut manager_config =
        RuntimeThreadManagerConfig::from_task_data_dir(task_cfg.data_dir.clone());
    manager_config.sessions_dir = Some(sessions_dir.clone());
    let (runtime_threads, _workshop_activation) = open_runtime_threads_for_host(
        &config,
        workspace.clone(),
        manager_config,
        plugin_discovery.registry_for_workspace(&workspace),
        acp_selected,
        preheld_lock,
        existing_binding.as_ref(),
    )?;
    let sessions_dir = runtime_threads.sessions_dir().to_path_buf();
    let task_manager =
        TaskManager::start_with_runtime_manager(task_cfg, config.clone(), runtime_threads.clone())
            .await?;
    let _task_shutdown = task_manager.shutdown_guard();
    let mut automation_service = AutomationManager::default_location()?;
    automation_service.bind_task_manager(&task_manager)?;
    let automations = Arc::new(Mutex::new(automation_service));
    runtime_threads.attach_automation_manager(automations.clone());
    let scheduler_cancel = CancellationToken::new();
    let scheduler_handle = spawn_scheduler(
        automations.clone(),
        task_manager.clone(),
        scheduler_cancel.clone(),
        AutomationSchedulerConfig::default(),
    );

    // Repair the saved-session store once per server start (#6144); this
    // server's own store is open by now, so it reads as in use.
    crate::session_reconcile::spawn_background_reconcile(None);
    let runtime_token_env = runtime_token_environment(&|name| std::env::var(name).ok());
    let runtime_token_alias_warning =
        runtime_token_alias_warning(options.auth_token.as_deref(), &runtime_token_env);
    let resolved_auth = resolve_runtime_auth(
        options.auth_token.clone(),
        runtime_token_env.token,
        options.insecure_no_auth,
    );
    let runtime_token = resolved_auth.token.clone();
    let auth_enabled = runtime_token.is_some();
    let (web, web_bootstrap) = if options.web {
        runtime_token
            .as_ref()
            .context("Codewhale web requires a Runtime authentication token")?;
        let (web, bootstrap) = web::RuntimeWebState::new();
        (Some(web), Some(bootstrap))
    } else {
        (None, None)
    };
    let (mobile, mobile_bootstrap) = if options.mobile && auth_enabled {
        let (mobile, bootstrap) = mobile::RuntimeMobileState::new();
        (Some(mobile), Some(bootstrap))
    } else {
        (None, None)
    };
    let skill_state = SkillStateStore::load_default()
        .context("load persistent Skill activation state for Runtime API")?;
    let sub_agent_manager = runtime_api_sub_agent_manager(&workspace, options.workers);
    let shutdown = RuntimeServerShutdown::default();
    // Opening a thread is every client's first read, and the store can only
    // answer it after one pass over the whole items directory (an item's
    // filename names the item, not its turn). Every open used to pay that pass;
    // here it is paid once, while the server is starting and nobody is waiting
    // for it. See [`RuntimeThreadStore::ensure_item_index`].
    let warm_threads = runtime_threads.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(error) = warm_threads.warm_item_index() {
            tracing::warn!(%error, "thread item index warm-up failed");
        }
    });
    let workspace_scopes =
        RuntimeWorkspaceScopes::new(runtime_threads.clone(), sub_agent_manager.clone());
    let workspace_scope = workspace_scopes.admit(workspace.clone()).await?;
    let state = RuntimeApiState {
        config: Arc::new(parking_lot::RwLock::new(config.clone())),
        workspace,
        plugin_discovery,
        task_manager: task_manager.clone(),
        runtime_threads,
        cors_origins: options.cors_origins.clone(),
        sessions_dir,
        config_path: options.config_path.clone(),
        config_profile: options.config_profile.clone(),
        automations,
        sub_agent_manager,
        runtime_token: runtime_token.clone(),
        skill_state: Arc::new(Mutex::new(skill_state)),
        auth_required: auth_enabled,
        bind_host: options.host.clone(),
        bind_port: bound_addr.port(),
        mobile_enabled: options.mobile,
        mobile,
        web,
        fleet_codewhale_binary: configured_codewhale_binary(),
        workspace_scopes,
        workspace_scope,
        computer: computer_display::ComputerState::from_env(),
        shutdown: shutdown.clone(),
        git_writes: Arc::new(tokio::sync::Mutex::new(())),
        provider_switches: Arc::new(tokio::sync::Mutex::new(())),
        #[cfg(test)]
        compat_stream_test_hook: None,
    };
    #[cfg(any(unix, windows))]
    let (owner_frontend, control_state) = bind_captured_runtime_frontends(
        &state,
        selected_control_socket(&options),
        match options.control_frontend.as_ref() {
            Some(codewhale_app_server::RuntimeControlFrontend::Acp { model }) => model.clone(),
            _ => task_default_model.clone(),
        },
        options.workers,
        resolved_auth.generated,
        recovery,
    )
    .await?;
    let listener_workspace = state.workspace.clone();
    let app = build_router(state);
    #[cfg(any(unix, windows))]
    let app = app.merge(codewhale_app_server::runtime_compatibility_router(
        control_state.clone(),
        &options.cors_origins,
        runtime_token.clone(),
        Some(listener_workspace),
    ));
    let owned_stdio = matches!(
        options.control_frontend.as_ref(),
        Some(
            codewhale_app_server::RuntimeControlFrontend::Stdio
                | codewhale_app_server::RuntimeControlFrontend::Acp { .. }
        )
    );

    if !owned_stdio {
        // First stdout line, flushed: a supervising parent reads the endpoint
        // from here instead of guessing a port (stdout is block-buffered on a pipe).
        println!("{RUNTIME_LISTENING_PREFIX}{bound_addr}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
        for line in runtime_auth_status_lines(&resolved_auth) {
            println!("{line}");
        }
        if let Some(warning) = runtime_token_alias_warning {
            println!("{warning}");
        }
        if options.mobile {
            print_mobile_urls(
                bound_addr,
                auth_enabled,
                resolved_auth.generated,
                options.show_qr,
                mobile_bootstrap.as_deref(),
            );
        }
        if let Some(bootstrap) = web_bootstrap {
            println!("Codewhale web enabled at http://{bound_addr}/");
            let bootstrap_url = web::bootstrap_url(bound_addr, &bootstrap);
            println!(
                "Codewhale web bootstrap (single-use, expires in {} min): {bootstrap_url}",
                web::BOOTSTRAP_TTL.as_secs() / 60
            );
            if let Some(warning) = web_launcher_warning(crate::utils::open_url(&bootstrap_url)) {
                println!("{warning}");
            }
        }
        let is_loopback = is_loopback_bind_host(&options.host);
        if is_loopback {
            println!(
                "Security: this server is local-first. Do not expose it to untrusted networks."
            );
        } else {
            println!(
                "Security: bound to {host}; reachable from any peer that can route to this address.",
                host = options.host
            );
            if !auth_enabled {
                println!(
                    "  WARNING: auth is disabled. Anyone on the network can call /v1/* without authentication."
                );
            }
            println!(
                "  /v1/runtime/info reports bind_host={host:?}, port={port}, auth_required={auth}.",
                host = options.host,
                port = bound_addr.port(),
                auth = auth_enabled,
            );
        }
    }
    let signal_registration = SignalShutdownRegistration::register(&shutdown);
    #[cfg(any(unix, windows))]
    let (serve_result, owner_result) = {
        let owner_handle = owner_frontend.shutdown_handle();
        let mut owner_task = tokio::spawn(owner_frontend.serve());
        let stdio = async {
            if acp_selected {
                codewhale_app_server::run_owned_acp(control_state).await
            } else if owned_stdio {
                codewhale_app_server::run_owned_stdio(control_state).await
            } else {
                std::future::pending::<Result<()>>().await
            }
        };
        tokio::pin!(stdio);
        let serve = serve_runtime_api(listener, app, shutdown.clone());
        tokio::pin!(serve);
        tokio::select! {
            result = &mut serve => {
                owner_handle.trigger();
                let owner = owner_task.await.context("owner control frontend task failed")
                    .and_then(|result| result.map_err(Into::into));
                (result.map_err(|e| anyhow!("Runtime API server error: {e}")), owner)
            }
            result = &mut stdio => {
                shutdown.requested.cancel();
                let served = serve.await.map_err(|error|anyhow!("Runtime API server error: {error}"));
                owner_handle.trigger();
                let owner=owner_task.await.context("owner control frontend task failed").and_then(|result|result.map_err(Into::into));
                (result.and(served),owner)
            }
            owner = &mut owner_task => {
                shutdown.requested.cancel();
                let result = serve.await.map_err(|e| anyhow!("Runtime API server error: {e}"));
                let owner = owner.context("owner control frontend task failed")
                    .and_then(|result| result.map_err(Into::into));
                (result, owner.and_then(|()| Err(anyhow!("owner control frontend stopped before its Runtime host"))))
            }
        }
    };
    #[cfg(not(any(unix, windows)))]
    let serve_result = serve_runtime_api(listener, app, shutdown)
        .await
        .map_err(|e| anyhow!("Runtime API server error: {e}"));
    drop(signal_registration);
    scheduler_cancel.cancel();
    scheduler_handle.abort();
    task_manager.shutdown_and_wait().await?;
    #[cfg(any(unix, windows))]
    owner_result?;
    serve_result
}

fn selected_control_socket(options: &RuntimeApiOptions) -> Option<PathBuf> {
    match options.control_frontend.as_ref() {
        Some(codewhale_app_server::RuntimeControlFrontend::Socket { path }) => path.clone(),
        _ => None,
    }
}

#[cfg(any(unix, windows))]
pub(crate) async fn validate_selected_owner(
    client: &codewhale_app_server::daemon_client::OwnerClient,
    selected_store: PathBuf,
) -> Result<()> {
    validate_selected_owner_receipt(client.receipt().clone(), selected_store)
        .await
        .map(|_| ())
}

#[cfg(any(unix, windows))]
async fn validate_selected_owner_receipt(
    receipt: codewhale_protocol::RuntimeOwnerReceipt,
    selected_store: PathBuf,
) -> Result<crate::runtime_threads::RuntimeStoreBinding> {
    codewhale_app_server::daemon_socket::owner_work(move || {
        let selected = crate::runtime_threads::RuntimeStoreBinding::for_store_dir(&selected_store)?;
        selected.validate_existing_store()?;
        anyhow::ensure!(
            selected.data_dir == receipt.data_dir
                && !selected.execution_scope.is_empty()
                && selected.execution_scope == receipt.execution_scope,
            "authenticated Runtime owner belongs to another selected store; refusing attachment"
        );
        Ok(selected)
    })
    .await
}

#[cfg(any(unix, windows))]
async fn bind_captured_runtime_frontends(
    state: &RuntimeApiState,
    selected_socket: Option<PathBuf>,
    model: String,
    worker_setting: usize,
    generated_auth: bool,
    recovery: Option<Arc<codewhale_app_server::daemon_client::UnavailablePublication>>,
) -> Result<(
    codewhale_app_server::daemon_socket::DaemonSocket,
    codewhale_app_server::AppState,
)> {
    let captured_manager = state.runtime_threads.clone();
    let (binding, generation) = codewhale_app_server::daemon_socket::owner_work(move || {
        captured_manager.capture_control_owner()
    })
    .await?;
    let config_path = state.config_path.clone();
    let socket_path =
        codewhale_app_server::daemon_socket::owner_work(move || match selected_socket {
            Some(path) => Ok(path),
            None => codewhale_app_server::daemon_socket::default_socket_path().map_err(Into::into),
        })
        .await?;
    let process_start =
        codewhale_app_server::daemon_socket::capture_process_start(std::process::id()).await?;
    #[cfg(unix)]
    let principal =
        codewhale_config::private_directory::PrivateDirectory::current_user_id().to_string();
    #[cfg(windows)]
    let principal = codewhale_app_server::daemon_socket::owner_work(|| {
        codewhale_config::windows_identity::CurrentWindowsUser::open()?.sid_string()
    })
    .await?;
    let owner = codewhale_protocol::RuntimeOwnerReceipt {
        version: 1,
        data_dir: binding.data_dir,
        execution_scope: binding.execution_scope,
        lease_generation: generation,
        pid: std::process::id(),
        process_start,
        principal,
        socket_path,
        config_path: config_path.clone(),
    };
    codewhale_app_server::bind_runtime_frontends(
        config_path,
        state.runtime_token.clone(),
        owner,
        codewhale_app_server::RuntimeOwnerRouting {
            workers: Some(worker_setting),
            workspace: Some(state.workspace.clone()),
            endpoint: runtime_bind_address(&state.bind_host, state.bind_port)?,
            mobile: state.mobile.is_some(),
            web: state.web.is_some(),
            acp: true,
            acp_only: state.runtime_threads.is_acp_host(),
        },
        Some(
            CapturedRuntimeFrontend::capture(state.clone(), model, worker_setting, generated_auth)
                .await?,
        ),
        recovery,
    )
    .await
}

#[cfg(any(unix, windows))]
async fn run_attached_frontend(
    mut client: codewhale_app_server::daemon_client::OwnerClient,
    options: &RuntimeApiOptions,
) -> Result<()> {
    let routing = client
        .routing()
        .context("authenticated owner has no selected frontend facts")?;
    let acp_selected = matches!(
        options.control_frontend.as_ref(),
        Some(codewhale_app_server::RuntimeControlFrontend::Acp { .. })
    );
    anyhow::ensure!(
        (!acp_selected || routing.acp) && (acp_selected || !routing.acp_only),
        "selected owner cannot admit this frontend within its captured base profile"
    );
    if matches!(
        options.control_frontend.as_ref(),
        Some(
            codewhale_app_server::RuntimeControlFrontend::Stdio
                | codewhale_app_server::RuntimeControlFrontend::Acp { .. }
        )
    ) {
        return client
            .forward(tokio::io::stdin(), tokio::io::stdout())
            .await;
    }
    if matches!(
        options.control_frontend.as_ref(),
        Some(codewhale_app_server::RuntimeControlFrontend::Socket { .. })
    ) {
        println!(
            "Attached to the authenticated Runtime control owner at {}.",
            routing.endpoint
        );
    } else {
        let frame = tokio::time::timeout(Duration::from_secs(10), client.recv())
            .await
            .context("selected listener readiness deadline expired; attachment outcome uncertain")??
            .context("owner closed before selected listener readiness")?;
        anyhow::ensure!(
            frame["jsonrpc"] == "2.0"
                && frame["method"] == "daemon/frontend_ready"
                && frame.get("id").is_none(),
            "invalid selected listener readiness response"
        );
        let ready: RuntimeFrontendReady = serde_json::from_value(frame["params"].clone())?;
        println!("{RUNTIME_LISTENING_PREFIX}{}", ready.endpoint);
        if ready.generated_auth {
            println!("Runtime authentication enabled; generated bearer is not printed.");
        }
        if let Some(url) = ready.web_bootstrap_url {
            println!("Codewhale web: {url}");
        }
        if let Some(url) = ready.mobile_bootstrap_url {
            println!("Codewhale mobile: {url}");
        }
    }

    // This local guest owns its connection only. A signal detaches; it never
    // sends the host shutdown request or replays an uncertain operation.
    tokio::select! {
        result=async {while client.recv().await?.is_some() {} Ok::<(), anyhow::Error>(())}=>result,
        result=tokio::signal::ctrl_c()=>result.map_err(Into::into),
    }
}

/// Mobile control uses plain HTTP only on loopback. It has no TLS or verified
/// overlay transport, so a non-loopback listener would expose the Runtime API
/// to peers that can observe or replay browser traffic.
fn validate_runtime_listener_security(options: &RuntimeApiOptions) -> Result<()> {
    if matches!(
        options.control_frontend.as_ref(),
        Some(codewhale_app_server::RuntimeControlFrontend::LegacyHttp)
    ) && !is_loopback_bind_host(&options.host)
        && options
            .auth_token
            .as_ref()
            .is_none_or(|token| token.trim().is_empty())
    {
        bail!("refusing non-loopback compatibility bind without explicit auth token");
    }
    if matches!(
        options.control_frontend.as_ref(),
        Some(
            codewhale_app_server::RuntimeControlFrontend::Stdio
                | codewhale_app_server::RuntimeControlFrontend::Socket { .. }
                | codewhale_app_server::RuntimeControlFrontend::Acp { .. }
        )
    ) && !is_loopback_bind_host(&options.host)
    {
        bail!("owned local control requires a loopback private Runtime listener");
    }
    // Port 0 asks the kernel for an ephemeral port. Only a plain loopback
    // Runtime may use it: web and mobile clients are given a fixed endpoint.
    if options.port == 0 && (options.web || options.mobile || !is_loopback_bind_host(&options.host))
    {
        bail!("Port must be > 0");
    }
    if options.web && options.host != "127.0.0.1" {
        bail!("Codewhale web is loopback-only and must bind to 127.0.0.1");
    }
    if options.web && options.insecure_no_auth {
        bail!("Codewhale web requires Runtime authentication; remove --insecure");
    }
    if options.mobile && !is_loopback_bind_host(&options.host) {
        bail!(
            "Codewhale mobile is loopback-only without TLS or a verified overlay; bind to 127.0.0.1 or ::1"
        );
    }
    if options.insecure_no_auth && !is_loopback_bind_host(&options.host) {
        bail!(
            "Unauthenticated Runtime access is loopback-only; remove --insecure or bind to 127.0.0.1 or ::1"
        );
    }
    Ok(())
}

fn is_loopback_bind_host(host: &str) -> bool {
    host.parse::<IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn runtime_bind_address(host: &str, port: u16) -> Result<SocketAddr> {
    let address = match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(_)) => format!("[{host}]:{port}"),
        _ => format!("{host}:{port}"),
    };
    address
        .parse()
        .with_context(|| format!("Invalid bind address '{host}:{port}'"))
}

fn web_launcher_warning(result: Result<()>) -> Option<String> {
    result.err().map(|error| {
        format!(
            "warning: could not open the default browser ({error}); open the bootstrap URL above manually"
        )
    })
}

fn fallback_sessions_dir() -> PathBuf {
    if let Some(home) = codewhale_paths::codewhale_home_override().ok().flatten() {
        return home.join("sessions");
    }
    codewhale_paths::legacy_deepseek_home()
        .unwrap_or_else(|| PathBuf::from(codewhale_paths::LEGACY_APP_DIR))
        .join("sessions")
}

pub fn build_router(state: RuntimeApiState) -> Router {
    diagnostics::mark_server_started();
    let api_routes = Router::new()
        .route(
            "/v1/sessions",
            get(list_sessions)
                .post(create_session_from_thread)
                .put(save_current_session),
        )
        .route("/v1/sessions/summary", get(list_sessions_summary))
        .route("/v1/sessions/repair", get(get_session_repair))
        .route(
            "/v1/sessions/{id}",
            get(get_session).patch(patch_session).delete(delete_session),
        )
        .route(
            "/v1/sessions/{id}/resume-thread",
            post(resume_session_thread),
        )
        .route("/v1/sessions/{id}/artifacts", get(list_session_artifacts))
        .route(
            "/v1/sessions/{id}/artifacts/{artifact_id}",
            get(read_session_artifact),
        )
        .route("/v1/workspace/status", get(workspace_status))
        // The Engine's terminal byte stream (#34). Auth is the route layer's,
        // not this module's; these never create a session — see terminal.rs.
        .route("/v1/terminal/{name}/output", get(terminal::terminal_output))
        .route("/v1/terminal/{name}/input", post(terminal::terminal_input))
        .route(
            "/v1/terminal/{name}/resize",
            post(terminal::terminal_resize),
        )
        .route("/v1/terminal/{name}/kill", post(terminal::terminal_kill))
        .route("/v1/workspace/files/search", get(workspace_file_search))
        .route(
            "/v1/workspace/files",
            get(workspace_files_list)
                .put(workspace_file_write)
                .layer(DefaultBodyLimit::max(
                    self::workspace::FILE_WRITE_BODY_LIMIT_BYTES,
                )),
        )
        .route("/v1/workspace/files/read", get(workspace_file_read))
        .route("/v1/workspace/instructions", get(workspace_instructions))
        .route("/v1/agent-runs", get(list_agent_runs))
        .route("/v1/agent-runs/{run_id}", get(get_agent_run))
        .route("/v1/agent-runs/{run_id}/cancel", post(cancel_agent_run))
        .route("/v1/fleet/profiles", get(list_fleet_profiles))
        .route(
            "/v1/fleet/runs",
            get(list_fleet_runs).post(create_fleet_run),
        )
        .route("/v1/fleet/runs/{run_id}", get(get_fleet_run))
        .route(
            "/v1/fleet/runs/{run_id}/workers",
            get(list_fleet_run_workers),
        )
        .route("/v1/fleet/runs/{run_id}/start", post(start_fleet_run))
        .route("/v1/fleet/runs/{run_id}/events", get(stream_fleet_events))
        .route(
            "/v1/fleet/runs/{run_id}/events/replay",
            get(replay_fleet_events),
        )
        .route("/v1/fleet/runs/{run_id}/stop", post(stop_fleet_run))
        .route(
            "/v1/fleet/runs/{run_id}/receipts",
            get(list_fleet_run_receipts),
        )
        .route(
            "/v1/fleet/runs/{run_id}/receipts/{task_id}",
            get(get_fleet_run_receipt),
        )
        .route(
            "/v1/fleet/runs/{run_id}/receipts/{task_id}/evidence",
            get(inspect_fleet_run_receipt_evidence),
        )
        .route("/v1/fleet/workers/{worker_id}", get(get_fleet_worker))
        .route(
            "/v1/fleet/workers/{worker_id}/interrupt",
            post(interrupt_fleet_worker),
        )
        .route(
            "/v1/fleet/workers/{worker_id}/stop",
            post(stop_fleet_worker),
        )
        .route(
            "/v1/fleet/workers/{worker_id}/restart",
            post(restart_fleet_worker),
        )
        .route(
            "/v1/stream",
            post(stream_turn).layer(DefaultBodyLimit::max(
                codewhale_protocol::runtime::MAX_RUNTIME_IMAGE_BODY_BYTES,
            )),
        )
        .route("/v1/git", get(git::git_status_detail))
        .route("/v1/changes", get(git::git_changes))
        .route("/v1/diff", get(git::git_diff))
        .route("/v1/workspace/diff", get(git::workspace_diff))
        .route("/v1/git/graph", get(git::git_graph))
        .route("/v1/git/stage", post(git::git_stage))
        .route("/v1/git/unstage", post(git::git_unstage))
        .route("/v1/git/discard", post(git::git_discard))
        .route("/v1/git/commit", post(git::git_commit))
        .route("/v1/git/push", post(git::git_push))
        .route("/v1/git/branch", post(git::git_branch))
        .route("/v1/logs", get(diagnostics::list_logs))
        .route("/v1/logs/{name}", get(diagnostics::read_log))
        .route("/v1/crashes", get(diagnostics::list_crashes))
        .route("/v1/crashes/{name}", get(diagnostics::read_crash))
        .route("/v1/process", get(diagnostics::process_info))
        .route("/v1/jobs", get(jobs::list_jobs))
        .route("/v1/threads", get(list_threads).post(create_thread))
        .route("/v1/threads/summary", get(list_threads_summary))
        .route("/v1/threads/running", get(list_running_threads))
        .route("/v1/threads/{id}/notices", get(list_thread_notices))
        .route(
            "/v1/threads/{id}/notices/{notice_id}",
            delete(ack_thread_notice),
        )
        .route("/v1/threads/{id}", get(get_thread).patch(update_thread))
        .route(
            "/v1/threads/{id}/history",
            get(thread_history::snapshot_thread_history),
        )
        .route(
            "/v1/thread-history/operations/lookup",
            post(thread_history::lookup_thread_history_operation),
        )
        .route(
            "/v1/thread-history/operations/recover",
            post(thread_history::recover_thread_history_operation),
        )
        .route(
            "/v1/thread-history/mutate",
            post(thread_history::mutate_thread_history),
        )
        .route(
            "/v1/thread-history/import",
            post(thread_history::import_thread_history),
        )
        .route(
            "/v1/threads/{id}/jobs",
            get(jobs::list_thread_jobs).post(jobs::create_thread_job),
        )
        .route("/v1/threads/{id}/jobs/{job_id}", get(jobs::get_thread_job))
        .route(
            "/v1/threads/{id}/jobs/{job_id}/output",
            get(jobs::get_thread_job_output),
        )
        .route(
            "/v1/threads/{id}/jobs/{job_id}/stdin",
            post(jobs::write_thread_job_stdin),
        )
        .route(
            "/v1/threads/{id}/jobs/{job_id}/kill",
            post(jobs::kill_thread_job),
        )
        .route(
            "/v1/threads/{id}/jobs/{job_id}/resize",
            post(jobs::resize_thread_job),
        )
        .route("/v1/threads/{id}/context", get(context::get_thread_context))
        .route("/v1/threads/{id}/plan", get(plans::get_thread_plan))
        .route("/v1/threads/{id}/todo", get(plans::get_thread_todo))
        .route("/v1/plan", get(plans::latest_plan))
        .route("/v1/todo", get(plans::latest_todo_route))
        .route("/v1/plans", get(plans::list_plans))
        .route("/v1/todos", get(plans::list_todos))
        .route(
            "/v1/targets",
            get(targets::list_targets).post(targets::create_target),
        )
        .route("/v1/targets/switch", post(targets::switch_target))
        .route("/v1/remote", get(targets::remote_status))
        .route("/v1/remote/connect", post(targets::remote_connect))
        .route(
            "/v1/ssh",
            get(targets::ssh_status).post(targets::ssh_connect),
        )
        .route("/v1/ssh/connect", post(targets::ssh_connect))
        .route(
            "/v1/cloud",
            get(targets::cloud_status).post(targets::cloud_attach),
        )
        .route("/v1/cloud/attach", post(targets::cloud_attach))
        .route("/v1/lsp", get(lsp::lsp_status))
        .route("/v1/diagnostics", get(lsp::lsp_diagnostics))
        .route("/v1/definition", get(lsp::lsp_definition))
        .route("/v1/references", get(lsp::lsp_references))
        .route("/v1/symbols", get(lsp::lsp_symbols))
        .route("/v1/voice", get(voice::voice_status))
        .route("/v1/voice/dictate", post(voice::voice_dictate))
        .route("/v1/voice/send", post(voice::voice_send))
        .route("/v1/voice/control", post(voice::voice_control))
        .route("/v1/threads/{id}/resume", post(resume_thread))
        .route("/v1/threads/{id}/fork", post(fork_thread))
        .route("/v1/threads/{id}/undo", post(undo_thread_turn))
        .route("/v1/threads/{id}/fork-at-turn", post(fork_thread_at_turn))
        .route("/v1/threads/{id}/patch-undo", post(patch_undo_thread_turn))
        .route("/v1/threads/{id}/file-revert", post(revert_thread_file))
        .route("/v1/threads/{id}/retry", post(retry_thread_turn))
        .route(
            "/v1/threads/{id}/turn-operations/{operation_key}",
            get(get_thread_turn_operation),
        )
        .route(
            "/v1/threads/{id}/turns",
            post(start_thread_turn).layer(DefaultBodyLimit::max(
                codewhale_protocol::runtime::MAX_RUNTIME_IMAGE_BODY_BYTES,
            )),
        )
        .route(
            "/v1/threads/{id}/turns/{turn_id}/steer",
            post(steer_thread_turn),
        )
        .route(
            "/v1/threads/{id}/turns/{turn_id}/artifacts",
            get(turn_artifacts::list_turn_artifacts),
        )
        .route(
            "/v1/threads/{id}/turns/{turn_id}/artifacts/{artifact_id}",
            get(turn_artifacts::read_turn_artifact),
        )
        // What one tool call changed, from the workspace restore points the
        // engine recorded around it. A shell command's writes belong to the
        // command here, not only to the turn (see `turn_artifacts`).
        .route(
            "/v1/threads/{id}/turns/{turn_id}/calls/{tool_call_id}/changes",
            get(turn_artifacts::list_call_changes),
        )
        .route(
            "/v1/threads/{id}/turns/{turn_id}/interrupt",
            post(interrupt_thread_turn),
        )
        .route(
            "/v1/threads/{id}/turns/{turn_id}/tool-calls/{call_id}/result",
            post(deliver_dynamic_tool_result),
        )
        .route("/v1/threads/{id}/compact", post(compact_thread))
        .route("/v1/threads/{id}/usage", get(get_thread_usage))
        .route("/v1/threads/{id}/receipt", get(get_thread_receipt))
        .route(
            "/v1/threads/{id}/turns/{turn_id}/receipt",
            get(get_turn_receipt),
        )
        .route("/v1/threads/{id}/events", get(stream_thread_events))
        .route("/v1/agent-mail", post(send_agent_mail))
        .route("/v1/threads/{id}/agent-mail", get(list_agent_mail))
        .route(
            "/v1/threads/{id}/agent-mail/{message_id}/deliver",
            post(deliver_agent_mail),
        )
        .route(
            "/v1/threads/{id}/agent-mail/{message_id}/read",
            post(mark_agent_mail_read),
        )
        .route(
            "/v1/threads/{id}/agent-mail/{message_id}/cancel",
            post(cancel_agent_mail),
        )
        .route(
            "/v1/threads/{id}/goal",
            get(get_thread_goal)
                .put(upsert_thread_goal)
                .delete(delete_thread_goal),
        )
        .route("/v1/threads/{id}/goal/complete", post(complete_thread_goal))
        .route("/v1/threads/{id}/goal/block", post(block_thread_goal))
        .route("/v1/goals", get(list_goals))
        .route("/v1/approvals", get(list_approvals))
        .route("/v1/approvals/{approval_id}", post(decide_approval))
        .route(
            "/v1/threads/{id}/approval-grants/{grant_id}",
            delete(revoke_approval_grant),
        )
        .route(
            "/v1/user-input/{thread_id}/{input_id}",
            post(submit_user_input),
        )
        .route("/v1/tasks", get(list_tasks).post(create_task))
        .route("/v1/tasks/{id}", get(get_task))
        .route("/v1/tasks/{id}/cancel", post(cancel_task))
        .route("/v1/skills", get(list_skills))
        .route("/v1/commands", get(list_commands))
        .route("/v1/hooks", get(list_hooks))
        .route(
            "/v1/skills/{name}",
            post(set_skill_enabled)
                .get(get_skill_detail)
                .delete(uninstall_skill_api),
        )
        .route(
            "/v1/apps/mcp/imports",
            get(mcp_import::preview).post(mcp_import::apply),
        )
        .route(
            "/v1/apps/mcp/servers",
            get(list_mcp_servers).post(create_mcp_server),
        )
        .route(
            "/v1/apps/mcp/servers/{name}",
            get(get_mcp_server)
                .patch(update_mcp_server)
                .delete(delete_mcp_server),
        )
        .route(
            "/v1/apps/mcp/servers/{name}/enable",
            post(enable_mcp_server),
        )
        .route(
            "/v1/apps/mcp/servers/{name}/disable",
            post(disable_mcp_server),
        )
        .route(
            "/v1/apps/mcp/servers/{name}/reconnect",
            post(reconnect_mcp_server),
        )
        .route("/v1/skills/install", post(install_skill_api))
        .route("/v1/skills/{name}/update", post(update_skill_api))
        .route("/v1/skills/{name}/trust", post(trust_skill_api))
        .route("/v1/skills/{name}/audit", get(audit_skill_api))
        .route("/v1/apps/mcp/tools", get(list_mcp_tools))
        .route("/v1/apps/plugins", get(plugins::list_plugins))
        .route("/v1/apps/avatars", get(avatars::list_avatars))
        .route(
            "/v1/apps/avatars/{handle}/atlases/{page}",
            get(avatars::get_atlas),
        )
        .route(
            "/v1/apps/plugins/install",
            post(plugins::install_plugin_api),
        )
        .route(
            "/v1/apps/plugins/import/dsh/preview",
            post(plugins::preview_dsh_plugin_api),
        )
        .route(
            "/v1/apps/plugins/{selector}",
            get(plugins::get_plugin).delete(plugins::uninstall_plugin_api),
        )
        .route(
            "/v1/apps/plugins/{selector}/update",
            post(plugins::update_plugin_api),
        )
        .route(
            "/v1/apps/plugins/{selector}/trust",
            post(plugins::trust_plugin_api),
        )
        .route(
            "/v1/apps/plugins/{selector}/enable",
            post(plugins::enable_plugin_api),
        )
        .route(
            "/v1/apps/plugins/{selector}/disable",
            post(plugins::disable_plugin_api),
        )
        .route(
            "/v1/apps/plugins/{selector}/revoke",
            post(plugins::revoke_plugin_api),
        )
        .route(
            "/v1/apps/marketplaces",
            get(plugins::list_marketplaces).post(plugins::add_marketplace),
        )
        .route(
            "/v1/apps/marketplaces/{name}",
            get(plugins::get_marketplace).delete(plugins::remove_marketplace),
        )
        .route(
            "/v1/apps/marketplaces/{name}/install",
            post(plugins::install_marketplace_candidate_api),
        )
        .route(
            "/v1/automations",
            get(list_automations).post(create_automation),
        )
        .route(
            "/v1/automations/{id}",
            get(get_automation)
                .patch(update_automation)
                .delete(delete_automation),
        )
        .route("/v1/automations/{id}/run", post(run_automation))
        .route("/v1/automations/{id}/pause", post(pause_automation))
        .route("/v1/automations/{id}/resume", post(resume_automation))
        .route("/v1/automations/{id}/runs", get(list_automation_runs))
        .route(
            "/v1/operate",
            get(get_operate).post(start_operate).patch(patch_operate),
        )
        .route("/v1/operate/keepalive", post(keepalive_operate))
        .route("/v1/operate/plan", put(put_operate_plan))
        .route("/v1/operate/cancel", post(cancel_operate))
        .route("/v1/operate/stop", post(cancel_operate))
        .route(
            "/v1/operate/auto-merge/check",
            post(check_operate_auto_merge),
        )
        .route("/v1/usage", get(get_usage))
        .route("/v1/snapshots", get(list_snapshots))
        .route("/v1/snapshots/{id}/restore", post(restore_snapshot))
        .route(
            "/v1/account/model-access",
            get(secrets::get_account_model_access)
                .put(secrets::set_account_model_access)
                .delete(secrets::clear_account_model_access)
                .layer(DefaultBodyLimit::max(
                    secrets::PROVIDER_KEY_BODY_LIMIT_BYTES,
                )),
        )
        .route("/v1/providers", get(list_providers))
        .route("/v1/providers/{id}/models", get(list_provider_models))
        .route(
            "/v1/providers/{id}/models/refresh",
            post(refresh_provider_models),
        )
        .route("/v1/providers/{id}/switch", post(switch_provider))
        .route(
            "/v1/providers/{id}/key",
            put(secrets::set_provider_key)
                .delete(secrets::clear_provider_key)
                .layer(DefaultBodyLimit::max(
                    secrets::PROVIDER_KEY_BODY_LIMIT_BYTES,
                )),
        )
        .route("/v1/config", get(get_config).post(set_config))
        .route("/v1/config/reload", post(reload_config))
        .route("/v1/constitution", get(constitution::get_constitution))
        .route(
            "/v1/constitution/preview",
            post(constitution::preview_constitution).layer(DefaultBodyLimit::max(32 * 1024)),
        )
        .route("/v1/settings/schema", get(get_settings_schema))
        .route(
            "/v1/threads/{id}/notifications/prepare",
            post(notification_delivery::prepare),
        )
        .route(
            "/v1/memory",
            get(list_memory)
                .post(create_memory_entry)
                .delete(clear_memory),
        )
        .route("/v1/memory/{id}", get(get_memory_entry))
        .merge(memory_lens::routes())
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_workspace_scope,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_runtime_token,
        ));

    Router::new()
        .route("/", get(web::web_page))
        .route("/assets/codewhale-web.css", get(web::web_styles))
        .route("/assets/codewhale-web.js", get(web::web_script))
        .route("/assets/codewhale-192.png", get(web::web_icon))
        .route(
            "/__codewhale/bootstrap/{nonce}",
            get(web::exchange_bootstrap),
        )
        .route(
            "/__codewhale/web/stream-ticket",
            post(web::refresh_stream_ticket),
        )
        .route(
            "/__codewhale/mobile/bootstrap/{nonce}",
            get(exchange_mobile_bootstrap),
        )
        .route("/__codewhale/mobile/session", post(exchange_mobile_session))
        .route(
            "/__codewhale/mobile/stream-ticket",
            post(refresh_mobile_stream_ticket),
        )
        .route("/health", get(health))
        .route("/mobile", get(mobile_page))
        .route("/mobile/", get(mobile_page))
        // Intentionally unauthenticated: loopback clients discover the
        // listener here. The response already redacts account detail for
        // unauthorized requests; it never mutates state.
        .route("/v1/runtime/info", get(runtime_info))
        // Authenticates per handler: the display WS also takes a single-use
        // ticket, and client-token minting is master-token only.
        .merge(computer_display::router(
            state.computer.clone(),
            state.runtime_token.clone(),
        ))
        .merge(api_routes)
        .layer(cors_layer(&state.cors_origins))
        .with_state(state)
}

async fn mobile_page(State(state): State<RuntimeApiState>) -> Result<Response, ApiError> {
    if !state.mobile_enabled {
        return Ok((
            StatusCode::NOT_FOUND,
            "mobile control is disabled; start with `codewhale serve --mobile`",
        )
            .into_response());
    }
    let settings = tokio::task::spawn_blocking(crate::settings::Settings::load_read_only)
        .await
        .map_err(|err| ApiError::internal(format!("mobile settings task failed: {err}")))?
        .map_err(|err| ApiError::internal(format!("mobile settings unavailable: {err}")))?;
    let locale = codewhale_localization::resolve_locale(&settings.locale);
    let mut response = Html(mobile_html(locale)).into_response();
    secure_mobile_response(&mut response);
    Ok(response)
}

#[derive(Serialize)]
struct MobileSessionResponse {
    request_proof: String,
    stream_ticket: String,
    session_expires_in_seconds: u64,
    stream_ticket_expires_in_seconds: u64,
}

async fn exchange_mobile_bootstrap(
    State(state): State<RuntimeApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(nonce): Path<String>,
) -> Response {
    let Some(mobile_state) = state.mobile.as_ref() else {
        return mobile_not_found();
    };
    let session = match mobile_state.consume_bootstrap(&nonce, peer.ip()) {
        Ok(session) => session,
        Err(mobile::BootstrapError::NonLoopback) => {
            return secured_mobile_text(StatusCode::FORBIDDEN, "bootstrap unavailable");
        }
        Err(mobile::BootstrapError::Invalid | mobile::BootstrapError::Expired) => {
            return secured_mobile_text(StatusCode::UNAUTHORIZED, "bootstrap unavailable");
        }
    };

    let location = format!(
        "/mobile#request_proof={}&stream_ticket={}",
        session.request_proof, session.stream_ticket
    );
    let cookie = mobile::mobile_session_cookie(&session.session_cookie);
    let mut response = (StatusCode::SEE_OTHER, "").into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location).expect("generated mobile fragment is a valid header"),
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("generated mobile cookie is a valid header"),
    );
    secure_mobile_response(&mut response);
    response
}

async fn exchange_mobile_session(State(state): State<RuntimeApiState>, req: Request) -> Response {
    let Some(mobile_state) = state.mobile.as_ref() else {
        return mobile_not_found();
    };
    let Some(expected) = state.runtime_token.as_deref() else {
        return mobile_not_found();
    };
    if !auth::request_has_header_runtime_token(&req, expected) {
        return mobile_unauthorized();
    }
    mobile_session_response(mobile_state.issue_session())
}

async fn refresh_mobile_stream_ticket(
    State(state): State<RuntimeApiState>,
    req: Request,
) -> Response {
    let Some(mobile_state) = state.mobile.as_ref() else {
        return mobile_not_found();
    };
    if !auth::mobile_session_request_is_authorized(&req, &state, mobile_state) {
        return mobile_unauthorized();
    }
    let ticket = mobile_state.refresh_stream_ticket(
        req.headers()
            .get(header::COOKIE)
            .and_then(|value| value.to_str().ok()),
        req.headers()
            .get(mobile::MOBILE_REQUEST_HEADER)
            .and_then(|value| value.to_str().ok()),
    );
    let Some(ticket) = ticket else {
        return mobile_unauthorized();
    };
    let mut response = Json(json!({
        "stream_ticket": ticket.ticket,
        "expires_in_seconds": ticket.expires_in_seconds,
    }))
    .into_response();
    secure_mobile_response(&mut response);
    response
}

fn mobile_session_response(session: mobile::MobileSessionBootstrap) -> Response {
    let cookie = mobile::mobile_session_cookie(&session.session_cookie);
    let mut response = Json(MobileSessionResponse {
        request_proof: session.request_proof,
        stream_ticket: session.stream_ticket,
        session_expires_in_seconds: session.session_ttl_seconds,
        stream_ticket_expires_in_seconds: session.stream_ticket_ttl_seconds,
    })
    .into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("generated mobile cookie is a valid header"),
    );
    secure_mobile_response(&mut response);
    response
}

fn mobile_not_found() -> Response {
    secured_mobile_text(StatusCode::NOT_FOUND, "not found")
}

fn mobile_unauthorized() -> Response {
    let mut response = auth::runtime_token_required_response();
    secure_mobile_response(&mut response);
    response
}

fn secured_mobile_text(status: StatusCode, body: &'static str) -> Response {
    let mut response = (status, body).into_response();
    secure_mobile_response(&mut response);
    response
}

fn secure_mobile_response(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'",
        ),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
}

fn print_mobile_urls(
    addr: SocketAddr,
    auth_enabled: bool,
    generated_auth: bool,
    show_qr: bool,
    mobile_bootstrap: Option<&str>,
) {
    println!("Mobile control page enabled.");

    let url = format!("http://{addr}/mobile");
    println!("  URL:   {url}");
    if auth_enabled {
        if let Some(bootstrap) = mobile_bootstrap {
            let bootstrap_url = mobile::bootstrap_url(addr, bootstrap);
            println!(
                "  Bootstrap (single-use, expires in {} min): {bootstrap_url}",
                mobile::BOOTSTRAP_TTL.as_secs() / 60
            );
        } else if generated_auth {
            println!(
                "  Auth uses an unprinted generated token; open the bootstrap URL printed above."
            );
        } else {
            println!(
                "  Use the bootstrap URL; the page also supports one-time bearer entry without storing it."
            );
        }
    }
    println!(
        "Mobile security: loopback-only; no LAN/VPN device access without a verified transport boundary."
    );

    if show_qr {
        println!("  QR is loopback-only and cannot pair another device.");
        match qrcode::QrCode::new(url.as_bytes()) {
            Ok(qr) => {
                let qr_str = qr.render::<qrcode::render::unicode::Dense1x2>().build();
                println!("\n{qr_str}");
            }
            Err(e) => {
                eprintln!("Warning: could not generate QR code: {e}");
            }
        }
    }
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "codewhale-runtime-api",
        mode: "local",
    })
}

fn runtime_request_model(config: &Config, requested: Option<&str>) -> Result<String, ApiError> {
    if let Some(model) = requested {
        return Ok(model.to_string());
    }
    let identity = config
        .active_provider_identity()
        .map_err(ApiError::bad_request)?;
    let model = provider_default_model_for_api(config, &identity);
    if model.is_empty() {
        return Err(ApiError::bad_request(
            "The active provider has no available default model; refresh its catalog or select an explicit model.",
        ));
    }
    Ok(model)
}

async fn create_task(
    State(state): State<RuntimeApiState>,
    Json(mut req): Json<NewTaskRequest>,
) -> Result<(StatusCode, Json<TaskRecord>), ApiError> {
    if req.prompt.trim().is_empty() {
        return Err(ApiError::bad_request("prompt is required"));
    }
    if req.workspace.is_none() {
        req.workspace = Some(state.workspace.clone());
    }
    if req.model.is_none() && req.model_provider.is_none() && req.model_provider_id.is_none() {
        req.model = Some(runtime_request_model(&state.config.read(), None)?);
    }
    let task = state
        .task_manager
        .add_task(req)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok((StatusCode::CREATED, Json(task)))
}

async fn create_thread(
    State(state): State<RuntimeApiState>,
    Json(mut req): Json<CreateThreadRequest>,
) -> Result<(StatusCode, Json<ThreadRecord>), ApiError> {
    if req.workspace.is_none() {
        req.workspace = Some(state.workspace.clone());
    }
    if req.mode.as_ref().is_none_or(|m| m.trim().is_empty()) {
        req.mode = Some("agent".to_string());
    }

    let thread = state
        .runtime_threads
        .create_thread_with_shell_policy(
            req,
            state.config_path.as_deref(),
            state.config_profile.as_deref(),
        )
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok((StatusCode::CREATED, Json(thread)))
}

async fn list_threads(
    State(state): State<RuntimeApiState>,
    Query(query): Query<ThreadsQuery>,
) -> Result<Json<Vec<ThreadRecord>>, ApiError> {
    let filter = resolve_thread_filter(query.include_archived, query.archived_only);
    let threads = state
        .runtime_threads
        .list_threads(filter, query.limit)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(threads))
}

/// Threads with queued or in-progress turns, for quit/background
/// accounting (#6180). One call, no inference from latest-turn status.
async fn list_running_threads(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Vec<crate::runtime_threads::RunningThread>>, ApiError> {
    let running = state
        .runtime_threads
        .running_threads()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(running))
}

/// Active notices on one thread (#6180): the TUI-visible conditions a
/// watch-only client must surface — subagent-terminal, elevation-needed,
/// model-notify — each with turn identity for targeting.
async fn list_thread_notices(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::runtime_threads::ActiveNotice>>, ApiError> {
    state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    Ok(Json(state.runtime_threads.list_notices(&id)))
}

/// Acknowledge (clear) one notice. Terminal/notify kinds clear only here;
/// elevation additionally auto-clears when its tool call completes.
async fn ack_thread_notice(
    State(state): State<RuntimeApiState>,
    Path((id, notice_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    if !state.runtime_threads.ack_notice(&id, &notice_id) {
        return Err(ApiError::not_found(format!(
            "thread '{id}' has no notice '{notice_id}'"
        )));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// First-appearance dedupe that preserves the order paths were seen in.
///
/// The thread summary resolves git metadata per distinct workspace rather than
/// per row. One resolution runs up to five blocking `git` processes, and rows
/// overwhelmingly share a single workspace, so resolving per row multiplied a
/// listing's process count by its row count.
fn distinct_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut distinct: Vec<PathBuf> = Vec::new();
    for path in paths {
        if !distinct.contains(&path) {
            distinct.push(path);
        }
    }
    distinct
}

async fn list_threads_summary(
    State(state): State<RuntimeApiState>,
    Query(query): Query<ThreadSummaryQuery>,
) -> Result<Json<Vec<ThreadSummary>>, ApiError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 500);
    let search = query.search.as_deref().map(str::to_ascii_lowercase);
    let thread_ids = query
        .thread_ids
        .map(|ids| {
            let ids = ids.split(',').map(str::to_owned).collect::<Vec<_>>();
            if ids.is_empty()
                || ids.len() > 200
                || ids.iter().any(|id| {
                    id.is_empty()
                        || id.trim() != id
                        || id.len() > 128
                        || id.chars().any(char::is_control)
                })
            {
                return Err(ApiError::bad_request(
                    "thread_ids must be at most 200 comma-separated nonempty IDs of at most 128 bytes",
                ));
            }
            Ok(ids.into_iter().collect::<BTreeSet<_>>())
        })
        .transpose()?;
    let filter = resolve_thread_filter(query.include_archived, query.archived_only);
    // `limit` bounds the rows this route returns, not how far a search looks.
    // Passing it to the store read as well matched only inside the newest
    // `limit` threads, so any older match — the row the caller typed the query
    // to find — was invisible. Unsearched listings keep the cheap bounded read;
    // a search or exact-ID filter scans in newest-first order and stops at
    // `limit` matches.
    //
    // Match on the thread record *before* harvesting row facts. Preview is
    // filled only for rows that are returned; it is not a search key.
    let scan_limit = if search.is_some() || thread_ids.is_some() {
        None
    } else {
        Some(limit)
    };
    let threads = state
        .runtime_threads
        .list_threads(filter, scan_limit)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let mut rows = Vec::new();
    for thread in threads {
        if rows.len() >= limit {
            break;
        }
        if let Some(thread_ids) = &thread_ids
            && !thread_ids.contains(&thread.id)
        {
            continue;
        }
        if let Some(search) = &search
            && !state
                .runtime_threads
                .thread_matches_summary_search(&thread, search)
        {
            continue;
        }
        rows.push(thread);
    }

    // Harvest every returned row's facts in ONE pass over the store. Reading a
    // whole thread detail per row made this route `rows x (all_turns +
    // all_items)` JSON reads and parses — seconds-per-thread, so the rail timed
    // out and went blank on a store of a few dozen threads. Preview and turn
    // status now come from that same scan; attention comes from live state.
    let row_ids: Vec<String> = rows.iter().map(|thread| thread.id.clone()).collect();

    // Settle queued recovery receipts before reading the rows, exactly as the
    // per-row detail read did. The flush cancels a recovered turn's pending
    // requests, and this page's attention count reads that state, so it has to
    // precede the scan.
    state
        .runtime_threads
        .flush_recovery_receipts(&row_ids)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let facts = state
        .runtime_threads
        .thread_list_facts(&row_ids)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    // Resolve git metadata once per workspace, not once per row. Each
    // resolution spawns up to five blocking `git` processes — `rev-parse
    // --is-inside-work-tree` twice, `--abbrev-ref HEAD`, `--short HEAD` and
    // `status --porcelain` — and rows overwhelmingly share one workspace, so a
    // per-row resolve turned a 72-thread listing into roughly 360 process
    // spawns, every one of them blocking whichever runtime thread ran it.
    // One blocking task now covers every distinct workspace on the page.
    let workspaces = distinct_paths(rows.iter().map(|thread| thread.workspace.clone()));
    let git_by_workspace: Vec<(PathBuf, WorkspaceGitMetadata)> =
        tokio::task::spawn_blocking(move || {
            workspaces
                .into_iter()
                .map(|workspace| {
                    let metadata = collect_workspace_git_metadata(&workspace);
                    (workspace, metadata)
                })
                .collect()
        })
        .await
        .map_err(|e| ApiError::internal(format!("Workspace git metadata task failed: {e}")))?;

    let mut summaries = Vec::with_capacity(rows.len());
    for thread in rows {
        let facts = facts.get(&thread.id);
        let latest_status = facts.and_then(|facts| facts.latest_turn_status.clone());
        let pending_attention_count = facts.map_or(0, |facts| facts.pending_attention_count);
        let latest_input_summary =
            facts.and_then(|facts| facts.latest_turn_input_summary.as_deref());

        let title = thread
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| truncate_text(t, 72))
            .unwrap_or_else(|| {
                latest_input_summary
                    .map(|summary| {
                        if summary.trim().is_empty() {
                            "New Thread".to_string()
                        } else {
                            truncate_text(summary, 72)
                        }
                    })
                    .unwrap_or_else(|| "New Thread".to_string())
            });

        let preview = facts
            .and_then(|facts| facts.preview.as_deref())
            .map(|text| truncate_text(text, 140))
            .unwrap_or_else(|| title.clone());

        let workspace_git = git_by_workspace
            .iter()
            .find(|(workspace, _)| workspace == &thread.workspace)
            .map(|(_, metadata)| metadata);
        summaries.push(ThreadSummary {
            id: thread.id,
            title,
            preview,
            model: thread.model,
            mode: thread.mode,
            branch: workspace_git.and_then(|git| git.branch.clone()),
            head: workspace_git.and_then(|git| git.head.clone()),
            dirty: workspace_git.is_some_and(|git| git.dirty),
            workspace: thread.workspace,
            archived: thread.archived,
            updated_at: thread.updated_at,
            latest_turn_id: thread.latest_turn_id,
            latest_turn_status: latest_status,
            pending_attention_count,
        });
    }

    Ok(Json(summaries))
}

fn same_agent_worker_launch(current: &AgentWorkerRecord, expected: &AgentWorkerRecord) -> bool {
    current.spec.worker_id == expected.spec.worker_id
        && current.owner_session_id == expected.owner_session_id
        && current.spec.run_id == expected.spec.run_id
        && current.created_at_ms == expected.created_at_ms
        && current
            .spec
            .launch_manifest
            .as_ref()
            .map(|manifest| manifest.generation)
            == expected
                .spec
                .launch_manifest
                .as_ref()
                .map(|manifest| manifest.generation)
}

fn fleet_worker_has_selected_lease(
    record: &AgentWorkerRecord,
    fleet: &crate::fleet::ledger::FleetLedgerState,
) -> bool {
    fleet.tasks.values().any(|task| {
        task.entry.run_id.0 == record.spec.run_id
            && task.leased_to.as_deref() == Some(record.spec.worker_id.as_str())
    })
}

async fn workspace_agent_runs(state: &RuntimeApiState) -> Result<Vec<AgentWorkerRecord>, ApiError> {
    let manager = state.sub_agent_manager.clone().read_owned().await;
    let selected_state = state.clone();
    codewhale_app_server::daemon_socket::owner_work(move || {
        let projected = manager
            .worker_records_for_workspace(&selected_state.workspace)
            .map_err(anyhow::Error::msg)?;
        let fleet = if projected.iter().any(|(_, fleet)| *fleet) {
            Some(
                open_fleet_manager(&selected_state)
                    .map_err(|error| anyhow::anyhow!(error.message))?
                    .rebuild_state()?,
            )
        } else {
            None
        };
        Ok(projected
            .into_iter()
            .filter_map(|(record, needs_lease)| {
                if needs_lease
                    && !fleet
                        .as_ref()
                        .is_some_and(|fleet| fleet_worker_has_selected_lease(&record, fleet))
                {
                    return None;
                }
                Some(record)
            })
            .collect())
    })
    .await
    .map_err(|error| ApiError::conflict(format!("agent run origin could not be verified: {error}")))
}

async fn list_agent_runs(
    State(state): State<RuntimeApiState>,
) -> Result<Json<AgentRunsResponse>, ApiError> {
    let runs = workspace_agent_runs(&state).await?;
    let snapshot = state
        .sub_agent_manager
        .read()
        .await
        .rate_limit_governor()
        .snapshot(std::time::Instant::now());
    Ok(Json(AgentRunsResponse {
        runs,
        governor: AgentRunsGovernor {
            launch_slots: snapshot.launch_capacity,
            max_launch_slots: snapshot.max_capacity,
            paused: snapshot.paused,
            recent_rate_limits: snapshot.window_limited,
            status: snapshot.status_line(),
        },
    }))
}

async fn get_agent_run(
    State(state): State<RuntimeApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<AgentWorkerRecord>, ApiError> {
    let runs = workspace_agent_runs(&state).await?;
    let run = runs
        .into_iter()
        .find(|record| agent_run_matches(record, &run_id))
        .ok_or_else(|| ApiError::not_found(format!("agent run '{run_id}' not found")))?;
    Ok(Json(run))
}

/// A run is addressed by its run id, or by its worker id for records that
/// predate run ids.
fn agent_run_matches(record: &AgentWorkerRecord, run_id: &str) -> bool {
    let effective_run_id = if record.spec.run_id.is_empty() {
        record.spec.worker_id.as_str()
    } else {
        record.spec.run_id.as_str()
    };
    effective_run_id == run_id || record.spec.worker_id == run_id
}

/// How long a stop request waits for the owning engine to record the
/// terminal receipt before answering `202 Accepted` with the live record.
const AGENT_RUN_CANCEL_SETTLE: Duration = Duration::from_secs(3);

/// `POST /v1/agent-runs/{run_id}/cancel`: stop a delegated agent run and
/// answer with its receipt (addendum F2).
///
/// The stop goes through the same session-scoped path as the TUI's `X` and
/// the `agent/cancel` tool, so descendants stop with it and a write-scoped
/// child's work is inventoried rather than dropped. The answer is:
/// - `200` with the terminal record once the run is stopped (or was already
///   finished — stopping is idempotent);
/// - `202` with the current record when the owning engine accepted the stop
///   but has not recorded the terminal receipt yet;
/// - `404` for an unknown run;
/// - `409` when the run belongs to a session this runtime does not host, so
///   nothing here can reach it.
async fn cancel_agent_run(
    State(state): State<RuntimeApiState>,
    Path(run_id): Path<String>,
) -> Result<(StatusCode, Json<AgentWorkerRecord>), ApiError> {
    let selected_record = workspace_agent_runs(&state)
        .await?
        .into_iter()
        .find(|record| agent_run_matches(record, &run_id))
        .ok_or_else(|| ApiError::not_found(format!("agent run '{run_id}' not found")))?;
    // Runs this runtime is executing itself (Fleet-launched children) stop
    // in place. Only a running child in this process qualifies for mutation;
    // a terminal receipt can be returned without mutating or consulting disk.
    // Other persisted runs still go through their owning session below.
    let owned = {
        let manager = state.sub_agent_manager.clone().read_owned().await;
        let selected_workspace = state.workspace.clone();
        let expected = selected_record.clone();
        codewhale_app_server::daemon_socket::owner_work(move || {
            Ok(manager
                .worker_records_for_workspace(&selected_workspace)
                .map_err(anyhow::Error::msg)?
                .into_iter()
                .map(|(record, _)| record)
                .find(|record| same_agent_worker_launch(record, &expected))
                .filter(|record| {
                    manager
                        .get_result(&record.spec.worker_id)
                        .is_ok_and(|agent| {
                            agent.status == SubAgentStatus::Running || record.status.is_terminal()
                        })
                }))
        })
        .await
        .map_err(|error| ApiError::conflict(error.to_string()))?
    };
    if let Some(record) = owned {
        // Persistence is asynchronous. A repeated stop must answer from the
        // owning manager's terminal receipt, not race the disk projection and
        // incorrectly report a run we just stopped as missing or still live.
        if record.status.is_terminal() {
            return Ok((StatusCode::OK, Json(record)));
        }
        let mut manager = state.sub_agent_manager.clone().write_owned().await;
        let selected_workspace = state.workspace.clone();
        let selected_state = state.clone();
        let expected_record = record.clone();
        let cancelled = codewhale_app_server::daemon_socket::owner_work(move || {
            let (current, needs_lease) = manager
                .worker_records_for_workspace(&selected_workspace)
                .map_err(anyhow::Error::msg)?
                .into_iter()
                .find(|(record, _)| same_agent_worker_launch(record, &expected_record))
                .ok_or_else(|| anyhow::anyhow!("worker origin changed before cancellation"))?;
            if needs_lease {
                let fleet = open_fleet_manager(&selected_state)
                    .map_err(|error| anyhow::anyhow!(error.message))?
                    .rebuild_state()?;
                anyhow::ensure!(
                    fleet_worker_has_selected_lease(&current, &fleet),
                    "selected Fleet lease changed before cancellation"
                );
                // Revalidate the held root after the bounded ledger read, before
                // the actor mutation. No awaited operation follows this check.
                anyhow::ensure!(
                    manager
                        .worker_records_for_workspace(&selected_workspace)
                        .map_err(anyhow::Error::msg)?
                        .iter()
                        .any(|(record, _)| same_agent_worker_launch(record, &current)),
                    "worker origin changed during selected Fleet lease validation"
                );
            }
            if current.owner_session_id.is_empty() {
                manager.cancel_agent(&current.spec.worker_id)
            } else {
                manager.cancel_agent_for_session(&current.owner_session_id, &current.spec.worker_id)
            }
        })
        .await
        .map_err(|err| {
            ApiError::conflict(format!("agent run '{run_id}' could not be stopped: {err}"))
        })?;
        let cancelled =
            crate::tools::subagent::settle_requested_child(&state.sub_agent_manager, cancelled)
                .await;
        crate::tools::subagent::preserve_cancelled_work(&state.sub_agent_manager, cancelled).await;
        let manager = state.sub_agent_manager.read().await;
        let record = manager
            .list_worker_records()
            .into_iter()
            .find(|current| same_agent_worker_launch(current, &record))
            .ok_or_else(|| {
                ApiError::conflict("worker launch changed while cancellation settled")
            })?;
        let status = if record.status.is_terminal() {
            StatusCode::OK
        } else {
            StatusCode::ACCEPTED
        };
        return Ok((status, Json(record)));
    }

    let record = selected_record;

    // A runtime thread's session id is its thread id: its live engine owns
    // the child and stops it through the session-scoped cancel path. The
    // on-disk projection cannot tell a live child from an orphan (loading it
    // marks every in-flight record interrupted), so a hosted thread is always
    // asked, and only its own write settles the answer.
    let engine = if record.owner_session_id.is_empty() {
        None
    } else {
        state
            .runtime_threads
            .loaded_engine(&record.owner_session_id)
            .await
    };
    let Some(engine) = engine else {
        if record.status.is_terminal() {
            return Ok((StatusCode::OK, Json(record)));
        }
        return Err(ApiError::conflict(format!(
            "agent run '{run_id}' belongs to a session this runtime is not hosting; stop it from that session"
        )));
    };
    engine
        .send(crate::core::ops::Op::CancelSubAgent {
            agent_id: record.spec.worker_id.clone(),
        })
        .await
        .map_err(|err| ApiError::internal(format!("Failed to reach the run's engine: {err}")))?;

    let settled = |current: &AgentWorkerRecord| {
        current.status.is_terminal()
            && (current.status != AgentWorkerStatus::Interrupted
                || current.latest_message != record.latest_message)
    };
    let deadline = tokio::time::Instant::now() + AGENT_RUN_CANCEL_SETTLE;
    loop {
        let current = workspace_agent_runs(&state)
            .await?
            .into_iter()
            .find(|current| same_agent_worker_launch(current, &record))
            .unwrap_or_else(|| record.clone());
        if settled(&current) {
            return Ok((StatusCode::OK, Json(current)));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok((StatusCode::ACCEPTED, Json(current)));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn list_fleet_profiles(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    // Same roster path the manager uses to validate `agent_profile` ids on
    // run creation, so GUI pickers can never offer a profile the runtime
    // would reject.
    let roster = manager.agent_roster();
    let profiles = roster
        .members()
        .iter()
        .map(|member| {
            json!({
                "id": member.id.clone(),
                "display_name": member.display_name.clone(),
                "description": member.description.clone(),
                "origin": member.origin.to_string(),
            })
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "profiles": profiles,
        "load_error": roster.load_error().map(str::to_string),
    })))
}

async fn create_fleet_run(
    State(state): State<RuntimeApiState>,
    Json(request): Json<CreateFleetRunRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if request.target != FleetRuntimeTarget::ThisComputer {
        return Err(ApiError::not_implemented(format!(
            "Fleet target {:?} is not available in this local Runtime; choose this_computer",
            request.target
        )));
    }
    let (document, descriptor, max_workers) = prepare_managed_fleet_run(request)?;
    let manager = open_fleet_manager(&state)?;
    let report = manager
        .create_queued_run_with_descriptor(document, max_workers, descriptor)
        .map_err(|error| ApiError::bad_request(format!("Failed to create Fleet run: {error}")))?;
    let ledger_state = manager
        .rebuild_state()
        .map_err(|error| ApiError::internal(format!("Failed to rebuild Fleet state: {error}")))?;
    let run = ledger_state
        .runs
        .get(&report.run_id.0)
        .ok_or_else(|| ApiError::internal("Created Fleet run was missing from its ledger"))?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "execution": "awaiting_start",
            "run": fleet_run_detail_json(&manager, run, &ledger_state)?,
            "warnings": report.warnings,
        })),
    ))
}

fn prepare_managed_fleet_run(
    request: CreateFleetRunRequest,
) -> Result<(FleetTaskSpecDocument, ManagedFleetRunDescriptor, usize), ApiError> {
    if request.security_policy.is_some() {
        return Err(ApiError::not_implemented(
            "Managed Fleet security_policy overrides are not executable yet; use named roles and bounded task workspace/tool scopes",
        ));
    }
    if !request.worker_specs.is_empty() {
        return Err(ApiError::not_implemented(
            "Managed Fleet custom worker_specs are not available yet; local Runtime worker IDs are generated per run so worker controls cannot collide across Fleets",
        ));
    }
    if request.roles.is_empty() {
        return Err(ApiError::bad_request(
            "roles must declare at least one named Fleet role",
        ));
    }
    if request.roles.len() > 128 {
        return Err(ApiError::bad_request(
            "roles cannot contain more than 128 entries",
        ));
    }
    let workflow_id = managed_fleet_token("workflow.id", &request.workflow.id)?;
    let workflow_kind = request.workflow.kind;
    let name = request
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(workflow_id.as_str())
        .to_string();
    if name.len() > 256 || name.chars().any(char::is_control) {
        return Err(ApiError::bad_request(
            "name must be one printable line no longer than 256 bytes",
        ));
    }

    let mut roles = BTreeMap::new();
    for role in request.roles {
        let normalized = canonical_public_role_name(&managed_fleet_token("role.name", &role.name)?);
        let agent_profile = role
            .agent_profile
            .as_deref()
            .map(|profile| managed_fleet_token("role.agent_profile", profile))
            .transpose()?;
        if roles.insert(normalized.clone(), agent_profile).is_some() {
            return Err(ApiError::bad_request(format!(
                "duplicate Fleet role '{normalized}'"
            )));
        }
    }

    let mut tasks = request.workflow.tasks;
    let mut used_roles = BTreeSet::new();
    for task in &mut tasks {
        let worker = task.worker.as_mut().ok_or_else(|| {
            ApiError::bad_request(format!(
                "Fleet task '{}' must select one named role through worker.role",
                task.id
            ))
        })?;
        let role = worker.role.as_deref().ok_or_else(|| {
            ApiError::bad_request(format!(
                "Fleet task '{}' must select one named role through worker.role",
                task.id
            ))
        })?;
        let role = canonical_public_role_name(&managed_fleet_token("task.worker.role", role)?);
        let declared_profile = roles.get(&role).ok_or_else(|| {
            ApiError::bad_request(format!(
                "Fleet task '{}' references undeclared role '{role}'",
                task.id
            ))
        })?;
        if let Some(profile) = declared_profile {
            match worker.agent_profile.as_deref() {
                Some(task_profile) if task_profile != profile => {
                    return Err(ApiError::bad_request(format!(
                        "Fleet task '{}' overrides role '{role}' agent_profile '{profile}' with '{task_profile}'",
                        task.id
                    )));
                }
                None => worker.agent_profile = Some(profile.clone()),
                Some(_) => {}
            }
        }
        worker.role = Some(role.clone());
        used_roles.insert(role);
    }
    let unused_roles = roles
        .keys()
        .filter(|role| !used_roles.contains(*role))
        .cloned()
        .collect::<Vec<_>>();
    if !unused_roles.is_empty() {
        return Err(ApiError::bad_request(format!(
            "Every declared Fleet role must own a Workflow task; unused roles: {}",
            unused_roles.join(", ")
        )));
    }
    reject_parallel_write_collisions(&tasks)?;

    let default_workers = roles.len().min(tasks.len()).max(1);
    let max_workers = request.max_workers.unwrap_or(default_workers);
    if !(1..=128).contains(&max_workers) {
        return Err(ApiError::bad_request(
            "max_workers must be between 1 and 128",
        ));
    }
    let role_names = roles.into_keys().collect::<Vec<_>>();
    Ok((
        FleetTaskSpecDocument {
            name: Some(name),
            labels: request.labels,
            security_policy: None,
            workers: Vec::new(),
            tasks,
            usage_ceiling: request.usage_ceiling,
        },
        ManagedFleetRunDescriptor {
            target: Some(request.target),
            workflow: Some(FleetWorkflowDescriptor {
                id: workflow_id,
                kind: workflow_kind,
            }),
            roles: role_names,
        },
        max_workers,
    ))
}

fn managed_fleet_token(field: &str, value: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 128
        || !value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Err(ApiError::bad_request(format!(
            "{field} must be a simple ASCII token no longer than 128 bytes"
        )));
    }
    Ok(value.to_string())
}

fn reject_parallel_write_collisions(tasks: &[FleetTaskSpec]) -> Result<(), ApiError> {
    let mut claims: Vec<(String, String)> = Vec::new();
    for task in tasks {
        let write_roots = fleet_write_roots(task).map_err(|error| {
            ApiError::bad_request(format!(
                "Fleet task '{}' has an invalid write scope: {error}",
                task.id
            ))
        })?;
        for normalized in write_roots {
            for (owner, existing) in &claims {
                if owner != &task.id && managed_paths_overlap(existing.as_str(), &normalized) {
                    return Err(ApiError::bad_request(format!(
                        "Parallel Workflow write scope collision: tasks '{owner}' and '{}' both claim overlapping paths",
                        task.id
                    )));
                }
            }
            claims.push((task.id.clone(), normalized));
        }
    }
    Ok(())
}

fn managed_paths_overlap(left: &str, right: &str) -> bool {
    // `normalize_fleet_relative_path` collapses the workspace root to ".", so
    // a task claiming the whole tree presents as "." rather than as a textual
    // prefix of its siblings. String containment alone never matched it, and
    // two workers could be admitted to write the same tree in parallel.
    if left == "." || right == "." {
        return true;
    }
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|suffix| suffix.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

async fn start_fleet_run(
    State(state): State<RuntimeApiState>,
    Path(run_id): Path<String>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let manager = open_fleet_manager(&state)?;
    let durable = manager
        .rebuild_state()
        .map_err(|error| ApiError::internal(format!("Failed to rebuild Fleet state: {error}")))?;
    let run = durable
        .runs
        .get(&run_id)
        .ok_or_else(|| ApiError::not_found(format!("Fleet run '{run_id}' not found")))?;
    match run.target {
        Some(FleetRuntimeTarget::ThisComputer) => {}
        Some(target) => {
            return Err(ApiError::not_implemented(format!(
                "Fleet target {target:?} is not available in this local Runtime"
            )));
        }
        None => {
            return Err(ApiError::bad_request(
                "Fleet run has no explicit Runtime target and cannot be started through the managed API",
            ));
        }
    }
    if run.workflow.is_none() || run.roles.is_empty() {
        return Err(ApiError::bad_request(
            "Fleet run has no managed Workflow/role descriptor and cannot be started through the managed API",
        ));
    }
    let run_id = FleetRunId::from(run_id);
    let report = manager.activate_run(&run_id).map_err(|error| {
        let message = format!("Failed to start Fleet run '{}': {error}", run_id.0);
        if message.contains("already terminal") {
            ApiError::conflict(message)
        } else {
            ApiError::bad_request(message)
        }
    })?;
    let max_workers = durable
        .runs
        .get(&run_id.0)
        .and_then(|run| run.max_workers)
        .unwrap_or_else(|| report.worker_ids.len().max(1));
    let workspace = state.workspace.clone();
    let codewhale_binary = state.fleet_codewhale_binary.clone();
    let sessions_dir = state.sessions_dir.clone();
    let execution_run_id = run_id.clone();
    let workspace_scope = state.workspace_scope.clone();
    tokio::spawn(async move {
        let _workspace_scope = workspace_scope;
        let mut executor = FleetExecutor::new(&workspace).with_sessions_dir(sessions_dir);
        if let Err(error) = manager
            .run_to_completion(
                &execution_run_id,
                max_workers,
                &mut executor,
                &codewhale_binary,
                None,
                Duration::from_millis(250),
            )
            .await
        {
            tracing::error!(
                run_id = %execution_run_id.0,
                error = %error,
                "Runtime API Fleet manager exited with an error"
            );
        }
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "action": "start",
            "execution": "scheduled",
            "run_id": run_id.0,
            "target": "this_computer",
            "leased": report.leased,
            "queued": report.queued,
            "worker_ids": report.worker_ids,
        })),
    ))
}

async fn replay_fleet_events(
    State(state): State<RuntimeApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<FleetEventsQuery>,
) -> Result<Json<FleetEventReplay>, ApiError> {
    let (after, limit) = validate_fleet_events_query(query)?;
    let replay = load_fleet_event_replay(state, FleetRunId::from(run_id), after, limit)
        .await
        .map_err(map_fleet_replay_error)?;
    Ok(Json(replay))
}

async fn stream_fleet_events(
    State(state): State<RuntimeApiState>,
    Path(run_id): Path<String>,
    Query(query): Query<FleetEventsQuery>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    let (after, limit) = validate_fleet_events_query(query)?;
    let run_id = FleetRunId::from(run_id);
    // Subscribe before the initial load so no append between the load and the
    // first wait is missed for longer than the fallback poll (#6211 R7b).
    let appends = subscribe_fleet_ledger_appends(&fleet_ledger_path(&state.workspace));
    let initial = load_fleet_event_replay(state.clone(), run_id.clone(), after.clone(), limit)
        .await
        .map_err(map_fleet_replay_error)?;
    let event_stream = replay_live_fleet_events(state, run_id, after, limit, initial, appends);
    Ok(Sse::new(event_stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

/// Fallback re-poll when no ledger-append wake arrives. Wakes cover every
/// in-process append; the fallback heals missed wakes, out-of-process
/// writers, and ledger compaction, which replaces rather than appends.
const FLEET_SSE_FALLBACK_POLL: Duration = Duration::from_secs(5);

fn replay_live_fleet_events(
    state: RuntimeApiState,
    run_id: FleetRunId,
    mut after: Option<String>,
    limit: usize,
    initial: FleetEventReplay,
    appends: std::sync::Arc<tokio::sync::Notify>,
) -> impl futures_util::Stream<Item = Result<SseEvent, Infallible>> {
    stream! {
        let mut page = initial;
        loop {
            if page.history_truncated {
                yield Ok(sse_json(
                    "fleet.replay.truncated",
                    json!({
                        "run_id": run_id.0.clone(),
                        "reload_projection": true,
                    }),
                ));
            }
            for event in page.events {
                after = Some(event.cursor.clone());
                yield Ok(fleet_sse_event(&event));
            }
            if !page.has_more {
                // Register interest before yielding to the runtime so an
                // append racing this wait still wakes us (#6211 R7b).
                let notified = appends.notified();
                tokio::pin!(notified);
                tokio::select! {
                    _ = &mut notified => {}
                    _ = tokio::time::sleep(FLEET_SSE_FALLBACK_POLL) => {}
                }
            }
            match load_fleet_event_replay(
                state.clone(),
                run_id.clone(),
                after.clone(),
                limit,
            )
            .await
            {
                Ok(next) => page = next,
                Err(FleetEventReplayError::CursorUnavailable { .. }) => {
                    yield Ok(sse_json(
                        "fleet.replay.cursor_unavailable",
                        json!({
                            "run_id": run_id.0.clone(),
                            "reload_projection": true,
                        }),
                    ));
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        run_id = %run_id.0,
                        error = %error,
                        "Fleet event stream stopped while reading durable history"
                    );
                    yield Ok(sse_json(
                        "fleet.stream.error",
                        json!({ "retryable": true }),
                    ));
                    return;
                }
            }
        }
    }
}

async fn load_fleet_event_replay(
    state: RuntimeApiState,
    run_id: FleetRunId,
    after: Option<String>,
    limit: usize,
) -> std::result::Result<FleetEventReplay, FleetEventReplayError> {
    tokio::task::spawn_blocking(move || {
        let manager =
            open_fleet_manager(&state).map_err(|error| FleetEventReplayError::Storage {
                message: error.message,
            })?;
        manager.replay_events(&run_id, after.as_deref(), limit)
    })
    .await
    .map_err(|error| FleetEventReplayError::Storage {
        message: format!("Fleet replay worker failed: {error}"),
    })?
}

fn validate_fleet_events_query(
    query: FleetEventsQuery,
) -> Result<(Option<String>, usize), ApiError> {
    let after = query
        .after
        .map(|cursor| cursor.trim().to_string())
        .filter(|cursor| !cursor.is_empty());
    if after.as_deref().is_some_and(|cursor| {
        cursor.len() > 96
            || !cursor.starts_with("fev1_")
            || !cursor
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }) {
        return Err(ApiError::bad_request(
            "after is not a valid Fleet event cursor",
        ));
    }
    let limit = query.limit.unwrap_or(DEFAULT_FLEET_EVENT_REPLAY_LIMIT);
    if !(1..=MAX_FLEET_EVENT_REPLAY_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_FLEET_EVENT_REPLAY_LIMIT}"
        )));
    }
    Ok((after, limit))
}

fn map_fleet_replay_error(error: FleetEventReplayError) -> ApiError {
    let message = error.to_string();
    match error {
        FleetEventReplayError::UnknownRun { .. } => ApiError::not_found(message),
        FleetEventReplayError::CursorUnavailable { .. } => ApiError::conflict(message),
        FleetEventReplayError::Storage { .. } => ApiError::internal(message),
    }
}

fn fleet_sse_event(event: &FleetRuntimeEvent) -> SseEvent {
    let data = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_string());
    SseEvent::default()
        .id(event.cursor.clone())
        .event(event.event.clone())
        .data(data)
}

async fn list_fleet_runs(State(state): State<RuntimeApiState>) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let ledger_state = manager
        .rebuild_state()
        .map_err(|err| ApiError::internal(format!("Failed to rebuild Fleet state: {err}")))?;
    let runs: Vec<_> = ledger_state
        .runs
        .values()
        .map(|run| fleet_run_summary_json(&manager, run, &ledger_state))
        .collect::<Result<Vec<_>, _>>()?;
    let status = manager
        .status()
        .map_err(|err| ApiError::internal(format!("Failed to read Fleet status: {err}")))?;
    Ok(Json(json!({
        "status": fleet_status_json(&status),
        "runs": runs,
    })))
}

async fn get_fleet_run(
    State(state): State<RuntimeApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let ledger_state = manager
        .rebuild_state()
        .map_err(|err| ApiError::internal(format!("Failed to rebuild Fleet state: {err}")))?;
    let run = ledger_state
        .runs
        .get(&run_id)
        .ok_or_else(|| ApiError::not_found(format!("Fleet run '{run_id}' not found")))?;
    Ok(Json(fleet_run_detail_json(&manager, run, &ledger_state)?))
}

async fn list_fleet_run_workers(
    State(state): State<RuntimeApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let ledger_state = manager
        .rebuild_state()
        .map_err(|err| ApiError::internal(format!("Failed to rebuild Fleet state: {err}")))?;
    let run = ledger_state
        .runs
        .get(&run_id)
        .ok_or_else(|| ApiError::not_found(format!("Fleet run '{run_id}' not found")))?;
    let workers = run
        .worker_specs
        .iter()
        .map(|worker| {
            manager
                .inspect_worker(&worker.id)
                .map(|inspection| fleet_worker_json(&inspection))
                .map_err(|err| {
                    ApiError::internal(format!(
                        "Failed to inspect Fleet worker {}: {err}",
                        worker.id
                    ))
                })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(json!({
        "run_id": run_id,
        "workers": workers,
    })))
}

async fn get_fleet_worker(
    State(state): State<RuntimeApiState>,
    Path(worker_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let inspection = manager.inspect_worker(&worker_id).map_err(|err| {
        ApiError::not_found(format!("Fleet worker '{worker_id}' not found: {err}"))
    })?;
    Ok(Json(fleet_worker_json(&inspection)))
}

async fn interrupt_fleet_worker(
    State(state): State<RuntimeApiState>,
    Path(worker_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let inspection = manager.interrupt_worker(&worker_id).map_err(|err| {
        ApiError::bad_request(format!(
            "Failed to interrupt Fleet worker '{worker_id}': {err}"
        ))
    })?;
    Ok(Json(json!({
        "action": "interrupt",
        "worker": fleet_worker_json(&inspection),
    })))
}

async fn stop_fleet_worker(
    State(state): State<RuntimeApiState>,
    Path(worker_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let inspection = manager.interrupt_worker(&worker_id).map_err(|err| {
        ApiError::bad_request(format!("Failed to stop Fleet worker '{worker_id}': {err}"))
    })?;
    Ok(Json(json!({
        "action": "stop",
        "worker": fleet_worker_json(&inspection),
    })))
}

async fn restart_fleet_worker(
    State(state): State<RuntimeApiState>,
    Path(worker_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let report = manager.restart_worker(&worker_id).map_err(|err| {
        ApiError::bad_request(format!(
            "Failed to restart Fleet worker '{worker_id}': {err}"
        ))
    })?;
    let worker = fleet_worker_json(&report.inspection);
    let run_id = report.run_id.clone();
    let max_workers = report.max_workers;
    let workspace = state.workspace.clone();
    let codewhale_binary = state.fleet_codewhale_binary.clone();
    let sessions_dir = state.sessions_dir.clone();
    let workspace_scope = state.workspace_scope.clone();
    tokio::spawn(async move {
        let _workspace_scope = workspace_scope;
        let mut executor = FleetExecutor::new(&workspace).with_sessions_dir(sessions_dir);
        if let Err(err) = manager
            .run_to_completion(
                &run_id,
                max_workers,
                &mut executor,
                &codewhale_binary,
                None,
                Duration::from_millis(250),
            )
            .await
        {
            tracing::error!(
                run_id = %run_id.0,
                error = %err,
                "Runtime API Fleet restart manager exited with an error"
            );
        }
    });
    Ok(Json(json!({
        "action": "restart",
        "execution": "scheduled",
        "run_id": report.run_id.0,
        "worker": worker,
    })))
}

async fn stop_fleet_run(
    State(state): State<RuntimeApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let run_id = FleetRunId::from(run_id);
    let stopped = manager.stop_run(&run_id).map_err(|err| {
        ApiError::bad_request(format!("Failed to stop Fleet run '{}': {err}", run_id.0))
    })?;
    let status = manager
        .run_status(&run_id)
        .map_err(|err| ApiError::internal(format!("Failed to read Fleet run status: {err}")))?;
    Ok(Json(json!({
        "action": "stop",
        "run_id": run_id.0,
        "stopped": stopped,
        "status": fleet_status_json(&status),
    })))
}

/// Maximum bytes read from a receipt evidence file for the inspection endpoint.
const MAX_RECEIPT_EVIDENCE_READ_BYTES: u64 = 65_536;

async fn list_fleet_run_receipts(
    State(state): State<RuntimeApiState>,
    Path(run_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let ledger_state = manager
        .rebuild_state()
        .map_err(|err| ApiError::internal(format!("Failed to rebuild Fleet state: {err}")))?;
    if !ledger_state.runs.contains_key(&run_id) {
        return Err(ApiError::not_found(format!(
            "Fleet run '{run_id}' not found"
        )));
    }
    let run_id_parsed = FleetRunId::from(run_id.clone());
    let receipts: Vec<Value> = ledger_state
        .receipts
        .values()
        .filter(|r| r.run_id == run_id_parsed)
        .map(fleet_receipt_json)
        .collect();
    Ok(Json(json!({
        "run_id": run_id,
        "receipts": receipts,
    })))
}

async fn get_fleet_run_receipt(
    State(state): State<RuntimeApiState>,
    Path((run_id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let ledger_state = manager
        .rebuild_state()
        .map_err(|err| ApiError::internal(format!("Failed to rebuild Fleet state: {err}")))?;
    let key = format!("{run_id}:{task_id}");
    let receipt = ledger_state.receipts.get(&key).ok_or_else(|| {
        ApiError::not_found(format!(
            "no receipt found for run '{run_id}' task '{task_id}'"
        ))
    })?;
    Ok(Json(fleet_receipt_json(receipt)))
}

async fn inspect_fleet_run_receipt_evidence(
    State(state): State<RuntimeApiState>,
    Path((run_id, task_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let manager = open_fleet_manager(&state)?;
    let ledger_state = manager
        .rebuild_state()
        .map_err(|err| ApiError::internal(format!("Failed to rebuild Fleet state: {err}")))?;
    let key = format!("{run_id}:{task_id}");
    let receipt = ledger_state.receipts.get(&key).ok_or_else(|| {
        ApiError::not_found(format!(
            "no receipt found for run '{run_id}' task '{task_id}'"
        ))
    })?;
    // Locate the most recent Receipt-kind artifact.
    let receipt_artifact = receipt
        .artifacts
        .iter()
        .rfind(|a| a.kind == FleetArtifactKind::Receipt)
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "no verifier evidence file for run '{run_id}' task '{task_id}'"
            ))
        })?;
    // Receipt artifacts are workspace-relative paths recorded by the verifier;
    // reject absolute paths and `..` escapes before joining onto the workspace.
    // An EMPTY recorded path is not a path at all: joining it would resolve
    // to the workspace directory itself and read it as a file.
    if receipt_artifact.path.as_os_str().is_empty() {
        return Err(ApiError::not_found(format!(
            "no verifier evidence file recorded for run '{run_id}' task '{task_id}'"
        )));
    }
    if !crate::fleet::artifacts::path_is_confined(&receipt_artifact.path) {
        return Err(ApiError::bad_request(format!(
            "evidence path for run '{run_id}' task '{task_id}' escapes the workspace"
        )));
    }
    let (raw, size_bytes) = crate::fleet::artifacts::read_verified(
        &state.workspace,
        receipt_artifact,
        MAX_RECEIPT_EVIDENCE_READ_BYTES,
    )
    .map_err(|err| {
        ApiError::bad_request(format!("Receipt evidence could not be verified: {err}"))
    })?;
    let truncated = size_bytes > MAX_RECEIPT_EVIDENCE_READ_BYTES;
    // Parse as JSON if possible; fall back to a raw string representation.
    let content: Value = serde_json::from_slice(&raw)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()));
    Ok(Json(json!({
        "run_id": run_id,
        "task_id": task_id,
        "path": receipt_artifact.path,
        "checksum": receipt_artifact.checksum,
        "size_bytes": size_bytes,
        "truncated": truncated,
        "content": content,
    })))
}

fn open_fleet_manager(state: &RuntimeApiState) -> Result<FleetManager, ApiError> {
    let (exec_config, fleet_config, session_model, route_config) = {
        let config = state.config.read();
        let exec_config = config
            .fleet
            .as_ref()
            .map(|fleet| fleet.exec.clone())
            .unwrap_or_default();
        // The active session route is the operator: workers without a
        // task/profile model pin inherit the model the user picked in /model.
        (
            exec_config,
            config.fleet_config(),
            runtime_request_model(&config, None).ok(),
            config.clone(),
        )
    };
    FleetManager::open(&state.workspace)
        .map(|manager| {
            let manager = manager
                .with_exec_config(exec_config)
                .with_fleet_config(fleet_config)
                .with_sub_agent_manager(state.sub_agent_manager.clone())
                .with_route_config(route_config);
            match session_model {
                Some(model) => manager.with_session_model(model),
                None => manager,
            }
        })
        .map_err(|err| ApiError::internal(format!("Failed to open Fleet manager: {err}")))
}

fn fleet_run_summary_json(
    manager: &FleetManager,
    run: &FleetRun,
    ledger_state: &FleetLedgerState,
) -> Result<Value, ApiError> {
    let status = manager
        .run_status(&run.id)
        .map_err(|err| ApiError::internal(format!("Failed to read Fleet run status: {err}")))?;
    let task_statuses = ledger_state
        .tasks
        .values()
        .filter(|task| task.entry.run_id == run.id)
        .map(|task| {
            json!({
                "task_id": task.entry.task_id.clone(),
                "status": fleet_task_status_label(task.status),
                "leased_to": task.leased_to.clone(),
                "attempts": task.entry.attempts,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "id": run.id.0.clone(),
        "name": run.name.clone(),
        "lifecycle_status": ledger_state
            .run_status_overrides
            .get(&run.id.0)
            .unwrap_or(&run.status),
        "status": fleet_status_json(&status),
        "target": run.target,
        "workflow": run.workflow.clone(),
        "roles": run.roles.clone(),
        "task_count": run.task_specs.len(),
        "worker_count": run.worker_specs.len(),
        "tasks": task_statuses,
        "labels": run.labels.clone(),
        "created_at": run.created_at.clone(),
        "updated_at": run.updated_at.clone(),
        "completed_at": run.completed_at.clone(),
    }))
}

fn fleet_run_detail_json(
    manager: &FleetManager,
    run: &FleetRun,
    ledger_state: &FleetLedgerState,
) -> Result<Value, ApiError> {
    let mut value = fleet_run_summary_json(manager, run, ledger_state)?;
    if let Some(map) = value.as_object_mut() {
        map.insert("task_specs".to_string(), json!(run.task_specs.clone()));
        map.insert("worker_specs".to_string(), json!(run.worker_specs.clone()));
    }
    Ok(value)
}

fn fleet_status_json(status: &FleetStatusSnapshot) -> Value {
    json!({
        "runs": status.runs,
        "queued": status.queued,
        "running": status.running,
        "completed": status.completed,
        "partial": status.partial,
        "failed": status.failed,
        "restarted": status.restarted,
        "escalated": status.escalated,
        "transport_failed": status.transport_failed,
        "task_failed": status.task_failed,
        "verifier_failed": status.verifier_failed,
        "cancelled": status.cancelled,
        "stale": status.stale,
        "workers": status
            .workers
            .iter()
            .map(|(worker_id, status)| {
                (
                    worker_id.clone(),
                    Value::String(worker_status_label(status).to_string()),
                )
            })
            .collect::<serde_json::Map<String, Value>>(),
    })
}

fn fleet_worker_json(inspection: &FleetWorkerInspection) -> Value {
    json!({
        "worker_id": inspection.worker_id.clone(),
        "status": worker_status_label(&inspection.status),
        "run_id": inspection.current_run_id.as_ref().map(|run_id| run_id.0.clone()),
        "task_id": inspection.current_task_id.clone(),
        "objective": inspection.objective.clone(),
        "role": inspection.role.clone(),
        "host": inspection.host.clone(),
        "latest_heartbeat_at": inspection.latest_heartbeat_at.clone(),
        "latest_event": inspection.latest_event.as_ref().map(fleet_event_json),
        "artifacts": inspection.artifacts.iter().map(fleet_artifact_json).collect::<Vec<_>>(),
        "last_error": inspection.last_error.clone(),
        "alert_state": inspection.alert_state.clone(),
        "runtime_state": inspection.runtime_state.as_ref().map(fleet_worker_runtime_json),
    })
}

fn fleet_worker_runtime_json(runtime: &FleetWorkerRuntimeProjection) -> Value {
    json!({
        "agent_status": runtime.agent_status.clone(),
        "steps_taken": runtime.steps_taken,
        "latest_message": runtime.latest_message.clone(),
        "error": runtime.error.clone(),
        "result_summary": runtime.result_summary.clone(),
        "has_session": runtime.has_session,
    })
}

fn fleet_artifact_json(artifact: &codewhale_protocol::fleet::FleetArtifactRef) -> Value {
    json!({
        "kind": artifact_kind_label(&artifact.kind),
        "path": artifact.path.clone(),
        "checksum": artifact.checksum.clone(),
        "mime_type": artifact.mime_type.clone(),
        "size_bytes": artifact.size_bytes,
    })
}

fn fleet_receipt_json(receipt: &codewhale_protocol::fleet::FleetReceipt) -> Value {
    use codewhale_protocol::fleet::{FleetTaskFailureKind, FleetTaskResult};

    let result_label = match receipt.result {
        FleetTaskResult::Pass => "pass",
        FleetTaskResult::Partial => "partial",
        FleetTaskResult::Fail => "fail",
        FleetTaskResult::Skip => "skip",
        FleetTaskResult::Timeout => "timeout",
    };
    let (failure_kind_label, failure_class, retry_eligible) = match receipt.failure_kind.as_ref() {
        Some(FleetTaskFailureKind::Transport) => (
            Some("transport"),
            Some("Infrastructure or network failure during task transport"),
            true,
        ),
        Some(FleetTaskFailureKind::Task) => (
            Some("task"),
            Some("Task logic exited unsuccessfully"),
            false,
        ),
        Some(FleetTaskFailureKind::Verifier) => (
            Some("verifier"),
            Some("Verifier rejected the task output; manual review or code change required"),
            false,
        ),
        None => (None, None, false),
    };
    let evidence_available = receipt
        .artifacts
        .iter()
        .any(|a| a.kind == FleetArtifactKind::Receipt);
    let score_json = receipt.score.as_ref().map(|s| {
        json!({
            "value": s.value,
            "max": s.max,
            "notes": s.notes,
        })
    });
    json!({
        "run_id": receipt.run_id.0.clone(),
        "task_id": receipt.task_id.clone(),
        "worker_id": receipt.worker_id.clone(),
        "attempt": receipt.attempt,
        "terminal_seq": receipt.terminal_seq,
        "completed_at": receipt.completed_at.clone(),
        "result": result_label,
        "failure_kind": failure_kind_label,
        "failure_class": failure_class,
        "retry_eligible": retry_eligible,
        "score": score_json,
        "artifacts": receipt.artifacts.iter().map(fleet_artifact_json).collect::<Vec<_>>(),
        "saved_session_id": receipt.saved_session_id.clone(),
        "evidence_available": evidence_available,
    })
}

fn fleet_event_json(event: &codewhale_protocol::fleet::FleetWorkerEvent) -> Value {
    json!({
        "seq": event.seq,
        "run_id": event.run_id.0.clone(),
        "worker_id": event.worker_id.clone(),
        "task_id": event.task_id.clone(),
        "timestamp": event.timestamp.clone(),
        "label": fleet_event_label(&event.payload),
        "payload": event.payload.clone(),
    })
}

fn worker_status_label(status: &FleetWorkerStatus) -> &'static str {
    match status {
        FleetWorkerStatus::Unknown => "unknown",
        FleetWorkerStatus::Online => "online",
        FleetWorkerStatus::Busy => "busy",
        FleetWorkerStatus::Offline => "offline",
        FleetWorkerStatus::Unhealthy => "unhealthy",
        FleetWorkerStatus::Draining => "draining",
        FleetWorkerStatus::Retired => "retired",
    }
}

fn fleet_task_status_label(status: FleetTaskLedgerStatus) -> &'static str {
    match status {
        FleetTaskLedgerStatus::Enqueued => "enqueued",
        FleetTaskLedgerStatus::Leased => "leased",
        FleetTaskLedgerStatus::Completed => "completed",
        FleetTaskLedgerStatus::Failed => "failed",
        FleetTaskLedgerStatus::Cancelled => "cancelled",
    }
}

fn artifact_kind_label(kind: &FleetArtifactKind) -> String {
    match kind {
        FleetArtifactKind::Log => "log".to_string(),
        FleetArtifactKind::Patch => "patch".to_string(),
        FleetArtifactKind::TestResult => "test_result".to_string(),
        FleetArtifactKind::Report => "report".to_string(),
        FleetArtifactKind::Checkpoint => "checkpoint".to_string(),
        FleetArtifactKind::Receipt => "receipt".to_string(),
        FleetArtifactKind::Other(value) => value.clone(),
    }
}

/// Bound on the `Completed.summary` excerpt inside a lifecycle event label.
const FLEET_EVENT_LABEL_SUMMARY_CHARS: usize = 160;

fn fleet_event_label(payload: &FleetWorkerEventPayload) -> String {
    match payload {
        FleetWorkerEventPayload::Queued => "queued".to_string(),
        FleetWorkerEventPayload::Leased { .. } => "leased".to_string(),
        FleetWorkerEventPayload::Starting => "starting".to_string(),
        FleetWorkerEventPayload::Running => "running".to_string(),
        FleetWorkerEventPayload::ModelWait { model } => model
            .as_ref()
            .map(|model| format!("model_wait model={model}"))
            .unwrap_or_else(|| "model_wait".to_string()),
        FleetWorkerEventPayload::RunningTool { tool, call_id } => call_id
            .as_ref()
            .map(|call_id| format!("running_tool tool={tool} call_id={call_id}"))
            .unwrap_or_else(|| format!("running_tool tool={tool}")),
        FleetWorkerEventPayload::WorkflowEvent {
            workflow_run_id,
            event,
        } => event
            .get("type")
            .and_then(serde_json::Value::as_str)
            .map(|kind| format!("workflow_event run_id={workflow_run_id} type={kind}"))
            .unwrap_or_else(|| format!("workflow_event run_id={workflow_run_id}")),
        FleetWorkerEventPayload::Heartbeat { .. } => "heartbeat".to_string(),
        FleetWorkerEventPayload::UsageReport {
            input_tokens,
            output_tokens,
        } => format!("usage_report input={input_tokens} output={output_tokens}"),
        FleetWorkerEventPayload::Artifact(artifact) => {
            format!("artifact kind={}", artifact_kind_label(&artifact.kind))
        }
        // `summary` may carry the worker's bounded final-answer excerpt (up
        // to a few thousand chars); the label is a one-line status surface,
        // so it gets a short excerpt while `payload` keeps the full text.
        FleetWorkerEventPayload::Completed { exit_code, summary } => match (
            exit_code,
            summary
                .as_deref()
                .map(|summary| truncate_text(summary, FLEET_EVENT_LABEL_SUMMARY_CHARS)),
        ) {
            (Some(code), Some(summary)) => format!("completed exit_code={code} {summary}"),
            (Some(code), None) => format!("completed exit_code={code}"),
            (None, Some(summary)) => format!("completed {summary}"),
            (None, None) => "completed".to_string(),
        },
        FleetWorkerEventPayload::Failed {
            reason,
            recoverable,
        } => {
            format!("failed recoverable={recoverable} reason={reason}")
        }
        FleetWorkerEventPayload::Cancelled { cancelled_by } => cancelled_by
            .as_ref()
            .map(|by| format!("cancelled by={by}"))
            .unwrap_or_else(|| "cancelled".to_string()),
        FleetWorkerEventPayload::Interrupted { signal } => signal
            .as_ref()
            .map(|signal| format!("interrupted signal={signal}"))
            .unwrap_or_else(|| "interrupted".to_string()),
        FleetWorkerEventPayload::Stale { last_heartbeat_at } => last_heartbeat_at
            .as_ref()
            .map(|ts| format!("stale last_heartbeat_at={ts}"))
            .unwrap_or_else(|| "stale".to_string()),
        FleetWorkerEventPayload::Restarted { restart_count } => {
            format!("restarted count={restart_count}")
        }
        FleetWorkerEventPayload::Escalated { channel, alert_id } => alert_id
            .as_ref()
            .map(|alert_id| format!("escalated channel={channel} alert_id={alert_id}"))
            .unwrap_or_else(|| format!("escalated channel={channel}")),
    }
}

/// One entry in the served slash-command catalog (`GET /v1/commands`, #6178).
///
/// Clients use this to complete and validate input without duplicating the
/// registry: a `binding: "host"` row must never be submitted as a model
/// prompt, and a user command shadowing a builtin name wins that spelling.
#[derive(Debug, Serialize)]
struct CommandCatalogEntry {
    name: String,
    aliases: Vec<String>,
    /// English source text; localizing is the client's surface.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<String>,
    /// Literal verbs declared by the usage line (`/goal <block|complete|…>`).
    subcommands: Vec<String>,
    takes_arguments: bool,
    /// Composer argument shape, computed the way the TUI composer computes
    /// it so clients do not re-derive it from the usage string.
    /// Usage mentions any argument, required or optional.
    requires_argument: bool,
    /// Usage has a `<required>` argument outside every `[optional]` group.
    requires_required_argument: bool,
    /// Accepting the command leaves a trailing space for its arguments.
    composer_wants_trailing_space: bool,
    /// The palette runs the command on selection instead of pasting it.
    palette_runs_directly: bool,
    /// Listed when the slash menu opens with no filter text.
    show_in_empty_discovery: bool,
    /// `builtin` is registered code; `user` expands a stored template.
    kind: &'static str,
    /// `host` runs locally and never reaches the model; `prompt` expands into
    /// the request the model sees.
    binding: &'static str,
    /// `primary` | `advanced` | `compatibility` — builtins only; `hidden`
    /// covers rows the product does not advertise anywhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    discovery: Option<&'static str>,
    hidden: bool,
    /// User command holding this builtin's canonical name.
    #[serde(skip_serializing_if = "Option::is_none")]
    shadowed_by: Option<String>,
    /// Alias spellings of this builtin taken by user commands.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    shadowed_aliases: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CommandsResponse {
    commands: Vec<CommandCatalogEntry>,
}

fn command_catalog(
    user_commands: &crate::commands::user_registry::UserCommandRegistry,
) -> Vec<CommandCatalogEntry> {
    let mut commands = Vec::new();
    for info in crate::commands::command_infos() {
        let shadowed_by = user_commands
            .get(info.name)
            .map(|command| command.name.clone());
        let shadowed_aliases = info
            .aliases
            .iter()
            .filter(|alias| user_commands.get(alias).is_some())
            .map(|alias| (*alias).to_string())
            .collect();
        commands.push(CommandCatalogEntry {
            name: info.name.to_string(),
            aliases: info
                .aliases
                .iter()
                .map(|alias| (*alias).to_string())
                .collect(),
            summary: Some(
                info.description_for(codewhale_localization::Locale::En)
                    .into_owned(),
            ),
            usage: Some(info.usage.to_string()),
            subcommands: crate::commands::traits::usage_subcommands(info.usage)
                .iter()
                .map(|token| (*token).to_string())
                .collect(),
            takes_arguments: crate::commands::user_registry::usage_describes_arguments(
                info.name, info.usage,
            ),
            requires_argument: info.requires_argument(),
            requires_required_argument: info.requires_required_argument(),
            composer_wants_trailing_space: info.composer_wants_trailing_space(),
            palette_runs_directly: info.palette_runs_directly(),
            show_in_empty_discovery: info.show_in_empty_discovery(),
            kind: "builtin",
            binding: "host",
            discovery: Some(match info.discovery() {
                crate::commands::traits::CommandDiscovery::Primary => "primary",
                crate::commands::traits::CommandDiscovery::Advanced => "advanced",
                crate::commands::traits::CommandDiscovery::Compatibility => "compatibility",
            }),
            hidden: crate::commands::traits::UNLISTED_COMMANDS.contains(&info.name),
            shadowed_by,
            shadowed_aliases,
        });
    }
    // Extension commands run in the extension host from the TUI's event loop;
    // a Runtime API client cannot run them, so they are not advertised here.
    for command in user_commands
        .iter()
        .filter(|command| command.extension.is_none())
    {
        let takes_arguments = command.takes_arguments();
        commands.push(CommandCatalogEntry {
            name: command.name.clone(),
            aliases: command.aliases.clone(),
            summary: command.description.clone(),
            usage: command.display_usage().map(str::to_string),
            subcommands: Vec::new(),
            takes_arguments,
            // A template may run bare, so its arguments are never required.
            requires_argument: takes_arguments,
            requires_required_argument: false,
            composer_wants_trailing_space: takes_arguments,
            palette_runs_directly: !takes_arguments,
            show_in_empty_discovery: !command.hidden,
            kind: "user",
            binding: "prompt",
            discovery: None,
            hidden: command.hidden,
            shadowed_by: None,
            shadowed_aliases: Vec::new(),
        });
    }
    commands
}

async fn list_commands(
    State(state): State<RuntimeApiState>,
) -> Result<Json<CommandsResponse>, ApiError> {
    let commands = crate::commands::user_registry::with_registry_for_workspace(
        Some(state.workspace.as_path()),
        command_catalog,
    );
    Ok(Json(CommandsResponse { commands }))
}

#[derive(Debug, Deserialize)]
struct HooksQuery {
    /// Report the hooks a thread's engine runs (its workspace); defaults to
    /// the server workspace.
    thread_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct HooksResponse {
    workspace: String,
    enabled: bool,
    hooks: Vec<HookEntry>,
    /// Hooks rejected or warned about at load, one redaction-safe line each.
    problems: Vec<String>,
}

#[derive(Debug, Serialize)]
struct HookEntry {
    name: Option<String>,
    event: &'static str,
    /// The shell command, with credential-shaped values masked.
    command: String,
    background: bool,
    timeout_secs: u64,
    /// `global` (user config), `plugin` (reviewed plugin) or `project`
    /// (trusted, approved `.codewhale/hooks.toml`).
    source: &'static str,
}

/// Mask a hook command for `GET /v1/hooks`. Keyed and credential-shaped
/// values go through the shared redactor; every URL additionally keeps only
/// its scheme and host, because webhook secrets live in the path
/// (`https://hooks.slack.com/services/T…/B…/<secret>`) where no key names
/// them.
fn redact_hook_command_for_listing(command: &str) -> String {
    let masked = codewhale_config::persistence::redact_secrets(command);
    masked
        .split(' ')
        .map(|word| match word.find("://") {
            Some(scheme_end) => {
                let rest = &word[scheme_end + 3..];
                let host_end = rest.find(['/', '?', '#', '"', '\'']).unwrap_or(rest.len());
                let host = &rest[..host_end];
                // Userinfo (`user:pass@host`) is a credential too.
                let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
                let tail = &rest[host_end..];
                let quote = tail
                    .chars()
                    .last()
                    .filter(|c| matches!(c, '"' | '\''))
                    .map(String::from)
                    .unwrap_or_default();
                let path = if tail.len() > quote.len() {
                    "/[redacted]"
                } else {
                    ""
                };
                format!("{}{host}{path}{quote}", &word[..scheme_end + 3])
            }
            None => word.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `GET /v1/hooks` (B4): the hook set Runtime API threads run, from the same
/// loader their engines use, so clients show one truth instead of keeping a
/// hook table of their own.
async fn list_hooks(
    State(state): State<RuntimeApiState>,
    Query(query): Query<HooksQuery>,
) -> Result<Json<HooksResponse>, ApiError> {
    let workspace = match query.thread_id.as_deref() {
        Some(id) => {
            state
                .runtime_threads
                .get_thread(id)
                .await
                .map_err(map_thread_err)?
                .workspace
        }
        None => state.workspace.clone(),
    };
    let config = state.config.read().clone();
    let plugins = state.plugin_discovery.registry_for_workspace(&workspace);
    let executor = state.runtime_threads.hook_executor_for_workspace(
        &config,
        &workspace,
        Some(plugins.as_ref()),
    );
    let hooks_config = executor.config();
    let hooks = hooks_config
        .hooks
        .iter()
        .map(|hook| HookEntry {
            name: hook.name.clone(),
            event: hook.event.as_str(),
            command: redact_hook_command_for_listing(&hook.command),
            background: hook.background,
            timeout_secs: hook.timeout_secs,
            source: if hook.project_authority.is_some() {
                "project"
            } else if hook.plugin_authority.is_some() {
                "plugin"
            } else {
                "global"
            },
        })
        .collect();
    Ok(Json(HooksResponse {
        workspace: workspace.display().to_string(),
        enabled: hooks_config.enabled,
        hooks,
        problems: hooks_config
            .problems
            .iter()
            .map(crate::hooks::HookConfigProblem::summary)
            .collect(),
    }))
}

async fn list_skills(
    State(state): State<RuntimeApiState>,
) -> Result<Json<SkillsResponse>, ApiError> {
    let (skills_dir, mode) = {
        let config = state.config.read();
        let skills_dir = resolve_skills_dir(&config, &state.workspace);
        let mode = crate::skills::SkillDiscoveryMode::from_config(&config.skills_config());
        (skills_dir, mode)
    };
    let plugin_registry = state
        .plugin_discovery
        .registry_for_workspace(&state.workspace);
    let (registry, directories) = discover_skills_for_runtime_api(
        &state.workspace,
        &skills_dir,
        mode,
        Some(plugin_registry.as_ref()),
    );
    let mut skill_state = state.skill_state.lock().await;
    skill_state
        .refresh()
        .map_err(|error| ApiError::internal(format!("refresh skill state: {error}")))?;
    let skills = registry
        .list()
        .iter()
        .map(|skill| {
            let enabled = skill_state
                .is_enabled_with_legacy(&skill.name, skill.legacy_activation_name.as_deref());
            skill_entry_for(skill, enabled, &skills_dir)
        })
        .collect();
    Ok(Json(SkillsResponse {
        directory: skills_dir,
        directories,
        warnings: registry.warnings().to_vec(),
        skills,
    }))
}

async fn set_skill_enabled(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    Json(req): Json<SetSkillEnabledRequest>,
) -> Result<Json<SetSkillEnabledResponse>, ApiError> {
    let (skills_dir, mode) = {
        let config = state.config.read();
        let skills_dir = resolve_skills_dir(&config, &state.workspace);
        let mode = crate::skills::SkillDiscoveryMode::from_config(&config.skills_config());
        (skills_dir, mode)
    };
    let plugin_registry = state
        .plugin_discovery
        .registry_for_workspace(&state.workspace);
    let (registry, directories) = discover_skills_for_runtime_api(
        &state.workspace,
        &skills_dir,
        mode,
        Some(plugin_registry.as_ref()),
    );
    let exists = registry.list().iter().any(|skill| skill.name == name);
    if !exists {
        return Err(ApiError::not_found(format!(
            "skill '{name}' not found in searched directories: {}",
            format_skill_search_paths(&directories)
        )));
    }

    let mut store = state.skill_state.lock().await;
    store
        .set_enabled(&name, req.enabled)
        .map_err(|err| ApiError::internal(format!("persist skill state: {err}")))?;
    Ok(Json(SetSkillEnabledResponse {
        name,
        enabled: req.enabled,
    }))
}

/// `GET /v1/skills/{name}` — one skill's full routing metadata plus its body.
///
/// Clients that activate a skill client-side (TUI's `/skill`, the VS Code GUI)
/// need the SKILL.md body to compose the next turn's instruction. `load_skill`
/// serves the model inside a turn; this endpoint serves the *client* before
/// one. The same discovery walk backs both, so a name the listing shows is a
/// name this resolves. Plugin bodies are already content-bound in the
/// in-memory registry snapshot; native bodies are re-checked against disk so
/// a deleted SKILL.md fails loudly instead of serving a stale body.
async fn get_skill_detail(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
) -> Result<Json<SkillDetailResponse>, ApiError> {
    // Discovery, plugin tree hashing/state locks, and skill-state refresh
    // all read disk; keep this new route's work off the async server worker.
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let (skills_dir, mode) = {
            let config = state.config.read();
            let skills_dir = resolve_skills_dir(&config, &state.workspace);
            let mode = crate::skills::SkillDiscoveryMode::from_config(&config.skills_config());
            (skills_dir, mode)
        };
        let plugin_registry = state
            .plugin_discovery
            .registry_for_workspace(&state.workspace);
        let (registry, directories) = discover_skills_for_runtime_api(
            &state.workspace,
            &skills_dir,
            mode,
            Some(plugin_registry.as_ref()),
        );
        let Some(skill) = registry.get(&name) else {
            return Err(ApiError::not_found(format!(
                "skill '{name}' not found in searched directories: {}",
                format_skill_search_paths(&directories)
            )));
        };

        // Only the checks the listing does not need: this route hands the body to
        // a client, so a native skill whose file has gone must fail rather than
        // serve the cached instructions, and a plugin must still hold the
        // authority its snapshot was reviewed under. Field derivation itself is
        // `skill_entry_for`'s, shared with the listing.
        match &skill.source {
            crate::skills::SkillSource::Native if !skill.path.is_file() => {
                return Err(ApiError::not_found(format!(
                    "skill '{}' is registered at {} but that file no longer exists on disk",
                    skill.name,
                    skill.path.display()
                )));
            }
            crate::skills::SkillSource::Plugin { authority, .. } => {
                // The same gate the TUI's own activation path runs: a plugin whose
                // trust or enablement changed since discovery must not hand its
                // body to a client.
                crate::plugins::registry::verify_plugin_component_authority(
                    authority,
                    crate::plugins::activation::PluginActivationCapability::Skills,
                )
                .map_err(|reason| {
                    ApiError::forbidden(format!(
                        "plugin skill '{}' is no longer active: {reason}",
                        skill.name
                    ))
                })?;
                if authority.workspace != state.workspace {
                    return Err(ApiError::forbidden(format!(
                        "plugin skill '{}' belongs to a different workspace",
                        skill.name
                    )));
                }
            }
            crate::skills::SkillSource::Native => {}
        }

        let mut skill_state = state.skill_state.blocking_lock();
        skill_state
            .refresh()
            .map_err(|error| ApiError::internal(format!("refresh skill state: {error}")))?;
        let enabled = skill_state
            .is_enabled_with_legacy(&skill.name, skill.legacy_activation_name.as_deref());
        Ok(Json(SkillDetailResponse {
            skill: skill_entry_for(skill, enabled, &skills_dir),
            body: skill.body.clone(),
        }))
    })
    .await
    .map_err(|error| ApiError::internal(format!("skill detail read task failed: {error}")))?
}

// ─── Skill lifecycle helpers ────────────────────────────────────────────────

/// Build a [`crate::skills::mutation::MutationContext`] from the current
/// server state. Reads the network policy and installer settings directly
/// from the config already held in `state`.
fn mutation_context_settings(
    state: &RuntimeApiState,
) -> (
    crate::network_policy::NetworkPolicy,
    u64,
    String,
    Option<PathBuf>,
) {
    use crate::skills::install::{DEFAULT_MAX_SIZE_BYTES, DEFAULT_REGISTRY_URL};
    let config = state.config.read();
    let network = config
        .network
        .clone()
        .map(|p| p.into_runtime())
        .unwrap_or_default();
    let skills_cfg = config.skills.as_ref();
    let max_size = skills_cfg
        .and_then(|s| s.max_install_size_bytes)
        .unwrap_or(DEFAULT_MAX_SIZE_BYTES);
    let registry_url = skills_cfg
        .and_then(|s| s.registry_url.clone())
        .unwrap_or_else(|| DEFAULT_REGISTRY_URL.to_string());
    let configured_skills_dir = config.skills_dir.as_ref().map(PathBuf::from);
    (network, max_size, registry_url, configured_skills_dir)
}

fn parse_api_scope(
    scope: Option<&str>,
) -> Result<Option<crate::skills::mutation::SkillTargetScope>, ApiError> {
    match scope {
        None => Ok(None),
        Some("project") => Ok(Some(crate::skills::mutation::SkillTargetScope::Project)),
        Some("global") => Ok(Some(crate::skills::mutation::SkillTargetScope::Global)),
        Some(other) => Err(ApiError::bad_request(format!(
            "invalid scope '{other}'; expected \"project\" or \"global\""
        ))),
    }
}

fn receipt_to_response(
    receipt: &crate::skills::mutation::SkillMutationReceipt,
) -> SkillMutationReceiptResponse {
    use crate::skills::mutation::SkillMutationOutcome;
    use crate::skills::roots::SkillScope;

    const TRUST_NOTE: &str = "The .trusted marker is advisory and digest-bound; \
         it records your review intent but does not sandbox or auto-authorize scripts.";

    let outcome: &'static str = match &receipt.outcome {
        SkillMutationOutcome::Installed => "installed",
        SkillMutationOutcome::Updated => "updated",
        SkillMutationOutcome::NoChange => "no_change",
        SkillMutationOutcome::Removed => "removed",
        SkillMutationOutcome::Trusted => "trusted",
        SkillMutationOutcome::Imported => "imported",
        SkillMutationOutcome::AlreadyPresent => "already_present",
        // NeedsApproval / NetworkDenied are returned as ApiError::forbidden
        // before reaching this conversion; they should not appear here.
        SkillMutationOutcome::NeedsApproval(_) => "needs_approval",
        SkillMutationOutcome::NetworkDenied(_) => "network_denied",
    };
    let scope = match receipt.scope {
        SkillScope::Project => "project".to_string(),
        SkillScope::Global => "global".to_string(),
        SkillScope::Logical => "logical".to_string(),
    };
    let trust_note = if receipt.outcome == SkillMutationOutcome::Trusted {
        Some(TRUST_NOTE)
    } else {
        None
    };
    SkillMutationReceiptResponse {
        name: receipt.name.clone(),
        outcome,
        scope,
        safe_target_path: receipt.safe_target_path.clone(),
        trust_note,
    }
}

fn outcome_is_policy_error(outcome: &crate::skills::mutation::SkillMutationOutcome) -> bool {
    matches!(
        outcome,
        crate::skills::mutation::SkillMutationOutcome::NeedsApproval(_)
            | crate::skills::mutation::SkillMutationOutcome::NetworkDenied(_)
    )
}

fn policy_error_message(outcome: &crate::skills::mutation::SkillMutationOutcome) -> String {
    match outcome {
        crate::skills::mutation::SkillMutationOutcome::NeedsApproval(host) => format!(
            "network access to '{host}' requires explicit approval; \
             approve the host in your network policy before installing this skill"
        ),
        crate::skills::mutation::SkillMutationOutcome::NetworkDenied(host) => {
            format!("network access to '{host}' was denied by the active network policy")
        }
        _ => "operation denied by policy".to_string(),
    }
}

// ─── POST /v1/skills/install ────────────────────────────────────────────────

async fn install_skill_api(
    State(state): State<RuntimeApiState>,
    Json(req): Json<InstallSkillRequest>,
) -> Result<(StatusCode, Json<SkillMutationReceiptResponse>), ApiError> {
    use crate::skills::install::InstallSource;
    use crate::skills::mutation::{MutationContext, SkillMutationRequest, SkillTargetScope};

    let source = InstallSource::parse(&req.source)
        .map_err(|err| ApiError::bad_request(format!("invalid install source: {err}")))?;
    let target = parse_api_scope(req.scope.as_deref())?.unwrap_or(SkillTargetScope::Global);

    let (network, max_size, registry_url, configured_skills_dir) =
        mutation_context_settings(&state);
    let home = crate::config::effective_home_dir();
    let workspace = state.workspace.clone();

    let receipt = crate::skills::mutation::execute(
        SkillMutationRequest::InstallRemote { source, target },
        &MutationContext {
            workspace: &workspace,
            home: home.as_deref(),
            configured_skills_dir: configured_skills_dir.as_deref(),
            network: &network,
            max_size,
            registry_url: &registry_url,
        },
    )
    .await
    .map_err(|err| ApiError::bad_request(format!("install failed: {err:#}")))?;

    if outcome_is_policy_error(&receipt.outcome) {
        return Err(ApiError::forbidden(policy_error_message(&receipt.outcome)));
    }

    let status = if receipt.outcome == crate::skills::mutation::SkillMutationOutcome::Installed {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(receipt_to_response(&receipt))))
}

// ─── POST /v1/skills/{name}/update ─────────────────────────────────────────

async fn update_skill_api(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    Json(req): Json<UpdateSkillRequest>,
) -> Result<Json<SkillMutationReceiptResponse>, ApiError> {
    use crate::skills::mutation::{MutationContext, SkillMutationRequest};

    let scope = parse_api_scope(req.scope.as_deref())?;
    let (network, max_size, registry_url, configured_skills_dir) =
        mutation_context_settings(&state);
    let home = crate::config::effective_home_dir();
    let workspace = state.workspace.clone();

    let receipt = crate::skills::mutation::execute(
        SkillMutationRequest::UpdateByName {
            name: name.clone(),
            scope,
            expected_digest: req.expected_digest,
        },
        &MutationContext {
            workspace: &workspace,
            home: home.as_deref(),
            configured_skills_dir: configured_skills_dir.as_deref(),
            network: &network,
            max_size,
            registry_url: &registry_url,
        },
    )
    .await
    .map_err(|err| {
        let msg = err.to_string();
        if msg.contains("not found") {
            ApiError::not_found(format!("update failed: {err:#}"))
        } else {
            ApiError::bad_request(format!("update failed: {err:#}"))
        }
    })?;

    if outcome_is_policy_error(&receipt.outcome) {
        return Err(ApiError::forbidden(policy_error_message(&receipt.outcome)));
    }

    Ok(Json(receipt_to_response(&receipt)))
}

// ─── DELETE /v1/skills/{name} (uninstall) ──────────────────────────────────

async fn uninstall_skill_api(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    Query(query): Query<UninstallSkillQuery>,
) -> Result<Json<SkillMutationReceiptResponse>, ApiError> {
    use crate::skills::mutation::{MutationContext, SkillMutationRequest};

    let scope = parse_api_scope(query.scope.as_deref())?;
    let (network, max_size, registry_url, configured_skills_dir) =
        mutation_context_settings(&state);
    let home = crate::config::effective_home_dir();

    let receipt = crate::skills::mutation::execute_sync(
        SkillMutationRequest::RemoveByName {
            name: name.clone(),
            scope,
            expected_digest: query.expected_digest,
        },
        &MutationContext {
            workspace: &state.workspace,
            home: home.as_deref(),
            configured_skills_dir: configured_skills_dir.as_deref(),
            network: &network,
            max_size,
            registry_url: &registry_url,
        },
    )
    .map_err(|err| {
        let msg = err.to_string();
        if msg.contains("not found") {
            ApiError::not_found(format!("uninstall failed: {err:#}"))
        } else {
            ApiError::bad_request(format!("uninstall failed: {err:#}"))
        }
    })?;

    Ok(Json(receipt_to_response(&receipt)))
}

// ─── POST /v1/skills/{name}/trust ──────────────────────────────────────────

async fn trust_skill_api(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    Json(req): Json<TrustSkillRequest>,
) -> Result<Json<SkillMutationReceiptResponse>, ApiError> {
    use crate::skills::mutation::{MutationContext, SkillMutationRequest};

    let scope = parse_api_scope(req.scope.as_deref())?;
    let (network, max_size, registry_url, configured_skills_dir) =
        mutation_context_settings(&state);
    let home = crate::config::effective_home_dir();

    let receipt = crate::skills::mutation::execute_sync(
        SkillMutationRequest::TrustByName {
            name: name.clone(),
            scope,
            expected_digest: req.expected_digest,
        },
        &MutationContext {
            workspace: &state.workspace,
            home: home.as_deref(),
            configured_skills_dir: configured_skills_dir.as_deref(),
            network: &network,
            max_size,
            registry_url: &registry_url,
        },
    )
    .map_err(|err| {
        let msg = err.to_string();
        if msg.contains("not found") {
            ApiError::not_found(format!("trust failed: {err:#}"))
        } else {
            ApiError::bad_request(format!("trust failed: {err:#}"))
        }
    })?;

    Ok(Json(receipt_to_response(&receipt)))
}

// ─── GET /v1/skills/{name}/audit ───────────────────────────────────────────

async fn audit_skill_api(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    Query(query): Query<SkillScopeQuery>,
) -> Result<Json<SkillAuditResponse>, ApiError> {
    use crate::skills::audit::{
        AuditedSkill, DigestState, IntegrityState, SkillActionKind, SkillAuditMode,
        SkillAuditWarning, SkillSourceKind, TrustState, scan_with_configured,
    };
    use crate::skills::roots::SkillRootKind;

    let scope_filter = parse_api_scope(query.scope.as_deref())?;
    let home = crate::config::effective_home_dir();
    let configured_skills_dir = {
        let config = state.config.read();
        config.skills_dir.as_ref().map(PathBuf::from)
    };
    let canonical = crate::skills::normalize_skill_name_for_lookup(&name);

    let snap = scan_with_configured(
        &state.workspace,
        home.as_deref(),
        configured_skills_dir.as_deref(),
        SkillAuditMode::Compatible,
        None,
    );

    let mut matches: Vec<&AuditedSkill> = snap
        .skills
        .iter()
        .filter(|s| s.id.canonical_name == canonical)
        .collect();

    if let Some(scope) = scope_filter {
        let want = match scope {
            crate::skills::mutation::SkillTargetScope::Project => SkillRootKind::CodeWhaleProject,
            crate::skills::mutation::SkillTargetScope::Global => SkillRootKind::CodeWhaleGlobal,
        };
        matches.retain(|s| s.root.kind == want);
    }

    if matches.is_empty() {
        return Err(ApiError::not_found(format!(
            "skill '{name}' not found in any audited root"
        )));
    }

    let ambiguous = matches.len() > 1;
    let entries = matches
        .into_iter()
        .map(|skill| {
            let source_kind = match skill.source_kind {
                SkillSourceKind::CodeWhaleManaged => "codewhale_managed",
                SkillSourceKind::CodeWhaleManual => "codewhale_manual",
                SkillSourceKind::CompatibleExternal => "compatible_external",
                SkillSourceKind::BuiltIn => "built_in",
                SkillSourceKind::ReviewedPluginSnapshot => "reviewed_plugin_snapshot",
                SkillSourceKind::RegistryCache => "registry_cache",
            };
            let scope_str = match skill.root.kind {
                SkillRootKind::CodeWhaleProject => "project",
                SkillRootKind::CodeWhaleGlobal => "global",
                _ => "other",
            };
            let digest = match &skill.digest {
                DigestState::Known(v) => SkillAuditDigest {
                    state: "known".to_string(),
                    value: Some(v.clone()),
                },
                DigestState::Unknown(reason) => SkillAuditDigest {
                    state: format!("unknown:{reason:?}").to_ascii_lowercase(),
                    value: None,
                },
            };
            let trust = match &skill.trust {
                TrustState::TrustedForDigest(_) => "trusted_for_digest",
                TrustState::TrustStale => "trust_stale",
                TrustState::LegacyAdvisory => "legacy_advisory",
                TrustState::Untrusted => "untrusted",
                TrustState::NotApplicable => "not_applicable",
                TrustState::Unknown => "unknown",
            };
            let integrity = match &skill.integrity {
                IntegrityState::Healthy => "healthy",
                IntegrityState::LocalContentDrift => "local_content_drift",
                IntegrityState::BrokenManagedInstall => "broken_managed_install",
                IntegrityState::LegacyMetadataUnknown => "legacy_metadata_unknown",
                IntegrityState::Unknown => "unknown",
            };
            let available_actions = skill
                .available_actions
                .iter()
                .map(|a| match a {
                    SkillActionKind::Install => "install",
                    SkillActionKind::Import => "import",
                    SkillActionKind::Update => "update",
                    SkillActionKind::Remove => "remove",
                    SkillActionKind::Trust => "trust",
                })
                .map(str::to_string)
                .collect();
            let warnings = skill
                .warnings
                .iter()
                .map(|w| match w {
                    SkillAuditWarning::Message(m) => m.clone(),
                })
                .collect();
            SkillAuditEntry {
                name: skill.name.clone(),
                safe_display_path: skill.safe_display_path.clone(),
                source_kind: source_kind.to_string(),
                scope: scope_str.to_string(),
                digest,
                trust: trust.to_string(),
                integrity: integrity.to_string(),
                available_actions,
                warnings,
            }
        })
        .collect();

    Ok(Json(SkillAuditResponse {
        ambiguous,
        skills: entries,
    }))
}

#[derive(Debug, Deserialize)]
struct ApprovalsQuery {
    limit: Option<usize>,
}

/// One row of the account-wide approval history: what the agent asked
/// permission to do and what was decided. `decided_at` is `None` while the
/// ask is still pending.
#[derive(Debug, Serialize)]
struct ApprovalHistoryRow {
    approval_id: String,
    tool_name: String,
    /// Redacted host or workspace-relative path the ask was about. Absent on
    /// records written before targets were kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    /// Redacted one-line description of what was asked, at most 120
    /// characters. Absent on records written before summaries were kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<String>,
    outcome: String,
    /// `allow` or `deny` when a standing session rule or grant answered the
    /// ask rather than a person; absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    standing_rule: Option<&'static str>,
    /// Who resolved it: `user`, `session_rule`, `posture`, or `host`.
    /// Absent while pending and on records written before deciders were kept.
    #[serde(skip_serializing_if = "Option::is_none")]
    decided_by: Option<crate::approval_log::ApprovalDecider>,
    asked_at: chrono::DateTime<Utc>,
    decided_at: Option<chrono::DateTime<Utc>>,
}

fn approval_row_standing_rule(
    outcome: &crate::approval_log::ApprovalOutcome,
    decided_by: Option<crate::approval_log::ApprovalDecider>,
) -> Option<&'static str> {
    use crate::approval_log::{ApprovalDecider, ApprovalOutcome};
    match (outcome, decided_by) {
        (ApprovalOutcome::ApprovedOnce, Some(ApprovalDecider::SessionRule)) => Some("allow"),
        (ApprovalOutcome::Denied, Some(ApprovalDecider::SessionRule)) => Some("deny"),
        _ => None,
    }
}

fn approval_outcome_label(outcome: &crate::approval_log::ApprovalOutcome) -> &'static str {
    use crate::approval_log::ApprovalOutcome;
    match outcome {
        ApprovalOutcome::ApprovedOnce => "allowed_once",
        ApprovalOutcome::Denied => "denied",
        ApprovalOutcome::Timeout => "timeout",
        ApprovalOutcome::Cancelled => "cancelled",
        ApprovalOutcome::Unavailable => "unavailable",
        ApprovalOutcome::RetryWithPolicy { .. } => "retry_with_policy",
    }
}

/// Flatten one session's replay into history rows, newest ask first. Pending
/// asks sort by asked time alongside decided rows — they are the newest
/// entries while live, and sink into place once decided.
fn approval_history_rows(replay: &crate::approval_log::ApprovalReplay) -> Vec<ApprovalHistoryRow> {
    let mut rows: Vec<ApprovalHistoryRow> = replay
        .completed
        .iter()
        .map(|completed| {
            let asked_at = completed.ask.created_at();
            ApprovalHistoryRow {
                approval_id: completed.ask.approval_id().to_string(),
                tool_name: completed.ask.tool_name().unwrap_or("unknown").to_string(),
                target: completed.ask.target().map(str::to_string),
                summary: completed.ask.summary().map(str::to_string),
                outcome: approval_outcome_label(&completed.outcome).to_string(),
                standing_rule: approval_row_standing_rule(&completed.outcome, completed.decided_by),
                decided_by: completed.decided_by,
                asked_at,
                decided_at: Some(completed.decided_at),
            }
        })
        .chain(replay.unmatched_asks.iter().map(|ask| ApprovalHistoryRow {
            approval_id: ask.approval_id().to_string(),
            tool_name: ask.tool_name().unwrap_or("unknown").to_string(),
            target: ask.target().map(str::to_string),
            summary: ask.summary().map(str::to_string),
            outcome: "pending".to_string(),
            standing_rule: None,
            decided_by: None,
            asked_at: ask.created_at(),
            decided_at: None,
        }))
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.asked_at));
    rows
}

/// `GET /v1/approvals` — the read-only history behind the approvals log:
/// every decided approval plus every still-pending ask, newest first, across
/// all sessions. A corrupt session log is skipped with a warning, never a
/// 500 for the whole history; the warn names the file to inspect (#5931).
async fn list_approvals(
    State(state): State<RuntimeApiState>,
    Query(query): Query<ApprovalsQuery>,
) -> Result<Json<Vec<ApprovalHistoryRow>>, ApiError> {
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let sessions_dir = state.sessions_dir.clone();
    let mut rows = tokio::task::spawn_blocking(move || {
        let store = crate::approval_log::ApprovalReceiptStore::new(sessions_dir);
        let mut rows = Vec::new();
        for session_id in store.sessions_with_logs() {
            match store.replay(&session_id) {
                Ok(replay) => rows.extend(approval_history_rows(&replay)),
                Err(error) => tracing::warn!(
                    target: "approval",
                    error_kind = ?error.kind(),
                    %error,
                    session_id,
                    "skipping unreadable approval log in history listing",
                ),
            }
        }
        rows
    })
    .await
    .map_err(|error| ApiError::internal(format!("approval history read failed: {error}")))?;
    rows.sort_by_key(|row| std::cmp::Reverse(row.asked_at));
    rows.truncate(limit);
    Ok(Json(rows))
}

async fn decide_approval(
    State(state): State<RuntimeApiState>,
    Path(approval_id): Path<String>,
    Json(req): Json<DecideApprovalBody>,
) -> Result<Json<DecideApprovalResponse>, ApiError> {
    let decision = match req.decision.as_str() {
        "allow" => ExternalApprovalDecision::Allow {
            remember: req.remember,
        },
        "deny" => ExternalApprovalDecision::Deny {
            remember: req.remember,
        },
        other => {
            return Err(ApiError::bad_request(format!(
                "invalid decision '{other}'; expected \"allow\" or \"deny\""
            )));
        }
    };
    let delivered = state
        .runtime_threads
        .deliver_external_approval(&approval_id, decision);
    if !delivered {
        return Err(ApiError::not_found(format!(
            "no pending approval with id '{approval_id}'"
        )));
    }
    Ok(Json(DecideApprovalResponse {
        ok: true,
        approval_id,
        decision: req.decision,
        delivered,
    }))
}

/// `DELETE /v1/threads/{id}/approval-grants/{grant_id}` — revoke one
/// "allow for this conversation" grant. The next matching call prompts again.
async fn revoke_approval_grant(
    State(state): State<RuntimeApiState>,
    Path((thread_id, grant_id)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let revoked = state
        .runtime_threads
        .revoke_approval_grant(&thread_id, &grant_id)
        .await
        .map_err(map_thread_err)?;
    if !revoked {
        return Err(ApiError::not_found(format!(
            "no approval grant with id '{grant_id}' on thread '{thread_id}'"
        )));
    }
    Ok(Json(
        json!({ "ok": true, "grant_id": grant_id, "revoked": true }),
    ))
}

async fn submit_user_input(
    State(state): State<RuntimeApiState>,
    Path((thread_id, input_id)): Path<(String, String)>,
    Json(req): Json<SubmitUserInputBody>,
) -> Result<Json<SubmitUserInputResponse>, ApiError> {
    use crate::tools::user_input::{UserInputAnswer, UserInputResponse};
    let answers: Vec<UserInputAnswer> = req
        .answers
        .into_iter()
        .map(|a| UserInputAnswer {
            id: a.id,
            label: a.label,
            value: a.value,
        })
        .collect();
    let response = UserInputResponse { answers };
    let delivered = state
        .runtime_threads
        .submit_user_input(&thread_id, &input_id, response)
        .await
        .map_err(map_thread_err)?;
    if !delivered {
        return Err(ApiError::not_found(format!(
            "no pending user-input request with id '{input_id}'"
        )));
    }
    Ok(Json(SubmitUserInputResponse {
        ok: true,
        input_id,
        delivered,
    }))
}

async fn runtime_info(
    State(state): State<RuntimeApiState>,
    request: Request,
) -> Json<RuntimeInfoResponse> {
    let version = env!("CARGO_PKG_VERSION");
    let commit = option_env!("CODEWHALE_BUILD_COMMIT").unwrap_or("unknown");
    let api_base = runtime_account_api_base();
    let account = runtime_account_info_for_request(
        runtime_request_is_authorized(&request, &state),
        &api_base,
        || runtime_account_info(state.config_profile.as_deref(), &api_base),
    );
    Json(RuntimeInfoResponse {
        service: "codewhale-runtime-api",
        runtime_api_version: RUNTIME_API_VERSION,
        codewhale_version: version,
        codewhale_commit: commit,
        bind_host: state.bind_host.clone(),
        port: state.bind_port,
        auth_required: state.auth_required,
        transports: vec!["http", "sse"],
        capabilities: default_runtime_capabilities(),
        account,
        experimental: RuntimeExperimentalCapabilities::default(),
        version,
    })
}

fn runtime_account_info(profile: Option<&str>, api_base: &str) -> RuntimeAccountInfo {
    #[cfg(test)]
    {
        let _ = profile;
        RuntimeAccountInfo::signed_out(api_base.to_string())
    }

    #[cfg(not(test))]
    {
        secure_account_session_secrets()
            .and_then(|secrets| {
                AccountSessionStore::new(secrets, profile, api_base).runtime_info_at(Utc::now())
            })
            .unwrap_or_else(|_| RuntimeAccountInfo::signed_out(api_base.to_string()))
    }
}

fn runtime_account_info_for_request(
    authorized: bool,
    api_base: &str,
    load: impl FnOnce() -> RuntimeAccountInfo,
) -> RuntimeAccountInfo {
    if authorized {
        load()
    } else {
        RuntimeAccountInfo::signed_out(api_base.to_string())
    }
}

pub(crate) fn runtime_account_api_base() -> String {
    std::env::var(ACCOUNT_API_BASE_ENV)
        .ok()
        .and_then(|value| normalize_runtime_account_api_base(&value))
        .unwrap_or_else(|| DEFAULT_ACCOUNT_API_BASE.to_string())
}

fn normalize_runtime_account_api_base(value: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(value.trim()).ok()?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return None;
    }
    let host = url.host_str()?;
    let loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return None;
    }
    url.set_path("/");
    Some(url.as_str().trim_end_matches('/').to_string())
}

/// Ownership is derived using the same trust/precedence as the existing MCP
/// loader. A global editor must never silently change a shadowed project entry
/// or manufacture an override for a reviewed plugin component.
fn mcp_management_config(
    state: &RuntimeApiState,
) -> Result<
    (
        crate::mcp::McpConfig,
        std::collections::HashMap<String, &'static str>,
    ),
    ApiError,
> {
    let global_path = state.config.read().mcp_config_path();
    let plugins = state
        .plugin_discovery
        .registry_for_workspace(&state.workspace);
    let config = crate::mcp::load_config_with_workspace_and_plugins(
        &global_path,
        &state.workspace,
        plugins.as_ref(),
    )
    .map_err(|e| ApiError::internal(format!("Failed to load MCP config: {e}")))?;
    let global = crate::mcp::load_config(&global_path)
        .map_err(|e| ApiError::internal(format!("Failed to load MCP config: {e}")))?;
    let project_path = crate::mcp::workspace_mcp_config_path(&state.workspace);
    let same_source = project_path == global_path
        || project_path
            .canonicalize()
            .ok()
            .zip(global_path.canonicalize().ok())
            .is_some_and(|(project, global)| project == global);
    let project = if !same_source && crate::config::is_workspace_trusted(&state.workspace) {
        crate::mcp::load_config(&project_path)
            .map_err(|e| ApiError::internal(format!("Failed to load project MCP config: {e}")))?
    } else {
        crate::mcp::McpConfig::default()
    };
    let origins = config
        .servers
        .iter()
        .map(|(name, server)| {
            let origin = if server.reviewed_plugin.is_some() {
                "plugin"
            } else if project.servers.contains_key(name) {
                "project"
            } else if global.servers.contains_key(name) {
                "global"
            } else {
                "unknown"
            };
            (name.clone(), origin)
        })
        .collect();
    Ok((config, origins))
}

#[derive(Debug)]
struct McpManagementFailure(ApiError);
impl std::fmt::Display for McpManagementFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.message)
    }
}
impl std::error::Error for McpManagementFailure {}

fn mcp_mutation_error(error: anyhow::Error) -> ApiError {
    if error.is::<crate::mcp::McpRevisionConflict>() {
        ApiError {
            status: StatusCode::PRECONDITION_FAILED,
            message: error.to_string(),
            code: None,
        }
    } else if let Some(error) = error.downcast_ref::<McpManagementFailure>() {
        error.0.clone()
    } else {
        ApiError::internal(error.to_string())
    }
}

fn mcp_expected_revision(headers: &axum::http::HeaderMap) -> Result<String, ApiError> {
    let value = headers
        .get(header::IF_MATCH)
        .ok_or_else(|| ApiError {
            status: StatusCode::PRECONDITION_REQUIRED,
            message: "Read the MCP configuration and send its revision in If-Match before saving"
                .into(),
            code: None,
        })?
        .to_str()
        .map_err(|_| ApiError::bad_request("Invalid MCP revision"))?
        .trim();
    let value = if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        &value[1..value.len() - 1]
    } else {
        value
    };
    if value != "mcp-v1-absent"
        && !value.strip_prefix("mcp-v1-").is_some_and(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
    {
        return Err(ApiError::bad_request("Invalid MCP revision"));
    }
    Ok(value.to_owned())
}

async fn mutate_mcp_management<T: Send + 'static>(
    state: RuntimeApiState,
    headers: axum::http::HeaderMap,
    mutate: impl FnOnce(&RuntimeApiState, &mut crate::mcp::McpConfig) -> Result<T, ApiError>
    + Send
    + 'static,
) -> Result<(T, String), ApiError> {
    let expected = mcp_expected_revision(&headers)?;
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        state
            .workspace_scope
            .validate_sync()
            .map_err(|_| ApiError::conflict("selected workspace identity changed"))?;
        let path = state.config.read().mcp_config_path();
        let result = crate::mcp::mutate_config(&path, Some(&expected), |config| {
            mutate(&state, config).map_err(|error| anyhow::Error::new(McpManagementFailure(error)))
        })
        .map_err(mcp_mutation_error)?;
        state
            .workspace_scopes
            .mcp_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(result)
    })
    .await
    .map_err(|_| ApiError::internal("MCP configuration write failed"))?
}

async fn mcp_management_snapshot(
    state: RuntimeApiState,
) -> Result<
    (
        (
            crate::mcp::McpConfig,
            std::collections::HashMap<String, &'static str>,
        ),
        String,
    ),
    ApiError,
> {
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let path = state.config.read().mcp_config_path();
        codewhale_config::with_config_write_lock(&path, |path| {
            let config = mcp_management_config(&state)
                .map_err(|error| anyhow::Error::new(McpManagementFailure(error)))?;
            Ok((config, crate::mcp::read_config_revision(path)?))
        })
        .map_err(mcp_mutation_error)
    })
    .await
    .map_err(|_| ApiError::internal("MCP configuration read failed"))?
}

fn require_writable_mcp_server(state: &RuntimeApiState, name: &str) -> Result<(), ApiError> {
    let (_, origins) = mcp_management_config(state)?;
    match origins.get(name) {
        Some(&"global") => Ok(()),
        Some(origin) => Err(ApiError {
            status: StatusCode::CONFLICT,
            message: format!(
                "MCP server '{name}' is owned by {origin} configuration; manage it at its source"
            ),
            code: None,
        }),
        None => Err(ApiError::not_found(format!(
            "MCP server '{name}' not found"
        ))),
    }
}

async fn mcp_pool_handle(
    state: &RuntimeApiState,
    create: bool,
) -> Result<Option<Arc<Mutex<McpPool>>>, ApiError> {
    state
        .workspace_scope
        .validate()
        .await
        .map_err(|_| ApiError::conflict("selected workspace identity changed"))?;
    let mut slot = state.workspace_scope.mcp.lock().await;
    state
        .workspace_scope
        .validate()
        .await
        .map_err(|_| ApiError::conflict("selected workspace identity changed"))?;
    loop {
        let generation = state
            .workspace_scopes
            .mcp_generation
            .load(std::sync::atomic::Ordering::SeqCst);
        if let Some((admitted, pool)) = slot.as_ref() {
            if *admitted == generation {
                return Ok(Some(pool.clone()));
            }
            let mut held = pool.clone().lock_owned().await;
            let current = state.clone();
            #[cfg(test)]
            let env_ticket = crate::test_support::env_scope_ticket();
            codewhale_app_server::daemon_socket::owner_work(move || {
                #[cfg(test)]
                let _membership = crate::test_support::join_env_scope(env_ticket);
                // Keep the exact scope/owner alive through synchronous disk
                // reload even if this request is cancelled after admission.
                current.workspace_scope.validate_sync()?;
                let path = current.config.read().mcp_config_path();
                let plugins = current
                    .plugin_discovery
                    .registry_for_workspace(&current.workspace);
                held.switch_workspace_config_source(&path, &current.workspace, plugins)
            })
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
            slot.as_mut().expect("retained pool").0 = generation;
        } else if create {
            let current = state.clone();
            #[cfg(test)]
            let env_ticket = crate::test_support::env_scope_ticket();
            let pool = codewhale_app_server::daemon_socket::owner_work(move || {
                #[cfg(test)]
                let _membership = crate::test_support::join_env_scope(env_ticket);
                current.workspace_scope.validate_sync()?;
                let path = current.config.read().mcp_config_path();
                let plugins = current
                    .plugin_discovery
                    .registry_for_workspace(&current.workspace);
                let mut pool = McpPool::from_config_path_with_workspace_and_plugins(
                    &path,
                    &current.workspace,
                    plugins,
                )?
                .with_backend(crate::mcp::McpBackend::from_config(&current.config.read()));
                pool.dynamic_servers = current.workspace_scopes.dynamic_servers.clone();
                Ok(pool)
            })
            .await
            .map_err(|error| ApiError::internal(format!("Failed to load MCP config: {error}")))?;
            *slot = Some((generation, Arc::new(Mutex::new(pool))));
        } else {
            return Ok(None);
        }
        // A concurrent global mutation may have settled during disk work.
        // Repeat validation under the same pool; no second connection owner.
    }
}

fn mcp_connection_outcome(
    pool: &McpPool,
    server: &str,
    error: Option<&anyhow::Error>,
) -> McpConnectionOutcome {
    McpConnectionOutcome {
        server: server.to_owned(),
        connected: pool.connected_servers().contains(&server),
        auth_required: pool.server_needs_auth(server),
        error: error
            .map(|error| truncate_text(&crate::mcp::format_mcp_error_for_display(error), 2048)),
    }
}

async fn list_mcp_servers(
    State(state): State<RuntimeApiState>,
) -> Result<Json<McpServersResponse>, ApiError> {
    let ((config, origins), revision) = mcp_management_snapshot(state.clone()).await?;
    let handle = mcp_pool_handle(&state, false).await?;
    let pool = match handle.as_ref() {
        Some(handle) => Some(handle.lock().await),
        None => None,
    };
    state
        .workspace_scope
        .validate()
        .await
        .map_err(|_| ApiError::conflict("selected workspace identity changed"))?;
    let mut servers = Vec::new();
    for (name, server_cfg) in config.servers {
        let origin = origins.get(&name).copied().unwrap_or("unknown");
        servers.push(McpServerEntry {
            name: name.clone(),
            origin,
            writable: origin == "global",
            auth_required: pool
                .as_ref()
                .is_some_and(|pool| pool.server_needs_auth(&name)),
            enabled: server_cfg.is_enabled(),
            required: server_cfg.required,
            command: server_cfg.command.clone(),
            url: server_cfg.url.clone(),
            connected: pool
                .as_ref()
                .is_some_and(|pool| pool.connected_servers().contains(&name.as_str())),
            enabled_tools: server_cfg.enabled_tools.clone(),
            disabled_tools: server_cfg.disabled_tools.clone(),
        });
    }
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(McpServersResponse { servers, revision }))
}

async fn list_mcp_tools(
    State(state): State<RuntimeApiState>,
    Query(query): Query<McpToolsQuery>,
) -> Result<Json<McpToolsResponse>, ApiError> {
    // An explicit connection request must not inherit the tool dispatcher's
    // best-effort reload behavior: unreadable/revoked sources fail closed.
    let fresh_config = if query.connect {
        Some(mcp_management_config(&state)?.0)
    } else {
        None
    };
    let Some(pool_handle) = mcp_pool_handle(&state, query.connect).await? else {
        return Ok(Json(McpToolsResponse {
            tools: Vec::new(),
            connections: Vec::new(),
        }));
    };
    let mut pool = pool_handle.lock().await;
    state
        .workspace_scope
        .validate()
        .await
        .map_err(|_| ApiError::conflict("selected workspace identity changed"))?;
    if fresh_config
        .as_ref()
        .is_some_and(|config| !pool.config_matches(config))
    {
        let error =
            anyhow::anyhow!("MCP configuration changed; reload it before connecting this server");
        let names = query
            .server
            .clone()
            .map(|name| vec![name])
            .unwrap_or_else(|| pool.server_names());
        return Ok(Json(McpToolsResponse {
            tools: Vec::new(),
            connections: names
                .iter()
                .map(|name| mcp_connection_outcome(&pool, name, Some(&error)))
                .collect(),
        }));
    }
    let errors = if query.connect {
        if let Some(server) = query.server.as_deref() {
            match pool.get_or_connect(server).await {
                Ok(_) => Vec::new(),
                Err(error) => vec![(server.to_owned(), error)],
            }
        } else {
            pool.connect_all().await
        }
    } else {
        Vec::new()
    };
    let mut names = query
        .server
        .clone()
        .map(|name| vec![name])
        .unwrap_or_else(|| pool.server_names());
    for (server, _) in &errors {
        if !names.contains(server) {
            names.push(server.clone());
        }
    }
    names.sort();
    let connections = names
        .iter()
        .map(|name| {
            mcp_connection_outcome(
                &pool,
                name,
                errors
                    .iter()
                    .find(|(server, _)| server == name)
                    .map(|(_, error)| error),
            )
        })
        .collect();

    let mut tools = Vec::new();
    for (prefixed_name, tool) in pool.all_tools() {
        let Ok((server, name)) = pool.parse_prefixed_name(&prefixed_name) else {
            continue;
        };

        if let Some(filter) = query.server.as_deref()
            && server != filter
        {
            continue;
        }

        tools.push(McpToolEntry {
            server: server.to_string(),
            name: name.to_string(),
            prefixed_name,
            description: tool.description.clone(),
            input_schema: tool.input_schema.clone(),
        });
    }

    tools.sort_by(|a, b| a.server.cmp(&b.server).then_with(|| a.name.cmp(&b.name)));

    Ok(Json(McpToolsResponse { tools, connections }))
}

/// `GET /v1/apps/mcp/servers/{name}` — fetch a single server's redacted config.
async fn get_mcp_server(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
) -> Result<Json<McpServerDetail>, ApiError> {
    let ((config, origins), revision) = mcp_management_snapshot(state.clone()).await?;
    let server_cfg = config
        .servers
        .get(&name)
        .ok_or_else(|| ApiError::not_found(format!("MCP server '{name}' not found")))?;
    let handle = mcp_pool_handle(&state, false).await?;
    let pool = match handle.as_ref() {
        Some(handle) => Some(handle.lock().await),
        None => None,
    };
    state
        .workspace_scope
        .validate()
        .await
        .map_err(|_| ApiError::conflict("selected workspace identity changed"))?;
    let connected = pool
        .as_ref()
        .is_some_and(|pool| pool.connected_servers().contains(&name.as_str()));
    let mut detail = McpServerDetail::from_config(&name, server_cfg, connected, revision);
    detail.origin = origins.get(&name).copied().unwrap_or("unknown");
    detail.writable = detail.origin == "global";
    detail.auth_required = pool
        .as_ref()
        .is_some_and(|pool| pool.server_needs_auth(&name));
    Ok(Json(detail))
}

/// `POST /v1/apps/mcp/servers` — add a new server to the persistent config.
///
/// Body: JSON object with all `McpServerWriteRequest` fields **plus** a
/// required top-level `"name"` string that will be the server key.
async fn create_mcp_server(
    State(state): State<RuntimeApiState>,
    headers: axum::http::HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> Result<(StatusCode, Json<McpServerDetail>), ApiError> {
    let name = body
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("'name' is required"))?
        .to_string();

    if name.trim().is_empty() {
        return Err(ApiError::bad_request("'name' must not be empty"));
    }

    let req: McpServerWriteRequest = serde_json::from_value(body)
        .map_err(|e| ApiError::bad_request(format!("Invalid request body: {e}")))?;

    if req.command.as_ref().and_then(Option::as_ref).is_none()
        && req.url.as_ref().and_then(Option::as_ref).is_none()
    {
        return Err(ApiError::bad_request(
            "Either 'command' or 'url' is required to create an MCP server",
        ));
    }

    if let Some(Some(transport)) = &req.transport {
        crate::mcp::validate_mcp_transport(Some(transport.as_str()))
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
    }

    let new_cfg = mcp_server_config_from_write_request(req, None);
    let target_name = name.clone();
    let (new_cfg, revision) =
        mutate_mcp_management(state.clone(), headers, move |state, config| {
            if mcp_management_config(state)?
                .0
                .servers
                .contains_key(&target_name)
            {
                return Err(ApiError {
                    status: StatusCode::CONFLICT,
                    message: format!(
                        "MCP server '{target_name}' already exists in the effective configuration"
                    ),
                    code: None,
                });
            }
            config.servers.insert(target_name, new_cfg.clone());
            Ok(new_cfg)
        })
        .await?;

    // Invalidate the in-memory pool so the next tool call reloads from disk.

    Ok((
        StatusCode::CREATED,
        Json(McpServerDetail::from_config(
            &name, &new_cfg, false, revision,
        )),
    ))
}

/// `PATCH /v1/apps/mcp/servers/{name}` — update an existing server's config.
async fn update_mcp_server(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
    Json(req): Json<McpServerWriteRequest>,
) -> Result<Json<McpServerDetail>, ApiError> {
    if let Some(Some(transport)) = &req.transport {
        crate::mcp::validate_mcp_transport(Some(transport.as_str()))
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
    }

    let target_name = name.clone();
    let (updated_cfg, revision) = mutate_mcp_management(state.clone(), headers, move |state, cfg| {
        let name = target_name;
        require_writable_mcp_server(state, &name)?;
        let existing = cfg
            .servers
            .get_mut(&name)
            .ok_or_else(|| ApiError::not_found(format!("MCP server '{name}' not found")))?;
        let previous_target = (
            existing.command.clone(),
            existing.args.clone(),
            existing.url.clone(),
            existing.transport.clone(),
        );
        apply_write_request_to_config(req, existing);
        let target_changed = previous_target
            != (
                existing.command.clone(),
                existing.args.clone(),
                existing.url.clone(),
                existing.transport.clone(),
            );
        if target_changed && mcp_credential_configured(existing) {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                message: "Clear this connector's credential configuration before changing its command, arguments, URL, or transport; retained credentials cannot be forwarded to a different target".to_owned(),
                code: None,
            });
        }
        if existing.command.is_none() && existing.url.is_none() {
            return Err(ApiError::bad_request(
                "Either 'command' or 'url' must remain configured for an MCP server",
            ));
        }
        Ok(existing.clone())
    }).await?;

    // Invalidate the in-memory pool.

    Ok(Json(McpServerDetail::from_config(
        &name,
        &updated_cfg,
        false,
        revision,
    )))
}

/// `DELETE /v1/apps/mcp/servers/{name}` — remove a server from the persistent config.
async fn delete_mcp_server(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<McpServerActionReceipt>, ApiError> {
    let target_name = name.clone();
    let (_, revision) = mutate_mcp_management(state.clone(), headers, move |state, cfg| {
        require_writable_mcp_server(state, &target_name)?;
        cfg.servers
            .remove(&target_name)
            .ok_or_else(|| ApiError::not_found("MCP server not found"))?;
        Ok(())
    })
    .await?;

    // Invalidate the in-memory pool.

    Ok(Json(McpServerActionReceipt {
        revision: Some(revision),
        name,
        action: "deleted",
        ok: true,
        connection: None,
    }))
}

/// `POST /v1/apps/mcp/servers/{name}/enable` — enable a configured server.
async fn enable_mcp_server(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<McpServerActionReceipt>, ApiError> {
    let target_name = name.clone();
    let (_, revision) = mutate_mcp_management(state.clone(), headers, move |state, cfg| {
        require_writable_mcp_server(state, &target_name)?;
        let server = cfg
            .servers
            .get_mut(&target_name)
            .ok_or_else(|| ApiError::not_found("MCP server not found"))?;
        server.enabled = true;
        server.disabled = false;
        Ok(())
    })
    .await?;

    // Invalidate the in-memory pool so the enabled server participates next time.

    Ok(Json(McpServerActionReceipt {
        revision: Some(revision),
        name,
        action: "enabled",
        ok: true,
        connection: None,
    }))
}

/// `POST /v1/apps/mcp/servers/{name}/disable` — disable a configured server.
async fn disable_mcp_server(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<McpServerActionReceipt>, ApiError> {
    let target_name = name.clone();
    let (_, revision) = mutate_mcp_management(state.clone(), headers, move |state, cfg| {
        require_writable_mcp_server(state, &target_name)?;
        let server = cfg
            .servers
            .get_mut(&target_name)
            .ok_or_else(|| ApiError::not_found("MCP server not found"))?;
        server.enabled = false;
        server.disabled = true;
        Ok(())
    })
    .await?;

    // Invalidate the in-memory pool so the disabled server is excluded next time.

    Ok(Json(McpServerActionReceipt {
        revision: Some(revision),
        name,
        action: "disabled",
        ok: true,
        connection: None,
    }))
}

/// `POST /v1/apps/mcp/servers/{name}/reconnect` — retry only this server and
/// return the actual result without replacing healthy sibling connections.
async fn reconnect_mcp_server(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
) -> Result<Json<McpServerActionReceipt>, ApiError> {
    let (config, _) = mcp_management_config(&state)?;
    if !config.servers.contains_key(&name) {
        return Err(ApiError::not_found(format!(
            "MCP server '{name}' not found"
        )));
    }
    let handle = mcp_pool_handle(&state, true)
        .await?
        .ok_or_else(|| ApiError::internal("MCP pool unavailable"))?;
    let mut pool = handle.lock().await;
    state
        .workspace_scope
        .validate()
        .await
        .map_err(|_| ApiError::conflict("selected workspace identity changed"))?;
    let error = if !config.servers[&name].is_enabled() {
        Some(anyhow::anyhow!("MCP server '{name}' is disabled"))
    } else if !pool.config_matches(&config) {
        Some(anyhow::anyhow!(
            "MCP configuration changed; reload it before retrying this server"
        ))
    } else {
        pool.retry_connection(&name).await.err()
    };
    let connection = mcp_connection_outcome(&pool, &name, error.as_ref());
    Ok(Json(McpServerActionReceipt {
        revision: None,
        name,
        action: if error.is_none() {
            "reconnected"
        } else {
            "reconnect_failed"
        },
        ok: error.is_none() && connection.connected,
        connection: Some(connection),
    }))
}

/// Build a fresh [`McpServerConfig`] from a create request.
fn mcp_server_config_from_write_request(
    req: McpServerWriteRequest,
    _existing: Option<&crate::mcp::McpServerConfig>,
) -> crate::mcp::McpServerConfig {
    let enabled = req.enabled.unwrap_or(true);
    crate::mcp::McpServerConfig {
        command: req.command.flatten(),
        args: req.args.unwrap_or_default(),
        env: req.env.unwrap_or_default(),
        cwd: None,
        url: req.url.flatten(),
        transport: req.transport.flatten(),
        connect_timeout: req.connect_timeout.flatten(),
        execute_timeout: req.execute_timeout.flatten(),
        read_timeout: req.read_timeout.flatten(),
        disabled: !enabled,
        enabled,
        required: req.required.unwrap_or(false),
        lazy: false,
        enabled_tools: req.enabled_tools.unwrap_or_default(),
        disabled_tools: req.disabled_tools.unwrap_or_default(),
        headers: std::collections::HashMap::new(),
        env_headers: req.env_headers.unwrap_or_default(),
        bearer_token_env_var: req.bearer_token_env_var.flatten(),
        scopes: req.scopes.unwrap_or_default(),
        oauth: None,
        oauth_resource: req.oauth_resource.flatten(),
        reviewed_plugin: None,
        runtime_added: false,
        allow_private_network: false,
    }
}

/// Nonsecret indicator and retargeting guard. Treat environment and OAuth
/// configuration as authority even when it only references a credential.
fn mcp_credential_configured(cfg: &crate::mcp::McpServerConfig) -> bool {
    !cfg.env.is_empty()
        || !cfg.headers.is_empty()
        || !cfg.env_headers.is_empty()
        || cfg.bearer_token_env_var.is_some()
        || cfg.oauth.is_some()
        || !cfg.scopes.is_empty()
        || cfg.oauth_resource.is_some()
}

/// Apply a partial update from a PATCH request onto an existing config entry.
fn apply_write_request_to_config(
    req: McpServerWriteRequest,
    cfg: &mut crate::mcp::McpServerConfig,
) {
    if let Some(v) = req.command {
        cfg.command = v;
    }
    if let Some(v) = req.args {
        cfg.args = v;
    }
    if let Some(v) = req.env {
        cfg.env = v;
    }
    if let Some(v) = req.url {
        cfg.url = v;
    }
    if let Some(v) = req.transport {
        cfg.transport = v;
    }
    if let Some(v) = req.connect_timeout {
        cfg.connect_timeout = v;
    }
    if let Some(v) = req.execute_timeout {
        cfg.execute_timeout = v;
    }
    if let Some(v) = req.read_timeout {
        cfg.read_timeout = v;
    }
    if let Some(v) = req.enabled {
        cfg.enabled = v;
        cfg.disabled = !v;
    }
    if let Some(v) = req.required {
        cfg.required = v;
    }
    if let Some(v) = req.enabled_tools {
        cfg.enabled_tools = v;
    }
    if let Some(v) = req.disabled_tools {
        cfg.disabled_tools = v;
    }
    if let Some(v) = req.env_headers {
        cfg.env_headers = v;
    }
    if let Some(v) = req.bearer_token_env_var {
        cfg.bearer_token_env_var = v;
    }
    if let Some(v) = req.scopes {
        cfg.scopes = v;
    }
    if let Some(v) = req.oauth_resource {
        cfg.oauth_resource = v;
    }
}

async fn list_automations(
    State(state): State<RuntimeApiState>,
) -> Result<Json<Vec<AutomationRecord>>, ApiError> {
    let manager = state.automations.lock().await;
    let automations = manager
        .list_automations()
        .map_err(|e| ApiError::internal(format!("Failed to list automations: {e}")))?;
    Ok(Json(automations))
}

#[derive(Debug, Deserialize, Default)]
struct AutomationPreconditionQuery {
    #[serde(default)]
    expected_revision: Option<u64>,
}

static AUTOMATION_IDEMPOTENCY_GATES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>,
> = std::sync::LazyLock::new(Default::default);

async fn lock_automation_idempotency_key(scoped_key: &str) -> tokio::sync::OwnedMutexGuard<()> {
    let gate = {
        let mut gates = AUTOMATION_IDEMPOTENCY_GATES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        gates.retain(|_, gate| std::sync::Arc::strong_count(gate) > 1);
        gates.entry(scoped_key.to_string()).or_default().clone()
    };
    gate.lock_owned().await
}

fn automation_idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let key = value
        .to_str()
        .map_err(|_| ApiError::bad_request("Idempotency-Key must be visible ASCII"))?
        .trim();
    if key.is_empty() || key.len() > 255 || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(ApiError::bad_request(
            "Idempotency-Key must be 1-255 visible ASCII characters",
        ));
    }
    Ok(Some(key.to_string()))
}

async fn automation_mutation<T, Fut>(
    state: &RuntimeApiState,
    headers: &HeaderMap,
    operation: &str,
    request: &impl Serialize,
    ok_status: StatusCode,
    map_err: fn(anyhow::Error) -> ApiError,
    run: Fut,
) -> Result<Response, ApiError>
where
    T: Serialize,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let run = async {
        match run.await {
            Ok(value) => Ok(Ok(value)),
            Err(err) => match err.downcast_ref::<crate::automation_manager::RevisionConflict>() {
                Some(conflict) => Ok(Err(conflict.current)),
                None => Err(map_err(err)),
            },
        }
    };
    let conflict_response = |current: u64| {
        (
            StatusCode::CONFLICT,
            Json(json!({
                "error": {
                    "message": format!(
                        "Automation revision conflict: current revision is {current}"
                    ),
                    "status": 409,
                    "code": "revision_conflict",
                    "current_revision": current,
                }
            })),
        )
            .into_response()
    };
    let respond = |status: StatusCode, body: serde_json::Value, replayed: bool| {
        let mut response = (status, Json(body)).into_response();
        if replayed {
            response
                .headers_mut()
                .insert("idempotency-replayed", HeaderValue::from_static("true"));
        }
        response
    };
    let Some(key) = automation_idempotency_key(headers)? else {
        let value = match run.await? {
            Ok(value) => value,
            Err(current) => return Ok(conflict_response(current)),
        };
        let body = serde_json::to_value(value)
            .map_err(|e| ApiError::internal(format!("Failed to encode response: {e}")))?;
        return Ok(respond(ok_status, body, false));
    };
    let fingerprint = {
        use sha2::{Digest as _, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(operation.as_bytes());
        hasher.update([0]);
        hasher.update(serde_json::to_vec(request).unwrap_or_default());
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let key = format!("{operation}\n{key}");
    let _gate = lock_automation_idempotency_key(&key).await;
    let lookup = state
        .automations
        .lock()
        .await
        .idempotency_lookup(&key, &fingerprint)
        .map_err(|e| ApiError::internal(format!("Idempotency store unavailable: {e}")))?;
    match lookup {
        crate::automation_manager::IdempotencyLookup::Replay { status, body } => {
            let status = StatusCode::from_u16(status).unwrap_or(ok_status);
            return Ok(respond(status, body, true));
        }
        crate::automation_manager::IdempotencyLookup::KeyReused => {
            return Err(ApiError::unprocessable(
                "Idempotency-Key was already used for a different request",
            )
            .with_code("idempotency_key_reuse"));
        }
        crate::automation_manager::IdempotencyLookup::Miss => {}
    }
    let value = match run.await? {
        Ok(value) => value,
        Err(current) => return Ok(conflict_response(current)),
    };
    let body = serde_json::to_value(value)
        .map_err(|e| ApiError::internal(format!("Failed to encode response: {e}")))?;
    if let Err(error) = state.automations.lock().await.idempotency_store(
        &key,
        &fingerprint,
        ok_status.as_u16(),
        body.clone(),
    ) {
        tracing::error!(
            %error,
            operation,
            "automation mutation committed but its idempotency record could not be stored"
        );
    }
    Ok(respond(ok_status, body, false))
}

async fn create_automation(
    State(state): State<RuntimeApiState>,
    headers: HeaderMap,
    Json(req): Json<CreateAutomationRequest>,
) -> Result<Response, ApiError> {
    automation_mutation(
        &state,
        &headers,
        "POST /v1/automations",
        &req,
        StatusCode::CREATED,
        |e| ApiError::bad_request(e.to_string()),
        async {
            state
                .automations
                .lock()
                .await
                .create_automation(req.clone())
        },
    )
    .await
}

async fn get_automation(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<AutomationRecord>, ApiError> {
    let manager = state.automations.lock().await;
    let automation = manager.get_automation(&id).map_err(map_automation_err)?;
    Ok(Json(automation))
}

async fn update_automation(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(query): Query<AutomationPreconditionQuery>,
    headers: HeaderMap,
    Json(mut req): Json<UpdateAutomationRequest>,
) -> Result<Response, ApiError> {
    req.expected_revision = req.expected_revision.or(query.expected_revision);
    automation_mutation(
        &state,
        &headers,
        &format!("PATCH /v1/automations/{id}"),
        &req,
        StatusCode::OK,
        map_automation_err,
        async {
            state
                .automations
                .lock()
                .await
                .update_automation(&id, req.clone())
        },
    )
    .await
}

async fn delete_automation(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<AutomationRecord>, ApiError> {
    let manager = state.automations.lock().await;
    let automation = manager.delete_automation(&id).map_err(map_automation_err)?;
    Ok(Json(automation))
}

async fn run_automation(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(query): Query<AutomationPreconditionQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    automation_mutation(
        &state,
        &headers,
        &format!("POST /v1/automations/{id}/run"),
        &query.expected_revision,
        StatusCode::OK,
        map_automation_err,
        crate::automation_manager::run_now_shared_if(
            &state.automations,
            &id,
            query.expected_revision,
            &state.task_manager,
        ),
    )
    .await
}

async fn set_automation_status(
    state: RuntimeApiState,
    id: String,
    query: AutomationPreconditionQuery,
    headers: HeaderMap,
    action: &str,
    status: crate::automation_manager::AutomationStatus,
) -> Result<Response, ApiError> {
    let req = UpdateAutomationRequest {
        status: Some(status),
        expected_revision: query.expected_revision,
        ..UpdateAutomationRequest::default()
    };
    automation_mutation(
        &state,
        &headers,
        &format!("POST /v1/automations/{id}/{action}"),
        &query.expected_revision,
        StatusCode::OK,
        map_automation_err,
        async { state.automations.lock().await.update_automation(&id, req) },
    )
    .await
}

async fn pause_automation(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(query): Query<AutomationPreconditionQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    set_automation_status(
        state,
        id,
        query,
        headers,
        "pause",
        crate::automation_manager::AutomationStatus::Paused,
    )
    .await
}

async fn resume_automation(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(query): Query<AutomationPreconditionQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    set_automation_status(
        state,
        id,
        query,
        headers,
        "resume",
        crate::automation_manager::AutomationStatus::Active,
    )
    .await
}

async fn list_automation_runs(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(query): Query<AutomationRunsQuery>,
) -> Result<Json<Vec<AutomationRunRecord>>, ApiError> {
    let manager = state.automations.lock().await;
    let runs = if query.archived {
        let mut runs = manager
            .list_archived_runs(&id)
            .map_err(map_automation_err)?;
        if let Some(limit) = query.limit {
            runs.truncate(limit);
        }
        runs
    } else {
        manager
            .list_runs(&id, query.limit)
            .map_err(map_automation_err)?
    };
    Ok(Json(runs))
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct StartOperateRequest {
    #[serde(default)]
    direction: Option<String>,
    /// CWC `OperateBurnRate` object, positive $/hr number, or null (unbounded).
    #[serde(default)]
    burn_rate: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct KeepAliveOperateRequest {
    #[serde(default)]
    spent_usd: Option<f64>,
    #[serde(default)]
    observed_burn_usd_per_hour: Option<f64>,
    #[serde(default)]
    credentials_present: Option<bool>,
    #[serde(default)]
    human_gated: Option<bool>,
}

#[derive(Debug, Serialize)]
struct OperateView {
    /// `None` until an operation is actually started — a GET before that
    /// must not fabricate an identity the client can never mutate.
    operation: Option<crate::operate::Operation>,
    board: String,
}

fn operate_store() -> Result<crate::operate::OperationStore, ApiError> {
    crate::operate::OperationStore::open(crate::operate::default_operate_dir())
        .map_err(|e| ApiError::internal(format!("Failed to open operate store: {e}")))
}

async fn operate_readiness(state: &RuntimeApiState) -> Result<(String, bool), ApiError> {
    let config = state.config.read().clone();
    let manager = state.automations.lock().await;
    crate::operate::keepalive_readiness(&manager, &config, None)
        .map_err(|error| ApiError::bad_request(format!("Operate route unavailable: {error}")))
}

fn operate_view(operation: crate::operate::Operation) -> Json<OperateView> {
    Json(OperateView {
        board: crate::operate::render_plan_board(&operation),
        operation: Some(operation),
    })
}

fn load_operate(
    store: &crate::operate::OperationStore,
) -> Result<Option<crate::operate::Operation>, ApiError> {
    store
        .load()
        .map_err(|e| ApiError::internal(format!("Failed to load operate: {e}")))
}

fn parse_request_burn_rate(value: Option<&serde_json::Value>) -> Result<Option<f64>, ApiError> {
    Ok(crate::operate::parse_burn_rate(value)
        .map_err(|e| ApiError::bad_request(e.to_string()))?
        .map(|rate| rate.amount_usd_per_hour))
}

async fn get_operate(State(_state): State<RuntimeApiState>) -> Result<Json<OperateView>, ApiError> {
    let store = operate_store()?;
    match load_operate(&store)? {
        Some(operation) => Ok(operate_view(operation)),
        // No operation has been started: a fabricated `Operation::new` would
        // mint a fresh id and timestamps on every poll — phantom records the
        // client can neither patch nor cancel. `operation: null` is the
        // stable no-operation answer.
        None => Ok(Json(OperateView {
            operation: None,
            board: String::new(),
        })),
    }
}

async fn start_operate(
    State(state): State<RuntimeApiState>,
    Json(req): Json<StartOperateRequest>,
) -> Result<Json<OperateView>, ApiError> {
    let store = operate_store()?;
    let burn = parse_request_burn_rate(req.burn_rate.as_ref())?;
    // Keepalive first: a persisted operation without its keepalive is not
    // always-on, and a fresh operation has no lead plan yet — kick the first
    // lead run to the next scheduler tick instead of waiting out the hourly
    // recurrence.
    let config = state.config.read().clone();
    let (model, credentials) = {
        let manager = state.automations.lock().await;
        crate::operate::upsert_keepalive(&manager, &state.workspace, true, &config, None)
            .map_err(|e| ApiError::bad_request(format!("Failed to keep operate alive: {e}")))?
    };
    let operation = crate::operate::start_operation(
        &store,
        &state.workspace,
        req.direction,
        burn,
        credentials,
        &model,
    )
    .map_err(|e| ApiError::bad_request(e.to_string()))?;
    Ok(operate_view(operation))
}

async fn patch_operate(
    State(state): State<RuntimeApiState>,
    Json(patch): Json<serde_json::Value>,
) -> Result<Json<OperateView>, ApiError> {
    let store = operate_store()?;
    let (model, credentials) = operate_readiness(&state).await?;
    // Read-merge-write under the operate store lock: a concurrent keepalive
    // or plan save can no longer be lost by a stale read.
    let direction_changed = std::cell::Cell::new(false);
    let operation = store
        .mutate(|op| {
            let before = op.direction.clone();
            crate::operate::apply_operate_patch(op, &patch)?;
            direction_changed.set(op.direction != before);
            op.set_lead_model(&model);
            op.credentials_present = credentials;
            op.project();
            Ok(())
        })
        .map_err(|e| {
            if e.to_string().contains("cancelled") {
                ApiError::conflict(e.to_string())
            } else {
                ApiError::bad_request(e.to_string())
            }
        })?
        .ok_or_else(|| ApiError::not_found("Unknown Operation."))?;
    // A changed direction invalidated the lead plan; pull the keepalive lead
    // run forward so the operation does not idle until the next recurrence.
    if direction_changed.get() {
        let manager = state.automations.lock().await;
        crate::operate::kick_keepalive(&manager)
            .map_err(|e| ApiError::internal(format!("Failed to reschedule operate: {e}")))?;
    }
    Ok(operate_view(operation))
}

async fn keepalive_operate(
    State(state): State<RuntimeApiState>,
    Json(req): Json<KeepAliveOperateRequest>,
) -> Result<Json<OperateView>, ApiError> {
    let store = operate_store()?;
    let (model, credentials) = match req.credentials_present {
        Some(observed) => (None, observed),
        None => {
            let (model, credentials) = operate_readiness(&state).await?;
            (Some(model), credentials)
        }
    };
    let operation = store
        .mutate(|op| {
            if let Some(model) = &model {
                op.set_lead_model(model);
            }
            crate::operate::keep_alive_observation(
                op,
                req.observed_burn_usd_per_hour,
                req.spent_usd,
                Some(credentials),
                req.human_gated,
            );
            Ok(())
        })
        .map_err(|e| ApiError::internal(format!("Failed to keep operate alive: {e}")))?
        .ok_or_else(|| ApiError::not_found("Unknown Operation."))?;
    Ok(operate_view(operation))
}

async fn put_operate_plan(
    Json(plan): Json<serde_json::Value>,
) -> Result<Json<OperateView>, ApiError> {
    let store = operate_store()?;
    let patch = serde_json::json!({ "leadPlan": plan });
    let operation = store
        .mutate(|op| crate::operate::apply_operate_patch(op, &patch))
        .map_err(|e| {
            if e.to_string().contains("cancelled") {
                ApiError::conflict(e.to_string())
            } else if e.to_string().contains("leadPlan") {
                ApiError::bad_request(e.to_string())
            } else {
                ApiError::internal(format!("Failed to save operate plan: {e}"))
            }
        })?
        .ok_or_else(|| ApiError::not_found("Unknown Operation."))?;
    Ok(operate_view(operation))
}

async fn cancel_operate(
    State(state): State<RuntimeApiState>,
) -> Result<Json<OperateView>, ApiError> {
    let store = operate_store()?;
    let operation = crate::operate::cancel_operation(&store)
        .map_err(|e| ApiError::internal(format!("Failed to cancel operate: {e}")))?
        .ok_or_else(|| ApiError::not_found("Unknown Operation."))?;
    // Cancel tears down the keepalive too: an unattended hourly lead run
    // after cancel is pure cost.
    {
        let manager = state.automations.lock().await;
        crate::operate::pause_keepalive(&manager)
            .map_err(|e| ApiError::internal(format!("Failed to pause operate keepalive: {e}")))?;
    }
    Ok(operate_view(operation))
}

#[derive(Debug, Deserialize)]
struct OperateAutoMergeCheckRequest {
    repo: String,
    pr: String,
    agent: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct OperateAutoMergeCheckView {
    allow: bool,
    reason: Option<String>,
    checker: Option<String>,
    check_args: Vec<String>,
    merge_args: Vec<String>,
}

async fn check_operate_auto_merge(
    State(state): State<RuntimeApiState>,
    Json(req): Json<OperateAutoMergeCheckRequest>,
) -> Result<Json<OperateAutoMergeCheckView>, ApiError> {
    crate::operate::validate_auto_merge_request(&crate::operate::AutoMergeRequest {
        repo: &req.repo,
        pr: &req.pr,
        role: &req.agent,
    })
    .map_err(ApiError::bad_request)?;
    let checker = crate::operate::discover_auto_merge_checker(&state.workspace);
    let repo = req.repo.clone();
    let pr = req.pr.clone();
    let agent = req.agent.clone();
    let checker_for_task = checker.clone();
    // The checker shells out synchronously (`python3 …; .status()`); run it on
    // the blocking pool so a slow `gh`/network wait cannot pin a Tokio worker.
    let decision = tokio::task::spawn_blocking(move || {
        crate::operate::evaluate_auto_merge(
            crate::operate::AutoMergeRequest {
                repo: &repo,
                pr: &pr,
                role: &agent,
            },
            checker_for_task.as_deref(),
        )
    })
    .await
    .map_err(|e| ApiError::internal(format!("auto-merge check join failed: {e}")))?;
    let (allow, reason) = match decision {
        crate::operate::AutoMergeDecision::Allow => (true, None),
        crate::operate::AutoMergeDecision::Deny { reason } => (false, Some(reason)),
    };
    Ok(Json(OperateAutoMergeCheckView {
        allow,
        reason,
        checker: checker.as_ref().map(|path| path.display().to_string()),
        check_args: crate::operate::check_auto_merge_args(&req.repo, &req.pr, &req.agent),
        merge_args: crate::operate::auto_merge_pr_args(&req.repo, &req.pr, &req.agent),
    }))
}

async fn get_thread(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<ThreadDetail>, ApiError> {
    let detail = state
        .runtime_threads
        .get_thread_detail(&id)
        .await
        .map_err(map_thread_err)?;
    Ok(Json(detail))
}

/// Response for `GET /v1/threads/{id}/usage`.
///
/// Thin adapter over `RuntimeThreadManager::aggregate_usage_for_thread`: the
/// GUI's session-cost surface reads provider-aware, recorded-time pricing in
/// both published currencies from the same accumulation that powers
/// `/v1/usage`, instead of reimplementing rate tables client-side.
#[derive(Debug, Serialize)]
struct ThreadUsageResponse {
    thread_id: String,
    totals: UsageTotals,
}

async fn get_thread_usage(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<ThreadUsageResponse>, ApiError> {
    let totals = state
        .runtime_threads
        .aggregate_usage_for_thread(&id)
        .await
        .map_err(map_thread_err)?
        .combined();
    Ok(Json(ThreadUsageResponse {
        thread_id: id,
        totals,
    }))
}

/// `GET /v1/threads/{id}/receipt` — what the thread did, built by the one
/// receipt builder from the thread snapshot and its `approval.*` events
/// (`docs/RECEIPTS.md`). Read-only.
async fn get_thread_receipt(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<crate::receipts::Receipt>, ApiError> {
    thread_receipt(&state, &id, None).await.map(Json)
}

/// `GET /v1/threads/{id}/turns/{turn_id}/receipt` — the same receipt scoped
/// to one turn.
async fn get_turn_receipt(
    State(state): State<RuntimeApiState>,
    Path((id, turn_id)): Path<(String, String)>,
) -> Result<Json<crate::receipts::Receipt>, ApiError> {
    thread_receipt(&state, &id, Some(&turn_id)).await.map(Json)
}

async fn thread_receipt(
    state: &RuntimeApiState,
    id: &str,
    turn: Option<&str>,
) -> Result<crate::receipts::Receipt, ApiError> {
    let detail = state
        .runtime_threads
        .get_thread_detail(id)
        .await
        .map_err(map_thread_err)?;
    let events = state
        .runtime_threads
        .events_since_async(id, None)
        .await
        .map_err(map_thread_err)?;
    crate::receipts::thread_receipt(&detail.thread, &detail.turns, &detail.items, &events, turn)
        .map_err(|error| ApiError::not_found(error.to_string()))
}

async fn update_thread(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateThreadRequest>,
) -> Result<Json<ThreadRecord>, ApiError> {
    let thread = state
        .runtime_threads
        .update_thread_with_shell_policy(
            &id,
            req,
            state.config_path.as_deref(),
            state.config_profile.as_deref(),
        )
        .await
        .map_err(map_thread_err)?;
    Ok(Json(thread))
}

async fn resume_thread(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<ThreadRecord>, ApiError> {
    let thread = state
        .runtime_threads
        .resume_thread(&id)
        .await
        .map_err(map_thread_err)?;
    Ok(Json(thread))
}

async fn fork_thread(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<ThreadRecord>), ApiError> {
    let thread = state
        .runtime_threads
        .fork_thread_in_sessions_dir(&id, &state.sessions_dir)
        .await
        .map_err(map_thread_err)?;
    Ok((StatusCode::CREATED, Json(thread)))
}

#[derive(Debug, Deserialize)]
struct UndoTurnRequest {
    /// How many turns back to undo (default 0 = last turn only).
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(Debug, Serialize)]
struct UndoTurnResponse {
    /// The new forked thread (with the last N turns removed).
    thread: ThreadRecord,
    /// The original user message text from the first dropped turn,
    /// so the GUI can pre-populate the input box.
    original_user_text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    original_user_images: Vec<codewhale_protocol::runtime::RuntimeImageInput>,
}

async fn undo_thread_turn(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<UndoTurnRequest>,
) -> Result<(StatusCode, Json<UndoTurnResponse>), ApiError> {
    let depth = req.depth.unwrap_or(0);
    let (forked_thread, original_user_text, original_user_images, _) = state
        .runtime_threads
        .fork_at_user_message_in_sessions_dir(&id, depth, &state.sessions_dir)
        .await
        .map_err(map_thread_err)?;
    Ok((
        StatusCode::CREATED,
        Json(UndoTurnResponse {
            thread: forked_thread,
            original_user_text,
            original_user_images,
        }),
    ))
}

#[derive(Debug, Deserialize)]
struct ForkAtTurnRequest {
    /// The user turn to fork at, as `GET /v1/threads/{id}` reports it. The
    /// fork keeps that turn and every turn before it, and drops the rest.
    turn_id: String,
}

/// Fork a thread at one named user turn — the client-side "continue from this
/// turn" affordance, which carries on in the new thread.
///
/// The fork keeps the named turn and everything before it, so the branch point
/// is the answer a person is looking at rather than the question above it;
/// naming the last turn keeps the whole conversation. The receipt is
/// deliberately the undo receipt: the first dropped turn's prompt comes back
/// with the new thread, so a client can put what was asked next into the
/// composer and let the person edit or replace it. The source thread, its
/// session document and the workspace are untouched — no file rollback happens
/// here, because the branch that was left behind shares the workspace.
async fn fork_thread_at_turn(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<ForkAtTurnRequest>,
) -> Result<(StatusCode, Json<UndoTurnResponse>), ApiError> {
    let (forked_thread, original_user_text, original_user_images, _) = state
        .runtime_threads
        .fork_at_user_turn_in_sessions_dir(&id, &req.turn_id, &state.sessions_dir)
        .await
        .map_err(map_thread_err)?;
    Ok((
        StatusCode::CREATED,
        Json(UndoTurnResponse {
            thread: forked_thread,
            original_user_text,
            original_user_images,
        }),
    ))
}

/// Result of the snapshot-based file rollback step of patch-undo, reported
/// alongside the new forked thread.
#[derive(Debug, Serialize)]
struct PatchUndoResult {
    /// Whether files were restored from a snapshot.
    files_restored: bool,
    /// Human-readable summary: one `<action> <path>` line per restored file,
    /// or why nothing needed restoring.
    summary: Option<String>,
    /// Label of the pre-turn snapshot the files went back to (e.g.
    /// "pre-turn:3: fix the parser").
    snapshot_label: Option<String>,
}

#[derive(Debug, Serialize)]
struct PatchUndoResponse {
    /// Result of the snapshot-based file rollback step.
    patch_result: PatchUndoResult,
    /// The new forked thread (with the last turn removed).
    thread: ThreadRecord,
    /// The original user text from the removed turn (for re-editing).
    original_user_text: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    original_user_images: Vec<codewhale_protocol::runtime::RuntimeImageInput>,
}

async fn patch_undo_thread_turn(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<UndoTurnRequest>,
) -> Result<(StatusCode, Json<PatchUndoResponse>), ApiError> {
    let depth = req.depth.unwrap_or(0);
    // Admission first, then the thread record under it: trust, session
    // binding and workspace are the values that hold while files change.
    // Active turns in an overlapping workspace are rejected (409). The wait
    // for admission stays on the request so a client that gives up while
    // queued cancels its undo instead of leaving it queued behind the next
    // one and walking the workspace back twice.
    let (reservation, thread) = state
        .runtime_threads
        .thread_restore_guard(&id)
        .await
        .map_err(map_thread_err)?;
    // Once admitted, own the operation even when the HTTP caller disconnects:
    // the reservation must outlive both the file mutation and the fork
    // publication, so a dropped connection cannot release it mid-Git.
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::spawn(async move {
        let reservation = reservation;
        // Validate depth/history before touching any file, so an invalid
        // undo request cannot leave a half-applied workspace.
        let prepared = state
            .runtime_threads
            .prepare_fork_at_user_message_in_sessions_dir(&id, depth, &state.sessions_dir)
            .await
            .map_err(map_thread_err)?;
        // File rollback is a workspace mutation, so it needs the trust the
        // TUI's `/undo` requires. Read from the thread's own record: the
        // client does not get to assert it.
        let trusted = thread.trust_mode || thread.auto_approve;
        let workspace = thread.workspace.clone();
        // The restore points come from the dropped turns' own records, not
        // from the thread's saved-session binding or a scan of the shared
        // snapshot store: those are the snapshots this thread owns.
        let dropped_turns = prepared.dropped_turns().to_vec();
        // Step 1: snapshot-based file rollback. The `?` is deliberate: a
        // refusal or a failed restore aborts *before* the conversation is
        // forked, so the turn never disappears while its file changes stay.
        let patch_result = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let _membership = crate::test_support::join_env_scope(env_ticket);
            patch_undo_workspace_files(&workspace, &dropped_turns, trusted)
        })
        .await
        .map_err(|e| ApiError::internal(format!("Patch undo task failed: {e}")))??;
        // Step 2: publish the already-validated fork.
        let (forked_thread, original_user_text, original_user_images, _) = state
            .runtime_threads
            .publish_prepared_fork(prepared)
            .await
            .map_err(|error| {
                if patch_result.files_restored {
                    ApiError::internal(format!(
                        "Workspace files were restored from snapshot {}, but the conversation fork could not be saved: {error}. The original thread still holds the undone turn; the `pre-restore:` safety snapshot holds the files as they were before this undo.",
                        patch_result
                            .snapshot_label
                            .as_deref()
                            .unwrap_or("(unknown)")
                    ))
                } else {
                    map_thread_err(error)
                }
            })?;
        drop(reservation);
        Ok((
            StatusCode::CREATED,
            Json(PatchUndoResponse {
                patch_result,
                thread: forked_thread,
                original_user_text,
                original_user_images,
            }),
        ))
    })
    .await
    .map_err(|e| ApiError::internal(format!("Patch undo task failed: {e}")))?
}

/// Error codes a patch-undo refusal carries in `error.code`. A client offers a
/// conversation-only `POST /v1/threads/{id}/undo` for the first four.
const PATCH_UNDO_NO_RESTORE_POINT: &str = "restore_point_unavailable";
const PATCH_UNDO_RESTORE_POINT_PRUNED: &str = "restore_point_pruned";
const PATCH_UNDO_PATH_NOT_SNAPSHOTTED: &str = "path_not_snapshotted";
const PATCH_UNDO_WORKSPACE_CHANGED: &str = "workspace_changed_since_turn";
const PATCH_UNDO_UNTRUSTED: &str = "restore_requires_trust";
const PATCH_UNDO_WORKSPACE_UNAVAILABLE: &str = "workspace_unavailable";

/// One pre-turn → post-turn window of a dropped turn, resolved to the trees
/// the snapshot store still holds.
struct UndoSegment {
    turn_id: String,
    pre: crate::snapshot::SnapshotId,
    post: crate::snapshot::SnapshotId,
    pre_label: String,
    /// Paths that changed in the window only while one of the turn's own
    /// tool calls was running: the turn's changes.
    owned: std::collections::BTreeSet<PathBuf>,
    /// Paths that changed in the window while none of the turn's tools could
    /// have written them: someone else's changes.
    foreign: std::collections::BTreeSet<PathBuf>,
}

/// Pair each `pre_turn` receipt of a turn with the `post_turn` receipt that
/// closes it, in recorded order, as index ranges into `snapshots` (the
/// `tool`/`post_tool` receipts between them are the window's inner spans). A
/// turn can hold more than one window (a shell turn and a model turn under
/// one runtime turn); a `post_turn` with no open window cannot belong to this
/// turn and is skipped. `None` means a window the engine never closed (the
/// turn died before its post-turn snapshot) or no window at all.
fn turn_snapshot_windows(
    snapshots: &[crate::snapshot::WorkspaceSnapshotRef],
) -> Option<Vec<std::ops::RangeInclusive<usize>>> {
    use crate::snapshot::WorkspaceSnapshotKind;
    let mut windows = Vec::new();
    let mut open = None;
    for (index, snapshot) in snapshots.iter().enumerate() {
        match snapshot.kind {
            WorkspaceSnapshotKind::PreTurn => {
                if open.is_some() {
                    return None;
                }
                open = Some(index);
            }
            WorkspaceSnapshotKind::PostTurn => {
                if let Some(pre) = open.take() {
                    windows.push(pre..=index);
                }
            }
            WorkspaceSnapshotKind::Tool | WorkspaceSnapshotKind::PostTool => {}
        }
    }
    (open.is_none() && !windows.is_empty()).then_some(windows)
}

/// A path a file tool declared it writes, as a workspace-relative path the
/// snapshots would hold, or `None` when it is outside the workspace.
fn declared_write_path(workspace: &FsPath, raw: &str) -> Option<PathBuf> {
    use std::path::Component;
    let candidate = FsPath::new(raw);
    let rel = if candidate.is_absolute() {
        let canonical_workspace = workspace.canonicalize().ok();
        let canonical_candidate = candidate
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .zip(candidate.file_name())
            .map(|(parent, name)| parent.join(name));
        [Some(workspace), canonical_workspace.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|root| {
                candidate
                    .strip_prefix(root)
                    .ok()
                    .or_else(|| canonical_candidate.as_deref()?.strip_prefix(root).ok())
                    .map(FsPath::to_path_buf)
            })?
    } else {
        candidate.to_path_buf()
    };
    let rel: PathBuf = rel
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect();
    crate::snapshot::workspace_relative_path(workspace, rel.to_str()?)
}

/// Who could have changed the workspace in the span after one receipt.
enum SpanWriter {
    /// None of the turn's tool calls was running.
    Nobody,
    /// A call whose writes are not declared (a shell command, a program).
    Undeclared,
    /// A file tool that declared exactly these paths.
    Declared(std::collections::BTreeSet<PathBuf>),
}

/// Split a window's changes into the turn's own and everyone else's, from
/// the spans its receipts bound: a path belongs to the turn only if it
/// changed while one of the turn's tool calls was running and, for a file
/// tool, is one the call declared. A span whose changes were not recorded
/// (a snapshot in it failed) cannot be attributed and fails closed.
fn attribute_window(
    workspace: &FsPath,
    turn_id: &str,
    receipts: &[crate::snapshot::WorkspaceSnapshotRef],
) -> Result<
    (
        std::collections::BTreeSet<PathBuf>,
        std::collections::BTreeSet<PathBuf>,
    ),
    ApiError,
> {
    use crate::snapshot::WorkspaceSnapshotKind;
    let mut owned = std::collections::BTreeSet::new();
    let mut foreign = std::collections::BTreeSet::new();
    // Undeclared calls whose `post_tool` receipt is still ahead: everything
    // up to it (a program's nested calls included) is theirs.
    let mut open_undeclared: Vec<&str> = Vec::new();
    let mut writer = SpanWriter::Nobody;
    for (index, receipt) in receipts.iter().enumerate() {
        if index > 0 {
            let Some(changed) = receipt.changed_paths.as_ref() else {
                return Err(no_restore_point(
                    turn_id,
                    "has an incomplete record of what changed while it ran (a snapshot during the turn failed), so its changes cannot be told apart from anyone else's",
                ));
            };
            for path in changed {
                let path = PathBuf::from(path);
                match &writer {
                    SpanWriter::Undeclared => {
                        owned.insert(path);
                    }
                    SpanWriter::Declared(declared) if declared.contains(&path) => {
                        owned.insert(path);
                    }
                    SpanWriter::Declared(_) | SpanWriter::Nobody => {
                        foreign.insert(path);
                    }
                }
            }
        }
        match receipt.kind {
            WorkspaceSnapshotKind::Tool => {
                if receipt.write_paths.is_none()
                    && let Some(call) = receipt.tool_call_id.as_deref()
                    && receipts[index + 1..].iter().any(|later| {
                        later.kind == WorkspaceSnapshotKind::PostTool
                            && later.tool_call_id.as_deref() == Some(call)
                    })
                {
                    open_undeclared.push(call);
                }
            }
            WorkspaceSnapshotKind::PostTool => {
                if let Some(call) = receipt.tool_call_id.as_deref() {
                    open_undeclared.retain(|open| *open != call);
                }
            }
            WorkspaceSnapshotKind::PreTurn | WorkspaceSnapshotKind::PostTurn => {}
        }
        writer = if !open_undeclared.is_empty() {
            SpanWriter::Undeclared
        } else {
            match receipt.kind {
                // A shell turn: its command runs from the pre-turn snapshot.
                WorkspaceSnapshotKind::PreTurn if receipt.tool_call_id.is_some() => {
                    SpanWriter::Undeclared
                }
                WorkspaceSnapshotKind::Tool => match receipt.write_paths.as_ref() {
                    Some(paths) => SpanWriter::Declared(
                        paths
                            .iter()
                            .filter_map(|raw| declared_write_path(workspace, raw))
                            .collect(),
                    ),
                    None => SpanWriter::Undeclared,
                },
                _ => SpanWriter::Nobody,
            }
        };
    }
    Ok((owned, foreign))
}

fn no_restore_point(turn_id: &str, why: &str) -> ApiError {
    ApiError::conflict(format!(
        "Turn {turn_id} {why}, so its workspace changes cannot be restored; nothing was changed. \
         Use POST /v1/threads/{{id}}/undo for a conversation-only undo."
    ))
    .with_code(PATCH_UNDO_NO_RESTORE_POINT)
}

/// Roll the workspace files back to where they were before the first dropped
/// turn — only the files the dropped turns changed, and only when nothing
/// else changed them since.
///
/// # Ownership
///
/// The restore points are the `pre_turn`/`post_turn` receipts recorded on the
/// dropped turns themselves (`TurnRecord::workspace_snapshots`), resolved by
/// tree and session tag against the snapshot store. Nothing is selected by
/// scanning the shared store, so another thread's (or the TUI's) snapshots in
/// the same workspace are never candidates, and a fork restores the turns it
/// inherited because it carries their records.
///
/// # What is restored
///
/// For each dropped turn's window, the paths that differ between its
/// pre-turn and post-turn snapshots are candidates, and each must be the
/// turn's own: it changed only while one of the turn's tool calls was running
/// (the engine bounds every call that may write with a `tool` and a
/// `post_tool` snapshot and records what changed in each span), and, for a
/// file tool, it is a path the call declared. A path that changed while none
/// of the turn's tools could have written it — another thread, an editor, a
/// background process — is someone else's change: the undo is refused rather
/// than revert it. Each of the turn's paths goes back to its content before
/// the first dropped turn that changed it, and nothing outside that set is
/// touched, so later work survives. The whole turn goes, not just its last
/// write.
///
/// A path a file tool declared that the snapshots cannot hold (ignored by
/// `.gitignore` or the built-in exclusions, or outside the workspace) is
/// refused too: no snapshot can put it back, so "nothing to restore" would
/// be a lie.
///
/// # The rollback contract
///
/// `Ok` is a decision the conversation fork may proceed on: either the files
/// were restored, or there was *provably* nothing to restore (the dropped
/// turns ran here without tools, changed no files, or the files are back at
/// their pre-turn state). `Err` aborts the whole undo, and the caller must not
/// fork either: a turn that has no recorded restore point, a pruned restore
/// point, or a path changed since the turn is a `409` with a stable
/// `error.code`, never a `201` that forks while the files stay changed.
///
/// `trusted` mirrors the gate the TUI's `patch_undo()` applies
/// (`yolo || trust_mode`), evaluated once a real change is known.
fn patch_undo_workspace_files(
    workspace: &FsPath,
    dropped_turns: &[crate::runtime_threads::DroppedTurnSnapshots],
    trusted: bool,
) -> Result<PatchUndoResult, ApiError> {
    // An unreadable workspace directory (unmounted volume, disconnected
    // share, permissions) proves nothing about the files a turn changed, so
    // the conversation is not forked away from them.
    if !workspace.is_dir() {
        return Err(ApiError::conflict(format!(
            "Workspace directory {} is not available; mount or restore it before undoing files, or use /undo for a conversation-only undo.",
            workspace.display()
        ))
        .with_code(PATCH_UNDO_WORKSPACE_UNAVAILABLE));
    }

    // Which windows matter. A turn that ran no tool changed no file; a turn
    // that did must have recorded where it started and ended.
    let mut windows = Vec::new();
    for turn in dropped_turns {
        if !turn.may_change_files {
            continue;
        }
        if turn.snapshots.is_empty() {
            return Err(no_restore_point(
                &turn.turn_id,
                "has no recorded workspace restore point (it predates restore-point receipts, was imported from a saved session, or ran with snapshots off or unavailable)",
            ));
        }
        let Some(turn_windows) = turn_snapshot_windows(&turn.snapshots) else {
            return Err(no_restore_point(
                &turn.turn_id,
                "has no complete pre-turn/post-turn restore point (its snapshot failed or the turn stopped before it was taken)",
            ));
        };
        windows.extend(
            turn_windows
                .into_iter()
                .map(|range| (turn, &turn.snapshots[range])),
        );
    }
    if windows.is_empty() {
        return Ok(PatchUndoResult {
            files_restored: false,
            summary: Some(
                "The undone turn(s) ran no tools, so they changed no workspace files; nothing to restore."
                    .to_string(),
            ),
            snapshot_label: None,
        });
    }

    // Every repository failure is operational and aborts: "nothing to
    // restore" cannot be proven while Git is unavailable.
    let repo = crate::snapshot::SnapshotRepo::open_or_init(workspace).map_err(|e| {
        ApiError::internal(format!(
            "Snapshot repo unavailable; conversation preserved: {e}"
        ))
    })?;
    // Resolve by id against the whole store — no listing cap, so an old but
    // retained restore point is never mistaken for a pruned one.
    let listed = repo
        .list(usize::MAX)
        .map_err(|e| ApiError::internal(format!("Failed to list snapshots: {e}")))?;
    let resolve = |turn_id: &str,
                   receipt: &crate::snapshot::WorkspaceSnapshotRef|
     -> Result<(crate::snapshot::SnapshotId, String), ApiError> {
        listed
            .iter()
            .find(|snapshot| receipt.matches(snapshot))
            .map(|snapshot| (snapshot.tree.clone(), snapshot.label.clone()))
            .ok_or_else(|| {
                ApiError::conflict(format!(
                    "The {} restore point of turn {turn_id} is no longer in the snapshot store (pruned, or its session tag changed), so its workspace changes cannot be restored; nothing was changed. Use POST /v1/threads/{{id}}/undo for a conversation-only undo.",
                    receipt.kind.label_prefix().trim_end_matches(':')
                ))
                .with_code(PATCH_UNDO_RESTORE_POINT_PRUNED)
            })
    };

    // Every path a dropped file-tool call declared must be one the snapshots
    // hold; otherwise its change is invisible to them and cannot be undone.
    let mut not_snapshotted = std::collections::BTreeSet::new();
    for turn in dropped_turns.iter().filter(|turn| turn.may_change_files) {
        let receipt_writes = turn
            .snapshots
            .iter()
            .filter(|receipt| {
                receipt
                    .tool_call_id
                    .as_ref()
                    .is_none_or(|call| !turn.unrun_tool_calls.contains(call))
            })
            .filter_map(|receipt| receipt.write_paths.as_ref())
            .flatten();
        for raw in turn.declared_writes.iter().chain(receipt_writes) {
            let covered = match declared_write_path(workspace, raw) {
                Some(rel) => !repo.path_is_excluded(&rel).map_err(|e| {
                    ApiError::internal(format!(
                        "Failed to check snapshot coverage; conversation preserved: {e}"
                    ))
                })?,
                None => false,
            };
            if !covered {
                not_snapshotted.insert(raw.clone());
            }
        }
    }
    if !not_snapshotted.is_empty() {
        return Err(ApiError::conflict(format!(
            "The undone turn(s) wrote {}, which workspace snapshots do not hold (ignored by .gitignore or the built-in snapshot exclusions, or outside the workspace), so those changes cannot be restored; nothing was changed. Use POST /v1/threads/{{id}}/undo for a conversation-only undo and restore those files yourself.",
            not_snapshotted.into_iter().collect::<Vec<_>>().join(", ")
        ))
        .with_code(PATCH_UNDO_PATH_NOT_SNAPSHOTTED));
    }

    let mut segments = Vec::with_capacity(windows.len());
    for (turn, receipts) in windows {
        let (pre, pre_label) = resolve(&turn.turn_id, &receipts[0])?;
        let (post, _) = resolve(&turn.turn_id, &receipts[receipts.len() - 1])?;
        let (owned, foreign) = attribute_window(workspace, &turn.turn_id, receipts)?;
        segments.push(UndoSegment {
            turn_id: turn.turn_id.clone(),
            pre,
            post,
            pre_label,
            owned,
            foreign,
        });
    }

    let compare_err = |e: std::io::Error| {
        if e.kind() == std::io::ErrorKind::InvalidInput {
            ApiError::conflict(format!(
                "A path the undone turn(s) changed cannot be restored file by file: {e}. Nothing was changed; use /restore for a whole-workspace rollback."
            ))
            .with_code(PATCH_UNDO_WORKSPACE_CHANGED)
        } else {
            ApiError::internal(format!(
                "Failed to compare snapshots; conversation preserved: {e}"
            ))
        }
    };

    // path -> (content to restore, content the dropped turns left)
    let mut plan: std::collections::BTreeMap<
        PathBuf,
        (crate::snapshot::SnapshotId, crate::snapshot::SnapshotId),
    > = std::collections::BTreeMap::new();
    for segment in &segments {
        let changed = repo
            .changed_paths_between(&segment.pre, &segment.post)
            .map_err(compare_err)?;
        // A path someone else changed while the turn ran cannot be told
        // apart from the turn's own change to it, and reverting it would
        // erase their work: refuse instead of guessing.
        let not_owned: Vec<String> = changed
            .iter()
            .filter(|path| segment.foreign.contains(*path) || !segment.owned.contains(*path))
            .map(|path| path.display().to_string())
            .collect();
        if !not_owned.is_empty() {
            return Err(ApiError::conflict(format!(
                "{} changed while turn {} ran but outside its own tool calls (another thread, an editor or a background process), so undoing the turn would revert changes it did not make; nothing was changed. Revert the turn's files individually with file-revert, or use /undo for a conversation-only undo.",
                not_owned.join(", "),
                segment.turn_id
            ))
            .with_code(PATCH_UNDO_WORKSPACE_CHANGED));
        }
        for path in changed {
            match plan.get_mut(&path) {
                None => {
                    plan.insert(path, (segment.pre.clone(), segment.post.clone()));
                }
                Some((_, left)) => {
                    // Between two dropped turns that both changed this path,
                    // something else changed it too; restoring the earlier
                    // content would erase that change.
                    if !repo
                        .path_same_in_snapshots(left, &segment.pre, &path)
                        .map_err(compare_err)?
                    {
                        return Err(ApiError::conflict(format!(
                            "'{}' was changed outside turn {} between the undone turns; undoing would erase that change. Nothing was changed.",
                            path.display(),
                            segment.turn_id
                        ))
                        .with_code(PATCH_UNDO_WORKSPACE_CHANGED));
                    }
                    *left = segment.post.clone();
                }
            }
        }
    }

    // Compare each changed path with the workspace now: already back at its
    // pre-turn content (skip), still as the turns left it (restore), or
    // changed since by someone else (refuse — never clobber later work).
    let mut to_restore = Vec::new();
    let mut changed_since = Vec::new();
    for (path, (before, after)) in &plan {
        if repo
            .path_matches_snapshot(before, path)
            .map_err(compare_err)?
        {
            continue;
        }
        if repo
            .path_matches_snapshot(after, path)
            .map_err(compare_err)?
        {
            to_restore.push((path.clone(), before.clone(), after.clone()));
        } else {
            changed_since.push(path.display().to_string());
        }
    }
    if !changed_since.is_empty() {
        return Err(ApiError::conflict(format!(
            "These files changed after the undone turn(s): {}. Undoing would overwrite those changes; nothing was changed. Revert individual files with file-revert, or use /undo for a conversation-only undo.",
            changed_since.join(", ")
        ))
        .with_code(PATCH_UNDO_WORKSPACE_CHANGED));
    }
    let first = &segments[0];
    if to_restore.is_empty() {
        return Ok(PatchUndoResult {
            files_restored: false,
            summary: Some(if plan.is_empty() {
                "The undone turn(s) left workspace files unchanged; nothing to restore.".to_string()
            } else {
                format!(
                    "The files the undone turn(s) changed are already at their state before turn {}; nothing to restore.",
                    first.turn_id
                )
            }),
            snapshot_label: None,
        });
    }

    // Restoring is a workspace mutation. Gate it exactly where the TUI gates
    // it — after a real, owned change is known — so the two surfaces cannot
    // drift into "one refuses, the other half-undoes".
    if !trusted {
        return Err(ApiError::conflict(
            "Refusing to undo workspace files outside trusted mode. \
             Turn on /trust or switch this thread to Full Access, then undo again.",
        )
        .with_code(PATCH_UNDO_UNTRUSTED));
    }

    let restore_plan: Vec<(PathBuf, crate::snapshot::SnapshotId)> = to_restore
        .iter()
        .map(|(path, before, _)| (path.clone(), before.clone()))
        .collect();
    let short = &first.pre.as_str()[..first.pre.as_str().len().min(12)];
    let outcomes = repo
        .restore_path_plan(&restore_plan, &format!("pre-restore:{short}"), true, || {
            // Re-verify immediately before the first mutation, after the
            // safety snapshot: a write that landed meanwhile is refused.
            for (path, _, after) in &to_restore {
                if !repo.path_matches_snapshot(after, path)? {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        format!(
                            "'{}' changed while the undo was being prepared; nothing was changed.",
                            path.display()
                        ),
                    ));
                }
            }
            Ok(())
        })
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::WouldBlock => {
                ApiError::conflict(e.to_string()).with_code(PATCH_UNDO_WORKSPACE_CHANGED)
            }
            std::io::ErrorKind::InvalidInput => compare_err(e),
            _ => ApiError::internal(format!("Restore failed: {e}")),
        })?;

    let lines: Vec<String> = outcomes
        .iter()
        .map(|outcome| format!("{} {}", outcome.action.as_str(), outcome.path.display()))
        .collect();
    Ok(PatchUndoResult {
        files_restored: true,
        summary: Some(format!(
            "Restored {} file(s) to their state before turn {} (snapshot '{}'):\n{}",
            outcomes.len(),
            first.turn_id,
            first.pre_label,
            lines.join("\n")
        )),
        snapshot_label: Some(first.pre_label.clone()),
    })
}

#[derive(Debug, Deserialize)]
struct RevertThreadFileRequest {
    /// The single file to restore, relative to the thread's workspace.
    /// Absolute paths inside the workspace are accepted and normalized.
    path: String,
    /// Exact pre-tool/pre-turn restore point from the change the user
    /// selected: a commit id from `GET /v1/snapshots`, or the `snapshot_id` /
    /// `tree_id` of a receipt in the thread's `workspace_snapshots`.
    snapshot_id: String,
    /// SHA-256 of the bytes reviewed by the client, or `absent` for deletion.
    expected_hash: String,
}

#[derive(Debug, Serialize)]
struct RevertThreadFileResponse {
    /// Workspace-relative path that was restored.
    path: String,
    /// What the restore did to the working tree: `modified`, `recreated`, or
    /// `removed`.
    action: String,
    /// Snapshot the file came from.
    snapshot_id: String,
    snapshot_label: String,
}

/// Restore one deliberately selected file revision.
///
/// The file-scoped counterpart of `patch-undo`. Where `patch-undo` checks out
/// a whole snapshot tree, this restores exactly one regular file, so unrelated
/// working-tree changes are never rolled back. The client names the exact
/// `tool:`/`pre-turn:` snapshot from the change record it displayed and the
/// hash of the bytes it reviewed; the server never guesses a "newest differing"
/// snapshot, because an unrelated newer snapshot can erase later user edits.
///
/// Ownership: only the `tool:`/`pre-turn:` restore points recorded on this
/// thread's own turns (`TurnRecord::workspace_snapshots`, fork-inherited turns
/// included) are candidates, never another thread's or the TUI's snapshots in
/// the same workspace, and the thread must be in trusted mode or Full Access. Nothing to revert is a `409`, not a silent
/// success, so the GUI can tell the user why the button did nothing.
async fn revert_thread_file(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<RevertThreadFileRequest>,
) -> Result<Json<RevertThreadFileResponse>, ApiError> {
    if !snapshot_id_is_well_formed(&req.snapshot_id) {
        return Err(ApiError::bad_request(
            "snapshot_id must be the exact hexadecimal id reported by GET /v1/snapshots",
        ));
    }
    if !expected_hash_is_well_formed(&req.expected_hash) {
        return Err(ApiError::bad_request(
            "expected_hash must be `sha256:<64 lowercase hex digits>` of the reviewed file bytes, or `absent` for a file the client saw as deleted",
        ));
    }
    // Admission first, then the thread record under it. Active turns in an
    // overlapping workspace are rejected instead of raced.
    let (reservation, thread) = state
        .runtime_threads
        .thread_restore_guard(&id)
        .await
        .map_err(map_thread_err)?;
    if !(thread.trust_mode || thread.auto_approve) {
        return Err(ApiError::conflict(
            "Refusing to restore workspace files outside trusted mode. Turn on /trust or switch this thread to Full Access, then retry.",
        ));
    }
    // The restore points this thread owns: the receipts on its own turns
    // (including turns a fork cloned). Read under the restore reservation, so
    // no turn is recording one meanwhile.
    let owned = state
        .runtime_threads
        .thread_workspace_snapshots(&thread.id)
        .map_err(map_thread_err)?;
    let workspace = thread.workspace;
    // The worker owns the reservation: a client disconnect cannot release it
    // while Git is still changing files. Snapshot listing, diffing and
    // checkout all shell out to git; keep that off the async workers.
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    let response = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let _reservation = reservation;
        revert_file_from_snapshot(&workspace, &owned, &req)
    })
    .await
    .map_err(|e| ApiError::internal(format!("file restore task failed: {e}")))??;
    Ok(Json(response))
}

fn snapshot_id_is_well_formed(id: &str) -> bool {
    crate::snapshot::SnapshotId::is_well_formed(id)
}

fn expected_hash_is_well_formed(hash: &str) -> bool {
    hash == "absent"
        || hash.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
}

fn revert_file_from_snapshot(
    workspace: &FsPath,
    owned: &[crate::snapshot::WorkspaceSnapshotRef],
    req: &RevertThreadFileRequest,
) -> Result<RevertThreadFileResponse, ApiError> {
    // Every caller-supplied path passes through this one gate. It accepts a
    // workspace-relative path or an absolute path inside the workspace and
    // rejects everything else (`..`, empty, or outside the work tree). The
    // name is used literally: brackets, spaces and glob characters are part
    // of the filename, never a pattern.
    let rel = crate::snapshot::workspace_relative_path(workspace, &req.path).ok_or_else(|| {
        ApiError::bad_request(format!(
            "path must name a regular file inside the thread workspace {}; got '{}'",
            workspace.display(),
            req.path
        ))
    })?;
    if !workspace.is_dir() {
        return Err(ApiError::conflict(format!(
            "Workspace directory {} is not available; mount or restore it before restoring files.",
            workspace.display()
        )));
    }
    let repo = crate::snapshot::SnapshotRepo::open_or_init(workspace)
        .map_err(|e| ApiError::internal(format!("Snapshot repo unavailable: {e}")))?;
    repo.validate_restore_file(&rel)
        .map_err(map_file_restore_err)?;
    let snapshots = repo
        .list(usize::MAX)
        .map_err(|e| ApiError::internal(format!("Failed to list snapshots: {e}")))?;
    // Exact identity only: the snapshot must still exist, be a tool/pre-turn
    // restore point recorded on one of this thread's turns, and still carry
    // the session tag it was recorded with. The client may name it by the
    // commit id `GET /v1/snapshots` lists now, or by the `snapshot_id` or
    // `tree_id` its turn record holds (a prune rewrites commit ids but keeps
    // trees). A foreign, unrecorded or pruned id is a conflict the client
    // resolves by refreshing its change record.
    let restore_points: Vec<&crate::snapshot::WorkspaceSnapshotRef> = owned
        .iter()
        .filter(|receipt| {
            matches!(
                receipt.kind,
                crate::snapshot::WorkspaceSnapshotKind::Tool
                    | crate::snapshot::WorkspaceSnapshotKind::PreTurn
            )
        })
        .collect();
    let target = match snapshots
        .iter()
        .find(|snapshot| snapshot.id.as_str() == req.snapshot_id)
    {
        Some(listed) => restore_points
            .iter()
            .any(|receipt| receipt.matches(listed))
            .then_some(listed),
        None => restore_points
            .iter()
            .find(|receipt| {
                receipt.snapshot_id == req.snapshot_id || receipt.tree_id == req.snapshot_id
            })
            .and_then(|receipt| snapshots.iter().find(|listed| receipt.matches(listed))),
    }
    .ok_or_else(|| {
        ApiError::conflict(
            "Selected restore point is unavailable or belongs to another thread; refresh the change record and select the change again.",
        )
    })?;

    if !repo
        .path_differs_from_snapshot(&target.id, &rel)
        .map_err(map_file_restore_err)?
    {
        return Err(ApiError::conflict(format!(
            "'{}' already matches snapshot '{}'; nothing to revert.",
            rel.display(),
            target.label
        )));
    }
    let outcomes = repo
        .restore_file_if_unchanged(&target.id, &rel, &req.expected_hash)
        .map_err(map_file_restore_err)?;
    let outcome = outcomes
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::conflict("Nothing was restored."))?;
    Ok(RevertThreadFileResponse {
        path: outcome.path.to_string_lossy().into_owned(),
        action: outcome.action.as_str().to_string(),
        snapshot_id: target.id.as_str().to_string(),
        snapshot_label: target.label.clone(),
    })
}

fn map_file_restore_err(error: std::io::Error) -> ApiError {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        ApiError::bad_request(error.to_string())
    } else if error.kind() == std::io::ErrorKind::WouldBlock {
        ApiError::conflict(error.to_string())
    } else {
        ApiError::internal(format!("File restore failed: {error}"))
    }
}

#[derive(Debug, Deserialize)]
struct RetryTurnRequest {
    /// How many turns back to retry (default 0 = last turn only).
    #[serde(default)]
    depth: Option<usize>,
    /// Override the user message text. If omitted, the original text
    /// from the dropped turn is re-used.
    #[serde(default)]
    prompt: Option<String>,
    /// Client-executed tools the retried turn offers, as on a fresh turn.
    /// Dynamic tools are per-turn and answered by the client that sent
    /// them, so a retry the client starts must re-send them; without it the
    /// retried turn silently lost tools such as the desktop's `open_in_app`.
    #[serde(default)]
    dynamic_tools: Vec<codewhale_protocol::runtime::DynamicToolSpec>,
}

#[derive(Debug, Serialize)]
struct RetryTurnResponse {
    /// The new forked thread (with the last N turns removed).
    thread: ThreadRecord,
    /// The turn created by the retry.
    turn: TurnRecord,
}

async fn retry_thread_turn(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<RetryTurnRequest>,
) -> Result<(StatusCode, Json<RetryTurnResponse>), ApiError> {
    let depth = req.depth.unwrap_or(0);
    let (forked_thread, original_user_text, original_user_images, max_output_tokens) = state
        .runtime_threads
        .fork_at_user_message_in_sessions_dir(&id, depth, &state.sessions_dir)
        .await
        .map_err(map_thread_err)?;

    let retry_prompt = req.prompt.or(original_user_text).unwrap_or_default();
    if retry_prompt.trim().is_empty() {
        return Err(ApiError::bad_request(
            "No user message to retry — the dropped turn had no user text",
        ));
    }

    let turn = state
        .runtime_threads
        .start_turn_from_stored_images(
            &forked_thread.id,
            StartTurnRequest {
                account_model_owner: None,
                profile_constitution: None,
                expected_workspace: None,
                max_output_tokens,
                prompt: retry_prompt,
                images: original_user_images,
                operation_key: None,
                input_summary: None,
                model: None,
                reasoning_effort: None,
                allowed_tools: None,
                mode: None,
                permission_posture: None,
                allow_shell: None,
                trust_mode: None,
                auto_approve: None,
                dynamic_tools: req.dynamic_tools,
                environment_id: None,
                model_provider: None,
                model_provider_id: None,
            },
        )
        .await
        .map_err(map_thread_err)?;

    Ok((
        StatusCode::CREATED,
        Json(RetryTurnResponse {
            thread: forked_thread,
            turn,
        }),
    ))
}

async fn start_thread_turn(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<StartTurnRequest>,
) -> Result<(StatusCode, Json<StartTurnResponse>), ApiError> {
    let (turn, replayed) = state
        .runtime_threads
        .start_turn_reporting_replay(&id, req)
        .await
        .map_err(map_thread_err)?;
    let thread = state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    // A replay acknowledges work already accepted rather than admitting new
    // work: 200 tells the client "this is the turn I already started", which
    // is what lets an ambiguous submit resolve without duplicate messages or
    // tools. A fresh admission stays 201.
    let status = if replayed {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((
        status,
        Json(StartTurnResponse {
            thread,
            turn,
            idempotent_replay: replayed,
        }),
    ))
}

async fn get_thread_turn_operation(
    State(state): State<RuntimeApiState>,
    Path((id, operation_key)): Path<(String, String)>,
) -> Result<Json<TurnRecord>, ApiError> {
    use crate::runtime_threads::RuntimeTurnOperationLookupError;
    let turn = state
        .runtime_threads
        .lookup_turn_operation(&id, &operation_key)
        .map_err(|error| match error {
            RuntimeTurnOperationLookupError::InvalidRequest => {
                ApiError::bad_request(error.to_string())
            }
            RuntimeTurnOperationLookupError::Incomplete => ApiError::conflict(error.to_string()),
            RuntimeTurnOperationLookupError::Unavailable => ApiError::internal(error.to_string()),
        })?
        .ok_or_else(|| ApiError::not_found("Turn operation not found"))?;
    Ok(Json(turn))
}

#[derive(Debug, Serialize)]
struct AgentMailDeliveryResponse {
    envelope: AgentMailEnvelope,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn: Option<TurnRecord>,
}

async fn send_agent_mail(
    State(state): State<RuntimeApiState>,
    Json(request): Json<AgentMailSendRequest>,
) -> Result<(StatusCode, Json<AgentMailSendResponse>), ApiError> {
    let mut response = state
        .runtime_threads
        .queue_agent_mail(request)
        .await
        .map_err(map_agent_mail_err)?;
    if response.envelope.delivery_mode == AgentMailDeliveryMode::WakeAtSafeBoundary
        && response.envelope.trigger_turn
    {
        let (envelope, _) = state
            .runtime_threads
            .deliver_agent_mail(
                &response.envelope.destination.thread_id,
                &response.envelope.message_id,
            )
            .await
            .map_err(map_agent_mail_err)?;
        response.envelope = envelope;
    }
    let status = if response.idempotent_replay {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    Ok((status, Json(response)))
}

async fn list_agent_mail(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<AgentMailEnvelope>>, ApiError> {
    let inbox = state
        .runtime_threads
        .list_agent_mail_for_thread(&id)
        .await
        .map_err(map_agent_mail_err)?;
    Ok(Json(inbox))
}

async fn deliver_agent_mail(
    State(state): State<RuntimeApiState>,
    Path((id, message_id)): Path<(String, String)>,
) -> Result<Json<AgentMailDeliveryResponse>, ApiError> {
    let message_id = AgentMailMessageId::parse(message_id)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let (envelope, turn) = state
        .runtime_threads
        .deliver_agent_mail(&id, &message_id)
        .await
        .map_err(map_agent_mail_err)?;
    Ok(Json(AgentMailDeliveryResponse { envelope, turn }))
}

async fn mark_agent_mail_read(
    State(state): State<RuntimeApiState>,
    Path((id, message_id)): Path<(String, String)>,
) -> Result<Json<AgentMailEnvelope>, ApiError> {
    let message_id = AgentMailMessageId::parse(message_id)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let envelope = state
        .runtime_threads
        .mark_agent_mail_read(&id, &message_id)
        .await
        .map_err(map_agent_mail_err)?;
    Ok(Json(envelope))
}

/// Withdraw a queued envelope before delivery (#6176). Idempotent: a
/// re-cancel returns the stored envelope; mail that already left `queued`
/// is a 409, never silently dropped.
async fn cancel_agent_mail(
    State(state): State<RuntimeApiState>,
    Path((id, message_id)): Path<(String, String)>,
) -> Result<Json<AgentMailEnvelope>, ApiError> {
    let message_id = AgentMailMessageId::parse(message_id)
        .map_err(|error| ApiError::bad_request(error.to_string()))?;
    let envelope = state
        .runtime_threads
        .cancel_agent_mail(&id, &message_id)
        .await
        .map_err(map_agent_mail_err)?;
    Ok(Json(envelope))
}

async fn steer_thread_turn(
    State(state): State<RuntimeApiState>,
    Path((id, turn_id)): Path<(String, String)>,
    Json(req): Json<SteerTurnRequest>,
) -> Result<Json<TurnRecord>, ApiError> {
    let turn = state
        .runtime_threads
        .steer_turn(&id, &turn_id, req)
        .await
        .map_err(map_thread_err)?;
    Ok(Json(turn))
}

async fn interrupt_thread_turn(
    State(state): State<RuntimeApiState>,
    Path((id, turn_id)): Path<(String, String)>,
) -> Result<Json<TurnRecord>, ApiError> {
    let turn = state
        .runtime_threads
        .interrupt_turn(&id, &turn_id)
        .await
        .map_err(map_thread_err)?;
    Ok(Json(turn))
}

async fn deliver_dynamic_tool_result(
    State(state): State<RuntimeApiState>,
    Path((id, turn_id, call_id)): Path<(String, String, String)>,
    Json(result): Json<DynamicToolCallResult>,
) -> Result<StatusCode, ApiError> {
    state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    if state
        .runtime_threads
        .deliver_dynamic_tool_result(&id, &turn_id, &call_id, result)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
    {
        Ok(StatusCode::ACCEPTED)
    } else {
        Err(ApiError::not_found(format!(
            "No pending dynamic tool call '{call_id}'"
        )))
    }
}

async fn compact_thread(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<CompactThreadRequest>,
) -> Result<(StatusCode, Json<StartTurnResponse>), ApiError> {
    let turn = state
        .runtime_threads
        .compact_thread(&id, req)
        .await
        .map_err(map_thread_err)?;
    let thread = state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    Ok((
        StatusCode::ACCEPTED,
        Json(StartTurnResponse {
            thread,
            turn,
            idempotent_replay: false,
        }),
    ))
}

// ---------------------------------------------------------------------------
// Thread goal endpoints
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GoalsQuery {
    limit: Option<usize>,
    cursor: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Serialize)]
struct GoalsPage {
    goals: Vec<codewhale_protocol::ThreadGoal>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

/// `GET /v1/goals?limit=50&cursor=<opaque>&status=<status>` — read-only index
/// of the per-thread goal records, newest update first. Pages are keyset
/// cursors over `(updated_at, thread_id)`.
async fn list_goals(
    State(state): State<RuntimeApiState>,
    Query(query): Query<GoalsQuery>,
) -> Result<Json<GoalsPage>, ApiError> {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let status = match query.status.as_deref() {
        None => None,
        Some(raw) => Some(
            serde_json::from_value::<codewhale_protocol::ThreadGoalStatus>(
                serde_json::Value::String(raw.to_string()),
            )
            .map_err(|_| ApiError::bad_request(format!("unknown goal status '{raw}'")))?,
        ),
    };
    let status_key = query.status.as_deref().unwrap_or("-");
    let after = match query.cursor.as_deref() {
        None => None,
        Some(raw) => {
            let mut parts = raw.splitn(3, ':');
            let (cursor_status, updated_at, thread_id) = match (
                parts.next(),
                parts.next().and_then(|at| at.parse::<i64>().ok()),
                parts.next(),
            ) {
                (Some(cursor_status), Some(at), Some(id)) => (cursor_status, at, id.to_string()),
                _ => return Err(ApiError::bad_request("invalid cursor")),
            };
            if cursor_status != status_key {
                return Err(ApiError::bad_request(
                    "cursor does not match the status filter",
                ));
            }
            Some((updated_at, thread_id))
        }
    };
    let mut goals = state
        .runtime_threads
        .list_goals()
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    goals.retain(|goal| status.as_ref().is_none_or(|s| &goal.status == s));
    goals.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.thread_id.cmp(&b.thread_id))
    });
    if let Some((updated_at, thread_id)) = after {
        goals.retain(|goal| {
            goal.updated_at < updated_at
                || (goal.updated_at == updated_at && goal.thread_id > thread_id)
        });
    }
    let mut page = Vec::with_capacity(limit.min(goals.len()) + 1);
    for goal in goals {
        if state.runtime_threads.thread_exists(&goal.thread_id).await {
            page.push(goal);
            if page.len() > limit {
                break;
            }
        }
    }
    let next_cursor = (page.len() > limit).then(|| {
        let last = &page[limit - 1];
        format!("{status_key}:{}:{}", last.updated_at, last.thread_id)
    });
    page.truncate(limit);
    let goals = page;
    Ok(Json(GoalsPage { goals, next_cursor }))
}

/// `GET /v1/threads/{id}/goal` — return the persistent goal for a thread, or
/// 404 if the thread has no goal.
async fn get_thread_goal(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<codewhale_protocol::ThreadGoal>, ApiError> {
    // Verify the thread exists so we can return a clean 404 for unknown threads.
    state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    let goal = state
        .runtime_threads
        .get_goal(&id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("thread '{id}' has no goal")))?;
    Ok(Json(goal))
}

#[derive(Debug, Deserialize)]
struct UpsertThreadGoalRequest {
    objective: String,
    #[serde(default)]
    token_budget: Option<i64>,
}

/// `PUT /v1/threads/{id}/goal` — create or replace the persistent goal for a
/// thread. Only `Active` goals may be created through this route; lifecycle
/// transitions (`complete`, `block`) have dedicated action endpoints.
async fn upsert_thread_goal(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<UpsertThreadGoalRequest>,
) -> Result<(StatusCode, Json<codewhale_protocol::ThreadGoal>), ApiError> {
    if req.objective.trim().is_empty() {
        return Err(ApiError::bad_request("objective must not be blank"));
    }
    // Verify the thread exists.
    state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    let now = chrono::Utc::now().timestamp();
    let existing = state
        .runtime_threads
        .get_goal(&id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let is_new = existing.is_none();
    let goal = codewhale_protocol::ThreadGoal {
        thread_id: id.clone(),
        goal_id: format!("goal-{}", uuid::Uuid::new_v4()),
        objective: req.objective.clone(),
        status: codewhale_protocol::ThreadGoalStatus::Active,
        token_budget: req.token_budget,
        tokens_used: 0,
        time_used_seconds: 0,
        continuation_count: 0,
        last_gap_fingerprint: None,
        repeated_gap_count: 0,
        last_gap_pass: None,
        pause_reason: None,
        created_at: now,
        updated_at: now,
    };
    state
        .runtime_threads
        .save_goal(goal.clone())
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let status_code = if is_new {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    // Emit a replayable goal-updated event so SSE subscribers can react.
    let _ = state
        .runtime_threads
        .emit_goal_updated_event(&id, goal.clone())
        .await;
    // Inject the goal into a cached engine (if any) and dispatch the kickoff
    // turn while the thread is idle. Errors are advisory: the goal record is
    // already durable and a subsequent turn still carries it.
    if let Err(err) = state.runtime_threads.activate_thread_goal(&id).await {
        tracing::warn!("failed to activate goal for thread '{id}': {err}");
    }
    Ok((status_code, Json(goal)))
}

/// `DELETE /v1/threads/{id}/goal` — remove the persistent goal from a thread.
/// Returns 204 No Content on success, 404 if there was no goal.
async fn delete_thread_goal(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    let deleted = state
        .runtime_threads
        .remove_goal(&id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    if !deleted {
        return Err(ApiError::not_found(format!("thread '{id}' has no goal")));
    }
    let _ = state.runtime_threads.emit_goal_cleared_event(&id).await;
    state
        .runtime_threads
        .sync_engine_goal_status(&id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/threads/{id}/goal/complete` — transition the goal to `Complete`.
/// Only valid from a non-terminal status; returns 409 Conflict if the goal is
/// already in a terminal state, and 404 if the thread has no goal.
async fn complete_thread_goal(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<codewhale_protocol::ThreadGoal>, ApiError> {
    state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    let goal = state
        .runtime_threads
        .get_goal(&id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("thread '{id}' has no goal")))?;
    if matches!(goal.status, codewhale_protocol::ThreadGoalStatus::Complete) {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            message: format!("goal for thread '{id}' is already complete"),
            code: None,
        });
    }
    let updated = state
        .runtime_threads
        .transition_goal_status(
            &id,
            &goal.goal_id,
            goal.status.clone(),
            codewhale_protocol::ThreadGoalStatus::Complete,
        )
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError {
            status: StatusCode::CONFLICT,
            message: format!("goal for thread '{id}' changed concurrently; retry"),
            code: None,
        })?;
    let _ = state
        .runtime_threads
        .emit_goal_updated_event(&id, updated.clone())
        .await;
    state
        .runtime_threads
        .sync_engine_goal_status(&id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(updated))
}

/// `POST /v1/threads/{id}/goal/block` — transition the goal to `Blocked`.
/// Rejects transitions from terminal states (returns 409).
async fn block_thread_goal(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<codewhale_protocol::ThreadGoal>, ApiError> {
    state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;
    let goal = state
        .runtime_threads
        .get_goal(&id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError::not_found(format!("thread '{id}' has no goal")))?;
    if matches!(goal.status, codewhale_protocol::ThreadGoalStatus::Complete) {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            message: format!(
                "goal for thread '{id}' is already complete; cannot transition to blocked"
            ),
            code: None,
        });
    }
    let updated = state
        .runtime_threads
        .transition_goal_status(
            &id,
            &goal.goal_id,
            goal.status.clone(),
            codewhale_protocol::ThreadGoalStatus::Blocked,
        )
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .ok_or_else(|| ApiError {
            status: StatusCode::CONFLICT,
            message: format!("goal for thread '{id}' changed concurrently; retry"),
            code: None,
        })?;
    let _ = state
        .runtime_threads
        .emit_goal_updated_event(&id, updated.clone())
        .await;
    state
        .runtime_threads
        .sync_engine_goal_status(&id)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(updated))
}

/// Runtime-authenticated administrative task inventory.
///
/// Unlike in-session TUI/model controls, the Runtime API token authorizes the
/// caller for the whole host runtime, so these endpoints intentionally span
/// sessions. Running with `--insecure` explicitly opts out of that host boundary.
async fn list_tasks(
    State(state): State<RuntimeApiState>,
    Query(query): Query<TasksQuery>,
) -> Result<Json<TasksResponse>, ApiError> {
    let tasks = match query.workspace.as_deref() {
        Some(workspace) => {
            state
                .task_manager
                .list_tasks_scoped(query.limit, Some(workspace))
                .await
        }
        None => state.task_manager.list_tasks(query.limit).await,
    }
    .map_err(|error| ApiError::internal(format!("Task inventory unavailable: {error}")))?;
    let counts = state
        .task_manager
        .counts()
        .await
        .map_err(|error| ApiError::internal(format!("Task inventory unavailable: {error}")))?;
    Ok(Json(TasksResponse { tasks, counts }))
}

/// Runtime-authenticated administrative task lookup across host sessions.
async fn get_task(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<TaskRecord>, ApiError> {
    let task = state
        .task_manager
        .get_task(&id)
        .await
        .map_err(map_task_err)?;
    Ok(Json(task))
}

/// Runtime-authenticated administrative task cancellation across host sessions.
async fn cancel_task(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<TaskRecord>, ApiError> {
    let cancellation = state
        .task_manager
        .cancel_task(&id)
        .await
        .map_err(map_task_err)?;
    Ok(Json(cancellation.task))
}

async fn stream_thread_events(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(query): Query<ThreadEventsQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let _ = state
        .runtime_threads
        .get_thread(&id)
        .await
        .map_err(map_thread_err)?;

    // Two clients, two cursors. A browser `EventSource` can only replay through
    // the `Last-Event-ID` header it sets on reconnect (the ids now ride the
    // journal frames below); every other client passes `since_seq`. An explicit
    // query cursor wins over the header, so a deliberate replay-from-zero is
    // never silently overridden by a stale header — the header is the fallback
    // when no cursor was asked for.
    let since_seq = query.since_seq.or_else(|| last_event_id(&headers));

    // Subscribe before reading durable history. An event emitted while replay
    // is loaded is then present in both places (and deduped below) or queued
    // live, never in an uncovered handoff window.
    let live = state.runtime_threads.subscribe_events();
    if query
        .replay_limit
        .is_some_and(|limit| limit > MAX_RUNTIME_EVENT_REPLAY_TAIL)
    {
        return Err(ApiError::bad_request(format!(
            "replay_limit cannot exceed {MAX_RUNTIME_EVENT_REPLAY_TAIL}"
        )));
    }
    let replay = state
        .runtime_threads
        .replay_events(&id, since_seq, query.replay_limit)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;

    let stream = replay_live_thread_events(
        state.runtime_threads.clone(),
        id,
        replay.base_seq,
        replay.batches,
        live,
        query.progress,
        state.shutdown.requested.clone(),
    );

    let mut response = Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response();
    // Every server-initiated end of this stream is a `stream.end` frame. The
    // header lets a client tell that EOF without one is transport loss, which
    // an older Runtime cannot promise.
    response
        .headers_mut()
        .insert("x-codewhale-stream-end", HeaderValue::from_static("1"));
    if query.progress {
        response
            .headers_mut()
            .insert("x-codewhale-event-progress", HeaderValue::from_static("1"));
    }
    Ok(response)
}

/// Opt-in transport frame at the existing journal cursor. It carries the same
/// envelope identity (`schema_version`, `event`, `kind`, `thread_id`) as the
/// journal and `stream.end`, but never a journal `seq` of its own.
fn thread_stream_progress(thread_id: &str, seq: u64, live: bool) -> SseEvent {
    sse_json(
        "stream.progress",
        json!({
            "schema_version": RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION,
            "event": "stream.progress", "kind": "stream.progress",
            "thread_id": thread_id, "seq": seq,
            "state": if live { "live" } else { "replaying" },
        }),
    )
}

/// Why the server ended a thread event stream it had already opened. Every
/// server-initiated end is one of these, sent as the final `stream.end` frame;
/// an EOF without that frame is the transport or the process dying, never the
/// server choosing to stop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadStreamEnd {
    /// The durable history read that feeds the opening replay failed.
    ReplayFailed,
    /// The durable re-read after broadcast lag could not be opened or failed.
    CatchUpFailed,
    /// The Runtime API server is stopping (a terminating signal).
    RuntimeShutdown,
}

impl ThreadStreamEnd {
    const fn reason(self) -> &'static str {
        match self {
            Self::ReplayFailed => "replay_failed",
            Self::CatchUpFailed => "catch_up_failed",
            Self::RuntimeShutdown => "runtime_shutdown",
        }
    }

    /// Every current end is resumable from `last_seq`. The flag exists so the
    /// client rule keys on it rather than on the reason list: a future
    /// non-resumable end stops clients without a client change.
    const fn retryable(self) -> bool {
        match self {
            Self::ReplayFailed | Self::CatchUpFailed | Self::RuntimeShutdown => true,
        }
    }
}

/// The final frame of a server-ended thread stream. It has no `seq` and no SSE
/// `id:` because it is not a journal event: seq-keyed consumers skip it, and a
/// browser `EventSource` keeps `Last-Event-ID` on the last real event.
/// `last_seq` is exactly the `since_seq` that resumes without loss or repeats.
/// The underlying error stays in the server log; it can carry store paths.
fn thread_stream_end(thread_id: &str, end: ThreadStreamEnd, last_seq: u64) -> SseEvent {
    sse_json(
        "stream.end",
        json!({
            "schema_version": RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION,
            "event": "stream.end", "kind": "stream.end",
            "thread_id": thread_id, "reason": end.reason(),
            "last_seq": last_seq, "retryable": end.retryable(),
        }),
    )
}

/// The journal frame for `event` on this thread's stream, advancing the
/// connection cursor. `None` for another thread's event or one already sent.
fn thread_journal_frame(
    thread_id: &str,
    last_seq: &mut u64,
    event: crate::runtime_threads::RuntimeEventRecord,
) -> Option<SseEvent> {
    if event.thread_id != thread_id || event.seq <= *last_seq {
        return None;
    }
    let previous_seq = std::mem::replace(last_seq, event.seq);
    let event_name = event.event.clone();
    Some(
        sse_json(
            &event_name,
            runtime_event_payload_with_previous(event, previous_seq),
        )
        .id(last_seq.to_string()),
    )
}

type ThreadReplayBatches = tokio::sync::mpsc::Receiver<
    std::result::Result<Vec<crate::runtime_threads::RuntimeEventRecord>, String>,
>;

enum ThreadReplayStep {
    Events(Vec<crate::runtime_threads::RuntimeEventRecord>),
    Complete,
    Failed(String),
    Shutdown,
}

/// The next durable-history batch, unless the server starts stopping first.
async fn next_thread_replay_step(
    shutdown: &CancellationToken,
    batches: &mut ThreadReplayBatches,
) -> ThreadReplayStep {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => ThreadReplayStep::Shutdown,
        batch = batches.recv() => match batch {
            None => ThreadReplayStep::Complete,
            Some(Ok(events)) => ThreadReplayStep::Events(events),
            Some(Err(error)) => ThreadReplayStep::Failed(error),
        },
    }
}

fn replay_live_thread_events(
    runtime_threads: SharedRuntimeThreadManager,
    thread_id: String,
    mut last_seq: u64,
    mut backlog: ThreadReplayBatches,
    mut live: tokio::sync::broadcast::Receiver<crate::runtime_threads::RuntimeEventRecord>,
    progress: bool,
    shutdown: CancellationToken,
) -> impl futures_util::Stream<Item = Result<SseEvent, Infallible>> {
    // Every exit below is a `stream.end` followed by `return`; the live loop
    // never breaks. An EOF this stream produces is therefore never silent.
    stream! {
        if progress { yield Ok(thread_stream_progress(&thread_id, last_seq, false)); }
        loop {
            match next_thread_replay_step(&shutdown, &mut backlog).await {
                ThreadReplayStep::Events(events) => {
                    for event in events {
                        if let Some(frame) = thread_journal_frame(&thread_id, &mut last_seq, event) {
                            yield Ok(frame);
                        }
                    }
                }
                ThreadReplayStep::Complete => break,
                ThreadReplayStep::Failed(error) => {
                    tracing::warn!(
                        thread_id = %thread_id,
                        last_seq,
                        %error,
                        "Failed to replay Runtime web event stream from durable history"
                    );
                    yield Ok(thread_stream_end(&thread_id, ThreadStreamEnd::ReplayFailed, last_seq));
                    return;
                }
                ThreadReplayStep::Shutdown => {
                    yield Ok(thread_stream_end(&thread_id, ThreadStreamEnd::RuntimeShutdown, last_seq));
                    return;
                }
            }
        }

        // Backlog completion alone is insufficient: a request may have been
        // answered while history was read. Drain the already-queued live tail
        // before declaring the observation current. These opt-in frames carry
        // transport progress, never new journal events or sequence numbers.
        let mut replaying = progress;
        loop {
            if shutdown.is_cancelled() {
                yield Ok(thread_stream_end(&thread_id, ThreadStreamEnd::RuntimeShutdown, last_seq));
                return;
            }
            let next = if replaying {
                use tokio::sync::broadcast::error::{RecvError, TryRecvError};
                match live.try_recv() {
                    Ok(event) => Ok(event),
                    Err(TryRecvError::Empty) => {
                        yield Ok(thread_stream_progress(&thread_id, last_seq, true));
                        replaying = false;
                        continue;
                    }
                    Err(TryRecvError::Lagged(skipped)) => Err(RecvError::Lagged(skipped)),
                    Err(TryRecvError::Closed) => Err(RecvError::Closed),
                }
            } else {
                let received = tokio::select! {
                    biased;
                    () = shutdown.cancelled() => None,
                    received = live.recv() => Some(received),
                };
                // Shutdown is answered by the check at the top of the loop.
                let Some(received) = received else { continue };
                received
            };
            match next {
                Ok(event) => {
                    if let Some(frame) = thread_journal_frame(&thread_id, &mut last_seq, event) {
                        yield Ok(frame);
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    if progress {
                        yield Ok(thread_stream_progress(&thread_id, last_seq, false));
                        replaying = true;
                    }
                    // Broadcast is only a wake-up path; durable history remains
                    // authoritative. Catch up from the last delivered cursor so
                    // receiver pressure cannot turn into a silent prompt loss.
                    let mut recovered = match runtime_threads
                        .replay_events(&thread_id, Some(last_seq), None)
                        .await
                    {
                        Ok(replay) => replay.batches,
                        Err(error) => {
                            tracing::warn!(
                                thread_id = %thread_id,
                                last_seq,
                                skipped,
                                %error,
                                "Failed to recover lagged Runtime web event stream from durable history"
                            );
                            yield Ok(thread_stream_end(&thread_id, ThreadStreamEnd::CatchUpFailed, last_seq));
                            return;
                        }
                    };
                    loop {
                        match next_thread_replay_step(&shutdown, &mut recovered).await {
                            ThreadReplayStep::Events(events) => {
                                for event in events {
                                    if let Some(frame) = thread_journal_frame(&thread_id, &mut last_seq, event) {
                                        yield Ok(frame);
                                    }
                                }
                            }
                            ThreadReplayStep::Complete => break,
                            ThreadReplayStep::Failed(error) => {
                                tracing::warn!(
                                    thread_id = %thread_id,
                                    last_seq,
                                    skipped,
                                    %error,
                                    "Failed to recover lagged Runtime web event stream from durable history"
                                );
                                yield Ok(thread_stream_end(&thread_id, ThreadStreamEnd::CatchUpFailed, last_seq));
                                return;
                            }
                            // Answered by the check at the top of the live loop.
                            ThreadReplayStep::Shutdown => break,
                        }
                    }
                }
                // The sender is owned by the `RuntimeThreadManager` this stream
                // holds an `Arc` of, so it cannot close while the stream runs.
                // If that ever changes, the live source is gone only because
                // the Runtime is: say so rather than end silently.
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    yield Ok(thread_stream_end(&thread_id, ThreadStreamEnd::RuntimeShutdown, last_seq));
                    return;
                }
            }
        }
    }
}

async fn stream_turn(
    State(state): State<RuntimeApiState>,
    Json(req): Json<StreamTurnRequest>,
) -> Result<Sse<impl futures_util::Stream<Item = Result<SseEvent, Infallible>>>, ApiError> {
    if req.prompt.trim().is_empty() {
        return Err(ApiError::bad_request("prompt is required"));
    }

    crate::image_attach::prepare_runtime_images(&req.images).map_err(map_thread_err)?;

    let model = runtime_request_model(&state.config.read(), req.model.as_deref())?;
    if req.max_output_tokens.is_some() {
        let config = state.config.read();
        let identity = config
            .active_provider_identity()
            .map_err(ApiError::bad_request)?;
        if model.eq_ignore_ascii_case("auto")
            || provider_model_output_token_limit_for_api(&config, &identity, &model)
                != codewhale_config::route::CapabilityState::Supported
        {
            return Err(ApiError::bad_request(
                "maxOutputTokens requires an exact model with output-limit support",
            ));
        }
    }
    let workspace = req
        .workspace
        .clone()
        .unwrap_or_else(|| state.workspace.clone());
    let mode = req.mode.clone().unwrap_or_else(|| "agent".to_string());
    let permission_posture = req.permission_posture.clone();
    let allow_shell = req.allow_shell.unwrap_or(state.config.read().allow_shell());
    let trust_mode = req.trust_mode.unwrap_or(false);
    let auto_approve = req.auto_approve.unwrap_or(false);
    let prompt = req.prompt;

    let thread = state
        .runtime_threads
        .create_thread(CreateThreadRequest {
            model: Some(model.clone()),
            workspace: Some(workspace.clone()),
            mode: Some(mode.clone()),
            permission_posture: permission_posture.clone(),
            allow_shell: Some(allow_shell),
            trust_mode: Some(trust_mode),
            auto_approve: Some(auto_approve),
            archived: true,
            system_prompt: None,
            task_id: None,
            ..Default::default()
        })
        .await
        .map_err(|e| ApiError::internal(format!("Failed to create stream thread: {e}")))?;

    #[cfg(test)]
    if let Some(hook) = &state.compat_stream_test_hook {
        let (resume, wait_for_resume) = tokio::sync::oneshot::channel();
        hook.send(CompatStreamTestPoint::ThreadCreated {
            thread_id: thread.id.clone(),
            resume,
        })
        .map_err(|_| ApiError::internal("Compatibility stream test hook closed"))?;
        wait_for_resume
            .await
            .map_err(|_| ApiError::internal("Compatibility stream test hook dropped resume"))?;
    }

    let turn_result = state
        .runtime_threads
        .start_turn(
            &thread.id,
            StartTurnRequest {
                account_model_owner: None,
                profile_constitution: None,
                max_output_tokens: req.max_output_tokens,
                prompt,
                images: req.images,
                input_summary: None,
                model: Some(model.clone()),
                mode: Some(mode.clone()),
                permission_posture,
                allow_shell: Some(allow_shell),
                trust_mode: Some(trust_mode),
                auto_approve: Some(auto_approve),
                ..Default::default()
            },
        )
        .await;
    let turn = match turn_result {
        Ok(turn) => turn,
        Err(error) => {
            // This helper refuses loaded threads and any thread owning a turn.
            // A failed/uncertain handoff must remain recoverable; only an empty,
            // never-loaded admission can be discarded.
            if let Err(cleanup_error) = state.runtime_threads.discard_empty_thread(&thread.id).await
            {
                tracing::warn!(thread_id = %thread.id, %cleanup_error, "Retained stream thread after failed admission");
            }
            return Err(map_thread_err(error));
        }
    };

    // Subscribe before reading the durable replay. Events produced while the
    // replay is loaded then exist in at least one source, and the sequence
    // cursor below removes overlap without dropping the handoff edge.
    let mut live = state.runtime_threads.subscribe_events();
    let thread_id = thread.id.clone();
    let turn_id = turn.id.clone();

    #[cfg(test)]
    if let Some(hook) = &state.compat_stream_test_hook {
        let (resume, wait_for_resume) = tokio::sync::oneshot::channel();
        hook.send(CompatStreamTestPoint::SubscribedBeforeReplay {
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            resume,
        })
        .map_err(|_| ApiError::internal("Compatibility stream test hook closed"))?;
        wait_for_resume
            .await
            .map_err(|_| ApiError::internal("Compatibility stream test hook dropped resume"))?;
    }

    let mut backlog = state
        .runtime_threads
        .replay_events(&thread.id, None, None)
        .await
        .map_err(|e| ApiError::internal(format!("Failed to load stream backlog: {e}")))?;

    #[cfg(test)]
    if let Some(hook) = &state.compat_stream_test_hook {
        let (resume, wait_for_resume) = tokio::sync::oneshot::channel();
        hook.send(CompatStreamTestPoint::ReplayLoaded {
            thread_id: thread_id.clone(),
            turn_id: turn_id.clone(),
            resume,
        })
        .map_err(|_| ApiError::internal("Compatibility stream test hook closed"))?;
        wait_for_resume
            .await
            .map_err(|_| ApiError::internal("Compatibility stream test hook dropped resume"))?;
    }

    let stream = stream! {
        let mut last_seq = 0;
        yield Ok(sse_json("turn.started", json!({
            "thread_id": thread.id,
            "turn_id": turn.id,
            "model": model,
            "mode": mode,
            "workspace": workspace,
        })));

        while let Some(batch) = backlog.batches.recv().await {
            let events = match batch {
                Ok(events) => events,
                Err(error) => {
                    tracing::warn!(
                        thread_id = %thread_id,
                        turn_id = %turn_id,
                        %error,
                        "Failed to replay compatibility stream from durable history"
                    );
                    yield Ok(sse_json("error", json!({
                        "message": "failed to replay durable event stream",
                    })));
                    return;
                }
            };
            for event in events {
                let Some((mapped, terminal)) = take_compat_turn_event(
                    &event,
                    &thread_id,
                    &turn_id,
                    &mut last_seq,
                ) else {
                    continue;
                };
                if let Some(mapped) = mapped {
                    yield Ok(mapped);
                }
                if terminal {
                    yield Ok(sse_json("done", json!({})));
                    return;
                }
            }
        }

        loop {
            match live.recv().await {
                Ok(event) => {
                    let Some((mapped, terminal)) = take_compat_turn_event(
                        &event,
                        &thread_id,
                        &turn_id,
                        &mut last_seq,
                    ) else {
                        continue;
                    };
                    if let Some(mapped) = mapped {
                        yield Ok(mapped);
                    }
                    if terminal {
                        yield Ok(sse_json("done", json!({})));
                        return;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    let mut recovered = match state.runtime_threads
                        .replay_events(&thread_id, Some(last_seq), None)
                        .await
                    {
                        Ok(replay) => replay.batches,
                        Err(error) => {
                            tracing::warn!(
                                thread_id = %thread_id,
                                turn_id = %turn_id,
                                last_seq,
                                skipped,
                                %error,
                                "Failed to recover lagged compatibility stream from durable history"
                            );
                            yield Ok(sse_json("error", json!({
                                "message": "failed to recover lagged event stream",
                            })));
                            return;
                        }
                    };
                    while let Some(batch) = recovered.recv().await {
                        let events = match batch {
                            Ok(events) => events,
                            Err(error) => {
                                tracing::warn!(
                                    thread_id = %thread_id,
                                    turn_id = %turn_id,
                                    last_seq,
                                    skipped,
                                    %error,
                                    "Failed to recover lagged compatibility stream from durable history"
                                );
                                yield Ok(sse_json("error", json!({
                                    "message": "failed to recover lagged event stream",
                                })));
                                return;
                            }
                        };
                        for event in events {
                            let Some((mapped, terminal)) = take_compat_turn_event(
                                &event,
                                &thread_id,
                                &turn_id,
                                &mut last_seq,
                            ) else {
                                continue;
                            };
                            if let Some(mapped) = mapped {
                                yield Ok(mapped);
                            }
                            if terminal {
                                yield Ok(sse_json("done", json!({})));
                                return;
                            }
                        }
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    yield Ok(sse_json("error", json!({ "message": "event channel closed" })));
                    return;
                }
            }
        }
    };

    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

fn take_compat_turn_event(
    event: &crate::runtime_threads::RuntimeEventRecord,
    thread_id: &str,
    turn_id: &str,
    last_seq: &mut u64,
) -> Option<(Option<SseEvent>, bool)> {
    if event.thread_id != thread_id
        || event.turn_id.as_deref() != Some(turn_id)
        || event.seq <= *last_seq
    {
        return None;
    }
    *last_seq = event.seq;
    Some((
        map_compat_stream_event(event),
        event.event == "turn.completed",
    ))
}

fn runtime_event_payload(event: crate::runtime_threads::RuntimeEventRecord) -> serde_json::Value {
    let event_name = event.event.clone();
    let timestamp = event.timestamp.to_rfc3339();
    let schema_version = RUNTIME_EVENT_ENVELOPE_SCHEMA_VERSION;
    let envelope = RuntimeEventEnvelope {
        schema_version,
        seq: event.seq,
        event: event_name.clone(),
        kind: event_name,
        thread_id: event.thread_id,
        turn_id: event.turn_id,
        item_id: event.item_id,
        timestamp: timestamp.clone(),
        created_at: Some(timestamp),
        payload: event.payload,
        extra: Default::default(),
    };
    serde_json::to_value(envelope).expect("serialize runtime event envelope")
}

fn runtime_event_payload_with_previous(
    event: crate::runtime_threads::RuntimeEventRecord,
    previous_seq: u64,
) -> serde_json::Value {
    let mut payload = runtime_event_payload(event);
    if let Some(object) = payload.as_object_mut() {
        object.insert("previous_seq".to_string(), json!(previous_seq));
    }
    payload
}

fn map_compat_stream_event(event: &crate::runtime_threads::RuntimeEventRecord) -> Option<SseEvent> {
    let payload = &event.payload;
    match event.event.as_str() {
        "item.delta" => {
            let kind = payload
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if kind == "agent_message" {
                let content = payload
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                Some(sse_json("message.delta", json!({ "content": content })))
            } else if kind == "tool_call" {
                let output = payload
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                Some(sse_json("tool.progress", json!({ "output": output })))
            } else {
                None
            }
        }
        "item.started" => {
            let tool = payload.get("tool")?;
            let id = tool.get("id").cloned().unwrap_or(Value::Null);
            let name = tool.get("name").cloned().unwrap_or(Value::Null);
            let input = tool.get("input").cloned().unwrap_or(Value::Null);
            Some(sse_json(
                "tool.started",
                json!({
                    "id": id,
                    "name": name,
                    "input": input,
                }),
            ))
        }
        "item.completed" | "item.failed" => {
            let item = payload.get("item")?;
            let kind = item
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if kind == "tool_call" || kind == "file_change" || kind == "command_execution" {
                let id = item.get("id").cloned().unwrap_or(Value::Null);
                let success = event.event == "item.completed";
                let output = item.get("detail").cloned().unwrap_or_else(|| {
                    Value::String(
                        item.get("summary")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    )
                });
                Some(sse_json(
                    "tool.completed",
                    json!({
                        "id": id,
                        "success": success,
                        "output": output,
                    }),
                ))
            } else if kind == "status" {
                let message = item
                    .get("detail")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("summary").and_then(|v| v.as_str()))
                    .unwrap_or_default();
                Some(sse_json("status", json!({ "message": message })))
            } else if kind == "error" {
                let message = item
                    .get("detail")
                    .and_then(|v| v.as_str())
                    .or_else(|| item.get("summary").and_then(|v| v.as_str()))
                    .unwrap_or_default();
                Some(sse_json("error", json!({ "message": message })))
            } else {
                None
            }
        }
        "approval.required" => {
            let approval_id = payload
                .get("approval_id")
                .or_else(|| payload.get("id"))?
                .clone();
            Some(sse_json(
                "approval.required",
                json!({
                    "id": approval_id,
                    "approval_id": approval_id,
                    "tool_call_id": payload.get("tool_call_id"),
                    "thread_id": event.thread_id,
                    "turn_id": event.turn_id,
                    "tool_name": payload.get("tool_name"),
                    "description": payload.get("description"),
                    "intent_summary": payload.get("intent_summary"),
                }),
            ))
        }
        "approval.decided" => {
            let approval_id = payload
                .get("approval_id")
                .or_else(|| payload.get("id"))?
                .clone();
            Some(sse_json(
                "approval.decided",
                json!({
                    "id": approval_id,
                    "approval_id": approval_id,
                    "tool_call_id": payload.get("tool_call_id"),
                    "thread_id": event.thread_id,
                    "turn_id": event.turn_id,
                    "decision": payload.get("decision"),
                    "remember": payload.get("remember"),
                    "auto": payload.get("auto"),
                    "timeout": payload.get("timeout"),
                    // Set when the decision was forced by a turn interrupt
                    // or turn teardown: no user selection was made,
                    // so clients must clear the pending prompt instead of
                    // reporting a refusal.
                    "cancelled": payload.get("cancelled"),
                }),
            ))
        }
        "approval.timeout" => {
            let approval_id = payload
                .get("approval_id")
                .or_else(|| payload.get("id"))?
                .clone();
            Some(sse_json(
                "approval.timeout",
                json!({
                    "id": approval_id,
                    "approval_id": approval_id,
                    "tool_call_id": payload.get("tool_call_id"),
                    "thread_id": event.thread_id,
                    "turn_id": event.turn_id,
                    "timeout_secs": payload.get("timeout_secs"),
                }),
            ))
        }
        "user_input.required" => {
            let input_id = payload
                .get("input_id")
                .or_else(|| payload.get("id"))?
                .clone();
            let request = payload.get("request")?.clone();
            Some(sse_json(
                "user_input.required",
                json!({
                    "id": input_id,
                    "input_id": input_id,
                    "thread_id": event.thread_id,
                    "turn_id": event.turn_id,
                    "status": "required",
                    "request": request,
                }),
            ))
        }
        "user_input.answered" | "user_input.canceled" => {
            let input_id = payload
                .get("input_id")
                .or_else(|| payload.get("id"))?
                .clone();
            let status = if event.event == "user_input.answered" {
                "submitted"
            } else {
                "canceled"
            };
            Some(sse_json(
                &event.event,
                json!({
                    "id": input_id,
                    "input_id": input_id,
                    "thread_id": event.thread_id,
                    "turn_id": event.turn_id,
                    "status": status,
                    "terminal": payload.get("terminal").and_then(Value::as_bool).unwrap_or(false),
                }),
            ))
        }
        "sandbox.denied" => Some(sse_json("sandbox.denied", payload.clone())),
        // The operator's own store failed; the payload names the file and
        // the next action, so compat clients see it too (#5931).
        crate::runtime_threads::RUNTIME_STORE_FAILURE_EVENT => Some(sse_json(
            crate::runtime_threads::RUNTIME_STORE_FAILURE_EVENT,
            payload.clone(),
        )),
        "turn.completed" => {
            let usage = payload
                .get("turn")
                .and_then(|turn| turn.get("usage"))
                .cloned()
                .unwrap_or(json!(null));
            Some(sse_json("turn.completed", json!({ "usage": usage })))
        }
        _ => None,
    }
}

fn sse_json(event: &str, payload: serde_json::Value) -> SseEvent {
    let data = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
    SseEvent::default().event(event).data(data)
}

/// Read a `Last-Event-ID` cursor off the request.
///
/// Only a decimal sequence number is ours. Anything else is ignored rather
/// than rejected: an opaque id from a proxy or an older client should start
/// the stream from the durable head, not fail to open it — a refused stream
/// looks like an outage to a reconnecting client.
fn last_event_id(headers: &HeaderMap) -> Option<u64> {
    headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    let truncated: String = text.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{truncated}...")
}

fn resolve_skills_dir(config: &Config, workspace: &std::path::Path) -> PathBuf {
    if config.skills_config().scan_codewhale_only() {
        if config.skills_dir.is_some() {
            return config.skills_dir();
        }
        if let Some(codewhale_skills_dir) = crate::skills::codewhale_workspace_skills_dir(workspace)
            && crate::skills::skills_dir_allowed_by_workspace_trust(
                workspace,
                &codewhale_skills_dir,
            )
            && let Ok(canonical_skills) = fs::canonicalize(&codewhale_skills_dir)
        {
            return canonical_skills;
        }
        return config.skills_dir();
    }

    // Canonicalize the workspace once so the symlink-containment check below
    // compares like-for-like. If the workspace can't be canonicalized at all
    // (e.g. it doesn't exist on disk yet) fall back to the configured global
    // skills dir rather than risk constructing paths from a non-existent root.
    let canonical_workspace = match fs::canonicalize(workspace) {
        Ok(path) => path,
        Err(_) => return config.skills_dir(),
    };
    for candidate in [
        canonical_workspace.join(".codewhale/skills"),
        canonical_workspace.join(".agents/skills"),
        canonical_workspace.join(".claude/skills"),
        canonical_workspace.join(".opencode/skills"),
        canonical_workspace.join(".cursor/skills"),
    ] {
        // Re-canonicalize the candidate so a `.agents/skills` symlink to e.g.
        // `/etc` cannot promote arbitrary filesystem locations into the
        // skills directory. The candidate must still resolve under the
        // canonicalized workspace root after symlink expansion.
        if let Ok(canon) = fs::canonicalize(&candidate)
            && canon.starts_with(&canonical_workspace)
            && canon.is_dir()
            && crate::skills::skills_dir_allowed_by_workspace_trust(workspace, &canon)
        {
            return canon;
        }
    }
    let flat = canonical_workspace.join("skills");
    if config.skills_config().flat_workspace_root()
        && let Ok(canonical) = fs::canonicalize(&flat)
        && canonical.starts_with(&canonical_workspace)
        && canonical.is_dir()
        && crate::skills::skills_dir_allowed_by_workspace_trust(workspace, &canonical)
    {
        return canonical;
    }
    config.skills_dir()
}

fn skills_search_directories(
    workspace: &FsPath,
    skills_dir: &FsPath,
    mode: crate::skills::SkillDiscoveryMode,
) -> Vec<PathBuf> {
    crate::skills::skill_directories_for_workspace_and_dir(workspace, skills_dir, mode)
}

fn discover_skills_for_runtime_api(
    workspace: &FsPath,
    skills_dir: &FsPath,
    mode: crate::skills::SkillDiscoveryMode,
    plugins: Option<&crate::plugins::PluginRegistry>,
) -> (crate::skills::SkillRegistry, Vec<PathBuf>) {
    let directories = skills_search_directories(workspace, skills_dir, mode);
    let registry = crate::skills::discover_from_directories_in_workspace(
        directories.clone(),
        Some(workspace),
        plugins,
    );
    (registry, directories)
}

fn skill_entry_is_bundled(skill: &crate::skills::Skill, skills_dir: &FsPath) -> bool {
    if !crate::skills::is_bundled_skill_name(&skill.name) {
        return false;
    }

    let expected_path = skills_dir.join(&skill.name).join("SKILL.md");
    paths_refer_to_same_file(&skill.path, &expected_path)
}

/// One `/v1/skills` row, and the row half of `GET /v1/skills/{name}`. Both
/// sites read the same fields from the same `Skill`, so a new routing field
/// lands in the listing and the detail together or not at all.
fn skill_entry_for(skill: &crate::skills::Skill, enabled: bool, skills_dir: &FsPath) -> SkillEntry {
    let (path, source, plugin_id, plugin_generation, plugin_content_hash) = match &skill.source {
        crate::skills::SkillSource::Native => (
            Some(skill.path.clone()),
            "native".to_string(),
            None,
            None,
            None,
        ),
        crate::skills::SkillSource::Plugin {
            plugin_id,
            plugin_name,
            authority,
            ..
        } => (
            None,
            format!("reviewed-plugin-snapshot:{plugin_name}"),
            Some(plugin_id.clone()),
            Some(authority.state_generation),
            Some(authority.content_hash.clone()),
        ),
    };
    let is_bundled = skill_entry_is_bundled(skill, skills_dir);
    SkillEntry {
        name: skill.name.clone(),
        description: skill.description.clone(),
        path,
        source,
        plugin_id,
        plugin_generation,
        plugin_content_hash,
        enabled,
        is_bundled,
        invocation: match skill.invocation {
            crate::skills::SkillInvocation::ModelAndUser => "model+user",
            crate::skills::SkillInvocation::ExplicitOnly => "explicit-only",
            crate::skills::SkillInvocation::ModelOnly => "model-only",
            crate::skills::SkillInvocation::Disabled => "disabled",
        },
        aliases: skill.aliases.clone(),
        // Reported only when `is_bundled` is true, so the two fields cannot
        // disagree: a row that is not the bundle-path copy of its name gets no
        // curated tier, however its name reads. `is_bundled` itself is the
        // pre-existing path test (`skills_dir/<name>/SKILL.md`), and this does
        // not change what it means.
        bundled_tier: is_bundled
            .then(|| crate::skills::bundled_skill_tier(&skill.name))
            .flatten()
            .map(|tier| tier.label()),
    }
}

fn paths_refer_to_same_file(left: &FsPath, right: &FsPath) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn format_skill_search_paths(directories: &[PathBuf]) -> String {
    if directories.is_empty() {
        return "<none>".to_string();
    }
    directories
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Deserialize)]
struct UsageQuery {
    /// ISO-8601 lower bound (inclusive). When omitted, no lower bound.
    since: Option<String>,
    /// ISO-8601 upper bound (inclusive). When omitted, no upper bound.
    until: Option<String>,
    /// Bucket key. One of `day` (default), `model`, `provider`, `thread`.
    group_by: Option<String>,
}

fn parse_iso8601(raw: &str, field: &str) -> Result<chrono::DateTime<Utc>, ApiError> {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| ApiError::bad_request(format!("Invalid {field} (expected RFC 3339): {e}")))
}

async fn get_usage(
    State(state): State<RuntimeApiState>,
    Query(query): Query<UsageQuery>,
) -> Result<Json<Value>, ApiError> {
    let since = match query.since.as_deref() {
        Some(raw) => Some(parse_iso8601(raw, "since")?),
        None => None,
    };
    let until = match query.until.as_deref() {
        Some(raw) => Some(parse_iso8601(raw, "until")?),
        None => None,
    };
    if let (Some(s), Some(u)) = (since, until)
        && s > u
    {
        return Err(ApiError::bad_request("since must be <= until".to_string()));
    }
    let group_by = match query.group_by.as_deref().unwrap_or("day") {
        "day" => UsageGroupBy::Day,
        "model" => UsageGroupBy::Model,
        "provider" => UsageGroupBy::Provider,
        "thread" => UsageGroupBy::Thread,
        other => {
            return Err(ApiError::bad_request(format!(
                "Unsupported group_by '{other}': expected one of day, model, provider, thread"
            )));
        }
    };

    let aggregation = state
        .runtime_threads
        .aggregate_usage(since, until, group_by)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(json!(aggregation)))
}

#[derive(Debug, Deserialize)]
struct SnapshotsQuery {
    /// Maximum number of snapshots to return. Mirrors `/restore list [N]`.
    limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct SnapshotEntry {
    id: String,
    label: String,
    timestamp: i64,
}

async fn list_snapshots(
    State(state): State<RuntimeApiState>,
    Query(query): Query<SnapshotsQuery>,
) -> Result<Json<Vec<SnapshotEntry>>, ApiError> {
    Ok(Json(snapshot_entries_for_workspace(
        &state.workspace,
        query,
    )?))
}

async fn restore_snapshot(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if !snapshot_id_is_well_formed(&id) {
        return Err(ApiError::bad_request(
            "snapshot id must be the exact hexadecimal id reported by GET /v1/snapshots",
        ));
    }
    let reservation = state
        .runtime_threads
        .workspace_restore_guard(&state.workspace)
        .await
        .map_err(map_thread_err)?;
    let restored_id = id.clone();
    tokio::task::spawn_blocking(move || {
        let _reservation = reservation;
        restore_snapshot_for_workspace(&state.workspace, &restored_id)
    })
    .await
    .map_err(|e| ApiError::internal(format!("Restore task failed: {e}")))??;
    Ok(Json(json!({
        "restored": id,
    })))
}

fn restore_snapshot_for_workspace(workspace: &FsPath, id: &str) -> Result<(), ApiError> {
    let repo = crate::snapshot::SnapshotRepo::open_or_init(workspace)
        .map_err(|e| ApiError::internal(format!("Snapshot repo init failed: {e}")))?;
    let snapshot_id = crate::snapshot::SnapshotId::parse(id)
        .map_err(|e| ApiError::bad_request(format!("Invalid snapshot id: {e}")))?;
    repo.restore(&snapshot_id)
        .map_err(|e| ApiError::internal(format!("Snapshot restore failed: {e}")))
}

fn snapshot_entries_for_workspace(
    workspace: &FsPath,
    query: SnapshotsQuery,
) -> Result<Vec<SnapshotEntry>, ApiError> {
    const DEFAULT_LIMIT: usize = 20;
    const MAX_LIMIT: usize = 100;

    let limit = match query.limit.unwrap_or(DEFAULT_LIMIT) {
        1..=MAX_LIMIT => query.limit.unwrap_or(DEFAULT_LIMIT),
        other => {
            return Err(ApiError::bad_request(format!(
                "limit must be between 1 and {MAX_LIMIT}; got {other}",
            )));
        }
    };
    let repo = crate::snapshot::SnapshotRepo::open_or_init(workspace)
        .map_err(|e| ApiError::internal(format!("Snapshot repo unavailable: {e}")))?;
    let snapshots = repo
        .list(limit)
        .map_err(|e| ApiError::internal(format!("Failed to list snapshots: {e}")))?;
    Ok(snapshots
        .into_iter()
        .map(|snapshot| SnapshotEntry {
            id: snapshot.id.as_str().to_string(),
            label: snapshot.label,
            timestamp: snapshot.timestamp,
        })
        .collect())
}

// ── Provider / Model catalog endpoints ──

/// Entry in `GET /v1/providers`.
///
/// Exposes the static provider registry so the GUI can render a dynamic
/// provider picker instead of hard-coding `deepseek` only, plus one entry per
/// user-defined `[providers.<name>]` route (#1519) — the same routes the TUI's
/// own provider picker lists, so a route configured in one surface is not
/// missing from the other. The `id` matches `ProviderKind::as_str()`; callers
/// must also preserve `model_provider_id` when present. Both can be pinned to
/// one new thread via `POST /v1/threads` without mutating the runtime's global
/// provider configuration.
#[derive(Debug, Clone, Serialize)]
struct ProviderEntry {
    /// Stable generic provider kind — matches `ProviderKind::as_str()` and is
    /// suitable for `CreateThreadRequest.model_provider`. This is not always
    /// the exact configured route id: named custom routes also require
    /// `model_provider_id` below.
    id: String,
    /// Exact configured provider key for the active route, when one exists.
    /// A named custom route such as `lm-studio` is represented as generic
    /// `id = "custom"` plus `model_provider_id = "lm-studio"` so a new
    /// thread never collapses back to the legacy root custom route.
    model_provider_id: Option<String>,
    /// Human-friendly name for picker UIs (e.g. "DeepSeek", "OpenAI").
    display_name: String,
    /// Default model id for this provider, if any. Empty for pass-through
    /// providers (Ollama / Custom) that expose no built-in catalog.
    default_model: String,
    /// Whether this provider exposes a built-in model list. When false, the
    /// GUI should render a free-text input instead of calling
    /// `/v1/providers/{id}/models`.
    has_model_catalog: bool,
    /// Sanitized structural credential classification for the exact route.
    /// This deliberately contains no credential, endpoint, path, environment
    /// variable, consent-source, or token metadata.
    #[serde(rename = "credentialState")]
    credential_state: ProviderCredentialState,
    /// Which *class* of source owns this route's credential (#6179). A class,
    /// never a value, a path, or an environment variable name — the guarantee
    /// above still holds. Clients need it to tell "you have no key" apart from
    /// "your key is owned elsewhere and this control cannot change it".
    #[serde(rename = "credentialSource")]
    credential_source: secrets::ProviderCredentialSource,
    /// Whether `PUT`/`DELETE /v1/providers/{id}/key` will act on this route.
    /// False means the write would be refused, so the control should be
    /// disabled rather than allowed to fail late.
    #[serde(rename = "credentialWritable")]
    credential_writable: bool,
    /// Why a write is refused, as user-facing copy. Present only when
    /// `credentialWritable` is false.
    #[serde(
        rename = "credentialWritableReason",
        skip_serializing_if = "Option::is_none"
    )]
    credential_writable_reason: Option<&'static str>,
}

/// Stable, non-secret wire projection of provider readiness.
///
/// The richer internal classification remains private to the Runtime. In
/// particular, saved API keys and imported tokens collapse to `configured`,
/// while login and external-consent states collapse to `login_required`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ProviderCredentialState {
    Configured,
    LoginRequired,
    Missing,
    NoAuth,
    Local,
    Legacy,
}

impl From<crate::provider_readiness::CredentialState> for ProviderCredentialState {
    fn from(value: crate::provider_readiness::CredentialState) -> Self {
        use crate::provider_readiness::CredentialState;

        match value {
            CredentialState::Saved | CredentialState::ImportedToken => Self::Configured,
            CredentialState::MissingLogin | CredentialState::ExternalConsent => Self::LoginRequired,
            CredentialState::MissingKey => Self::Missing,
            CredentialState::NoAuth => Self::NoAuth,
            CredentialState::Local => Self::Local,
            CredentialState::Legacy => Self::Legacy,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct ProvidersResponse {
    /// Currently active provider id (matches `GET /v1/config`'s `provider`).
    current: String,
    /// Exact configured id of the active route, when it has one — the same
    /// additive identity a [`ProviderEntry`] carries as `model_provider_id`.
    /// A named custom route reports `current = "custom"` plus this field, so a
    /// client marks the one route that is actually selected instead of
    /// whichever entry happens to share the generic kind.
    current_provider_id: Option<String>,
    providers: Vec<ProviderEntry>,
}

/// Entry in `GET /v1/providers/{id}/models`.
#[derive(Debug, Clone, Serialize)]
struct ProviderModelEntry {
    /// Canonical model id suitable for `POST /v1/threads`'s `model` field.
    id: String,
    /// Image-input support reported by the exact resolved provider/model
    /// offering. Unknown stays unknown: the API never guesses from a model
    /// name or transport protocol.
    image_input: codewhale_config::route::CapabilityState,
    output_token_limit: codewhale_config::route::CapabilityState,
    reasoning_effort: codewhale_config::route::CapabilityState,
    reasoning_effort_levels: Vec<String>,
    reasoning_effort_source: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
struct ProviderModelsResponse {
    provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_provider_id: Option<String>,
    models: Vec<ProviderModelEntry>,
    total: usize,
    #[serde(rename = "nextCursor", skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
}

const DEFAULT_PROVIDER_MODELS_PAGE_SIZE: usize = 100;
const MAX_PROVIDER_MODELS_PAGE_SIZE: usize = 250;
const MAX_PROVIDER_MODELS_CATALOG_SIZE: usize = 10_000;
const PROVIDER_MODELS_CURSOR_VERSION: u8 = 1;
const MAX_PROVIDER_MODELS_CURSOR_BYTES: usize = 1_024;
const MAX_PROVIDER_MODELS_FILTER_CHARS: usize = 128;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProviderModelsCursor {
    version: u8,
    provider: String,
    filter: String,
    catalog_fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    route_fingerprint: Option<String>,
    offset: usize,
}

fn normalized_provider_model_filter(filter: Option<&str>) -> Result<String, ApiError> {
    let filter = filter.unwrap_or_default().trim();
    if filter.chars().count() > MAX_PROVIDER_MODELS_FILTER_CHARS {
        return Err(ApiError::bad_request(format!(
            "Provider model filter exceeds {MAX_PROVIDER_MODELS_FILTER_CHARS} characters"
        )));
    }
    Ok(filter.to_lowercase())
}

fn encode_provider_models_cursor(cursor: &ProviderModelsCursor) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(cursor)
        .map_err(|error| ApiError::internal(format!("Could not encode model cursor: {error}")))?;
    if bytes.len() > MAX_PROVIDER_MODELS_CURSOR_BYTES {
        return Err(ApiError::internal(
            "Provider model cursor exceeds the safe size limit",
        ));
    }
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_provider_models_cursor(value: &str) -> Result<ProviderModelsCursor, ApiError> {
    if value.is_empty() || value.len() > MAX_PROVIDER_MODELS_CURSOR_BYTES.div_ceil(3) * 4 {
        return Err(ApiError::bad_request("Invalid provider model cursor"));
    }
    let bytes = URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::bad_request("Invalid provider model cursor"))?;
    if bytes.len() > MAX_PROVIDER_MODELS_CURSOR_BYTES {
        return Err(ApiError::bad_request("Invalid provider model cursor"));
    }
    let cursor: ProviderModelsCursor = serde_json::from_slice(&bytes)
        .map_err(|_| ApiError::bad_request("Invalid provider model cursor"))?;
    if cursor.version != PROVIDER_MODELS_CURSOR_VERSION
        || cursor.provider.is_empty()
        || cursor.offset == 0
        || cursor.offset > MAX_PROVIDER_MODELS_CATALOG_SIZE
        || cursor.catalog_fingerprint.len() != 64
        || !cursor
            .catalog_fingerprint
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApiError::bad_request("Invalid provider model cursor"));
    }
    Ok(cursor)
}

fn paginate_provider_models(
    provider: &str,
    mut models: Vec<ProviderModelEntry>,
    params: &ListProviderModelsParams,
    route_fingerprint: Option<String>,
) -> Result<ProviderModelsResponse, ApiError> {
    let filter = normalized_provider_model_filter(params.filter.as_deref())?;
    let limit = params.limit.unwrap_or(DEFAULT_PROVIDER_MODELS_PAGE_SIZE);
    if limit == 0 || limit > MAX_PROVIDER_MODELS_PAGE_SIZE {
        return Err(ApiError::bad_request(format!(
            "Provider model page limit must be between 1 and {MAX_PROVIDER_MODELS_PAGE_SIZE}"
        )));
    }

    models.sort_by(|left, right| {
        left.id
            .to_lowercase()
            .cmp(&right.id.to_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    models.dedup_by(|left, right| left.id.eq_ignore_ascii_case(&right.id));
    if models.len() > MAX_PROVIDER_MODELS_CATALOG_SIZE {
        return Err(ApiError::internal(format!(
            "Provider model catalog exceeds the safe {MAX_PROVIDER_MODELS_CATALOG_SIZE}-row limit"
        )));
    }
    if !filter.is_empty() {
        models.retain(|entry| entry.id.to_lowercase().contains(&filter));
    }

    // A live catalog can refresh between requests. Bind the opaque position
    // to the exact sorted projection so additions before the cursor cannot
    // disappear silently from a multi-page response.
    let catalog_bytes = serde_json::to_vec(&models).map_err(|error| {
        ApiError::internal(format!("Could not fingerprint model catalog: {error}"))
    })?;
    let catalog_fingerprint = Sha256::digest(catalog_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let start = if let Some(encoded) = params.cursor.as_deref() {
        let cursor = decode_provider_models_cursor(encoded)?;
        if cursor.provider != provider
            || cursor.filter != filter
            || cursor.route_fingerprint != route_fingerprint
        {
            return Err(ApiError::bad_request(
                "Provider model cursor does not match this provider, configured route, and filter",
            ));
        }
        if cursor.catalog_fingerprint != catalog_fingerprint {
            return Err(ApiError::bad_request(
                "Provider model cursor is stale; restart from the first page",
            ));
        }
        cursor.offset
    } else {
        0
    };
    let total = models.len();
    let end = start.saturating_add(limit).min(total);
    let page = models
        .get(start..end)
        .ok_or_else(|| ApiError::bad_request("Provider model cursor is outside the catalog"))?
        .to_vec();
    let next_cursor = if end < total {
        Some(encode_provider_models_cursor(&ProviderModelsCursor {
            version: PROVIDER_MODELS_CURSOR_VERSION,
            provider: provider.to_string(),
            filter,
            catalog_fingerprint,
            route_fingerprint,
            offset: end,
        })?)
    } else {
        None
    };

    Ok(ProviderModelsResponse {
        provider: provider.to_string(),
        model_provider_id: params.model_provider_id.clone(),
        models: page,
        total,
        next_cursor,
    })
}

fn push_unique_model(models: &mut Vec<String>, model: &str) {
    let model = model.trim();
    if !model.is_empty()
        && !models
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(model))
    {
        models.push(model.to_string());
    }
}

fn provider_models_for_api(
    config: &Config,
    identity: &crate::config::ProviderIdentity,
) -> Vec<String> {
    if config.verify_provider_identity(identity).is_err() {
        return Vec::new();
    }
    let provider = identity.provider;
    let mut models = Vec::new();
    if let Some(model) = config
        .provider_config_for(identity)
        .and_then(|entry| entry.model.as_deref())
    {
        push_unique_model(&mut models, model);
    }
    if config.active_provider_identity().ok().as_ref() == Some(identity) {
        let active_model = provider_default_model_for_api(config, identity);
        if !active_model.trim().eq_ignore_ascii_case("auto") {
            push_unique_model(&mut models, &active_model);
        }
    }
    let exact_catalog = crate::provider_catalog_live::cached_entry_for_route(
        provider,
        identity.key.as_str(),
        &config.base_url_for_route(identity),
    )
    .ok()
    .flatten()
    .is_some_and(|entry| entry.fetched_at > 0);
    // A pass-through provider normally lists only what its own live catalog
    // returned. When that catalog cannot exist (an OAuth route), the catalog
    // lake's next layers (Models.dev, then the bundled snapshot) answer.
    if !config.model_ids_pass_through_for_provider(identity)
        || exact_catalog
        || crate::provider_lake::live_catalog_unavailable(config, identity)
    {
        for model in crate::provider_lake::models_for_provider(config, identity) {
            push_unique_model(&mut models, &model);
        }
    }
    for model in config.custom_models.as_deref().unwrap_or_default() {
        if crate::provider_lake::configured_model_for_route(
            config,
            provider,
            identity.key.as_str(),
            &config.base_url_for_route(identity),
            &model.id,
        )
        .is_some()
            && !models.contains(&model.id)
        {
            models.push(model.id.clone());
        }
    }
    if provider == ProviderKind::Ollama {
        models.retain(|model| !crate::config::is_unresolved_local_ollama_model(model));
    }
    models
}

fn provider_model_image_input_for_api(
    config: &Config,
    identity: &crate::config::ProviderIdentity,
    model: &str,
) -> codewhale_config::route::CapabilityState {
    crate::route_runtime::resolve_runtime_route_for_identity(config, identity, Some(model))
        .map(|route| route.candidate.capabilities().image_input)
        .unwrap_or_default()
}

fn provider_model_output_token_limit_for_api(
    config: &Config,
    identity: &crate::config::ProviderIdentity,
    model: &str,
) -> codewhale_config::route::CapabilityState {
    use codewhale_config::route::CapabilityState;
    crate::route_runtime::resolve_runtime_route_for_identity(config, identity, Some(model))
        .map(|route| {
            if crate::route_budget::route_supports_output_token_limit(
                route.identity.provider,
                route.candidate.protocol(),
            ) {
                CapabilityState::Supported
            } else {
                CapabilityState::Unsupported
            }
        })
        .unwrap_or_default()
}

fn provider_model_entry_for_api(
    config: &Config,
    identity: &crate::config::ProviderIdentity,
    model: String,
) -> ProviderModelEntry {
    use crate::reasoning_preference::ReasoningEffort;
    use codewhale_config::route::CapabilityState;

    let provider = identity.provider;
    let mut entry = ProviderModelEntry {
        image_input: provider_model_image_input_for_api(config, identity, &model),
        output_token_limit: provider_model_output_token_limit_for_api(config, identity, &model),
        id: model,
        reasoning_effort: CapabilityState::Unknown,
        reasoning_effort_levels: Vec::new(),
        reasoning_effort_source: None,
    };
    // A provider kind and a familiar model name do not establish the
    // capabilities of a different endpoint or named compatible route.
    if provider == ProviderKind::Custom || config.provider_uses_custom_endpoint(identity) {
        return entry;
    }
    if provider == ProviderKind::OpenaiCodex {
        let roster = crate::codex_model_cache::model_roster_for(config);
        if roster.freshness != crate::codex_model_cache::CodexModelCacheFreshness::Fresh {
            return entry;
        }
        let Some(metadata) = roster.metadata_for(&entry.id) else {
            return entry;
        };
        for effort in metadata
            .efforts
            .iter()
            .filter_map(|raw| ReasoningEffort::from_catalog_token(raw))
            // This API advertises active effort controls. Apps currently
            // treats off as omission, not a provider's explicit none value.
            .filter(|effort| *effort != ReasoningEffort::Off)
            // Native compatibility still aliases minimal to low (and auto
            // to medium). Do not advertise a manual tier the wire changes.
            .filter(|effort| effort.api_value_for_provider(provider) == Some(effort.as_setting()))
        {
            let level = effort.as_setting().to_string();
            if !entry.reasoning_effort_levels.contains(&level) {
                entry.reasoning_effort_levels.push(level);
            }
        }
        entry.reasoning_effort_source = Some(roster.source);
        if metadata.reasoning == Some(false) {
            entry.reasoning_effort = CapabilityState::Unsupported;
        }
    } else if let Some(efforts) = ReasoningEffort::catalog_effort_values(provider, &entry.id) {
        entry.reasoning_effort_levels = efforts
            .into_iter()
            .filter(|effort| *effort != ReasoningEffort::Off)
            .map(|effort| effort.as_setting().to_string())
            .collect();
        entry.reasoning_effort_source = Some("catalog");
    } else if crate::route_runtime::resolve_runtime_route_for_identity(
        config,
        identity,
        Some(&entry.id),
    )
    .is_ok_and(|route| route.candidate.capabilities().reasoning == CapabilityState::Unsupported)
    {
        entry.reasoning_effort = CapabilityState::Unsupported;
        entry.reasoning_effort_source = Some("catalog");
    }
    if !entry.reasoning_effort_levels.is_empty() {
        entry.reasoning_effort = CapabilityState::Supported;
    }
    entry
}

fn provider_default_model_for_api(
    config: &Config,
    identity: &crate::config::ProviderIdentity,
) -> String {
    let provider = identity.provider;
    let model = crate::model_inventory::provider_default_model(config, identity);
    if provider == ProviderKind::Ollama && crate::config::is_unresolved_local_ollama_model(&model) {
        String::new()
    } else {
        model
    }
}

pub(crate) fn runtime_chat_model_id_is_safe(value: &str) -> bool {
    let sanitized = crate::cost_status::sanitize_persisted_route_label(value);
    value == value.trim()
        && !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && !value.contains("..")
        && !value.contains("://")
        // Runtime Chat publishes a non-secret selector, never an endpoint or
        // userinfo-bearing authority. Model families that need revisions can
        // use their ordinary slash/dash ids; `@` is intentionally excluded at
        // this trust boundary because `user:password@host:port/path` otherwise
        // passes the generic route-label sanitizer.
        && !value.contains('@')
        && !runtime_chat_model_id_looks_like_host_port(value)
        && !value.starts_with("redacted-")
        && sanitized == value
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b':' | b'/' | b'@' | b'+' | b'-')
        })
}

fn runtime_chat_model_id_looks_like_host_port(value: &str) -> bool {
    let authority = value.split('/').next().unwrap_or(value);
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    !host.is_empty() && !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
}

pub(crate) fn runtime_chat_route_id_is_safe(value: &str) -> bool {
    let sanitized = crate::cost_status::sanitize_persisted_route_label(value);
    value == value.trim()
        && !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && !value.contains("..")
        && !value.starts_with("redacted-")
        && sanitized == value
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn runtime_chat_safe_models(mut models: Vec<String>) -> Result<Vec<String>, String> {
    models.retain(|model| runtime_chat_model_id_is_safe(model));
    models.sort();
    models.dedup();
    if models.len() > MAX_PROVIDER_MODELS_CATALOG_SIZE {
        return Err(format!(
            "The active Runtime provider catalog exceeds the safe {MAX_PROVIDER_MODELS_CATALOG_SIZE}-model relay limit."
        ));
    }
    if models.is_empty() {
        return Err("The active Runtime provider has no safe model catalog.".to_string());
    }
    Ok(models)
}

/// Build the deliberately narrow provider projection used by the account-owned
/// Runtime Chat relay. This is the same active-route truth exposed by the
/// authenticated native `/v1/runtime/info`, `/v1/providers`, and
/// `/v1/providers/{id}/models` endpoints, collapsed to the one exact route the
/// current Runtime can use without moving credentials across the relay.
pub(crate) fn runtime_chat_relay_catalog(
    config: &Config,
    challenge: &str,
) -> Result<Value, String> {
    use crate::provider_readiness::CredentialState;

    const PROTOCOL: &str = "codewhale.runtime-chat-relay.v1";
    if !(32..=128).contains(&challenge.len())
        || !challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("Codewhale returned an invalid Runtime Chat relay challenge.".to_string());
    }

    let mut account_config;
    let ready = config
        .active_provider_identity()
        .ok()
        .is_some_and(|identity| {
            matches!(
                crate::provider_readiness::credential_state_for_provider(config, &identity),
                CredentialState::Saved
                    | CredentialState::ImportedToken
                    | CredentialState::Local
                    | CredentialState::NoAuth
            )
        });
    let config = if ready {
        config
    } else {
        account_config = config.clone();
        let identity = account_config
            .resolve_persisted_provider_identity(Some("codewhale"), Some("codewhale"))?;
        if account_config.account_model_api_key(&identity).is_none() {
            return Err(
                "Connect account model access before using the Codewhale agent.".to_string(),
            );
        }
        account_config.scope_to_provider_identity(&identity)?;
        &account_config
    };

    let identity = config
        .active_provider_identity()
        .map_err(|_| "The active Runtime provider identity is invalid.".to_string())?;
    let provider = identity.provider;
    let credential_state =
        match crate::provider_readiness::credential_state_for_provider(config, &identity) {
            CredentialState::Saved | CredentialState::ImportedToken => "configured",
            CredentialState::Local => "local",
            CredentialState::NoAuth => "no_auth",
            CredentialState::MissingKey
            | CredentialState::MissingLogin
            | CredentialState::ExternalConsent
            | CredentialState::Legacy => {
                return Err("The active Runtime provider is not ready for Chat.".to_string());
            }
        };

    let models = runtime_chat_safe_models(provider_models_for_api(config, &identity))?;
    let requested_default = provider_default_model_for_api(config, &identity);
    if provider == ProviderKind::Ollama && requested_default.is_empty() {
        return Err("The active local provider has no fresh default model catalog.".to_string());
    }
    let default_model = models
        .iter()
        .find(|model| model.as_str() == requested_default)
        .cloned()
        .unwrap_or_else(|| models[0].clone());
    let model_provider_id = identity
        .persisted_id()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| provider.as_str())
        .to_string();
    if !runtime_chat_route_id_is_safe(&model_provider_id) {
        return Err("The active Runtime model-provider identity is invalid.".to_string());
    }

    Ok(json!({
        "protocol": PROTOCOL,
        "challenge": challenge,
        "runtime": {
            "service": "codewhale-runtime-api",
            "apiVersion": RUNTIME_API_VERSION,
            "codewhaleVersion": env!("CARGO_PKG_VERSION"),
            "authRequired": true,
            "capabilities": {
                "relay_chat_v1": true,
                "isolated_chat_threads": true,
                "turn_operation_idempotency": true,
                "turn_image_inputs": true,
                "turn_output_token_limit": true,
                "profile_constitution": true,
                "account_model_owner": true,
                "tool_execution": false,
                "stable_event_ids": true,
            },
        },
        "providers": [{
            "id": provider.as_str(),
            "modelProviderId": model_provider_id,
            "displayName": identity.compatibility().map(|row| row.label).unwrap_or(identity.key.as_str()),
            "defaultModel": default_model,
            "credentialState": credential_state,
            "models": models.into_iter().map(|model| {
                let entry = provider_model_entry_for_api(config, &identity, model);
                json!({
                    "imageInput": entry.image_input,
                    "outputTokenLimit": entry.output_token_limit,
                    "id": entry.id,
                    "reasoningEffort": entry.reasoning_effort,
                    "reasoningEffortLevels": entry.reasoning_effort_levels,
                    "reasoningEffortSource": entry.reasoning_effort_source,
                })
            }).collect::<Vec<_>>(),
        }],
    }))
}

/// Names of the user-defined `[providers.<name>]` routes this runtime can
/// route to, sorted case-insensitively.
///
/// Mirrors the TUI provider picker's own row filter
/// (`custom_provider_dashboard_rows`), so the routes one surface offers are the
/// routes the other offers. A table without the `openai-compatible` kind is not
/// a routable custom route and stays out of both.
fn configured_custom_provider_routes(config: &Config) -> Vec<String> {
    let Some(providers) = config.providers.as_ref() else {
        return Vec::new();
    };
    let mut names: Vec<String> = providers
        .custom
        .iter()
        .filter(|(_, entry)| entry.is_openai_compatible_custom())
        .map(|(name, _)| name.clone())
        .collect();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names
}

/// Project one provider route into the `GET /v1/providers` wire shape.
///
/// `exact_route` names the user-defined `[providers.<name>]` entry being
/// listed, and `config` must already be scoped to it. The entry then describes
/// that route — its own name and its own catalog — while `id` stays the generic
/// kind every other endpoint addresses it by. Without it, the entry describes
/// the built-in provider as before.
fn provider_entry_for_api(
    config: &Config,
    identity: &crate::config::ProviderIdentity,
) -> ProviderEntry {
    let writeability = secrets::credential_writeability(config, identity);
    ProviderEntry {
        id: identity.persisted_kind().to_string(),
        model_provider_id: identity.persisted_id().map(str::to_string),
        display_name: identity
            .compatibility()
            .map(|row| row.label.to_string())
            .unwrap_or_else(|| format!("{} (custom)", identity.key)),
        default_model: provider_default_model_for_api(config, identity),
        has_model_catalog: !provider_models_for_api(config, identity).is_empty(),
        credential_state: crate::provider_readiness::credential_state_for_provider(
            config, identity,
        )
        .into(),
        credential_source: writeability.source,
        credential_writable: writeability.writable,
        credential_writable_reason: writeability.reason,
    }
}

async fn list_providers(
    State(state): State<RuntimeApiState>,
) -> Result<Json<ProvidersResponse>, ApiError> {
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let config = state.config.read().clone();
        secrets::invalidate_stale_account_catalog(&config);
        let active_identity = config.active_provider_identity().ok();
        let current = active_identity.as_ref().map_or_else(|| config.provider.clone().unwrap_or_else(|| "unavailable".into()), |identity| identity.persisted_kind().to_string());
        let mut providers = config.provider_identities().iter().filter(|identity| identity.provider != ProviderKind::Antigravity).map(|identity| provider_entry_for_api(&config, identity)).collect::<Vec<_>>();
        providers.extend(config.unadmitted_provider_keys().into_iter().map(|key| ProviderEntry {
            id: key.to_string(), model_provider_id: Some(key.to_string()), display_name: format!("{key} (unavailable)"),
            default_model: String::new(), has_model_catalog: false,
            credential_state: ProviderCredentialState::Legacy, credential_source: secrets::ProviderCredentialSource::None,
            credential_writable: false, credential_writable_reason: Some("This configured route is unavailable; repair its exact provider definition before using it."),
        }));
        Ok(Json(ProvidersResponse {
            current_provider_id: active_identity.as_ref().and_then(|identity| identity.persisted_id()).map(str::to_string),
            current,
            providers,
        }))
    })
    .await
    .map_err(|_| ApiError::internal("Provider listing failed"))?
}

#[derive(Debug, Default, Deserialize)]
struct ListProviderModelsParams {
    /// Exact configured provider identity; omission retains the legacy projection.
    #[serde(default)]
    model_provider_id: Option<String>,
    /// Optional case-insensitive substring filter applied before pagination.
    #[serde(default)]
    filter: Option<String>,
    /// Opaque continuation cursor returned as `nextCursor` by the prior page.
    #[serde(default)]
    cursor: Option<String>,
    /// Page size. The bounded default is 100 and the maximum is 250.
    #[serde(default)]
    limit: Option<usize>,
}

fn provider_models_identity(
    config: &Config,
    id: &str,
    exact_id: Option<&str>,
) -> Result<crate::config::ProviderIdentity, ApiError> {
    if id.is_empty() || id != id.trim() || id.chars().any(char::is_control) {
        return Err(ApiError::bad_request("provider must be an exact selection"));
    }
    if exact_id.is_some_and(|value| {
        value.is_empty() || value != value.trim() || value.chars().any(char::is_control)
    }) {
        return Err(ApiError::bad_request(
            "model_provider_id must be an exact configured identity",
        ));
    }
    // A supplied pair is re-admitted exactly. An omitted id denotes the named
    // selection, with literal custom using only its parse-proven root origin.
    let identity = match exact_id {
        Some(exact_id) => config.resolve_persisted_provider_identity(Some(id), Some(exact_id)),
        None => config.legacy_selection_identity(id),
    }
    .map_err(ApiError::bad_request)?;
    config
        .verify_provider_identity(&identity)
        .map_err(ApiError::bad_request)?;
    Ok(identity)
}

async fn list_provider_models(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(params): Query<ListProviderModelsParams>,
) -> Result<Json<ProviderModelsResponse>, ApiError> {
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let config = state.config.read().clone();
        secrets::invalidate_stale_account_catalog(&config);
        let identity = provider_models_identity(&config, &id, params.model_provider_id.as_deref())?;
        let route = serde_json::to_vec(&(
            identity.persisted_kind(),
            identity.persisted_id(),
            config.base_url_for_route(&identity),
        ))
        .map_err(|error| {
            ApiError::internal(format!("Could not fingerprint provider route: {error}"))
        })?;
        let route_fingerprint = Some(crate::hashing::sha256_hex(route));
        let models = provider_models_for_api(&config, &identity)
            .into_iter()
            .map(|id| provider_model_entry_for_api(&config, &identity, id))
            .collect();
        paginate_provider_models(
            identity.persisted_kind(),
            models,
            &params,
            route_fingerprint,
        )
        .map(Json)
    })
    .await
    .map_err(|_| ApiError::internal("Provider model listing failed"))?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RefreshProviderModelsParams {
    model_provider_id: Option<String>,
}

async fn refresh_provider_models(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Query(params): Query<RefreshProviderModelsParams>,
) -> Result<Json<crate::provider_lake::CatalogUpdateReceipt>, ApiError> {
    let runtime = tokio::runtime::Handle::current();
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        let _membership = crate::test_support::join_env_scope(env_ticket);
        let config = state.config.read().clone();
        let identity = provider_models_identity(&config, &id, params.model_provider_id.as_deref())?;
        Ok(Json(runtime.block_on(
            crate::provider_lake::update_provider_catalog(&config, &identity),
        )))
    })
    .await
    .map_err(|_| ApiError::internal("Provider model refresh failed"))?
}

/// Request body for `POST /v1/providers/{id}/switch`.
///
/// Mirrors the TUI's `AppAction::SwitchProvider { provider, model }` payload
/// (see `tui/ui.rs::switch_provider`). `model` is optional: when omitted,
/// the runtime resolves the active model from `[providers.<id>].model` (or
/// the provider's built-in default) and **does not** persist a `model` key,
/// so the user's per-provider config is preserved. When provided, the model
/// is normalized and persisted in the target provider's canonical model slot.
#[derive(Debug, Deserialize, Default)]
struct SwitchProviderRequest {
    #[serde(default)]
    model: Option<String>,
    /// Exact configured provider id for the target route.
    ///
    /// Named `[providers.<name>]` routes are addressed the same way this API
    /// addresses them everywhere else: the generic kind in the id (`custom`)
    /// plus this additive exact id, exactly as `ProviderEntry`
    /// `model_provider_id` and `POST /v1/threads` already carry it. Omitted
    /// keeps the pre-existing meaning — the built-in id, or the literal
    /// `[providers.custom]` route (where the older top-level custom route
    /// lives since #6394).
    #[serde(default)]
    model_provider_id: Option<String>,
}

/// Response for `POST /v1/providers/{id}/switch`.
#[derive(Debug, Serialize)]
struct SwitchProviderResponse {
    /// The provider id that was switched to (echoes the path).
    provider: String,
    /// The resolved active model after the switch. This is the model the
    /// runtime will use for new turns — either the user-supplied override
    /// or the value resolved from `[providers.<id>].model` / the
    /// provider's built-in default. The GUI should display *this* value,
    /// not `ProviderEntry.default_model`, to avoid showing the catalog
    /// default when the user has configured a different model.
    model: String,
    /// False while the selected local endpoint has no executable default.
    model_available: bool,
    /// Human-readable status message for logging/toasts.
    message: String,
    /// Whether the new provider + model were persisted to config.toml.
    persisted: bool,
}

/// `POST /v1/providers/{id}/switch` — switch the active provider, optionally
/// overriding the model.
///
/// `{id}` is the generic provider kind (`custom` for every user-defined route).
/// A named `[providers.<name>]` route is named by `model_provider_id` in the
/// body, not by the path: the path keeps naming the kind, so one endpoint
/// cannot disagree with `GET /v1/providers` about what an entry's `id` means.
///
/// This is the GUI-facing counterpart of the TUI's `/provider` slash command
/// (`commands/groups/core/provider.rs`) and `AppAction::SwitchProvider`
/// (`tui/ui.rs::switch_provider`). It exists so the GUI does not have to
/// simulate the switch with multiple `POST /v1/config` calls + a reload,
/// which historically led to two bugs:
///
/// 1. The GUI persisted `model = <catalog default>` even when the user
///    clicked the picker without choosing a model, clobbering a user-set
///    `[providers.<id>].model` (e.g. `glm-2` overwritten with
///    `deepseek-v4-pro`).
/// 2. The GUI then displayed the catalog default instead of the actually
///    resolved model, because it never asked the backend what model was
///    selected.
///
/// Persistence mirrors `switch_provider` (ui.rs:9390-9410):
/// - `provider` is always persisted (root `provider` key).
/// - `model` is persisted **only** when `model_override.is_some()`, via
///   `persist_provider_model_key` (writes `[providers.<id>].model`, retaining
///   the root field only for a legacy literal custom route). Provider and model
///   are committed together through the canonical Config writer.
/// - Config is reloaded from disk and synced to active engines via
///   `runtime_threads.reload_config`, exactly like `POST /v1/config/reload`.
/// - A reload that fails or is rejected rolls the persisted selection back
///   (only while the file still holds what this write left), so disk and the
///   running config never disagree about the provider. The error says which.
async fn switch_provider(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<SwitchProviderRequest>,
) -> Result<Json<SwitchProviderResponse>, ApiError> {
    use crate::config_persistence;

    let trimmed_id = id.trim();
    let _target = ProviderKind::parse_config_identity(trimmed_id).ok_or_else(|| {
        // A configured `[providers.<name>]` route is a route this runtime can
        // and does switch to — but only through the generic kind, because the
        // same name is what `GET /v1/providers` reports as
        // `model_provider_id`. Tell the caller exactly that instead of
        // pretending the route does not exist.
        let named_route = configured_custom_provider_routes(&state.config.read())
            .iter()
            .any(|route| route == trimmed_id);
        if named_route {
            ApiError::bad_request(format!(
                "'{trimmed_id}' is a user-defined route: switch to the generic 'custom' kind with model_provider_id = \"{trimmed_id}\" instead"
            ))
        } else {
            ApiError::bad_request(format!(
                "Unknown provider id '{trimmed_id}'. Call GET /v1/providers for the list of supported ids."
            ))
        }
    })?;
    // Reject the legacy deepseek-cn alias — same guard as list_provider_models.
    if codewhale_config::descriptors::compatibility_for_selector(trimmed_id)
        .is_some_and(|row| row.id == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id)
    {
        return Err(ApiError::bad_request(
            "provider 'deepseek-cn' is a legacy alias; use 'deepseek' instead",
        ));
    }
    let exact_provider_id = req.model_provider_id.as_deref();
    if exact_provider_id.is_some_and(|value| value.trim().is_empty() || value != value.trim()) {
        return Err(ApiError::bad_request(
            "model_provider_id must be a nonempty exact id",
        ));
    }

    // Normalize the optional model override against the *target* provider.
    // Mirrors `set_config`'s `model` branch, which validates against the
    // active route — except here we validate against the target provider,
    // because the active route is about to change.
    // Read normalization and persistence identity from the same route snapshot.
    let (model_override, provider_identity) = {
        let config = state.config.read();
        // An additive exact id is the stronger selector: it names one
        // configured route, so it resolves through the same pinned-identity
        // path saved threads use. Absent, the id keeps its previous meaning.
        let identity = match exact_provider_id {
            Some(exact) => config
                .resolve_persisted_provider_identity(Some(&id), Some(exact))
                .map_err(ApiError::bad_request)?,
            None => config
                .resolve_provider_pin_identity(&id)
                .map_err(ApiError::bad_request)?,
        };
        let mut scoped = config.clone();
        scoped
            .scope_to_provider_identity(&identity)
            .map_err(ApiError::bad_request)?;
        let model = match req.model.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(raw) => Some(normalize_runtime_config_model(&scoped, &identity, raw)?),
        };
        (model, identity)
    };

    // Persist `provider` (always) + `model` (only when explicitly given).
    // This is the critical TUI-parity rule: a bare `/provider <id>` (no
    // model arg) MUST NOT write a `model` key, otherwise the user's
    // per-provider `[providers.<id>].model` config gets overwritten with
    // whatever the runtime resolves as the default.
    // The save, the reload and the undo of a refused save run as one task
    // detached from this request: a client that disconnects or times out
    // mid-reload drops only its wait, never the undo, and never leaves the
    // engines on the new config while `state.config` keeps the old one.
    let task_state = state.clone();
    let task_identity = provider_identity.clone();
    let task_model = model_override.clone();
    let runtime = tokio::runtime::Handle::current();
    #[cfg(test)]
    let env_ticket = crate::test_support::env_scope_ticket();
    let (active_provider, active_model) = tokio::spawn(async move {
        let state = task_state;
        let _one_switch_at_a_time = state.provider_switches.lock().await;
        // Keep the cancellation-safe owned switch, while all filesystem and
        // keyring work runs off the async worker under the same serialization.
        tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let _membership = crate::test_support::join_env_scope(env_ticket);
            let (config_toml, undo) = config_persistence::persist_provider_selection(
                state.config_path.as_deref(),
                &task_identity,
                task_model.as_deref(),
            )
            .map_err(|e| ApiError::internal(format!("Failed to persist provider selection: {e}")))?;

            // Reload config from disk and sync to active engines. This matches
            // `POST /v1/config/reload` exactly: load → validate thread routes →
            // swap in the new config. A failure here means an active thread's
            // route is invalid under the new provider — surface it so the GUI can
            // tell the user to fix their config.
            let applied =
                match Config::load(state.config_path.clone(), state.config_profile.as_deref()) {
                    Ok(mut reloaded) => {
                        reloaded.account_model_access =
                            state.config.read().account_model_access.clone();
                        match runtime.block_on(state.runtime_threads.reload_config(reloaded.clone())) {
                            Ok(_) => Ok(reloaded),
                            Err(err) => Err(ApiError::bad_request(format!(
                                "Config reload rejected: {err}"
                            ))),
                        }
                    }
                    Err(e) => Err(ApiError::internal(format!("Failed to reload config: {e}"))),
                };
            match applied {
                // Report the route this switch applied, not whatever a later
                // switch leaves in `state.config` by the time this reply is built.
                Ok(reloaded) => {
                    let applied_identity = reloaded.active_provider_identity().map_err(ApiError::bad_request)?;
                    reloaded.verify_provider_identity(&task_identity).map_err(ApiError::conflict)?;
                    if applied_identity != task_identity { return Err(ApiError::conflict("Persisted provider selection differs from the captured switch.")); }
                    let provider = applied_identity.provider;
                    let model = provider_default_model_for_api(&reloaded, &applied_identity);
                    *state.config.write() = reloaded;
                    Ok::<_, ApiError>((provider, model))
                }
                // A rejected switch must not stay on disk, or the next restart or
                // reload silently applies the switch this response reports as
                // refused. Only this save is taken back; a newer one wins.
                Err(mut error) => {
                    let path = config_toml.display();
                    match undo.undo() {
                        Ok(true) => {}
                        Ok(false) => {
                            error.message = format!(
                                "{}; {path} changed after this switch was saved, so the newer contents were kept",
                                error.message
                            );
                        }
                        Err(restore) => {
                            error.message = format!(
                                "{}; the provider selection could not be reverted in {path}: {restore}",
                                error.message
                            );
                        }
                    }
                    Err(error)
                }
            }
        })
        .await
        .map_err(|_| ApiError::internal("provider switch blocking task failed"))?
    })
    .await
    .map_err(|_| ApiError::internal("provider switch task failed"))??;

    let model_available = !active_model.is_empty();
    // Name the route the user selected, not the kind it routes through: a
    // named custom route reports `custom` as its kind, which names nothing the
    // user ever typed. Every other route keeps reporting its canonical kind.
    let active_label = if active_provider == ProviderKind::Custom {
        provider_identity.key.to_string()
    } else {
        active_provider.as_str().to_string()
    };
    let message = if !model_available {
        format!(
            "Provider switched to {active_label}; refresh its catalog or select an explicit model."
        )
    } else if model_override.is_some() {
        format!("Provider switched to {active_label} (model: {active_model}).")
    } else {
        format!(
            "Provider switched to {active_label} (model: {active_model}, resolved from config)."
        )
    };

    Ok(Json(SwitchProviderResponse {
        provider: active_label,
        model: active_model,
        model_available,
        message,
        persisted: true,
    }))
}

// ── Config endpoints ──

/// GUI-relevant config snapshot returned by `GET /v1/config`.
#[derive(Debug, Clone, Serialize)]
struct GuiConfigResponse {
    model: String,
    model_available: bool,
    provider: String,
    approval_mode: String,
    reasoning_effort: String,
    auto_compact: bool,
    cost_currency: String,
    default_mode: String,
    default_model: String,
    base_url: String,
    allow_shell: bool,
    mcp_config_path: String,
    subagents_enabled: bool,
    subagents_max_depth: u32,
    show_thinking: bool,
    thinking_default_expanded: bool,
    thinking_highlight: bool,
    show_tool_details: bool,
    inline_diffs: String,
    locale: String,
    max_history: usize,
    workspace_follow_symlinks: bool,
    calm_mode: bool,
    sandbox_mode: String,
    strict_tool_mode: bool,
    memory_enabled: bool,
    search_provider: String,
    /// How `search_provider` was chosen: `default` / `config` /
    /// `env override` / `tavily key`. Runtime-only — never persisted.
    search_provider_source: String,
    prompt_suggestion: bool,
    /// Effective device settings, using the same leaf vocabulary as CLI/TUI.
    notifications: std::collections::BTreeMap<String, String>,
}

/// Request body for `POST /v1/config` (set a single config key).
#[derive(Debug, Deserialize)]
struct SetConfigRequest {
    key: String,
    value: String,
    #[serde(default)]
    persist: bool,
}

/// Response for `POST /v1/config` (set a single config key).
#[derive(Debug, Serialize)]
struct SetConfigResponse {
    key: String,
    value: String,
    message: String,
    persisted: bool,
    requires_reload: bool,
}

fn persist_runtime_tui_setting(key: &str, value: &str) -> Result<(), ApiError> {
    // Validate against a throwaway copy first, so an invalid value is still a
    // 400 rather than an internal error raised from inside the transaction.
    let mut probe = crate::settings::Settings::load_persisted()
        .map_err(|e| ApiError::internal(format!("Failed to load settings: {e}")))?;
    probe
        .set(key, value)
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    // The write itself re-applies the key inside `Settings::transact`, so it
    // cannot save the stale snapshot above over a concurrent writer's field.
    crate::settings::Settings::transact(|settings| settings.set(key, value))
        .map_err(|e| ApiError::internal(format!("Failed to save settings: {e}")))
}

/// Response for `POST /v1/config/reload`.
#[derive(Debug, Serialize)]
struct ReloadConfigResponse {
    message: String,
}

async fn get_config(
    State(state): State<RuntimeApiState>,
) -> Result<Json<GuiConfigResponse>, ApiError> {
    let config = state.config.read();
    let settings = crate::settings::Settings::load_persisted().unwrap_or_default();
    let mcp_config_path = config.mcp_config_path().display().to_string();

    let resolved_model = runtime_request_model(&config, None);
    let model_available = resolved_model.is_ok();
    let model = resolved_model.unwrap_or_default();

    let active_identity = config
        .active_provider_identity()
        .map_err(ApiError::bad_request)?;
    let provider = active_identity.key.to_string();

    let approval_mode = config
        .approval_policy
        .as_deref()
        .unwrap_or("suggest")
        .to_string();
    let reasoning_effort = config.reasoning_effort().unwrap_or("auto").to_string();
    let cost_currency = settings.cost_currency.clone();
    let default_mode = settings.default_mode.as_str().to_string();
    // This field remains the DeepSeek preference even when another provider
    // is active, and follows the CN slot while the CN route is active — the CN
    // route resolves `[providers.deepseek_cn].model`, not the primary slot.
    // The root field is a legacy fallback for unmigrated configs.
    let identity =
        if active_identity.key.as_str() == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id {
            active_identity.clone()
        } else {
            config
                .builtin_provider_identity(ProviderKind::Deepseek)
                .map_err(ApiError::bad_request)?
        };
    let mut deepseek_config = config.clone();
    deepseek_config
        .scope_to_provider_identity(&identity)
        .map_err(ApiError::bad_request)?;
    let default_model = deepseek_config.default_model();
    let base_url = config.active_route_base_url().to_string();

    Ok(Json(GuiConfigResponse {
        model,
        model_available,
        provider,
        approval_mode,
        reasoning_effort,
        auto_compact: settings.auto_compact,
        cost_currency,
        default_mode,
        default_model,
        base_url,
        allow_shell: config.allow_shell(),
        mcp_config_path,
        subagents_enabled: config.subagents_enabled(),
        subagents_max_depth: config.subagent_max_spawn_depth(),
        show_thinking: settings.show_thinking,
        thinking_default_expanded: settings.thinking_default_expanded,
        thinking_highlight: settings.thinking_highlight,
        show_tool_details: settings.show_tool_details,
        inline_diffs: settings.inline_diffs.clone(),
        locale: settings.locale.clone(),
        max_history: settings.max_input_history,
        workspace_follow_symlinks: settings.workspace_follow_symlinks,
        calm_mode: settings.calm_mode,
        sandbox_mode: config
            .sandbox_mode
            .clone()
            .unwrap_or_else(|| "workspace-write".to_string()),
        strict_tool_mode: config.strict_tool_mode.unwrap_or(false),
        memory_enabled: config.memory_enabled(),
        search_provider: config.search_provider().as_str().to_string(),
        search_provider_source: config
            .search_provider_resolution()
            .source
            .as_str()
            .to_string(),
        prompt_suggestion: config.prompt_suggestion_enabled(),
        notifications: codewhale_config::notifications::NotificationSetting::ALL
            .into_iter()
            .map(|setting| {
                (
                    setting.key().to_string(),
                    config.notifications_config().display(setting),
                )
            })
            .collect(),
    }))
}

async fn set_config(
    State(state): State<RuntimeApiState>,
    Json(req): Json<SetConfigRequest>,
) -> Result<Json<SetConfigResponse>, ApiError> {
    use crate::config_persistence;

    let key = req.key.to_lowercase();
    let mut value = req.value;
    let persist = req.persist;

    // Reuse the shared validator and locked leaf writer, including the active
    // profile's existing owner. Dry runs validate too; a typo must never look
    // like an accepted device setting. Reload remains the existing apply step.
    if codewhale_config::notifications::in_namespace(&key) {
        use codewhale_config::notifications::{NotificationConfigUpdate, NotificationSetting};
        let setting = NotificationSetting::required(&key)
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        let update = NotificationConfigUpdate::parse(setting, &value)
            .map_err(|error| ApiError::bad_request(error.to_string()))?;
        if persist {
            let path = config_persistence::config_toml_path(state.config_path.as_deref()).map_err(
                |error| ApiError::internal(format!("Failed to resolve config: {error}")),
            )?;
            update
                .persist_for_profile(&path, state.config_profile.as_deref())
                .map_err(|error| {
                    ApiError::internal(format!("Failed to persist notification setting: {error}"))
                })?;
        }
        return Ok(Json(SetConfigResponse {
            key: format!("notifications.{}", setting.key()),
            value: update.display(),
            message: if persist {
                "Config persisted. Call /v1/config/reload to apply."
            } else {
                "Config not persisted (add persist: true to save)"
            }
            .to_string(),
            persisted: persist,
            requires_reload: persist,
        }));
    }

    // Validate model keys even for dry-run requests. Model ids are provider
    // owned; accepting a DeepSeek id while Z.ai is active creates a saved
    // route that cannot execute after reload.
    let active_route = {
        let config = state.config.read();
        match key.as_str() {
            "model" | "base_url" | "provider_url" | "provider_base_url" => Some(
                config
                    .active_provider_identity()
                    .map_err(ApiError::bad_request)?,
            ),
            "default_model" => {
                let active = config.active_provider_identity().ok();
                Some(
                    match active.filter(|identity| {
                        identity.key.as_str()
                            == codewhale_config::descriptors::LEGACY_DEEPSEEK_CN.id
                    }) {
                        Some(identity) => identity,
                        None => config
                            .builtin_provider_identity(ProviderKind::Deepseek)
                            .map_err(ApiError::bad_request)?,
                    },
                )
            }
            _ => None,
        }
    };
    if matches!(key.as_str(), "model" | "default_model") {
        let config = state.config.read();
        let identity = active_route
            .as_ref()
            .ok_or_else(|| ApiError::bad_request("No admitted model route"))?;
        value = normalize_runtime_config_model(&config, identity, &value)?;
    }

    // All persisted config keys require a reload to take effect in the
    // runtime (including syncing to active engines). The caller should
    // POST /v1/config/reload after persisting.
    let requires_reload = persist;

    // Handle persistence directly via config_persistence.
    // The runtime's in-memory state is NOT mutated here; the caller
    // should POST /v1/config/reload after persisting to apply changes.
    if persist {
        let config_path = state.config_path.as_deref();
        let result: anyhow::Result<PathBuf> = match key.as_str() {
            "model" | "default_model" => config_persistence::persist_provider_model_key(
                config_path,
                active_route
                    .as_ref()
                    .ok_or_else(|| ApiError::bad_request("No admitted model route"))?,
                &value,
            ),
            "reasoning_effort" => {
                config_persistence::persist_root_string_key(config_path, "reasoning_effort", &value)
            }
            "approval_mode" | "approval_policy" => {
                config_persistence::persist_root_string_key(config_path, "approval_policy", &value)
            }
            "base_url" | "provider_url" | "provider_base_url" => {
                config_persistence::persist_route_base_url(
                    config_path,
                    active_route
                        .as_ref()
                        .ok_or_else(|| ApiError::bad_request("No admitted endpoint route"))?,
                    &value,
                )
            }
            "provider" => {
                // Validate the provider id against the static registry so the
                // GUI gets a clear error instead of silently persisting an
                // unknown value that `Config::api_provider()` would later
                // ignore (falling back to DeepSeek). A user-defined
                // `[providers.<name>]` route is a valid selection as well: the
                // persistent `provider` key holds exactly that name, and
                // `Config::resolve_provider_identity` resolves it back to the
                // route, so refusing it here would refuse a value the runtime
                // honours. Anything else is still refused.
                let identity = state
                    .config
                    .read()
                    .resolve_provider_selection_identity(&value)
                    .map_err(ApiError::bad_request)?;
                let result =
                    config_persistence::persist_provider_selection(config_path, &identity, None)
                        .map(|(path, _undo)| path);
                if result.is_ok() {
                    state.config.write().provider = Some(
                        identity
                            .persisted_id()
                            .unwrap_or(identity.key.as_str())
                            .to_string(),
                    );
                }
                result
            }
            "cost_currency"
            | "default_mode"
            | "auto_compact"
            | "show_thinking"
            | "thinking_default_expanded"
            | "thinking_highlight"
            | "show_tool_details"
            | "inline_diffs"
            | "calm_mode"
            | "workspace_follow_symlinks"
            | "locale"
            | "max_history" => {
                persist_runtime_tui_setting(&key, &value)?;
                return Ok(Json(SetConfigResponse {
                    key,
                    value,
                    message: "Config persisted. Call /v1/config/reload to apply.".to_string(),
                    persisted: true,
                    requires_reload,
                }));
            }
            "allow_shell" => {
                let enabled = value.parse::<bool>().map_err(|_| {
                    ApiError::bad_request(format!(
                        "Invalid value '{value}' for allow_shell: expected 'true' or 'false'"
                    ))
                })?;
                config_persistence::persist_root_bool_key(config_path, "allow_shell", enabled)
            }
            "mcp_config_path" => {
                config_persistence::persist_root_string_key(config_path, "mcp_config_path", &value)
            }
            "subagents_enabled" => {
                let enabled = value.parse::<bool>().map_err(|_| {
                    ApiError::bad_request(format!(
                        "Invalid value '{value}' for subagents_enabled: expected 'true' or 'false'"
                    ))
                })?;
                config_persistence::persist_subagents_bool_key(config_path, "enabled", enabled)
            }
            "subagents_max_depth" => {
                let raw = value.parse::<u64>().map_err(|_| {
                    ApiError::bad_request(format!(
                        "Invalid value '{value}' for subagents_max_depth: expected a non-negative integer"
                    ))
                })?;
                let clamped = raw.min(u64::from(codewhale_config::MAX_SPAWN_DEPTH_CEILING));
                config_persistence::persist_subagents_integer_key(config_path, "max_depth", clamped)
            }
            "sandbox_mode" => {
                let normalized = match value.to_lowercase().as_str() {
                    "none" | "off" | "disabled" => "none".to_string(),
                    "opensandbox" | "external-sandbox" | "external" => "opensandbox".to_string(),
                    "workspace-write" | "workspace_write" => "workspace-write".to_string(),
                    "read-only" | "read_only" => "read-only".to_string(),
                    "danger-full-access" | "danger_full_access" | "full" => {
                        "danger-full-access".to_string()
                    }
                    "workspace" | "workspace-read-write" | "workspace_read_write" => {
                        "workspace-write".to_string()
                    }
                    _ => {
                        return Err(ApiError::bad_request(format!(
                            "Invalid sandbox_mode '{value}'. Supported: none, read-only, workspace-write, danger-full-access, opensandbox"
                        )));
                    }
                };
                config_persistence::persist_root_string_key(
                    config_path,
                    "sandbox_mode",
                    &normalized,
                )
            }
            "strict_tool_mode" => {
                let enabled = value.parse::<bool>().map_err(|_| {
                    ApiError::bad_request(format!(
                        "Invalid value '{value}' for strict_tool_mode: expected 'true' or 'false'"
                    ))
                })?;
                config_persistence::persist_root_bool_key(config_path, "strict_tool_mode", enabled)
            }
            "memory_enabled" => {
                let enabled = value.parse::<bool>().map_err(|_| {
                    ApiError::bad_request(format!(
                        "Invalid value '{value}' for memory_enabled: expected 'true' or 'false'"
                    ))
                })?;
                config_persistence::persist_table_bool_key(
                    config_path,
                    "memory",
                    "enabled",
                    enabled,
                )
            }
            "search_provider" => {
                let normalized = value.to_lowercase();
                // GET returns the *resolved* provider. A settings save that
                // round-trips that value must not turn autodetect (or the
                // Firecrawl default) into a disk pin — `provider = "firecrawl"`
                // would flip the source to `config` and permanently block a
                // later Tavily key. A POST that differs from the resolved
                // provider is an explicit change and still persists.
                let resolution = state.config.read().search_provider_resolution();
                let posted = crate::config::SearchProvider::parse(&normalized);
                if posted == Some(resolution.provider)
                    && matches!(
                        resolution.source,
                        crate::config::SearchProviderSource::Default
                            | crate::config::SearchProviderSource::TavilyKey
                    )
                {
                    return Ok(Json(SetConfigResponse {
                        key,
                        value,
                        message: format!(
                            "Config not persisted: '{}' is the resolved {} (source: {}), not a pin. Set a different provider, or pin it in config.toml.",
                            normalized,
                            resolution.provider.as_str(),
                            resolution.source.as_str()
                        ),
                        persisted: false,
                        requires_reload: true,
                    }));
                }
                config_persistence::persist_table_string_key(
                    config_path,
                    "search",
                    "provider",
                    &normalized,
                )
            }
            "prompt_suggestion" => {
                let enabled = value.parse::<bool>().map_err(|_| {
                    ApiError::bad_request(format!(
                        "Invalid value '{value}' for prompt_suggestion: expected 'true' or 'false'"
                    ))
                })?;
                config_persistence::persist_root_bool_key(config_path, "prompt_suggestion", enabled)
            }
            _ => {
                // Every other declared settings.toml key persists through the
                // shared validator rather than a curated list — the schema
                // route advertises them, so a known setting must not die
                // here. Unknown keys still 400 through `Settings::set`.
                persist_runtime_tui_setting(&key, &value)?;
                return Ok(Json(SetConfigResponse {
                    key,
                    value,
                    message: "Config persisted. Call /v1/config/reload to apply.".to_string(),
                    persisted: true,
                    requires_reload,
                }));
            }
        };

        if let Err(e) = result {
            return Err(ApiError::internal(format!(
                "Failed to persist config key '{key}': {e}"
            )));
        }
    }

    Ok(Json(SetConfigResponse {
        key,
        value,
        message: if persist {
            "Config persisted. Call /v1/config/reload to apply.".to_string()
        } else {
            "Config not persisted (add persist: true to save)".to_string()
        },
        persisted: persist,
        requires_reload,
    }))
}

/// `GET /v1/settings/schema` — the Engine-declared settings surface.
///
/// `codewhale_config::SETTINGS_SCHEMA` is the single declaration table: one
/// entry per setting with kind, closed value set, default, and placement.
/// This route projects it for HTTP clients — current values resolved from
/// the owning store (settings.toml via [`crate::settings::Settings`],
/// config.toml, or the notifications table), labels and descriptions
/// resolved through the locale pack. Writes stay on `POST /v1/config`;
/// this route never invents a value, an option, or a validator.
#[derive(Debug, Serialize)]
struct SettingsSchemaResponse {
    /// Payload version. Additive fields may appear without a bump; clients
    /// must ignore fields and `kind`/`row` values they do not know.
    version: u32,
    tabs: Vec<SettingsSchemaTab>,
    settings: Vec<SettingsSchemaRow>,
}

#[derive(Debug, Serialize)]
struct SettingsSchemaTab {
    id: String,
    /// Humanized tab id — tab labels have no message keys in the schema.
    label: String,
}

#[derive(Debug, Serialize)]
struct SettingsSchemaRow {
    key: &'static str,
    /// `bool` | `int` | `enum` | `string`. Unknown kinds degrade to a text
    /// field on the client; writes still validate server-side.
    kind: &'static str,
    tab: &'static str,
    group: &'static str,
    label: String,
    description: String,
    default: &'static str,
    /// `setting` | `action` | `diagnostic` | `session` — from
    /// [`codewhale_config::SettingRowKind`].
    row: &'static str,
    /// Current value in written-to-disk string form, when a store resolves
    /// it. Absent for actions, unresolvable diagnostics, and session rows
    /// the headless runtime cannot read.
    #[serde(skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    /// Whether the value is a persisted user choice rather than an
    /// inherited default. Absent where no store can prove either way.
    #[serde(skip_serializing_if = "Option::is_none")]
    persisted: Option<bool>,
    /// Whether a generic client may offer a write control. Action,
    /// diagnostic and session rows are never editable through this surface;
    /// conditional rows (managed policy wins) report false.
    editable: bool,
    /// False for the hidden member of a conditional pair — e.g. a
    /// `managed_*` row when no managed policy applies, or `base_url` when
    /// the active route reads `provider_url`. Clients should not render
    /// invisible rows.
    visible: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    options: Vec<SettingsSchemaOption>,
}

#[derive(Debug, Serialize)]
struct SettingsSchemaOption {
    value: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    label: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    description: String,
}

/// "Turn a schema key or tab id into a title-case label" — the same
/// humanization the TUI applies to rows declared without a label message.
fn humanize_schema_key(key: &str) -> String {
    key.split(['.', '_', '-'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            let mut word = first.to_uppercase().collect::<String>();
            word.push_str(chars.as_str());
            word
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// config.toml-owned keys `POST /v1/config` persists through curated arms.
/// Kept beside `set_config`'s match: a schema Setting row outside this list
/// and outside `Settings` has no write path and reports `editable: false`.
const RUNTIME_CONFIG_KEYS: &[&str] = &[
    "model",
    "default_model",
    "reasoning_effort",
    "approval_mode",
    "approval_policy",
    "base_url",
    "provider",
    "provider_url",
    "provider_base_url",
    "cost_currency",
    "max_history",
    "allow_shell",
    "mcp_config_path",
    "subagents_enabled",
    "subagents_max_depth",
    "sandbox_mode",
    "strict_tool_mode",
    "memory_enabled",
    "search_provider",
    "prompt_suggestion",
];

/// A dotted-path lookup over a TOML document — used to decide `persisted`
/// for config.toml-owned rows without trusting a decorated display string.
fn toml_value_at_path<'a>(document: &'a toml::Value, segments: &[&str]) -> Option<&'a toml::Value> {
    let mut current = document;
    for segment in segments {
        current = current.as_table()?.get(*segment)?;
    }
    Some(current)
}

async fn get_settings_schema(
    State(state): State<RuntimeApiState>,
) -> Result<Json<SettingsSchemaResponse>, ApiError> {
    use codewhale_config::notifications::NotificationSetting;
    use codewhale_config::settings_schema::{
        SettingKind, SettingRowKind, schema_rows, schema_tabs,
    };
    use codewhale_localization::{MessageId, resolve_locale, tr, tr_key};

    let config = state.config.read().clone();
    let settings = crate::settings::Settings::load_persisted().unwrap_or_default();
    let locale = resolve_locale(&settings.locale);
    let notifications = config.notifications_config();

    // Conditional pairs share the TUI's rule: exactly one member is shown,
    // chosen by which store or policy owns the fact right now.
    let permission_control = config.approval_policy_control(
        state.config_path.as_deref(),
        state.config_profile.as_deref(),
        &state.workspace,
    );
    let shell_control = config.allow_shell_control(
        state.config_path.as_deref(),
        state.config_profile.as_deref(),
        &state.workspace,
    );
    let base_url_row_key = if config
        .active_provider_identity()
        .ok()
        .is_some_and(|identity| identity.key.as_str() == ProviderKind::Deepseek.as_str())
    {
        "base_url"
    } else {
        "provider_url"
    };
    let visible = |key: &str| -> bool {
        match key {
            "permission_posture" => matches!(
                permission_control,
                crate::config::ApprovalPolicyControl::Unset
            ),
            "approval_policy" => matches!(
                permission_control,
                crate::config::ApprovalPolicyControl::RootConfig
            ),
            "managed_approval_policy" => !matches!(
                permission_control,
                crate::config::ApprovalPolicyControl::Unset
                    | crate::config::ApprovalPolicyControl::RootConfig
            ),
            "allow_shell" => shell_control.editable_root(),
            "managed_allow_shell" => !shell_control.editable_root(),
            "base_url" | "provider_url" => key == base_url_row_key,
            _ => true,
        }
    };

    // Raw config.toml for `persisted` on config-owned rows. A missing or
    // unparsable file means nothing was persisted there — the live config
    // still serves defaults through `value`. This is an async axum route, so
    // the read rides the blocking pool instead of parking a Tokio worker
    // (#6149).
    let config_document = match state.config_path.as_deref() {
        Some(path) => tokio::fs::read_to_string(path)
            .await
            .ok()
            .and_then(|body| toml::from_str::<toml::Value>(&body).ok()),
        None => None,
    };
    let notifications_persisted = |key: &str| -> Option<bool> {
        let setting = NotificationSetting::parse(key)?;
        let document = config_document.as_ref()?;
        Some(
            toml_value_at_path(document, &setting.segments()).is_some()
                // Legacy location the loader still honors.
                || (matches!(setting, NotificationSetting::Condition)
                    && toml_value_at_path(document, &["tui", "notification_condition"]).is_some()),
        )
    };

    let tabs = schema_tabs()
        .into_iter()
        .map(|id| SettingsSchemaTab {
            id: id.to_string(),
            label: humanize_schema_key(id),
        })
        .collect();

    let settings_rows = schema_rows()
        .map(|def| {
            let ui = def.ui.as_ref().expect("schema_rows filters on ui");
            let kind = match def.kind {
                SettingKind::Bool(_) => "bool",
                SettingKind::Int => "int",
                SettingKind::Float => "float",
                SettingKind::Enum(_) => "enum",
                SettingKind::String => "string",
            };
            let row = match ui.row {
                SettingRowKind::Setting => "setting",
                SettingRowKind::Action => "action",
                SettingRowKind::Diagnostic => "diagnostic",
                SettingRowKind::Session => "session",
            };
            let options = match def.kind {
                SettingKind::Bool(options) | SettingKind::Enum(options) => options
                    .iter()
                    .map(|option| SettingsSchemaOption {
                        value: option.value,
                        label: if option.label.is_empty() {
                            String::new()
                        } else {
                            tr_key(locale, option.label).into_owned()
                        },
                        description: if option.description.is_empty() {
                            String::new()
                        } else {
                            tr_key(locale, option.description).into_owned()
                        },
                    })
                    .collect(),
                SettingKind::Int | SettingKind::String | SettingKind::Float => Vec::new(),
            };
            // Bool rows with an empty option slice carry the surface's
            // default on/off labels — emit the bare values so clients can
            // still build a labeled control.
            let options = if options.is_empty() && matches!(def.kind, SettingKind::Bool(_)) {
                vec![
                    SettingsSchemaOption {
                        value: "false",
                        label: tr_key(locale, "ConfigValueOff").into_owned(),
                        description: String::new(),
                    },
                    SettingsSchemaOption {
                        value: "true",
                        label: tr_key(locale, "ConfigValueOn").into_owned(),
                        description: String::new(),
                    },
                ]
            } else {
                options
            };

            let notification_owned = NotificationSetting::parse(def.key).is_some();
            // `Settings::set` is the authority on which keys settings.toml
            // owns — including `Option` fields whose unset value serializes
            // to nothing (e.g. permission_posture). The probe reuses the
            // write validator on the declared default, so `editable` cannot
            // claim a key the real write path would reject.
            let settings_writable = crate::settings::Settings::default()
                .set(def.key, def.default)
                .is_ok();
            let (value, persisted) = if notification_owned {
                let setting = NotificationSetting::parse(def.key).expect("checked above");
                (
                    Some(notifications.display(setting)),
                    notifications_persisted(def.key),
                )
            } else if settings_writable {
                // Effective = the persisted value or the declared default;
                // `is_set` says which.
                (
                    Some(
                        settings
                            .value(def.key)
                            .unwrap_or_else(|| def.default.to_string()),
                    ),
                    Some(settings.is_set(def.key)),
                )
            } else if let Some(feature_key) = def.key.strip_prefix("features.") {
                // Feature rows are diagnostics: the effective flag state plus
                // whether config.toml names the leaf — no decorated phrasing,
                // the client owns presentation of default-vs-configured.
                let value = crate::features::FEATURES
                    .iter()
                    .find(|spec| spec.key == feature_key)
                    .map(|spec| config.features().enabled(spec.id).to_string());
                let persisted = config_document.as_ref().map(|document| {
                    toml_value_at_path(document, &["features", feature_key]).is_some()
                });
                (value, persisted)
            } else {
                // Managed-policy receipts name the winning source rather than
                // a writable value; everything else resolves from config.toml
                // or stays absent for a diagnostic the runtime cannot read.
                let managed_value = match def.key {
                    "managed_approval_policy" => match permission_control {
                        crate::config::ApprovalPolicyControl::Unset
                        | crate::config::ApprovalPolicyControl::RootConfig => None,
                        source => Some(source.label().to_string()),
                    },
                    "managed_allow_shell" if !shell_control.editable_root() => Some(format!(
                        "{} · {}",
                        config.allow_shell(),
                        shell_control.label()
                    )),
                    _ => None,
                };
                (
                    managed_value.or_else(|| config_schema_value(def.key, &config)),
                    None,
                )
            };

            // `editable` means POST /v1/config accepts the key today:
            // notifications.* through the namespace branch, settings.toml
            // keys through the Settings::set fallthrough, and the curated
            // config.toml arm list. A Setting row without a write path
            // (e.g. telemetry, which persists through its own notice
            // module) renders read-only rather than promising a 400. The
            // endpoint rows stay receipts: writing a live route's base URL
            // cannot mutate an already-running client, so the TUI marks
            // them read-only and the schema agrees.
            let endpoint_receipt = matches!(def.key, "base_url" | "provider_url");
            // A managed or profile-owned approval policy freezes the
            // session-level mode switch too, not just the saved row.
            let session_locked = def.key == "approval_mode"
                && !matches!(
                    permission_control,
                    crate::config::ApprovalPolicyControl::Unset
                );
            let editable = !endpoint_receipt
                && !session_locked
                && matches!(ui.row, SettingRowKind::Setting | SettingRowKind::Session)
                && visible(def.key)
                && (notification_owned
                    || settings_writable
                    || RUNTIME_CONFIG_KEYS.contains(&def.key));

            SettingsSchemaRow {
                key: def.key,
                kind,
                tab: ui.tab,
                group: ui.group,
                label: if !ui.label.is_empty() {
                    tr_key(locale, ui.label).into_owned()
                } else if def.key.starts_with("features.") {
                    tr(locale, MessageId::ConfigLabelFeaturePrefix).replace(
                        "{name}",
                        &humanize_schema_key(def.key.rsplit('.').next().unwrap_or(def.key)),
                    )
                } else {
                    humanize_schema_key(def.key.rsplit('.').next().unwrap_or(def.key))
                },
                description: if ui.description.is_empty() {
                    String::new()
                } else {
                    tr_key(locale, ui.description).into_owned()
                },
                default: def.default,
                row,
                value,
                persisted,
                editable,
                visible: visible(def.key),
                options,
            }
        })
        .collect();

    Ok(Json(SettingsSchemaResponse {
        version: 1,
        tabs,
        settings: settings_rows,
    }))
}

/// Current value of a config.toml-owned schema row, when one resolves
/// cheaply. Diagnostics that need per-route or credential computation are
/// omitted rather than approximated.
fn config_schema_value(key: &str, config: &Config) -> Option<String> {
    match key {
        "provider" => config
            .active_provider_identity()
            .ok()
            .map(|identity| identity.key.to_string()),
        "model" => runtime_request_model(config, None).ok(),
        "approval_policy" => config
            .approval_policy
            .clone()
            .or_else(|| Some("suggest".to_string())),
        "telemetry" => Some(crate::telemetry_notice::saved_preference_enabled(config).to_string()),
        "allow_shell" => Some(config.allow_shell().to_string()),
        "base_url" => config
            .active_provider_identity()
            .ok()
            .map(|identity| config.base_url_for_route(&identity)),
        "provider_url" => config
            .active_provider_identity()
            .ok()
            .map(|identity| config.base_url_for_route(&identity)),
        "mcp_config_path" => Some(config.mcp_config_path().display().to_string()),
        "sandbox_mode" => config.sandbox_mode.clone(),
        "fleet.exec.max_spawn_depth" => Some(config.subagent_max_spawn_depth().to_string()),
        "reasoning_effort" => Some(config.reasoning_effort().unwrap_or("auto").to_string()),
        _ => None,
    }
}

fn normalize_runtime_config_model(
    config: &Config,
    identity: &crate::config::ProviderIdentity,
    value: &str,
) -> Result<String, ApiError> {
    config
        .verify_provider_identity(identity)
        .map_err(ApiError::bad_request)?;
    let provider = identity.provider;
    let value = value.trim();
    if crate::provider_lake::configured_model_for_route(
        config,
        provider,
        identity.key.as_str(),
        &config.base_url_for_route(identity),
        value,
    )
    .is_some()
    {
        // The shared resolver preserves exact declarations only after its
        // protocol and provider allowlist guards. Metadata cannot bypass them.
        return crate::route_runtime::resolve_runtime_route_for_identity(
            config,
            identity,
            Some(value),
        )
        .map(|route| route.model)
        .map_err(ApiError::bad_request);
    }
    validate_route(provider, value).map_err(ApiError::bad_request)?;
    if value.eq_ignore_ascii_case("auto") {
        return Ok("auto".to_string());
    }
    normalize_model_name_for_provider(provider, value).ok_or_else(|| {
        ApiError::bad_request(format!(
            "Invalid model '{value}' for provider '{}'.",
            provider.as_str()
        ))
    })
}

async fn reload_config(
    State(state): State<RuntimeApiState>,
) -> Result<Json<ReloadConfigResponse>, ApiError> {
    let mut reloaded = Config::load(state.config_path.clone(), state.config_profile.as_deref())
        .map_err(|e| ApiError::internal(format!("Failed to reload config: {e}")))?;
    reloaded.account_model_access = state.config.read().account_model_access.clone();
    state
        .runtime_threads
        .reload_config(reloaded.clone())
        .await
        .map_err(|err| ApiError::bad_request(format!("Config reload rejected: {err}")))?;
    {
        let mut config = state.config.write();
        *config = reloaded;
    }
    Ok(Json(ReloadConfigResponse {
        message: "Config reloaded from disk; new turns will resolve the updated provider routes"
            .to_string(),
    }))
}

// ── Memory inspection and lifecycle endpoints ──

/// Maximum summary length returned per entry. Bounds the API surface so raw
/// private text cannot exfiltrate through JSON responses.
const MEMORY_SUMMARY_MAX_CHARS: usize = 300;
/// Default result cap for `GET /v1/memory`.
const MEMORY_LIST_DEFAULT_LIMIT: usize = 50;
/// Hard ceiling — protects against oversized responses.
const MEMORY_LIST_MAX_LIMIT: usize = 200;

/// Typed, redacted projection of a single native memory entry.
///
/// Raw file-system paths are never exposed; `scope` and `workspace_id` (a
/// SHA-256 digest of the repository origin URL, not a local path) give
/// managed clients enough provenance to reason about each entry.
#[derive(Debug, Serialize)]
struct MemoryEntryRecord {
    /// SQLite row id. Stable across reindexes unless the source Markdown
    /// file is cleared and rewritten.
    id: i64,
    /// `"global"` or `"workspace"`.
    scope: &'static str,
    /// SHA-256 digest of the repository origin URL for workspace-scoped
    /// entries; `null` for global entries.
    workspace_id: Option<String>,
    /// Bounded plain-text summary (max `MEMORY_SUMMARY_MAX_CHARS` chars).
    /// Truncated with `…` when the source text is longer. Never contains
    /// raw prompt or turn content.
    summary: String,
    /// `true` when the source Markdown file has been modified since the
    /// entry was last indexed.
    stale: bool,
    /// 1-based start line in the source Markdown file.
    line_start: usize,
    /// 1-based end line in the source Markdown file.
    line_end: usize,
    /// `"active"` or `"stale"` (human-readable alias for `stale`).
    status: &'static str,
}

#[derive(Debug, Deserialize)]
struct ListMemoryQuery {
    /// Filter by scope: `"global"`, `"workspace"`, or `"all"` (default).
    scope: Option<String>,
    /// FTS search query (max 256 chars). When absent all entries for the
    /// requested scope are returned in insertion order.
    q: Option<String>,
    /// Maximum entries to return (default 50, max 200).
    limit: Option<usize>,
}

/// Request body for `POST /v1/memory`.
#[derive(Debug, Deserialize)]
struct CreateMemoryRequest {
    /// The memory note text (max 64 KiB after normalisation).
    text: String,
    /// `"global"` (default) or `"workspace"`.
    #[serde(default)]
    scope: String,
}

/// Query params for `DELETE /v1/memory`.
#[derive(Debug, Deserialize)]
struct ClearMemoryQuery {
    /// One of `"global"`, `"workspace"`, or `"all"`. Required.
    scope: String,
}

/// Build a `NativeMemoryStore` rooted at the same location the TUI uses.
/// Mirrors `native_store()` in `commands/groups/memory/memory.rs`.
fn native_store_for_state(state: &RuntimeApiState) -> crate::native_memory::NativeMemoryStore {
    let memory_path = state.config.read().memory_path();
    crate::native_memory::NativeMemoryStore::from_memory_anchor(&memory_path)
}

/// Derive a scope label from a source path relative to the store root.
/// Returns `"global"`, `"workspace"`, or `"unknown"`.
fn scope_label_for_source(source: &FsPath, store_root: &FsPath) -> &'static str {
    let Ok(rel) = source.strip_prefix(store_root) else {
        return "unknown";
    };
    match rel.components().next().and_then(|c| c.as_os_str().to_str()) {
        Some("global") => "global",
        Some("workspace") => "workspace",
        _ => "unknown",
    }
}

/// Extract the workspace_id component from a workspace-scoped source path.
fn workspace_id_for_source(source: &FsPath, store_root: &FsPath) -> Option<String> {
    let rel = source.strip_prefix(store_root).ok()?;
    let mut comps = rel.components();
    if comps.next()?.as_os_str().to_str()? != "workspace" {
        return None;
    }
    Some(comps.next()?.as_os_str().to_str()?.to_string())
}

/// Convert a `MemoryHit` into a redacted, bounded `MemoryEntryRecord`.
fn memory_hit_to_record(
    hit: crate::native_memory::MemoryHit,
    store_root: &FsPath,
) -> MemoryEntryRecord {
    let scope = scope_label_for_source(&hit.source, store_root);
    let workspace_id = workspace_id_for_source(&hit.source, store_root);
    let summary = truncate_text(&hit.text, MEMORY_SUMMARY_MAX_CHARS);
    let status = if hit.stale { "stale" } else { "active" };
    MemoryEntryRecord {
        id: hit.id,
        scope,
        workspace_id,
        summary,
        stale: hit.stale,
        line_start: hit.line_start,
        line_end: hit.line_end,
        status,
    }
}

/// Resolve a scope query parameter into a `MemoryScope` filter and an
/// optional workspace_id. `"all"` / absent → `(None, None)`: each caller
/// decides what "all" spans (see `list_memory` and `clear_memory`).
fn resolve_memory_scope(
    scope_param: &Option<String>,
    workspace: &FsPath,
) -> Result<(Option<crate::native_memory::MemoryScope>, Option<String>), ApiError> {
    match scope_param.as_deref().unwrap_or("all").trim() {
        "all" | "" => Ok((None, None)),
        "global" => Ok((Some(crate::native_memory::MemoryScope::Global), None)),
        "workspace" => {
            let workspace_id = crate::native_memory::NativeMemoryStore::workspace_id(workspace)
                .map_err(|e| ApiError::internal(format!("resolve workspace id: {e}")))?
                .ok_or_else(|| {
                    ApiError::bad_request(
                        "workspace scope requires a git repository with a remote origin",
                    )
                })?;
            Ok((
                Some(crate::native_memory::MemoryScope::Workspace),
                Some(workspace_id),
            ))
        }
        other => Err(ApiError::bad_request(format!(
            "Invalid scope '{other}': expected one of all, global, workspace"
        ))),
    }
}

/// `GET /v1/memory` — list memory entries with optional scope and FTS
/// filtering.
///
/// Query params:
/// - `scope` — `"global"`, `"workspace"`, or `"all"` (default: global memory
///   plus this repository's workspace memory)
/// - `q` — FTS search query (max 256 chars; omit to list all)
/// - `limit` — max results (default 50, max 200)
async fn list_memory(
    State(state): State<RuntimeApiState>,
    Query(query): Query<ListMemoryQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = match query.limit.unwrap_or(MEMORY_LIST_DEFAULT_LIMIT) {
        0 => {
            return Err(ApiError::bad_request("limit must be at least 1"));
        }
        n if n > MEMORY_LIST_MAX_LIMIT => {
            return Err(ApiError::bad_request(format!(
                "limit must be at most {MEMORY_LIST_MAX_LIMIT}; got {n}"
            )));
        }
        n => n,
    };

    let store = native_store_for_state(&state);
    let root = store.root().to_path_buf();
    let (scope_filter, mut workspace_id) = resolve_memory_scope(&query.scope, &state.workspace)?;
    if scope_filter.is_none() {
        // "all" is global memory plus this repository's. With no identity
        // (no origin remote, or git unavailable) there is no workspace memory
        // to show, and the listing still serves global memory.
        workspace_id = crate::native_memory::NativeMemoryStore::workspace_id(&state.workspace)
            .unwrap_or_else(|error| {
                tracing::warn!("memory list shows global memory only: {error}");
                None
            });
    }

    let hits = if let Some(ref q) = query.q {
        let q = q.trim();
        if q.is_empty() || q.chars().count() > 256 {
            return Err(ApiError::bad_request("q must be 1–256 characters"));
        }
        match scope_filter {
            None if workspace_id.is_some() => {
                store.search_in_workspace(workspace_id.as_deref(), &state.workspace, q, limit)
            }
            None => store.search(q, limit),
            Some(crate::native_memory::MemoryScope::Global) => store.search(q, limit).map(|h| {
                h.into_iter()
                    .filter(|h| scope_label_for_source(&h.source, &root) == "global")
                    .collect()
            }),
            Some(crate::native_memory::MemoryScope::Workspace) => store
                .search_for_workspace(&state.workspace, q, limit)
                .map(|h| {
                    h.into_iter()
                        .filter(|h| scope_label_for_source(&h.source, &root) == "workspace")
                        .collect()
                }),
        }
    } else {
        store.list_all(scope_filter, workspace_id.as_deref(), limit)
    }
    .map_err(|e| ApiError::internal(format!("memory list error: {e}")))?;

    let entries: Vec<MemoryEntryRecord> = hits
        .into_iter()
        .map(|h| memory_hit_to_record(h, &root))
        .collect();
    let total = entries.len();
    Ok(Json(json!({ "entries": entries, "total": total })))
}

/// `GET /v1/memory/{id}` — inspect a single memory entry.
///
/// The lookup is scoped to global memory plus the current repository's
/// workspace memory; numeric IDs from a different machine or repository
/// will not resolve.
async fn get_memory_entry(
    State(state): State<RuntimeApiState>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let store = native_store_for_state(&state);
    let root = store.root().to_path_buf();
    let hit = store
        .get_for_workspace(&state.workspace, id)
        .map_err(|e| ApiError::internal(format!("memory lookup error: {e}")))?
        .ok_or_else(|| ApiError::not_found(format!("memory entry '{id}' not found")))?;
    let entry = memory_hit_to_record(hit, &root);
    Ok(Json(json!({ "entry": entry })))
}

/// `POST /v1/memory` — append a new memory entry.
///
/// The note is treated as user data (lower authority than instructions).
/// Requires the standard Runtime auth token when auth is configured.
async fn create_memory_entry(
    State(state): State<RuntimeApiState>,
    Json(req): Json<CreateMemoryRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let scope_str = if req.scope.is_empty() {
        "global"
    } else {
        req.scope.as_str()
    };
    let scope = match scope_str.trim() {
        "global" => crate::native_memory::MemoryScope::Global,
        "workspace" => crate::native_memory::MemoryScope::Workspace,
        other => {
            return Err(ApiError::bad_request(format!(
                "Invalid scope '{other}': expected 'global' or 'workspace'"
            )));
        }
    };
    let workspace_id = if scope == crate::native_memory::MemoryScope::Workspace {
        let id = crate::native_memory::NativeMemoryStore::workspace_id(&state.workspace)
            .map_err(|e| ApiError::internal(format!("resolve workspace id: {e}")))?
            .ok_or_else(|| {
                ApiError::bad_request(
                    "workspace scope requires a git repository with a remote origin",
                )
            })?;
        Some(id)
    } else {
        None
    };
    let store = native_store_for_state(&state);
    let root = store.root().to_path_buf();
    // This endpoint is an authenticated operator surface: the explicit request
    // is the review, so the entry lands active — matching the Lens remember
    // action. Model-reachable capture stays candidate-only.
    let hit = store
        .remember_reviewed(scope, workspace_id.as_deref(), &req.text)
        .map_err(|e| ApiError::bad_request(format!("memory create error: {e}")))?;
    let entry = memory_hit_to_record(hit, &root);
    Ok((StatusCode::CREATED, Json(json!({ "entry": entry }))))
}

/// `DELETE /v1/memory` — clear all memory entries for the given scope.
///
/// The `scope` query parameter is required: `"global"`, `"workspace"`, or
/// `"all"`. `"all"` clears every local scope, including other repositories'
/// workspace memory, which is wider than what `GET` lists for `"all"`. This
/// is a destructive, non-reversible operation.
async fn clear_memory(
    State(state): State<RuntimeApiState>,
    Query(query): Query<ClearMemoryQuery>,
) -> Result<Json<Value>, ApiError> {
    let (scope_filter, workspace_id) = resolve_memory_scope(&Some(query.scope), &state.workspace)?;
    let store = native_store_for_state(&state);
    store
        .delete_all(scope_filter, workspace_id.as_deref())
        .map_err(|e| ApiError::internal(format!("memory clear error: {e}")))?;
    Ok(Json(json!({ "cleared": true })))
}

const MOBILE_HTML: &str = include_str!("runtime_mobile.html");

// Only stream statuses are localized here; the rest of the mobile shell is
// still English. Reload the page after changing the Runtime's UI locale.
fn mobile_html(locale: codewhale_localization::Locale) -> String {
    use codewhale_localization::{MessageId, tr};

    let messages = json!({
        "replay_failed": tr(locale, MessageId::MobileStreamReplayFailed),
        "catch_up_failed": tr(locale, MessageId::MobileStreamCatchUpFailed),
        "runtime_shutdown": tr(locale, MessageId::MobileStreamRuntimeShutdown),
        "ended": tr(locale, MessageId::MobileStreamEnded),
        "closed": tr(locale, MessageId::MobileStreamClosed),
        "reconnecting": tr(locale, MessageId::MobileStreamReconnecting),
        "connected": tr(locale, MessageId::MobileStreamConnected),
    });
    // JSON quoting protects JS strings; escaping '<' also prevents a catalog
    // value from closing the enclosing script element.
    MOBILE_HTML.replace(
        "__CODEWHALE_STREAM_MESSAGES__",
        &messages.to_string().replace('<', "\\u003c"),
    )
}

/// Built-in dev origins always allowed by the runtime API (whalescale#255).
const DEFAULT_CORS_ORIGINS: &[&str] = &[
    "http://localhost:3000",
    "http://127.0.0.1:3000",
    "http://localhost:1420",
    "http://127.0.0.1:1420",
    "tauri://localhost",
];

fn cors_layer(extra_origins: &[String]) -> CorsLayer {
    let mut origins: Vec<HeaderValue> = DEFAULT_CORS_ORIGINS
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    for raw in extra_origins {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        match HeaderValue::from_str(trimmed) {
            Ok(value) if !origins.contains(&value) => origins.push(value),
            Ok(_) => {}
            Err(err) => tracing::warn!(
                "Ignoring invalid CORS origin '{trimmed}': {err}; expected scheme://host[:port]"
            ),
        }
    }
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
            Method::OPTIONS,
        ])
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            header::IF_MATCH,
            HeaderName::from_static("x-codewhale-runtime-token"),
            HeaderName::from_static("x-deepseek-runtime-token"),
        ])
        .expose_headers([
            HeaderName::from_static("x-codewhale-stream-end"),
            HeaderName::from_static("x-codewhale-event-progress"),
        ])
}

fn map_task_err(err: anyhow::Error) -> ApiError {
    let message = err.to_string();
    if message.contains("not found") {
        ApiError::not_found(message)
    } else {
        ApiError::bad_request(message)
    }
}

fn map_automation_err(err: anyhow::Error) -> ApiError {
    let message = err.to_string();
    if message.contains("Failed to read automation")
        || message.contains("No such file or directory")
    {
        ApiError::not_found(message)
    } else {
        ApiError::bad_request(message)
    }
}

fn map_thread_err(err: anyhow::Error) -> ApiError {
    let message = err.to_string();
    let lower = message.to_ascii_lowercase();
    if (lower.starts_with("thread '") && lower.ends_with("' not found"))
        || lower.starts_with("thread not found:")
    {
        ApiError::not_found(message)
    } else if message.starts_with("shell commands are restricted by ") {
        ApiError::forbidden(message)
    } else if message.contains("already has an active turn")
        || message.contains("thread permissions changed during update")
        || message.contains("No active turn")
        || message.contains("is not active")
        // A steer the engine dropped: the turn moved on before the model saw
        // it. 409 lets a client keep the text and resend rather than trust a
        // delivery that never happened (#6276).
        || message.contains("moved on before the steer")
        || lower.contains("operation_key is already bound")
        || lower.contains("operation_key binding is incomplete")
        || lower.contains("operation_key binding does not match")
    {
        ApiError::conflict(message)
    } else {
        ApiError::bad_request(message)
    }
}

fn map_agent_mail_err(err: anyhow::Error) -> ApiError {
    let message = err.to_string();
    let lower = message.to_ascii_lowercase();
    if lower.contains("ownership denied") {
        ApiError::forbidden(message)
    } else if lower.contains("already exists with different delivery intent")
        || lower.contains("can be canceled only while queued")
    {
        ApiError::conflict(message)
    } else if (lower.contains("failed to read agent mail envelope")
        && (lower.contains("no such file")
            || err.chain().skip(1).any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
            })))
        || (lower.starts_with("thread '") && lower.ends_with("' not found"))
    {
        ApiError::not_found(message)
    } else {
        ApiError::bad_request(message)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ApiError {
    status: StatusCode,
    pub(crate) message: String,
    /// Stable machine-readable reason, serialized as `error.code` when set,
    /// for refusals a client must branch on rather than show.
    code: Option<&'static str>,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            code: None,
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            code: None,
        }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
            code: None,
        }
    }

    fn not_implemented(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            message: message.into(),
            code: None,
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            code: None,
        }
    }

    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
            code: None,
        }
    }

    fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: message.into(),
            code: None,
        }
    }

    fn gone(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GONE,
            message: message.into(),
            code: None,
        }
    }

    fn unprocessable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
            code: None,
        }
    }

    fn with_code(mut self, code: &'static str) -> Self {
        self.code = Some(code);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(match self.code {
                Some(code) => json!({
                    "error": {
                        "message": self.message,
                        "status": self.status.as_u16(),
                        "code": code,
                    }
                }),
                None => json!({
                    "error": {
                        "message": self.message,
                        "status": self.status.as_u16(),
                    }
                }),
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod configured_model_api_tests {
    use super::*;
    use crate::test_support::{EnvVarGuard, lock_test_env};

    fn fixture(provider: &str, model: &str) -> String {
        format!(
            r#"provider = "{provider}"
default_text_model = "deepseek-v4-pro"
telemetry = false

[[custom_models]]
provider = "{provider}"
base_url = "http://127.0.0.1:9/v1"
id = "{model}"
limit = {{ context = 96000, input = 88000, output = 8000 }}
cost = {{ input = 0.4, output = 1.6 }}
reasoning = false
tool_call = false

[providers.{provider}]
base_url = "http://127.0.0.1:9/v1"
"#
        )
    }

    fn isolate_model_environment() -> Vec<EnvVarGuard> {
        let mut guards: Vec<_> = [
            "CODEWHALE_CONFIG_PATH",
            "DEEPSEEK_CONFIG_PATH",
            "CODEWHALE_BASE_URL",
            "DEEPSEEK_BASE_URL",
            "CODEWHALE_PROVIDER",
            "DEEPSEEK_PROVIDER",
            "CODEWHALE_MODEL",
            "DEEPSEEK_MODEL",
            "DEEPSEEK_DEFAULT_TEXT_MODEL",
            "OPENROUTER_BASE_URL",
            "OPENROUTER_MODEL",
            "TOGETHER_BASE_URL",
            "TOGETHER_MODEL",
            "CODEWHALE_PROFILE",
            "DEEPSEEK_PROFILE",
            "OLLAMA_MODEL",
            "OLLAMA_CLOUD_MODEL",
            "OLLAMA_BASE_URL",
            "OLLAMA_CLOUD_BASE_URL",
        ]
        .into_iter()
        .map(EnvVarGuard::remove)
        .collect();
        guards.push(EnvVarGuard::set("CODEWHALE_DISABLE_CLOUD_FACTS", "1"));
        guards
    }

    async fn serve_fixture(
        config_path: PathBuf,
    ) -> Result<(SocketAddr, RuntimeApiState, tokio::task::JoinHandle<()>)> {
        let root = config_path.parent().expect("fixture root");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace)?;
        let config = Config::load(Some(config_path.clone()), None)?;
        let sessions_dir = root.join("sessions");
        let mut manager_config =
            RuntimeThreadManagerConfig::from_task_data_dir(root.join("runtime"));
        manager_config.sessions_dir = Some(sessions_dir.clone());
        let runtime_threads = Arc::new(RuntimeThreadManager::open_with_plugin_registry(
            config.clone(),
            workspace.clone(),
            manager_config,
            Arc::new(crate::plugins::PluginRegistry::empty(&workspace)),
        )?);
        let sessions_dir = runtime_threads.sessions_dir().to_path_buf();
        let task_manager = TaskManager::start_with_runtime_manager(
            TaskManagerConfig {
                data_dir: root.join("tasks"),
                worker_count: 1,
                default_workspace: workspace.clone(),
                default_model: "auto".to_string(),
                default_mode: "agent".to_string(),
                allow_shell: false,
                trust_mode: false,
                execution_limits: Default::default(),
            },
            config.clone(),
            runtime_threads.clone(),
        )
        .await?;
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let sub_agent_manager = runtime_api_sub_agent_manager(&workspace, 2);
        let workspace_scopes =
            RuntimeWorkspaceScopes::new(runtime_threads.clone(), sub_agent_manager.clone());
        let workspace_scope = workspace_scopes.admit(workspace.clone()).await?;
        let state = RuntimeApiState {
            config: Arc::new(parking_lot::RwLock::new(config)),
            workspace: workspace.clone(),
            plugin_discovery: crate::plugins::PluginDiscoveryContext::capture_pre_dotenv(),
            task_manager,
            runtime_threads,
            cors_origins: Vec::new(),
            sessions_dir,
            config_path: Some(config_path.clone()),
            config_profile: None,
            automations: Arc::new(Mutex::new(AutomationManager::open_for_test(
                root.join("automations"),
            )?)),
            sub_agent_manager,
            runtime_token: None,
            skill_state: Arc::new(Mutex::new(SkillStateStore::load_from(
                root.join("skills_state.toml"),
            )?)),
            auth_required: false,
            bind_host: "127.0.0.1".to_string(),
            bind_port: addr.port(),
            mobile_enabled: false,
            mobile: None,
            web: None,
            fleet_codewhale_binary: "unused-test-binary".to_string(),
            workspace_scopes,
            workspace_scope,
            computer: computer_display::ComputerState::from_env(),
            shutdown: RuntimeServerShutdown::default(),
            git_writes: Arc::new(tokio::sync::Mutex::new(())),
            provider_switches: Arc::new(tokio::sync::Mutex::new(())),
            compat_stream_test_hook: None,
        };
        let router = build_router(state.clone());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("local fixture server");
        });
        Ok((addr, state, server))
    }

    async fn post_json(addr: SocketAddr, path: &str, body: Value) -> Result<Value> {
        let response = crate::tls::reqwest_client()
            .post(format!("http://{addr}{path}"))
            .json(&body)
            .send()
            .await?;
        let status = response.status();
        let body = response.json::<Value>().await?;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        Ok(body)
    }

    fn assert_declared_route(config_path: &FsPath, provider: ProviderKind, model: &str) {
        let config = Config::load(Some(config_path.to_path_buf()), None).expect("reloaded config");
        let persisted = config
            .provider_config_for(&config.test_identity_for_kind(provider))
            .and_then(|entry| entry.model.as_deref());
        assert_eq!(persisted, Some(model));
        let selected =
            provider_default_model_for_api(&config, &(config).test_identity_for_kind(provider));
        assert_eq!(selected, model);
        let route = crate::route_runtime::resolve_runtime_route(&config, provider, Some(&selected))
            .expect("saved declared route");
        assert_eq!(route.model, model);
        assert!(route.candidate.canonical_model().is_none());
        assert_eq!(route.candidate.limits().context_tokens, Some(96_000));
        assert_eq!(
            route.context_window.source,
            crate::route_runtime::ContextWindowSource::UserDeclared
        );
    }

    fn write_remembered_selection_fixture(home: &FsPath, config_path: &FsPath) -> Result<()> {
        fs::create_dir_all(home)?;
        fs::create_dir_all(config_path.parent().expect("config parent"))?;
        fs::write(
            home.join("settings.toml"),
            "default_provider = \"zai\"\n[provider_models]\nzai = \"GLM-5.3\"\n",
        )?;
        fs::write(
            config_path,
            r#"provider = "deepseek"
default_text_model = "deepseek-v4-pro"
telemetry = false

[cloud_facts]
enabled = false

[providers.zai]
base_url = "https://api.z.ai/api/coding/paas/v4"
model = "GLM-5.2"
"#,
        )?;
        Ok(())
    }

    async fn assert_catalog_and_new_thread_selection(
        addr: SocketAddr,
        provider: &str,
        model: &str,
    ) -> Result<Value> {
        let client = crate::tls::reqwest_client();
        let catalog = client
            .get(format!("http://{addr}/v1/providers"))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        assert_eq!(catalog["current"], provider);
        let entry = catalog["providers"]
            .as_array()
            .expect("provider catalog")
            .iter()
            .find(|entry| entry["id"] == provider)
            .expect("selected provider");
        assert_eq!(entry["default_model"], model);
        let config = client
            .get(format!("http://{addr}/v1/config"))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        assert_eq!(config["model"], model);
        if provider == "deepseek" {
            assert_eq!(config["default_model"], model);
        }
        // Creation only saves the route; this fixture never starts a turn or
        // contacts any provider, including the official catalog URLs above.
        let response = client
            .post(format!("http://{addr}/v1/threads"))
            .json(&json!({}))
            .send()
            .await?;
        let status = response.status();
        let thread = response.json::<Value>().await?;
        assert_eq!(status, StatusCode::CREATED, "{thread}");
        assert_eq!(thread["model_provider"], provider);
        assert_eq!(thread["model"], model);
        assert_eq!(thread["model_provider_id"], entry["model_provider_id"]);
        Ok(thread)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remembered_selection_aligns_catalog_and_new_thread_after_load() -> Result<()> {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let root = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _model_environment = isolate_model_environment();
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let config_path = root.path().join("config.toml");
        write_remembered_selection_fixture(root.path(), &config_path)?;
        let original_config = fs::read(&config_path)?;
        let original_settings = fs::read(root.path().join("settings.toml"))?;
        let (addr, state, server) = serve_fixture(config_path.clone()).await?;
        let _shutdown = state.task_manager.shutdown_guard();
        assert_catalog_and_new_thread_selection(addr, "zai", "GLM-5.3").await?;
        post_json(addr, "/v1/config/reload", json!({})).await?;
        assert_catalog_and_new_thread_selection(addr, "zai", "GLM-5.3").await?;
        assert_eq!(fs::read(&config_path)?, original_config);
        assert_eq!(
            fs::read(root.path().join("settings.toml"))?,
            original_settings
        );
        server.abort();
        state.task_manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn explicit_runtime_selections_migrate_legacy_memory_into_config_once() -> Result<()> {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let root = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _model_environment = isolate_model_environment();
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let config_path = root.path().join("config.toml");
        write_remembered_selection_fixture(root.path(), &config_path)?;
        let (addr, state, server) = serve_fixture(config_path.clone()).await?;
        let _shutdown = state.task_manager.shutdown_guard();
        let original_thread =
            assert_catalog_and_new_thread_selection(addr, "zai", "GLM-5.3").await?;
        let original_settings = fs::read(root.path().join("settings.toml"))?;
        post_json(
            addr,
            "/v1/config",
            json!({ "key": "model", "value": "GLM-5.2", "persist": false }),
        )
        .await?;
        assert_eq!(
            fs::read(root.path().join("settings.toml"))?,
            original_settings
        );
        post_json(
            addr,
            "/v1/config",
            json!({ "key": "model", "value": "GLM-5.2", "persist": true }),
        )
        .await?;
        assert_eq!(
            runtime_request_model(&state.config.read(), None).expect("current default"),
            "GLM-5.3"
        );
        let migrated: toml::Value = toml::from_str(&fs::read_to_string(&config_path)?)?;
        assert_eq!(migrated["route_preferences_version"].as_integer(), Some(1));
        assert_eq!(migrated["provider"].as_str(), Some("zai"));
        assert_eq!(
            migrated["providers"]["zai"]["model"].as_str(),
            Some("GLM-5.2")
        );
        post_json(addr, "/v1/config/reload", json!({})).await?;
        assert_catalog_and_new_thread_selection(addr, "zai", "GLM-5.2").await?;
        let saved_thread = state
            .runtime_threads
            .get_thread(original_thread["id"].as_str().expect("thread id"))
            .await?;
        assert_eq!(saved_thread.model, "GLM-5.3");

        post_json(
            addr,
            "/v1/providers/deepseek/switch",
            json!({ "model": "deepseek-v4-flash" }),
        )
        .await?;
        assert_catalog_and_new_thread_selection(addr, "deepseek", "deepseek-v4-flash").await?;
        post_json(
            addr,
            "/v1/config",
            json!({ "key": "default_model", "value": "deepseek-v4-pro", "persist": true }),
        )
        .await?;
        post_json(addr, "/v1/config/reload", json!({})).await?;
        assert_catalog_and_new_thread_selection(addr, "deepseek", "deepseek-v4-pro").await?;

        for (key, value) in [("provider", "zai"), ("model", "GLM-5.3")] {
            post_json(
                addr,
                "/v1/config",
                json!({ "key": key, "value": value, "persist": true }),
            )
            .await?;
        }
        post_json(addr, "/v1/config/reload", json!({})).await?;
        assert_catalog_and_new_thread_selection(addr, "zai", "GLM-5.3").await?;
        post_json(addr, "/v1/providers/deepseek/switch", json!({})).await?;
        post_json(addr, "/v1/config/reload", json!({})).await?;
        assert_catalog_and_new_thread_selection(addr, "deepseek", "deepseek-v4-pro").await?;
        // The old Settings selection remains unchanged and cannot reassert
        // itself once Config owns the migrated route preferences.
        assert_eq!(
            fs::read(root.path().join("settings.toml"))?,
            original_settings
        );
        server.abort();
        state.task_manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_switch_migrates_legacy_selection_before_explicit_choice() -> Result<()> {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let root = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _model_environment = isolate_model_environment();
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let config_path = root.path().join("config.toml");
        write_remembered_selection_fixture(root.path(), &config_path)?;
        let original_settings = fs::read(root.path().join("settings.toml"))?;
        let (addr, state, server) = serve_fixture(config_path.clone()).await?;
        let _shutdown = state.task_manager.shutdown_guard();
        post_json(
            addr,
            "/v1/providers/deepseek/switch",
            json!({ "model": "deepseek-v4-flash" }),
        )
        .await?;
        let migrated: toml::Value = toml::from_str(&fs::read_to_string(&config_path)?)?;
        assert_eq!(migrated["route_preferences_version"].as_integer(), Some(1));
        assert_eq!(migrated["provider"].as_str(), Some("deepseek"));
        assert_eq!(
            migrated["providers"]["deepseek"]["model"].as_str(),
            Some("deepseek-v4-flash")
        );
        assert_eq!(
            migrated["providers"]["zai"]["model"].as_str(),
            Some("GLM-5.3")
        );
        post_json(addr, "/v1/config/reload", json!({})).await?;
        assert_catalog_and_new_thread_selection(addr, "deepseek", "deepseek-v4-flash").await?;
        assert_eq!(
            fs::read(root.path().join("settings.toml"))?,
            original_settings
        );
        server.abort();
        state.task_manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_model_writes_keep_legacy_hosted_ollama_identity() -> Result<()> {
        #[derive(Deserialize)]
        struct Selection {
            provider: String,
            model: String,
            persisted: bool,
        }

        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _model_environment = isolate_model_environment();
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let config_path = home.path().join("config.toml");
        fs::write(
            &config_path,
            "provider = 'ollama'\ntelemetry = false\n[providers.ollama]\nbase_url = 'https://ollama.com/v1'\nmodel = 'old-cloud-model'\n[providers.ollama_cloud]\nmodel = 'explicit-cloud-model'\n",
        )?;
        let settings = "default_provider = 'ollama'\n[provider_models]\nollama-cloud = 'remembered-cloud-model'\n";
        fs::write(home.path().join("settings.toml"), settings)?;
        let (addr, state, server) = serve_fixture(config_path.clone()).await?;
        let _shutdown = state.task_manager.shutdown_guard();
        assert_eq!(
            state.config.read().default_model(),
            "remembered-cloud-model"
        );
        post_json(
            addr,
            "/v1/config",
            json!({"key": "model", "value": "current-cloud-model", "persist": true}),
        )
        .await?;
        post_json(addr, "/v1/config/reload", json!({})).await?;
        assert_eq!(state.config.read().default_model(), "current-cloud-model");
        for (selector, model) in [
            ("ollama", "legacy-choice"),
            ("ollama-cloud", "explicit-choice"),
            ("ollama", "legacy-final"),
        ] {
            let selection: Selection = serde_json::from_value(
                post_json(
                    addr,
                    &format!("/v1/providers/{selector}/switch"),
                    json!({"model": model}),
                )
                .await?,
            )?;
            assert_eq!(selection.provider, "ollama-cloud");
            assert_eq!(selection.model, model);
            assert!(selection.persisted);
            post_json(addr, "/v1/config/reload", json!({})).await?;
            let config = state.config.read();
            let identity = config
                .active_provider_identity()
                .map_err(anyhow::Error::msg)?;
            assert_eq!(identity.persisted_id(), Some(selector));
            assert_eq!(config.default_model(), model);
        }
        let document: toml::Value = toml::from_str(&fs::read_to_string(&config_path)?)?;
        assert_eq!(document["route_preferences_version"].as_integer(), Some(1));
        assert_eq!(document["provider"].as_str(), Some("ollama"));
        assert_eq!(
            document["providers"]["ollama"]["model"].as_str(),
            Some("legacy-final")
        );
        assert_eq!(
            document["providers"]["ollama_cloud"]["model"].as_str(),
            Some("explicit-choice")
        );
        assert_eq!(
            fs::read_to_string(home.path().join("settings.toml"))?,
            settings
        );
        let thread =
            assert_catalog_and_new_thread_selection(addr, "ollama-cloud", "legacy-final").await?;
        assert_eq!(thread["model_provider_id"], "ollama");
        server.abort();
        state.task_manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn scoped_runtime_selections_leave_device_memory_unchanged() -> Result<()> {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", &home);
        let _model_environment = isolate_model_environment();
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let config_path = root.path().join("project/config.toml");
        write_remembered_selection_fixture(&home, &config_path)?;
        let original_settings = fs::read(home.join("settings.toml"))?;
        let (addr, state, server) = serve_fixture(config_path).await?;
        let _shutdown = state.task_manager.shutdown_guard();
        assert_catalog_and_new_thread_selection(addr, "deepseek", "deepseek-v4-pro").await?;
        for (key, value) in [
            ("provider", "zai"),
            ("model", "GLM-5.1"),
            ("provider", "deepseek"),
            ("default_model", "deepseek-v4-flash"),
        ] {
            post_json(
                addr,
                "/v1/config",
                json!({ "key": key, "value": value, "persist": true }),
            )
            .await?;
        }
        post_json(
            addr,
            "/v1/providers/zai/switch",
            json!({ "model": "GLM-5.2" }),
        )
        .await?;
        post_json(addr, "/v1/config/reload", json!({})).await?;
        assert_catalog_and_new_thread_selection(addr, "zai", "GLM-5.2").await?;
        assert_eq!(fs::read(home.join("settings.toml"))?, original_settings);
        server.abort();
        state.task_manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn declared_model_posts_preserve_exact_identity_after_reload() -> Result<()> {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _model_environment = isolate_model_environment();
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        for (provider, model) in [
            (ProviderKind::Deepseek, "deepseek-v4pro"),
            (ProviderKind::Openrouter, "deepseek-v4-pro"),
            (ProviderKind::Together, "deepseek-v4-pro"),
        ] {
            let root = tempfile::tempdir()?;
            let config_path = root.path().join("config.toml");
            fs::write(&config_path, fixture(provider.as_str(), model))?;
            let (addr, state, server) = serve_fixture(config_path.clone()).await?;
            let _shutdown = state.task_manager.shutdown_guard();
            let body = post_json(
                addr,
                &format!("/v1/providers/{}/switch", provider.as_str()),
                json!({ "model": model }),
            )
            .await?;
            assert_eq!(body["model"], model);
            assert_declared_route(&config_path, provider, model);
            let keys = if provider == ProviderKind::Deepseek {
                vec!["model", "default_model"]
            } else {
                vec!["model"]
            };
            for key in keys {
                let body = post_json(
                    addr,
                    "/v1/config",
                    json!({ "key": key, "value": model, "persist": true }),
                )
                .await?;
                assert_eq!(body["value"], model);
                post_json(addr, "/v1/config/reload", json!({})).await?;
                assert_declared_route(&config_path, provider, model);
                let reloaded = state.config.read();
                let selected = provider_default_model_for_api(
                    &reloaded,
                    &(reloaded).test_identity_for_kind(provider),
                );
                let route = crate::route_runtime::resolve_runtime_route(
                    &reloaded,
                    provider,
                    Some(&selected),
                )
                .expect("active reloaded route");
                assert_eq!(route.model, model);
                assert_eq!(route.candidate.limits().context_tokens, Some(96_000));
            }
            server.abort();
            state.task_manager.shutdown_and_wait().await?;
        }
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn declared_model_posts_do_not_preserve_alias_at_wrong_endpoint() -> Result<()> {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let root = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path().join("home"));
        let _model_environment = isolate_model_environment();
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let config_path = root.path().join("config.toml");
        let fixture = fixture("deepseek", "deepseek-v4pro").replace(
            "[providers.deepseek]\nbase_url = \"http://127.0.0.1:9/v1\"",
            "[providers.deepseek]\nbase_url = \"http://127.0.0.1:10/v1\"",
        );
        fs::write(&config_path, fixture)?;
        let (addr, state, server) = serve_fixture(config_path.clone()).await?;
        let _shutdown = state.task_manager.shutdown_guard();
        let body = post_json(
            addr,
            "/v1/providers/deepseek/switch",
            json!({ "model": "deepseek-v4pro" }),
        )
        .await?;
        assert_eq!(body["model"], "deepseek-v4-pro");
        let body = post_json(
            addr,
            "/v1/config",
            json!({ "key": "model", "value": "deepseek-v4pro", "persist": true }),
        )
        .await?;
        assert_eq!(body["value"], "deepseek-v4-pro");
        post_json(addr, "/v1/config/reload", json!({})).await?;
        let config = Config::load(Some(config_path), None)?;
        assert_eq!(
            config
                .provider_config_for(&config.test_identity_for_kind(ProviderKind::Deepseek))
                .and_then(|provider| provider.model.as_deref()),
            Some("deepseek-v4-pro")
        );
        let selected = provider_default_model_for_api(
            &config,
            &(config).test_identity_for_kind(ProviderKind::Deepseek),
        );
        let route = crate::route_runtime::resolve_runtime_route(
            &config,
            ProviderKind::Deepseek,
            Some(&selected),
        )
        .expect("ordinary saved route");
        assert_eq!(route.model, "deepseek-v4-pro");
        assert_ne!(route.candidate.limits().context_tokens, Some(96_000));
        assert_ne!(
            route.context_window.source,
            crate::route_runtime::ContextWindowSource::UserDeclared
        );
        server.abort();
        state.task_manager.shutdown_and_wait().await?;
        Ok(())
    }

    #[test]
    fn declared_model_normalization_keeps_identity_and_protocol_guards() {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let root = tempfile::tempdir().expect("test root");
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _model_environment = isolate_model_environment();
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let mut config: Config =
            toml::from_str(&fixture("deepseek", "deepseek-v4pro")).expect("fixture config");
        config.custom_models.as_mut().unwrap()[0].provider = "other".to_string();
        assert_eq!(
            normalize_runtime_config_model(
                &config,
                &(config).test_identity_for_kind(ProviderKind::Deepseek),
                "deepseek-v4pro"
            )
            .expect("legacy alias remains accepted"),
            "deepseek-v4-pro"
        );
        for provider in [ProviderKind::OpencodeGo, ProviderKind::OpencodeZen] {
            let config: Config = toml::from_str(&fixture(provider.as_str(), "unlisted-model"))
                .expect("fixture config");
            assert!(
                normalize_runtime_config_model(
                    &config,
                    &(config).test_identity_for_kind(provider),
                    "unlisted-model"
                )
                .is_err(),
                "a declaration cannot expand the {provider:?} protocol roster"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn runtime_default_model_reads_and_writes_the_deepseek_cn_slot() -> Result<()> {
        let _env = lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let root = tempfile::tempdir()?;
        let _home = EnvVarGuard::set("CODEWHALE_HOME", root.path());
        let _model_environment = isolate_model_environment();
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let config_path = root.path().join("config.toml");
        fs::write(
            &config_path,
            "provider = 'deepseek-cn'\ntelemetry = false\n[cloud_facts]\nenabled = false\n[providers.deepseek_cn]\nmodel = 'deepseek-v4-pro'\n",
        )?;
        let (addr, state, server) = serve_fixture(config_path.clone()).await?;
        let _shutdown = state.task_manager.shutdown_guard();
        let client = crate::tls::reqwest_client();
        // The active CN route resolves `[providers.deepseek_cn].model`; the
        // runtime default_model surface must report that slot, not the primary.
        let reported: Value = client
            .get(format!("http://{addr}/v1/config"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(reported["default_model"], "deepseek-v4-pro");

        post_json(
            addr,
            "/v1/config",
            json!({ "key": "default_model", "value": "deepseek-v4-flash", "persist": true }),
        )
        .await?;
        let saved: toml::Value = toml::from_str(&fs::read_to_string(&config_path)?)?;
        assert_eq!(
            saved["providers"]["deepseek_cn"]["model"].as_str(),
            Some("deepseek-v4-flash")
        );
        assert!(
            saved
                .get("providers")
                .and_then(|providers| providers.get("deepseek"))
                .and_then(|deepseek| deepseek.get("model"))
                .is_none(),
            "the CN write must not create an unread primary slot: {saved}"
        );

        post_json(addr, "/v1/config/reload", json!({})).await?;
        let reported: Value = client
            .get(format!("http://{addr}/v1/config"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(reported["default_model"], "deepseek-v4-flash");
        post_json(
            addr,
            "/v1/config",
            json!({ "key": "default_model", "value": "auto", "persist": true }),
        )
        .await?;
        post_json(addr, "/v1/config/reload", json!({})).await?;
        let reported: Value = client
            .get(format!("http://{addr}/v1/config"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        assert_eq!(reported["default_model"], "auto");
        server.abort();
        state.task_manager.shutdown_and_wait().await?;
        Ok(())
    }
}

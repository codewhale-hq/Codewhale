//! The pinned SDK owns MCP framing; Rust owns every process/network operation.
//! The selected Host backend immediately adopts proc/* and FetchProxy sessions.
use crate::mcp::McpServerConfigExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use codewhale_config::OwnerRef;
use serde_json::{Value, json};
use tokio::io::BufReader;
use tokio::sync::{Mutex as AsyncMutex, mpsc};
use tokio_util::sync::CancellationToken;

use super::protocol::*;
use super::registry::OwnerState;
use super::supervisor::{HostProcess, HostRequestContext};
use super::ticket::{Grant, Presented, Ticket, TicketKind};
use super::tier::HostTier;
use super::{ExtensionHostManager, ManagerShared};
use crate::core::engine::HumanDecision;
use crate::mcp::process_broker::BrokerSession;
use crate::mcp::{MAX_MCP_RESPONSE_BYTES, McpPool};
use codewhale_mcp::config::McpServerConfig;

const MAX_SESSIONS: usize = 64;
const MAX_PENDING: usize = 256;
const MAX_DEADLINE: Duration = Duration::from_secs(86_400);
const SUPPORTED_METHODS: &[&str] = &[
    "server/discover",
    "initialize",
    "notifications/initialized",
    "tools/list",
    "resources/list",
    "resources/templates/list",
    "prompts/list",
    "tools/call",
    "resources/read",
    "prompts/get",
    "skills/list",
    "skills/get",
];

#[derive(Clone)]
struct Operation {
    target: Value,
    ticket: Ticket,
    expires: Instant,
    decision: Option<HumanDecision>,
}
struct BoundRequest {
    operation_id: String,
    method: String,
}

/// No launch command, environment mapping, credential or decision key is wire data.
struct Session {
    session_id: String,
    owner: OwnerRef,
    host_generation: u64,
    name: String,
    config: McpServerConfig,
    auth_context: Arc<dyn codewhale_runtime::credentials::AuthContext>,
    cancel: CancellationToken,
    broker: AsyncMutex<Option<BrokerSession>>,
    frames: AsyncMutex<mpsc::Receiver<Value>>,
    frame_sender: mpsc::Sender<Value>,
    operations: Mutex<HashMap<String, Operation>>,
    pending: Mutex<HashMap<String, BoundRequest>>,
    replies: Mutex<HashMap<String, Value>>,
    server_requests: Mutex<HashMap<String, String>>,
    decision_key: Mutex<Option<[u8; 32]>>,
    protocol_version: Mutex<Option<String>>,
    legacy_allowed: std::sync::atomic::AtomicBool,
    list_changed: std::sync::atomic::AtomicBool,
    launched: Mutex<bool>,
    launch_ticket: Mutex<Option<Ticket>>,
    http: Option<http::HttpSession>,
}
fn modern_metadata() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": crate::mcp::MCP_MODERN_PROTOCOL_VERSION,
        "io.modelcontextprotocol/clientInfo": {"name": "codewhale-tui", "version": env!("CARGO_PKG_VERSION")},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}
impl Session {
    fn modern(&self) -> bool {
        self.protocol_version
            .lock()
            .expect("MCP protocol lock")
            .as_deref()
            == Some(crate::mcp::MCP_MODERN_PROTOCOL_VERSION)
    }
    fn validate(&self, shared: &ManagerShared, generation: u64, owner: &OwnerRef) -> Result<()> {
        if self.cancel.is_cancelled() || generation != self.host_generation || owner != &self.owner
        {
            bail!("MCP broker session is stale or closed");
        }
        if shared
            .builtin
            .host_generation
            .load(std::sync::atomic::Ordering::SeqCst)
            != generation
        {
            bail!("MCP broker host generation is stale");
        }
        shared
            .live_owner_authority(HostTier::Builtin, |registry| {
                registry
                    .owner(&owner.plugin_id)
                    .filter(|entry| entry.owner == *owner && entry.state == OwnerState::Active)
                    .map(|entry| entry.owner.clone())
                    .ok_or_else(|| "MCP builtin owner is no longer live".to_string())
            })
            .map_err(anyhow::Error::msg)?;
        if let Some(source) = self.config.reviewed_plugin_source() {
            source.validate_before_use(&self.name, "broker operation")?;
        }
        Ok(())
    }
    fn target(
        &self,
        operation_id: &str,
        method: &str,
        params: &Value,
        wire_id: Option<&str>,
    ) -> Value {
        json!({"session_id": self.id(), "operation_id": operation_id, "method": method, "params": params, "wire_id": wire_id})
    }
    fn observe(&self, frame: &Value) -> Result<()> {
        if frame.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            bail!("invalid MCP server frame");
        }
        if frame.get("id").is_none()
            && frame["method"]
                .as_str()
                .is_some_and(crate::mcp::is_mcp_list_changed_method)
        {
            self.list_changed
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }
        if let Some(id) = frame.get("id") {
            let id = wire_id(id)?;
            if frame.get("method").is_some() {
                let mut requests = self
                    .server_requests
                    .lock()
                    .expect("MCP server request lock");
                if requests.len() >= MAX_PENDING || requests.contains_key(&id) {
                    bail!("MCP server request bound exceeded");
                }
                requests.insert(
                    id,
                    frame["method"]
                        .as_str()
                        .context("invalid MCP method")?
                        .to_string(),
                );
            } else if let Some(binding) = self.pending.lock().expect("MCP pending lock").remove(&id)
            {
                if binding.method == "server/discover" {
                    let error = &frame["error"];
                    let legacy = if let Some(result) = frame.get("result") {
                        !result["supportedVersions"]
                            .as_array()
                            .is_some_and(|versions| {
                                versions.iter().any(|version| {
                                    version == crate::mcp::MCP_MODERN_PROTOCOL_VERSION
                                })
                            })
                    } else {
                        error["code"] != -32022
                            || (error["code"] == -32022
                                && error["data"]["supported"]
                                    .as_array()
                                    .is_some_and(|versions| {
                                        !versions.is_empty()
                                            && versions.iter().all(|version| {
                                                version.as_str().is_some_and(|version| {
                                            version != crate::mcp::MCP_MODERN_PROTOCOL_VERSION
                                                && crate::mcp::MCP_CLIENT_ACCEPTED_PROTOCOL_VERSIONS
                                                    .contains(&version)
                                        })
                                            })
                                    }))
                    };
                    if legacy {
                        self.legacy_allowed
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
                let mut replies = self.replies.lock().expect("MCP reply lock");
                if replies.len() >= MAX_PENDING || replies.contains_key(&binding.operation_id) {
                    bail!("MCP reply bound exceeded");
                }
                replies.insert(binding.operation_id, frame.clone());
            }
        }
        Ok(())
    }
    fn cancel(&self) {
        self.cancel.cancel();
        if let Some(http) = &self.http {
            http.close();
        }
    }
    fn id(&self) -> &str {
        &self.session_id
    }
}

#[derive(Default)]
pub(super) struct Broker {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}
impl Drop for Broker {
    fn drop(&mut self) {
        for session in self.sessions.get_mut().expect("MCP broker lock").values() {
            session.cancel();
        }
    }
}
impl Broker {
    pub(super) fn revoke_owner(&self, owner: &str) -> u64 {
        let mut sessions = self.sessions.lock().expect("MCP broker lock");
        let before = sessions.len();
        sessions.retain(|_, session| {
            if session.owner.plugin_id == owner {
                session.cancel();
                false
            } else {
                true
            }
        });
        (before - sessions.len()) as u64
    }
    pub(super) fn revoke_host(&self, tier: HostTier, generation: u64) -> u64 {
        if tier != HostTier::Builtin {
            return 0;
        }
        let mut sessions = self.sessions.lock().expect("MCP broker lock");
        let before = sessions.len();
        sessions.retain(|_, session| {
            if session.host_generation == generation {
                session.cancel();
                false
            } else {
                true
            }
        });
        (before - sessions.len()) as u64
    }
    fn session(&self, id: &str) -> Result<Arc<Session>> {
        self.sessions
            .lock()
            .expect("MCP broker lock")
            .get(id)
            .cloned()
            .context("MCP broker session is closed")
    }
    fn remove(&self, shared: &ManagerShared, id: &str) -> Option<Arc<Session>> {
        let session = self.sessions.lock().expect("MCP broker lock").remove(id)?;
        session.cancel();
        for operation in session
            .operations
            .lock()
            .expect("MCP operation lock")
            .drain()
            .map(|(_, v)| v)
        {
            shared.core_calls.tickets.revoke(&operation.ticket);
        }
        if let Some(ticket) = session
            .launch_ticket
            .lock()
            .expect("MCP launch ticket lock")
            .take()
        {
            shared.core_calls.tickets.revoke(&ticket);
        }
        shared
            .mcp_users
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        Some(session)
    }
    pub(super) async fn serve(
        &self,
        shared: &ManagerShared,
        generation: u64,
        request: HostRequest,
        cx: HostRequestContext,
    ) -> Result<Value, RpcErrorWire> {
        let launch = matches!(request, HostRequest::ProcLaunch(_));
        let run = async {
            match request {
                request @ (HostRequest::NetStart(_)
                | HostRequest::NetFetch(_)
                | HostRequest::NetRead(_)
                | HostRequest::NetRelease(_)
                | HostRequest::NetClose(_)) => {
                    http::serve(self, shared, generation, request, &cx).await
                }
                HostRequest::ProcLaunch(params) => {
                    self.launch(shared, generation, params, &cx).await
                }
                HostRequest::ProcRead(params) => {
                    let session = self.session(&params.session_id)?;
                    session.validate(shared, generation, &params.owner)?;
                    let mut receiver = session.frames.lock().await;
                    let frame = tokio::select! { biased;
                        _ = cx.cancel.cancelled() => bail!("MCP broker read cancelled"),
                        _ = session.cancel.cancelled() => bail!("MCP broker session closed"),
                        frame = receiver.recv() => frame,
                    };
                    session.validate(shared, generation, &params.owner)?;
                    Ok(frame
                        .map_or_else(|| json!({"closed": true}), |frame| json!({"frame": frame})))
                }
                HostRequest::ProcWrite(params) => self.write(shared, generation, params, &cx).await,
                HostRequest::ProcClose(params) => {
                    let session = self.session(&params.session_id)?;
                    // Closing needs exact ownership even after cancellation, but
                    // never grants a stale owner access to a replacement session.
                    if session.owner != params.owner || session.host_generation != generation {
                        bail!("MCP broker close has the wrong owner");
                    }
                    if let Some(session) = self.remove(shared, &params.session_id)
                        && let Some(mut broker) = session.broker.lock().await.take()
                    {
                        broker.shutdown().await;
                    }
                    Ok(json!({}))
                }
                _ => bail!("not an MCP broker request"),
            }
        };
        run.await.map_err(|error| RpcErrorWire {
            code: error_code::REFUSED,
            message: if launch {
                format!("MCP server launch failed: {error:#}")
            } else {
                "MCP broker request refused or closed".to_string()
            },
            data: None,
        })
    }
    async fn launch(
        &self,
        shared: &ManagerShared,
        generation: u64,
        params: ProcLaunchParams,
        cx: &HostRequestContext,
    ) -> Result<Value> {
        let session = self.session(&params.session_id)?;
        session.validate(shared, generation, &params.owner)?;
        let mut launch_guard = CancelGuard {
            cancel: session.cancel.clone(),
            armed: true,
        };
        if session.http.is_some() {
            bail!("HTTP session cannot launch a process");
        }
        let target = json!({"session_id": params.session_id});
        shared
            .core_calls
            .tickets
            .redeem(&Presented {
                ticket: &params.ticket,
                kind: TicketKind::McpLaunch,
                tier: HostTier::Builtin,
                host_generation: generation,
                owner: &params.owner,
                method: "proc/launch",
                target: Some(&target),
            })
            .map_err(|refused| {
                if refused.violation {
                    cx.violation("too many invalid MCP broker tickets".to_string());
                }
                anyhow::anyhow!("MCP launch ticket refused")
            })?;
        session
            .launch_ticket
            .lock()
            .expect("MCP launch ticket lock")
            .take();
        {
            let mut launched = session.launched.lock().expect("MCP launch lock");
            if *launched {
                bail!("MCP session already launched");
            }
            *launched = true;
        }
        if cx.cancel.is_cancelled() {
            session.cancel();
            bail!("MCP launch cancelled");
        }
        let command = session
            .config
            .command
            .as_deref()
            .context("MCP host stdio requires a command")?;
        let (mut broker, stdout) = BrokerSession::spawn_with_auth_context(
            &session.name,
            command,
            &session.config,
            session.cancel.clone(),
            Arc::clone(&session.auth_context),
        )?;
        session.validate(shared, generation, &params.owner)?;
        if session
            .config
            .reviewed_plugin_source()
            .is_some_and(|source| source.plugin_name() == crate::mcp::COMPUTER_USE_PLUGIN_NAME)
        {
            let key = crate::mcp::random_key()?;
            let keys = json!({"jsonrpc": "2.0", "method": crate::mcp::COMPUTER_USE_HOST_KEYS_METHOD, "params": {"decision_key": crate::mcp::hex_encode(&key), "ledger_key": crate::mcp::computer_use_ledger_key().await}});
            let mut bytes = serde_json::to_vec(&keys)?;
            bytes.push(b'\n');
            tokio::select! { biased;
                _ = cx.cancel.cancelled() => { session.cancel(); bail!("MCP launch cancelled"); },
                _ = session.cancel.cancelled() => bail!("MCP launch revoked"),
                result = broker.write(&bytes) => result?,
            }
            *session.decision_key.lock().expect("MCP decision key lock") = Some(key);
        }
        *session.broker.lock().await = Some(broker);
        let reading = Arc::clone(&session);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let run = async {
                loop {
                    let mut bytes = Vec::new();
                    let count = tokio::select! { biased;
                        _ = reading.cancel.cancelled() => break,
                        result = crate::mcp::read_line_capped(&mut reader, &mut bytes, MAX_MCP_RESPONSE_BYTES) => result?,
                    };
                    if count == 0 {
                        break;
                    }
                    if bytes.iter().all(u8::is_ascii_whitespace) {
                        continue;
                    }
                    let frame: Value = serde_json::from_slice(&bytes)?;
                    reading.observe(&frame)?;
                    tokio::select! { biased;
                        _ = reading.cancel.cancelled() => break,
                        result = reading.frame_sender.send(frame) => result.map_err(|_| anyhow::anyhow!("MCP frame consumer closed"))?,
                    }
                }
                Ok::<_, anyhow::Error>(())
            };
            let _ = run.await;
            reading.cancel();
            if let Some(mut broker) = reading.broker.lock().await.take() {
                broker.shutdown().await;
            }
        });
        session.validate(shared, generation, &params.owner)?;
        if cx.cancel.is_cancelled() {
            bail!("MCP launch cancelled");
        }
        launch_guard.armed = false;
        Ok(json!({}))
    }
    async fn write(
        &self,
        shared: &ManagerShared,
        generation: u64,
        params: ProcWriteParams,
        cx: &HostRequestContext,
    ) -> Result<Value> {
        let session = self.session(&params.session_id)?;
        session.validate(shared, generation, &params.owner)?;
        let mut write_guard = CancelGuard {
            cancel: session.cancel.clone(),
            armed: true,
        };
        if session.http.is_some() {
            bail!("HTTP session cannot write a pipe");
        }
        let owner = params.owner.clone();
        let (frame, operation_deadline) =
            self.authorize_frame(shared, generation, &session, params, cx, "proc/write")?;
        let mut bytes = serde_json::to_vec(&frame)?;
        bytes.push(b'\n');
        session.validate(shared, generation, &owner)?;
        let result = async {
            let mut slot = session.broker.lock().await;
            let broker = slot.as_mut().context("MCP process not launched")?;
            tokio::select! { biased;
                _ = cx.cancel.cancelled() => bail!("MCP write cancelled"),
                _ = session.cancel.cancelled() => bail!("MCP write revoked"),
                result = tokio::time::timeout_at(tokio::time::Instant::from_std(operation_deadline), broker.write(&bytes)) => result.context("MCP pipe write expired")??,
            }
            Ok::<_, anyhow::Error>(())
        }.await;
        if result.is_err() {
            session.cancel();
        }
        result?;
        session.validate(shared, generation, &owner)?;
        write_guard.armed = false;
        Ok(json!({}))
    }
    fn authorize_frame(
        &self,
        shared: &ManagerShared,
        generation: u64,
        session: &Arc<Session>,
        params: ProcWriteParams,
        cx: &HostRequestContext,
        ticket_method: &'static str,
    ) -> Result<(Value, Instant)> {
        let mut frame = params.frame;
        let object = frame.as_object().context("MCP frame must be an object")?;
        if object.keys().any(|key| {
            !["jsonrpc", "id", "method", "params", "error", "result"].contains(&key.as_str())
        }) || frame["jsonrpc"] != "2.0"
        {
            bail!("invalid MCP outbound frame");
        }
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        if method.is_some() && (frame.get("error").is_some() || frame.get("result").is_some()) {
            bail!("MCP request cannot contain an error");
        }
        let mut operation_deadline = Instant::now() + Duration::from_secs(2);
        match method.as_deref() {
            Some("notifications/cancelled") => {
                if params.ticket.is_some() || frame.get("id").is_some() {
                    bail!("invalid MCP cancellation");
                }
                let id = wire_id(&frame["params"]["requestId"])?;
                let pending = session.pending.lock().expect("MCP pending lock");
                let binding = pending
                    .get(&id)
                    .context("MCP cancellation has no active request")?;
                if params.operation_id.as_deref() != Some(binding.operation_id.as_str()) {
                    bail!("MCP cancellation target mismatch");
                }
                let fields = frame["params"]
                    .as_object()
                    .context("invalid MCP cancellation params")?;
                if fields
                    .keys()
                    .any(|key| !["requestId", "reason", "_meta"].contains(&key.as_str()))
                {
                    bail!("invalid MCP cancellation params");
                }
                if fields.get("_meta")
                    != expected_metadata(session, "notifications/cancelled").as_ref()
                {
                    bail!("MCP cancellation metadata refused");
                }
            }
            Some(method) => {
                if !SUPPORTED_METHODS.contains(&method) {
                    bail!("MCP method has no Rust grant");
                }
                let operation_id = params
                    .operation_id
                    .as_deref()
                    .context("MCP operation id absent")?;
                let body = frame.get("params").cloned().unwrap_or_else(|| json!({}));
                if !body.is_object() {
                    bail!("MCP named params required");
                }
                let admitted_id = frame
                    .get("id")
                    .map(|id| {
                        id.as_str()
                            .context("MCP wire request ID must be a Rust string")
                    })
                    .transpose()?;
                let target = session.target(operation_id, method, &body, admitted_id);
                let ticket = params
                    .ticket
                    .as_deref()
                    .context("MCP operation ticket absent")?;
                shared
                    .core_calls
                    .tickets
                    .redeem(&Presented {
                        ticket,
                        kind: TicketKind::McpOperation,
                        tier: HostTier::Builtin,
                        host_generation: generation,
                        owner: &params.owner,
                        method: ticket_method,
                        target: Some(&target),
                    })
                    .map_err(|refused| {
                        if refused.violation {
                            cx.violation("too many invalid MCP broker tickets".to_string());
                        }
                        anyhow::anyhow!("MCP operation ticket refused")
                    })?;
                let operation = session
                    .operations
                    .lock()
                    .expect("MCP operation lock")
                    .remove(operation_id)
                    .context("MCP operation absent")?;
                if operation.target != target || ticket != operation.ticket.expose() {
                    bail!("MCP operation does not match");
                }
                if body.get("_meta") != expected_metadata(session, method).as_ref() {
                    bail!("host-supplied MCP metadata refused");
                }
                if matches!(method, "initialize" | "notifications/initialized")
                    && (!session
                        .legacy_allowed
                        .load(std::sync::atomic::Ordering::SeqCst)
                        || session.modern())
                {
                    bail!("legacy MCP handshake has no typed refusal");
                }
                if operation.expires <= Instant::now() {
                    bail!("MCP operation expired");
                }
                operation_deadline = operation.expires;
                if method == "tools/call" {
                    let tool = body["name"].as_str().context("MCP tool name absent")?;
                    if !session.config.is_tool_enabled(tool) {
                        bail!("MCP tool disabled");
                    }
                    if let Some(decision) = operation.decision.as_ref() {
                        if !decision.authorizes(
                            &McpPool::mcp_model_tool_name(&session.name, tool),
                            &body["arguments"],
                        ) {
                            bail!("MCP decision does not match");
                        }
                        if let Some(key) = session
                            .decision_key
                            .lock()
                            .expect("MCP decision key lock")
                            .as_ref()
                        {
                            frame["params"]
                                .as_object_mut()
                                .context("MCP params absent")?
                                .entry("_meta")
                                .or_insert_with(|| json!({}));
                            frame["params"]["_meta"][crate::mcp::COMPUTER_USE_DECISION_META] =
                                crate::mcp::attest_decision(key, tool, &body["arguments"])?;
                        }
                    }
                }
                if method == "notifications/initialized" {
                    if frame.get("id").is_some() {
                        bail!("initialized must be a notification");
                    }
                } else {
                    let id = wire_id(frame.get("id").context("MCP request id absent")?)?;
                    let mut pending = session.pending.lock().expect("MCP pending lock");
                    if pending.len() >= MAX_PENDING || pending.contains_key(&id) {
                        bail!("MCP request id unavailable");
                    }
                    pending.insert(
                        id,
                        BoundRequest {
                            operation_id: operation_id.to_string(),
                            method: method.to_string(),
                        },
                    );
                }
            }
            None => {
                if params.ticket.is_some()
                    || params.operation_id.is_some()
                    || frame.get("params").is_some()
                {
                    bail!("invalid MCP server request answer");
                }
                let id = wire_id(frame.get("id").context("MCP response id absent")?)?;
                let mut requests = session
                    .server_requests
                    .lock()
                    .expect("MCP server request lock");
                let method = requests
                    .get(&id)
                    .context("MCP response has no observed server request")?;
                let refusal = frame.get("result").is_none()
                    && frame["error"]["code"] == -32601
                    && frame["error"]["message"].is_string();
                let ping = method == "ping"
                    && frame.get("error").is_none()
                    && frame
                        .get("result")
                        .and_then(Value::as_object)
                        .is_some_and(|result| result.is_empty());
                if !refusal && !ping {
                    bail!("MCP server request success requires Rust authority");
                }
                requests.remove(&id);
            }
        }
        if serde_json::to_vec(&frame)?.len() > MAX_MCP_RESPONSE_BYTES {
            bail!("MCP outbound frame exceeds bound");
        }
        Ok((frame, operation_deadline))
    }
}
fn expected_metadata(session: &Session, method: &str) -> Option<Value> {
    (method == "server/discover" || session.modern()).then(modern_metadata)
}
struct CancelGuard {
    cancel: CancellationToken,
    armed: bool,
}
impl Drop for CancelGuard {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
        }
    }
}
fn wire_id(id: &Value) -> Result<String> {
    if !(id.is_string() || id.as_i64().is_some() || id.as_u64().is_some()) {
        bail!("invalid MCP request id");
    }
    Ok(serde_json::to_string(id)?)
}

/// Semantic adapter: the existing Rust MCP connection still consumes raw
/// server replies for policy/catalog admission; it never writes MCP bytes.
pub struct SdkTransport {
    manager: Arc<ExtensionHostManager>,
    host: Arc<HostProcess>,
    session: Arc<Session>,
    session_id: String,
    replies: std::collections::VecDeque<Vec<u8>>,
    discovery_timeout: Duration,
}
impl SdkTransport {
    #[cfg(any(test, feature = "test-support"))]
    pub async fn connect(
        name: &str,
        config: &McpServerConfig,
        cancel: CancellationToken,
        timeout: Duration,
    ) -> Result<Self> {
        Self::connect_with_http(name, config, cancel, timeout, None).await
    }
    pub async fn connect_with_http(
        name: &str,
        config: &McpServerConfig,
        cancel: CancellationToken,
        timeout: Duration,
        client: Option<crate::mcp::http_client::McpHttpClient>,
    ) -> Result<Self> {
        Self::connect_with_http_and_auth_context(
            name,
            config,
            cancel,
            timeout,
            client,
            crate::mcp::process_mcp_auth_context(),
        )
        .await
    }

    pub async fn connect_with_http_and_auth_context(
        name: &str,
        config: &McpServerConfig,
        cancel: CancellationToken,
        timeout: Duration,
        client: Option<crate::mcp::http_client::McpHttpClient>,
        auth_context: Arc<dyn codewhale_runtime::credentials::AuthContext>,
    ) -> Result<Self> {
        if (config.url.is_some() != client.is_some())
            || (config.url.is_none() && config.command.is_none())
        {
            bail!(
                "Host MCP backend requires a Rust-prepared HTTP client or a stdio command; no Rust fallback is allowed"
            );
        }
        let manager = super::manager();
        manager
            .shared
            .mcp_users
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let ready = manager.ensure_mcp_builtin().await;
        let (host, owner, host_generation) = match ready {
            Ok(ready) => ready,
            Err(error) => {
                manager
                    .shared
                    .mcp_users
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                return Err(anyhow::Error::msg(error));
            }
        };
        let session_id = uuid::Uuid::new_v4().to_string();
        let (frame_sender, frames) = mpsc::channel(1);
        let session = Arc::new(Session {
            session_id: session_id.clone(),
            owner,
            host_generation,
            name: name.to_string(),
            config: config.clone(),
            auth_context,
            // The facade owns caller cancellation. Retiring this transport
            // must release its resources without cancelling that facade and
            // masking the concrete reply or failure it is about to receive.
            cancel: cancel.child_token(),
            broker: AsyncMutex::new(None),
            frames: AsyncMutex::new(frames),
            frame_sender,
            operations: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            replies: Mutex::new(HashMap::new()),
            server_requests: Mutex::new(HashMap::new()),
            decision_key: Mutex::new(None),
            protocol_version: Mutex::new(None),
            legacy_allowed: std::sync::atomic::AtomicBool::new(
                crate::mcp::is_legacy_sse_transport(config),
            ),
            list_changed: std::sync::atomic::AtomicBool::new(false),
            launched: Mutex::new(false),
            launch_ticket: Mutex::new(None),
            http: client.map(|client| {
                http::HttpSession::new(
                    client,
                    config.url.as_deref().expect("HTTP config URL"),
                    crate::mcp::is_legacy_sse_transport(config),
                )
            }),
        });
        if session.http.is_some() {
            // Handler cancellation can drop a body future outside an exchange.
            // The weak watch releases retained network bodies on every token
            // cancellation without extending the broker session's lifetime.
            let weak = Arc::downgrade(&session);
            let cancel = session.cancel.clone();
            tokio::spawn(async move {
                cancel.cancelled().await;
                if let Some(session) = weak.upgrade()
                    && let Some(http) = &session.http
                {
                    http.close();
                }
            });
        }
        {
            let mut sessions = manager
                .shared
                .mcp_broker
                .sessions
                .lock()
                .expect("MCP broker lock");
            if sessions.len() >= MAX_SESSIONS {
                manager
                    .shared
                    .mcp_users
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                bail!("MCP host session limit exceeded");
            }
            sessions.insert(session_id.clone(), Arc::clone(&session));
        }
        let transport = Self {
            manager,
            host,
            session,
            session_id,
            replies: Default::default(),
            discovery_timeout: timeout,
        };
        Ok(transport)
    }
    fn grant(
        &self,
        method: &str,
        mut params: Value,
        timeout: Duration,
        decision: Option<&HumanDecision>,
        wire_id: Option<&str>,
    ) -> Result<McpOperationGrant> {
        if timeout.is_zero()
            || timeout > MAX_DEADLINE
            || !params.is_object()
            || serde_json::to_vec(&params)?.len() > MAX_MCP_RESPONSE_BYTES
        {
            bail!("MCP grant exceeds bound");
        }
        self.session.validate(
            &self.manager.shared,
            self.session.host_generation,
            &self.session.owner,
        )?;
        if params.get("_meta").is_some() {
            bail!("MCP semantic metadata is Rust-owned");
        }
        if let Some(metadata) = expected_metadata(&self.session, method) {
            params["_meta"] = metadata;
        }
        let operation_id = uuid::Uuid::new_v4().to_string();
        let target = self.session.target(&operation_id, method, &params, wire_id);
        let mut operations = self.session.operations.lock().expect("MCP operation lock");
        if operations.len() >= MAX_PENDING {
            bail!("MCP grant bound exceeded");
        }
        let ticket = self.manager.shared.core_calls.tickets.mint(Grant {
            kind: TicketKind::McpOperation,
            tier: HostTier::Builtin,
            host_generation: self.session.host_generation,
            owner: self.session.owner.clone(),
            method: if self.session.http.is_some() {
                "net/fetch"
            } else {
                "proc/write"
            },
            target: target.clone(),
            ttl: timeout,
            uses: 1,
        });
        let wire_ticket = ticket.expose().to_string();
        operations.insert(
            operation_id.clone(),
            Operation {
                target,
                ticket,
                expires: Instant::now() + timeout,
                decision: decision.cloned(),
            },
        );
        Ok(McpOperationGrant {
            ticket: wire_ticket,
            operation_id,
            method: method.to_string(),
            wire_id: wire_id.map(str::to_owned),
            params,
        })
    }
    async fn exchange(
        &mut self,
        bytes: Vec<u8>,
        decision: Option<&HumanDecision>,
        timeout: Duration,
    ) -> Result<()> {
        let value: Value = serde_json::from_slice(&bytes)?;
        let method = value["method"]
            .as_str()
            .context("MCP semantic method absent")?;
        // The SDK owns the legacy handshake; modern connections use discovery.
        // Completing the facade handshake never writes another notification.
        if method == "notifications/initialized" {
            return Ok(());
        }
        if !SUPPORTED_METHODS.contains(&method) {
            bail!("MCP semantic operation unavailable");
        }
        let params = value.get("params").cloned().unwrap_or_else(|| json!({}));
        let wire_id = value
            .get("id")
            .map(|id| {
                id.as_str()
                    .context("Rust MCP facade request ID must be a string")
            })
            .transpose()?;
        let grant = self.grant(method, params, timeout, decision, wire_id)?;
        let operation_id = grant.operation_id.clone();
        let mut guard = ExchangeGuard {
            transport: self,
            armed: true,
        };
        let mut discovery_operation = None;
        let mut opening_operations = Vec::new();
        let request = if method == "initialize" {
            let discover = if crate::mcp::is_legacy_sse_transport(&guard.transport.session.config) {
                None
            } else {
                Some(guard.transport.grant(
                    "server/discover",
                    json!({}),
                    timeout,
                    None,
                    Some(&uuid::Uuid::new_v4().to_string()),
                )?)
            };
            discovery_operation = discover.as_ref().map(|grant| grant.operation_id.clone());
            let initialized = guard.transport.grant(
                "notifications/initialized",
                json!({}),
                timeout,
                None,
                None,
            )?;
            opening_operations
                .extend([grant.operation_id.clone(), initialized.operation_id.clone()]);
            opening_operations.extend(discovery_operation.iter().cloned());
            let target = json!({"session_id": guard.transport.session_id});
            let launch = guard
                .transport
                .manager
                .shared
                .core_calls
                .tickets
                .mint(Grant {
                    kind: TicketKind::McpLaunch,
                    tier: HostTier::Builtin,
                    host_generation: guard.transport.session.host_generation,
                    owner: guard.transport.session.owner.clone(),
                    method: if guard.transport.session.http.is_some() {
                        "net/start"
                    } else {
                        "proc/launch"
                    },
                    target,
                    ttl: timeout,
                    uses: 1,
                });
            *guard
                .transport
                .session
                .launch_ticket
                .lock()
                .expect("MCP launch ticket lock") = Some(launch.clone());
            CoreRequest::McpOpen(Box::new(McpOpenParams {
                owner: guard.transport.session.owner.clone(),
                session_id: guard.transport.session_id.clone(),
                launch_ticket: launch.expose().to_string(),
                transport: if guard.transport.session.http.is_none() {
                    "stdio"
                } else if crate::mcp::is_legacy_sse_transport(&guard.transport.session.config) {
                    "sse"
                } else {
                    "http"
                }
                .to_string(),
                discover_grant: discover,
                initialize_grant: grant,
                initialized_grant: initialized,
                client_version: env!("CARGO_PKG_VERSION").to_string(),
                deadline_ms: timeout.as_millis() as u64,
            }))
        } else {
            CoreRequest::McpRequest(McpRequestParams {
                owner: guard.transport.session.owner.clone(),
                session_id: guard.transport.session_id.clone(),
                grant,
                deadline_ms: timeout.as_millis() as u64,
            })
        };
        let outcome = guard
            .transport
            .host
            .call(request, Some("host:mcp".to_string()))
            .await;
        for operation_id in opening_operations {
            guard
                .transport
                .session
                .pending
                .lock()
                .expect("MCP pending lock")
                .retain(|_, binding| binding.operation_id != operation_id);
            if let Some(operation) = guard
                .transport
                .session
                .operations
                .lock()
                .expect("MCP operation lock")
                .remove(&operation_id)
            {
                guard
                    .transport
                    .manager
                    .shared
                    .core_calls
                    .tickets
                    .revoke(&operation.ticket);
            }
        }
        let discovery = discovery_operation.and_then(|id| {
            guard
                .transport
                .session
                .replies
                .lock()
                .expect("MCP reply lock")
                .remove(&id)
        });
        if method == "initialize"
            && let Ok(opened) = &outcome
        {
            let version = opened["protocolVersion"]
                .as_str()
                .context("MCP SDK omitted negotiated version")?;
            if version == crate::mcp::MCP_MODERN_PROTOCOL_VERSION {
                let mut result = discovery
                    .as_ref()
                    .and_then(|reply| reply.get("result"))
                    .cloned()
                    .context("modern MCP discovery reply absent")?;
                anyhow::ensure!(
                    result["resultType"] == "complete"
                        && result["capabilities"].is_object()
                        && result["supportedVersions"]
                            .as_array()
                            .is_some_and(|versions| versions.iter().any(|v| v == version)),
                    "modern MCP discovery result refused"
                );
                result["protocolVersion"] = json!(version);
                if let Some(info) = result["_meta"]
                    .get("io.modelcontextprotocol/serverInfo")
                    .cloned()
                {
                    result["serverInfo"] = info;
                }
                *guard
                    .transport
                    .session
                    .protocol_version
                    .lock()
                    .expect("MCP protocol lock") = Some(version.to_string());
                guard.transport.replies.push_back(serde_json::to_vec(
                    &json!({"jsonrpc":"2.0", "id":value["id"], "result":result}),
                )?);
                guard.armed = false;
                return Ok(());
            }
            anyhow::ensure!(
                crate::mcp::MCP_CLIENT_ACCEPTED_PROTOCOL_VERSIONS.contains(&version),
                "MCP SDK negotiated unsupported revision"
            );
            *guard
                .transport
                .session
                .protocol_version
                .lock()
                .expect("MCP protocol lock") = Some(version.to_string());
        }
        let raw = guard
            .transport
            .session
            .replies
            .lock()
            .expect("MCP reply lock")
            .remove(&operation_id);
        let legacy_allowed = guard
            .transport
            .session
            .legacy_allowed
            .load(std::sync::atomic::Ordering::SeqCst);
        let raw = raw
            .or_else(|| discovery.filter(|reply| reply.get("error").is_some() && !legacy_allowed));
        if let Some(mut raw) = raw {
            if guard.transport.session.modern()
                && let Some(result) = raw.get("result")
            {
                anyhow::ensure!(
                    matches!(
                        result["resultType"].as_str(),
                        Some("complete" | "input_required")
                    ),
                    "MCP modern result type is missing or unsupported; request was not replayed"
                );
            }
            // Preserve the server's exact result/error (including extra MCP
            // fields), changing only correlation back to the Rust facade id.
            raw["id"] = value["id"].clone();
            // Rust's existing initialize validation owns the diagnostic and
            // accepted-revision contract, including when the SDK refused it.
            guard.transport.replies.push_back(serde_json::to_vec(&raw)?);
            guard.armed = false;
            return Ok(());
        }
        if let Some(error) = guard
            .transport
            .session
            .http
            .as_ref()
            .and_then(http::HttpSession::failure)
        {
            return Err(error);
        }
        outcome
            .map_err(anyhow::Error::msg)
            .context("MCP SDK request failed before an admitted reply")?;
        bail!("MCP SDK connection closed before an admitted reply")
    }
}
struct ExchangeGuard<'a> {
    transport: &'a mut SdkTransport,
    armed: bool,
}
impl Drop for ExchangeGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.transport.session.cancel();
        }
    }
}
impl Drop for SdkTransport {
    fn drop(&mut self) {
        self.manager
            .shared
            .mcp_broker
            .remove(&self.manager.shared, &self.session_id);
    }
}
#[async_trait::async_trait]
impl crate::mcp::McpTransport for SdkTransport {
    async fn send(&mut self, bytes: Vec<u8>) -> Result<()> {
        self.exchange(bytes, None, self.discovery_timeout).await
    }
    async fn send_decided(
        &mut self,
        bytes: Vec<u8>,
        decision: Option<&HumanDecision>,
        timeout: Duration,
    ) -> Result<()> {
        self.exchange(bytes, decision, timeout).await
    }
    async fn recv(&mut self) -> Result<Vec<u8>> {
        self.replies
            .pop_front()
            .context("MCP SDK connection closed without a reply")
    }
    fn is_modern(&self) -> bool {
        self.session.modern()
    }
    async fn last_stderr_line(&self) -> Option<String> {
        let slot = self.session.broker.lock().await;
        slot.as_ref()?.last_stderr_line().await
    }
    fn probe_dead(&self) -> bool {
        self.session.cancel.is_cancelled() || self.host.has_exited() || self.host.is_retiring()
    }
    fn take_list_changed(&self) -> bool {
        self.session
            .list_changed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }
    async fn shutdown(&mut self) {
        if let Some(session) = self
            .manager
            .shared
            .mcp_broker
            .remove(&self.manager.shared, &self.session_id)
            && let Some(mut broker) = session.broker.lock().await.take()
        {
            broker.shutdown().await;
        }
        let _ = self
            .host
            .call(
                CoreRequest::McpClose(McpCloseParams {
                    owner: self.session.owner.clone(),
                    session_id: self.session_id.clone(),
                    deadline_ms: 2500,
                }),
                Some("host:mcp".to_string()),
            )
            .await;
    }
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;

#[path = "mcp_http.rs"]
mod http;

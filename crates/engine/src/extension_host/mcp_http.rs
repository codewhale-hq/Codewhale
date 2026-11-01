//! The official SDK's Fetch implementation; all credentials and network
//! authority stay in the existing Rust McpHttpClient. URLs on the wire are
//! opaque session selectors, never configured URL/query/credential material.
use super::*;
use crate::mcp::http_client::McpHttpClient;
use crate::mcp::wire::{
    MAX_SSE_FRAME_BYTES, McpSessionRejected, find_sse_event_separator_bytes,
    is_streamable_http_incompatible_status, is_streamable_http_stale_session_status,
    resolve_sse_endpoint_url, sse_field_value,
};
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_RESPONSES: usize = 4;
const CHUNK: usize = 32 * 1024;
pub(super) struct HttpSession {
    client: McpHttpClient,
    url: String,
    legacy: AtomicBool,
    started: Mutex<bool>,
    get_opened: Mutex<bool>,
    endpoint: Mutex<Option<String>>,
    server_session: Mutex<Option<String>>,
    responses: Mutex<HashMap<String, Arc<AsyncMutex<Body>>>>,
    slots: Arc<std::sync::atomic::AtomicUsize>,
    failure: Mutex<Option<HttpFailure>>,
}
impl HttpSession {
    pub(super) fn new(client: McpHttpClient, url: &str, legacy: bool) -> Self {
        Self {
            client,
            url: url.to_string(),
            legacy: AtomicBool::new(legacy),
            started: Mutex::new(false),
            get_opened: Mutex::new(false),
            endpoint: Mutex::new(None),
            server_session: Mutex::new(None),
            responses: Mutex::new(HashMap::new()),
            slots: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            failure: Mutex::new(None),
        }
    }
    pub(super) async fn preflight(&self, session: &Session, shared: &ManagerShared) -> Result<()> {
        if self.legacy() {
            return Ok(());
        }
        let run = async {
            let request = self.client.send_mcp_request(
                self.client.get(&self.url),
                false,
                false,
                false,
                || session.validate(shared, session.host_generation, &session.owner),
                |_| Ok(()),
            );
            let response = tokio::time::timeout(Duration::from_secs(5), request).await??;
            session.validate(shared, session.host_generation, &session.owner)?;
            if let Some(sid) = response
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
            {
                if sid.len() > 8192 {
                    bail!("MCP response framing header exceeds bound");
                }
                *self.server_session.lock().expect("MCP session lock") = Some(sid.to_string());
            }
            // Observe only headers. Dropping the response closes quiet SSE.
            Ok::<(), anyhow::Error>(())
        };
        tokio::select! { biased;
            _ = session.cancel.cancelled() => bail!("MCP preflight revoked"),
            _ = run => {}, // ordinary preflight failures are best effort
        }
        session.validate(shared, session.host_generation, &session.owner)
    }
    pub(super) fn close(&self) {
        self.responses.lock().expect("MCP response lock").clear();
    }
    pub(super) fn failure(&self) -> Option<anyhow::Error> {
        self.failure
            .lock()
            .expect("MCP HTTP failure lock")
            .as_ref()
            .map(|failure| match failure {
                HttpFailure::Detail(detail) => anyhow::Error::msg(detail.clone()),
                HttpFailure::Stale(detail) => McpSessionRejected(detail.clone()).into(),
            })
    }
    fn legacy(&self) -> bool {
        self.legacy.load(Ordering::SeqCst)
    }
}
#[derive(Clone)]
enum HttpFailure {
    Detail(String),
    Stale(String),
}
struct Permit(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
struct Body {
    _permit: Permit,
    response: Option<reqwest::Response>,
    sse: bool,
    buffer: Vec<u8>,
    ready: Vec<u8>,
    offset: usize,
    finished: bool,
}
fn opaque(session: &Session) -> String {
    format!("https://mcp-proxy.invalid/{}", session.session_id)
}

pub(super) async fn serve(
    broker: &Broker,
    shared: &ManagerShared,
    generation: u64,
    request: HostRequest,
    cx: &HostRequestContext,
) -> Result<Value> {
    let (id, owner) = match &request {
        HostRequest::NetStart(p) => (&p.session_id, &p.owner),
        HostRequest::NetFetch(p) => (&p.session_id, &p.owner),
        HostRequest::NetRead(p) | HostRequest::NetRelease(p) => (&p.session_id, &p.owner),
        HostRequest::NetClose(p) => (&p.session_id, &p.owner),
        _ => bail!("not a FetchProxy request"),
    };
    let session = broker.session(id)?;
    if matches!(
        &request,
        HostRequest::NetClose(_) | HostRequest::NetRelease(_)
    ) {
        if &session.owner != owner || session.host_generation != generation {
            bail!("MCP HTTP cleanup has wrong owner");
        }
    } else {
        session.validate(shared, generation, owner)?;
    }
    let http = session.http.as_ref().context("not an HTTP session")?;
    match request {
        HostRequest::NetStart(params) => {
            let mut guard = CancelGuard {
                cancel: session.cancel.clone(),
                armed: true,
            };
            shared
                .core_calls
                .tickets
                .redeem(&Presented {
                    ticket: &params.ticket,
                    kind: TicketKind::McpLaunch,
                    tier: HostTier::Builtin,
                    host_generation: generation,
                    owner: &params.owner,
                    method: "net/start",
                    target: Some(&json!({"session_id": params.session_id})),
                })
                .map_err(|bad| {
                    if bad.violation {
                        cx.violation("too many invalid MCP HTTP tickets".into());
                    }
                    anyhow::anyhow!("MCP HTTP start grant refused")
                })?;
            session
                .launch_ticket
                .lock()
                .expect("MCP launch ticket lock")
                .take();
            {
                let mut started = http.started.lock().expect("MCP HTTP state lock");
                if *started || cx.cancel.is_cancelled() {
                    bail!("MCP HTTP start cancelled or replayed");
                }
                *started = true;
            }
            guard.armed = false;
            Ok(json!({}))
        }
        HostRequest::NetFetch(params) => {
            fetch(broker, shared, generation, &session, http, params, cx).await
        }
        HostRequest::NetRead(params) => {
            let mut guard = CancelGuard {
                cancel: session.cancel.clone(),
                armed: true,
            };
            let body = http
                .responses
                .lock()
                .expect("MCP response lock")
                .get(&params.response_id)
                .cloned()
                .context("MCP HTTP response is closed")?;
            let mut body = tokio::select! { biased; _ = cx.cancel.cancelled() => bail!("MCP HTTP read cancelled"), _ = session.cancel.cancelled() => bail!("MCP HTTP withdrawn"), body = body.lock() => body };
            let run = read(&session, http, &mut body);
            let result = tokio::select! { biased; _ = cx.cancel.cancelled() => bail!("MCP HTTP read cancelled"), _ = session.cancel.cancelled() => bail!("MCP HTTP withdrawn"), result = run => result };
            if result.is_err() {
                session.cancel();
            }
            let (data, done) = result?;
            session.validate(shared, generation, &params.owner)?;
            guard.armed = false;
            Ok(json!({"data":data,"done":done}))
        }
        HostRequest::NetRelease(params) => {
            http.responses
                .lock()
                .expect("MCP response lock")
                .remove(&params.response_id);
            Ok(json!({}))
        }
        HostRequest::NetClose(_) => {
            http.close();
            broker.remove(shared, &session.session_id);
            Ok(json!({}))
        }
        _ => bail!("not a FetchProxy request"),
    }
}
async fn fetch(
    broker: &Broker,
    shared: &ManagerShared,
    generation: u64,
    session: &Arc<Session>,
    http: &HttpSession,
    params: NetFetchParams,
    cx: &HostRequestContext,
) -> Result<Value> {
    let mut guard = CancelGuard {
        cancel: session.cancel.clone(),
        armed: true,
    };
    if !*http.started.lock().expect("MCP HTTP state lock")
        || http.responses.lock().expect("MCP response lock").len() >= MAX_RESPONSES
    {
        bail!("MCP HTTP response bound or stale start");
    }
    let slots = Arc::clone(&http.slots);
    // Match the existing admission loops: try_update exceeds our Rust MSRV.
    let mut count = slots.load(std::sync::atomic::Ordering::SeqCst);
    loop {
        if count >= MAX_RESPONSES {
            bail!("MCP HTTP active response cap exceeded");
        }
        match slots.compare_exchange_weak(
            count,
            count + 1,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        ) {
            Ok(_) => break,
            Err(current) => count = current,
        }
    }
    let permit = Permit(slots);
    let base = opaque(session);
    let endpoint_selector = format!("{base}/endpoint");
    let url = if params.url == base {
        http.url.clone()
    } else if http.legacy() && params.url == endpoint_selector {
        http.endpoint
            .lock()
            .expect("MCP endpoint lock")
            .clone()
            .context("SSE endpoint not admitted")?
    } else {
        bail!("MCP HTTP URL has no Rust selector");
    };
    let modern = params.frame.as_ref().is_some_and(|frame| {
        frame["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"]
            == crate::mcp::MCP_MODERN_PROTOCOL_VERSION
    });
    if modern {
        anyhow::ensure!(
            params.headers.mcp_session_id.is_none(),
            "modern MCP has no session header"
        );
        let frame = params.frame.as_ref().context("modern MCP frame absent")?;
        anyhow::ensure!(
            params.headers.mcp_protocol_version.as_deref()
                == Some(crate::mcp::MCP_MODERN_PROTOCOL_VERSION),
            "MCP envelope/header version mismatch"
        );
        let method = frame["method"].as_str().context("MCP method absent")?;
        let expected_name = match method {
            "tools/call" | "prompts/get" => frame["params"]["name"].as_str(),
            "resources/read" => frame["params"]["uri"].as_str(),
            _ => None,
        }
        .map(mcp_name_header);
        anyhow::ensure!(
            params.headers.mcp_method.as_deref() == frame.get("id").map(|_| method)
                && params.headers.mcp_name == expected_name,
            "MCP body/header target mismatch"
        );
    } else {
        anyhow::ensure!(
            params.headers.mcp_method.is_none()
                && params.headers.mcp_name.is_none()
                && params.headers.mcp_protocol_version.as_deref()
                    != Some(crate::mcp::MCP_MODERN_PROTOCOL_VERSION),
            "legacy MCP cannot claim modern target headers"
        );
    }
    let headers = [
        ("accept", params.headers.accept.as_deref()),
        ("mcp-method", params.headers.mcp_method.as_deref()),
        ("mcp-name", params.headers.mcp_name.as_deref()),
        ("content-type", params.headers.content_type.as_deref()),
        ("mcp-session-id", params.headers.mcp_session_id.as_deref()),
        (
            "mcp-protocol-version",
            params.headers.mcp_protocol_version.as_deref(),
        ),
    ];
    if headers
        .iter()
        .any(|(_, value)| value.is_some_and(|value| value.len() > 8192))
    {
        bail!("MCP HTTP framing header exceeds bound");
    }
    if let Some(value) = params.headers.mcp_session_id.as_deref()
        && http
            .server_session
            .lock()
            .expect("MCP session lock")
            .as_deref()
            != Some(value)
    {
        bail!("MCP server session ID not observed");
    }
    if let Some(value) = params.headers.mcp_protocol_version.as_deref()
        && value != crate::mcp::MCP_MODERN_PROTOCOL_VERSION
        && !crate::mcp::MCP_CLIENT_ACCEPTED_PROTOCOL_VERSIONS.contains(&value)
    {
        bail!("MCP protocol header refused");
    }
    let mut deadline = Instant::now() + Duration::from_secs(30);
    let original = params.operation_id.as_ref().and_then(|id| {
        session
            .operations
            .lock()
            .expect("MCP operation lock")
            .get(id)
            .cloned()
    });
    let request = match params.method.as_str() {
        "POST" => {
            let write = ProcWriteParams {
                owner: params.owner.clone(),
                session_id: params.session_id.clone(),
                frame: params.frame.clone().context("MCP HTTP frame absent")?,
                ticket: params.ticket.clone(),
                operation_id: params.operation_id.clone(),
            };
            let (frame, expires) =
                broker.authorize_frame(shared, generation, session, write, cx, "net/fetch")?;
            deadline = expires;
            if frame["method"] == "initialize" && !http.legacy() {
                http.preflight(session, shared).await?;
            }
            http.client.post(&url).body(serde_json::to_vec(&frame)?)
        }
        "GET" => {
            anyhow::ensure!(
                !session.modern(),
                "modern MCP channel has no subscription grant"
            );
            if params.frame.is_some()
                || params.ticket.is_some()
                || params.operation_id.is_some()
                || params.url != base
            {
                bail!("MCP HTTP GET is not a channel open");
            }
            let mut opened = http.get_opened.lock().expect("MCP GET state lock");
            if *opened {
                bail!("MCP HTTP channel cannot reconnect or replay");
            }
            *opened = true;
            http.client.get(&url)
        }
        _ => bail!("MCP HTTP method has no authority"),
    };
    // A preflight session header is an observed Rust value. The SDK removes
    // it from initialize by spec, so apply it here for strict legacy servers.
    let mut request = request;
    for (name, value) in headers {
        if let Some(value) = value {
            request = request.header(name, value);
        }
    }
    if params.method == "POST"
        && !http.legacy()
        && !modern
        && let Some(sid) = http
            .server_session
            .lock()
            .expect("MCP session lock")
            .as_ref()
    {
        request = request.header("mcp-session-id", sid);
    }
    let send = http.client.send_mcp_request(
        request,
        params.method == "POST",
        params.method == "GET",
        params.method == "POST"
            && !http.legacy()
            && (!modern
                || params
                    .frame
                    .as_ref()
                    .is_some_and(|frame| frame["method"] == "server/discover")),
        || {
            session.validate(shared, generation, &params.owner)?;
            if cx.cancel.is_cancelled() || Instant::now() >= deadline {
                bail!("MCP HTTP operation expired or cancelled");
            }
            Ok(())
        },
        |response| {
            if !modern
                && let Some(sid) = response
                    .headers()
                    .get("mcp-session-id")
                    .and_then(|v| v.to_str().ok())
            {
                if sid.len() > 8192 {
                    bail!("MCP response framing header exceeds bound");
                }
                *http.server_session.lock().expect("MCP session lock") = Some(sid.to_string());
            }
            Ok(())
        },
    );
    let result = tokio::select! { biased;
        _ = cx.cancel.cancelled() => bail!("MCP HTTP cancelled"),
        _ = session.cancel.cancelled() => bail!("MCP HTTP revoked"),
        result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), send) => result.context("MCP HTTP operation expired")?,
    };
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            // Retain a Rust-only diagnostic; net/* RPC never exposes configured
            // origins, OAuth metadata, credentials or reflected provider text.
            *http.failure.lock().expect("MCP HTTP failure lock") =
                Some(HttpFailure::Detail(format!("{error:#}")));
            return Err(error);
        }
    };
    session.validate(shared, generation, &params.owner)?;
    let status = response.status().as_u16();
    let mut headers = HashMap::new();
    for name in ["content-type", "mcp-session-id"] {
        if modern && name == "mcp-session-id" {
            continue;
        }
        if let Some(value) = response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
        {
            if value.len() > 8192 {
                bail!("MCP response framing header exceeds bound");
            }
            headers.insert(name.to_string(), value.to_string());
        }
    }
    if let Some(sid) = headers.get("mcp-session-id") {
        *http.server_session.lock().expect("MCP session lock") = Some(sid.clone());
    }
    if params.method == "POST"
        && !http.legacy()
        && !modern
        && !headers.contains_key("mcp-session-id")
        && let Some(sid) = http
            .server_session
            .lock()
            .expect("MCP session lock")
            .as_ref()
    {
        headers.insert("mcp-session-id".to_string(), sid.clone());
    }
    let sse = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("text/event-stream")
        });
    if !sse
        && response
            .content_length()
            .is_some_and(|size| size > MAX_MCP_RESPONSE_BYTES as u64)
    {
        bail!("MCP HTTP body exceeds bound");
    }
    // Only a parsed, explicit transport refusal admits a fresh exact grant.
    // Never negotiate on auth, cancellation, partial writes, malformed replies
    // or JSON-RPC errors. A stale session is a typed Rust refusal instead.
    if params.method == "POST" && !(200..300).contains(&status) {
        let had_session = http
            .server_session
            .lock()
            .expect("MCP session lock")
            .is_some();
        let excerpt = tokio::select! { biased;
            _ = cx.cancel.cancelled() => bail!("MCP refusal read cancelled"),
            _ = session.cancel.cancelled() => bail!("MCP refusal read revoked"),
            excerpt = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline),
                crate::mcp::bounded_body_excerpt(response, crate::mcp::ERROR_BODY_PREVIEW_BYTES)) => excerpt.context("MCP refusal read expired")?,
        };
        session.validate(shared, generation, &params.owner)?;
        if cx.cancel.is_cancelled() || Instant::now() >= deadline {
            bail!("MCP refusal expired or cancelled");
        }
        let safe_excerpt = http.client.server_error_preview(&excerpt);
        let detail = format!(
            "status={} body={safe_excerpt}",
            reqwest::StatusCode::from_u16(status)?
        );
        let stale = if modern {
            false
        } else if http.legacy() {
            crate::mcp::wire::is_mcp_stale_session_body(&excerpt)
        } else {
            had_session
                && is_streamable_http_stale_session_status(
                    reqwest::StatusCode::from_u16(status)?,
                    &excerpt,
                )
        };
        if modern
            && params
                .frame
                .as_ref()
                .is_some_and(|frame| frame["method"] == "server/discover")
            && (matches!(status, 404 | 405 | 406 | 415)
                || (status == 400 && crate::mcp::wire::is_json_rpc_error_body(&excerpt)))
        {
            session.legacy_allowed.store(true, Ordering::SeqCst);
            guard.armed = false;
            return Ok(json!({"status":status,"headers":headers,"legacy_handshake":true}));
        }
        if stale {
            let detail = if http.legacy() {
                format!(
                    "MCP session expired (transport=sse endpoint={} status={}): {safe_excerpt}",
                    crate::mcp::mask_url_secrets(&url),
                    reqwest::StatusCode::from_u16(status)?
                )
            } else {
                format!(
                    "MCP Streamable HTTP session expired; retry with a new session required ({detail})"
                )
            };
            *http.failure.lock().expect("MCP HTTP failure lock") = Some(HttpFailure::Stale(detail));
        } else if !modern
            && !http.legacy()
            && original
                .as_ref()
                .is_some_and(|operation| operation.target["method"] == "initialize")
            && (is_streamable_http_incompatible_status(reqwest::StatusCode::from_u16(status)?)
                || (status == 400 && crate::mcp::wire::is_json_rpc_error_body(&excerpt)))
        {
            let operation = original.context("transport refusal has no semantic operation")?;
            let grant = renegotiate(
                shared,
                generation,
                session,
                http,
                &params.owner,
                operation,
                deadline,
            )?;
            guard.armed = false;
            return Ok(json!({"status":status,"headers":headers,"legacy_grant":grant}));
        } else {
            let detail = if http.legacy() {
                format!(
                    "MCP SSE POST rejected (transport=sse endpoint={} status={}): {safe_excerpt}",
                    crate::mcp::mask_url_secrets(&url),
                    reqwest::StatusCode::from_u16(status)?
                )
            } else {
                format!(
                    "MCP Streamable HTTP rejected (transport=http url={} status={}): {safe_excerpt}",
                    crate::mcp::mask_url_secrets(&url),
                    reqwest::StatusCode::from_u16(status)?
                )
            };
            *http.failure.lock().expect("MCP HTTP failure lock") =
                Some(HttpFailure::Detail(detail));
        }
        guard.armed = false;
        return Ok(json!({"status":status,"headers":headers}));
    }
    // Never expose reflected provider errors or authentication headers to TS.
    if !(200..300).contains(&status) {
        guard.armed = false;
        return Ok(json!({"status":status,"headers":headers}));
    }
    if status == 204 || status == 202 {
        guard.armed = false;
        return Ok(json!({"status": status,"headers":headers}));
    }
    if params.method == "GET" && !sse {
        bail!("MCP channel response is not SSE");
    }
    let response_id = uuid::Uuid::new_v4().to_string();
    let body = Body {
        _permit: permit,
        response: Some(response),
        sse,
        buffer: Vec::new(),
        ready: Vec::new(),
        offset: 0,
        finished: false,
    };
    http.responses
        .lock()
        .expect("MCP response lock")
        .insert(response_id.clone(), Arc::new(AsyncMutex::new(body)));
    guard.armed = false;
    Ok(json!({"response_id":response_id,"status":status,"headers":headers}))
}
fn renegotiate(
    shared: &ManagerShared,
    generation: u64,
    session: &Session,
    http: &HttpSession,
    owner: &OwnerRef,
    mut operation: Operation,
    deadline: Instant,
) -> Result<McpOperationGrant> {
    session.validate(shared, generation, owner)?;
    let ttl = deadline
        .checked_duration_since(Instant::now())
        .context("MCP negotiation expired")?;
    if operation.target["method"] != "initialize"
        || ttl.is_zero()
        || http.legacy.swap(true, Ordering::SeqCst)
    {
        bail!("MCP negotiation unavailable");
    }
    let operation_id = operation.target["operation_id"]
        .as_str()
        .context("MCP operation id absent")?
        .to_string();
    let method = operation.target["method"]
        .as_str()
        .context("MCP operation method absent")?
        .to_string();
    let params = operation.target["params"].clone();
    let id = operation.target["wire_id"].as_str().map(str::to_string);
    if let Some(id) = &id {
        let mut pending = session.pending.lock().expect("MCP pending lock");
        let key = wire_id(&json!(id))?;
        if pending
            .get(&key)
            .is_none_or(|binding| binding.operation_id != operation_id)
        {
            bail!("MCP negotiation target changed");
        }
        pending.remove(&key);
    }
    let ticket = shared.core_calls.tickets.mint(Grant {
        kind: TicketKind::McpOperation,
        tier: HostTier::Builtin,
        host_generation: generation,
        owner: owner.clone(),
        method: "net/fetch",
        target: operation.target.clone(),
        ttl,
        uses: 1,
    });
    operation.ticket = ticket.clone();
    let mut operations = session.operations.lock().expect("MCP operation lock");
    if operations.len() >= MAX_PENDING || operations.contains_key(&operation_id) {
        shared.core_calls.tickets.revoke(&ticket);
        bail!("MCP negotiation operation unavailable");
    }
    operations.insert(operation_id.clone(), operation);
    *http.get_opened.lock().expect("MCP GET state lock") = false;
    *http.server_session.lock().expect("MCP session lock") = None;
    http.close();
    Ok(McpOperationGrant {
        ticket: ticket.expose().to_string(),
        operation_id,
        method,
        wire_id: id,
        params,
    })
}

fn mcp_name_header(value: &str) -> String {
    if value.is_empty()
        || value.trim() != value
        || (value.starts_with("=?base64?") && value.ends_with("?="))
        || value
            .bytes()
            .any(|byte| byte != b'\t' && !(0x20..=0x7e).contains(&byte))
    {
        format!(
            "=?base64?{}?=",
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, value.as_bytes())
        )
    } else {
        value.to_string()
    }
}
async fn read(session: &Session, http: &HttpSession, body: &mut Body) -> Result<(Vec<u8>, bool)> {
    loop {
        if body.offset < body.ready.len() {
            let end = (body.offset + CHUNK).min(body.ready.len());
            let chunk = body.ready[body.offset..end].to_vec();
            body.offset = end;
            if body.offset == body.ready.len() {
                body.ready.clear();
                body.offset = 0;
            }
            return Ok((chunk, body.finished && body.ready.is_empty()));
        }
        if body.finished {
            return Ok((Vec::new(), true));
        }
        if body.sse
            && let Some((at, separator)) = find_sse_event_separator_bytes(&body.buffer)
        {
            if at > MAX_SSE_FRAME_BYTES {
                bail!("MCP SSE event exceeds bound");
            }
            let mut event = body.buffer.drain(..at + separator).collect::<Vec<_>>();
            let text = std::str::from_utf8(&event)?;
            let mut kind = "message";
            let mut data = String::new();
            for line in text.lines() {
                if let Some(value) = sse_field_value(line, "event:") {
                    kind = value;
                } else if let Some(value) = sse_field_value(line, "data:") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value);
                }
            }
            if kind == "endpoint" {
                if !http.legacy() || http.endpoint.lock().expect("MCP endpoint lock").is_some() {
                    bail!("unexpected SSE endpoint");
                }
                let endpoint = resolve_sse_endpoint_url(&http.url, &data)?;
                *http.endpoint.lock().expect("MCP endpoint lock") = Some(endpoint);
                event =
                    format!("event: endpoint\ndata: {}/endpoint\n\n", opaque(session)).into_bytes();
            } else if kind == "message" && !data.trim().is_empty() {
                if http.legacy() && http.endpoint.lock().expect("MCP endpoint lock").is_none() {
                    bail!("SSE message before endpoint");
                }
                session.observe(&serde_json::from_str(&data)?)?;
            }
            body.ready = event;
            continue;
        }
        let response = body.response.as_mut().context("MCP body is closed")?;
        match response.chunk().await? {
            Some(chunk) => {
                let max = MAX_MCP_RESPONSE_BYTES;
                if body
                    .buffer
                    .len()
                    .checked_add(chunk.len())
                    .is_none_or(|n| n > max)
                {
                    bail!("MCP response assembly exceeds bound");
                }
                body.buffer.extend_from_slice(&chunk);
            }
            None => {
                body.response.take();
                body.finished = true;
                if body.sse {
                    if !body.buffer.is_empty() {
                        bail!("MCP SSE event ended without separator");
                    }
                } else if !body.buffer.is_empty() {
                    let value: Value = serde_json::from_slice(&body.buffer)?;
                    if let Some(values) = value.as_array() {
                        if values.len() > MAX_PENDING {
                            bail!("MCP batch exceeds bound");
                        }
                        for frame in values {
                            session.observe(frame)?;
                        }
                    } else {
                        session.observe(&value)?;
                    }
                    body.ready = std::mem::take(&mut body.buffer);
                }
            }
        }
    }
}
#[cfg(test)]
#[path = "mcp_http_tests.rs"]
mod tests;

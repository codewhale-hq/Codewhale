//! Committed official SDK + real Rust FetchProxy/tickets + guarded loopback
//! HTTP peer. No provider, credentials, SDK-side fetch or user filesystem.
use super::*;
use crate::extension_host::TestManagerGuard;
use crate::extension_host::test_support::{FixturePlugins, node_for_tests};
use crate::mcp::McpPool;
use codewhale_config::McpBackend;
use codewhale_mcp::config::McpConfig;
use codewhale_plugins::activation::TestPolicyGuard;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy)]
enum Mode {
    Json,
    Modern,
    PostSse,
    Legacy,
    RequirePreflight,
    Negotiate,
}
struct Peer {
    url: String,
    frames: Arc<Mutex<Vec<Value>>>,
    headers: Arc<Mutex<Vec<String>>>,
    unauthorized: Arc<std::sync::atomic::AtomicBool>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Peer {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}
async fn request(socket: &mut TcpStream) -> Option<(String, Value)> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 4096];
    let header_end = loop {
        let count = socket.read(&mut chunk).await.ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
        assert!(bytes.len() <= MAX_MCP_RESPONSE_BYTES + 16 * 1024);
        if let Some(at) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break at + 4;
        }
    };
    let head = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    assert!(length <= MAX_MCP_RESPONSE_BYTES);
    while bytes.len() < header_end + length {
        let count = socket.read(&mut chunk).await.ok()?;
        if count == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    let frame = if length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap()
    };
    Some((head, frame))
}
async fn reply(socket: &mut TcpStream, status: &str, content: &str, body: &[u8], session: bool) {
    let header = format!(
        "HTTP/1.1 {status}\r\nConnection: close\r\nContent-Type: {content}\r\n{}Content-Length: {}\r\n\r\n",
        if session {
            "Mcp-Session-Id: observed-fixture-session\r\n"
        } else {
            ""
        },
        body.len()
    );
    socket.write_all(header.as_bytes()).await.unwrap();
    socket.write_all(body).await.unwrap();
}
async fn peer(mode: Mode) -> Peer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let legacy = matches!(mode, Mode::Legacy | Mode::Negotiate);
    let path = if matches!(mode, Mode::Legacy) {
        "sse"
    } else {
        "mcp"
    };
    let preflights = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let frames = Arc::new(Mutex::new(Vec::new()));
    let headers = Arc::new(Mutex::new(Vec::new()));
    let unauthorized = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let received = Arc::clone(&frames);
    let seen_headers = Arc::clone(&headers);
    let reject = Arc::clone(&unauthorized);
    let channel: Arc<Mutex<Option<mpsc::UnboundedSender<Value>>>> = Arc::new(Mutex::new(None));
    let task = tokio::spawn(async move {
        loop {
            let accepted = tokio::select! { biased; _ = token.cancelled() => break, accepted = listener.accept() => accepted };
            let Ok((mut socket, _)) = accepted else {
                break;
            };
            let token = token.clone();
            let received = Arc::clone(&received);
            let headers = Arc::clone(&seen_headers);
            let channel = Arc::clone(&channel);
            let reject = Arc::clone(&reject);
            let preflights = Arc::clone(&preflights);
            tokio::spawn(async move {
                let Some((head, frame)) = (tokio::select! { biased; _ = token.cancelled() => return, value = request(&mut socket) => value })
                else {
                    return;
                };
                headers.lock().unwrap().push(head.clone());
                if head.starts_with("GET ") {
                    if matches!(mode, Mode::RequirePreflight)
                        && preflights.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0
                    {
                        reply(&mut socket, "200 OK", "application/json", b"", true).await;
                        return;
                    }
                    if !legacy {
                        reply(
                            &mut socket,
                            "405 Method Not Allowed",
                            "text/plain",
                            b"",
                            false,
                        )
                        .await;
                        return;
                    }
                    let (send, mut rx) = mpsc::unbounded_channel();
                    *channel.lock().unwrap() = Some(send);
                    let initial = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\nevent: endpoint\ndata: /messages?fixture=only\n\n";
                    socket.write_all(initial.as_bytes()).await.unwrap();
                    loop {
                        let frame = tokio::select! { biased; _ = token.cancelled() => break, next = rx.recv() => next };
                        let Some(frame) = frame else {
                            break;
                        };
                        if socket
                            .write_all(format!("event: message\ndata: {frame}\n\n").as_bytes())
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    return;
                }
                assert!(
                    head.to_ascii_lowercase()
                        .contains("authorization: bearer fixture-only-bearer")
                );
                if matches!(mode, Mode::Negotiate) && head.starts_with("POST /mcp ") {
                    received.lock().unwrap().push(frame.clone());
                    reply(
                        &mut socket,
                        "405 Method Not Allowed",
                        "text/plain",
                        b"",
                        false,
                    )
                    .await;
                    return;
                }
                if matches!(mode, Mode::RequirePreflight) && frame["method"] != "server/discover" {
                    assert!(
                        head.to_ascii_lowercase()
                            .contains("mcp-session-id: observed-fixture-session"),
                        "preflight session must accompany initialize too"
                    );
                }
                assert!(head.starts_with(if legacy {
                    "POST /messages?fixture=only "
                } else {
                    "POST /mcp "
                }));
                if frame.get("id").is_some() && frame.get("method").is_some() {
                    assert!(
                        frame["id"].is_string(),
                        "official SDK numeric IDs must be translated before the peer"
                    );
                }
                received.lock().unwrap().push(frame.clone());
                let method = frame["method"].as_str().unwrap();
                if reject.load(std::sync::atomic::Ordering::SeqCst) && method != "initialize" {
                    reply(
                        &mut socket,
                        "401 Unauthorized",
                        "text/plain",
                        b"untrusted provider error must stay out of TS",
                        false,
                    )
                    .await;
                    return;
                }
                if frame.get("id").is_none() {
                    reply(&mut socket, "202 Accepted", "text/plain", b"", false).await;
                    return;
                }
                if method == "server/discover" {
                    let response = if matches!(mode, Mode::Modern) {
                        json!({"jsonrpc":"2.0","id":frame["id"],"result":{"resultType":"complete","ttlMs":0,"cacheScope":"private","supportedVersions":["2026-07-28"],"capabilities":{"tools":{}}}})
                    } else {
                        json!({"jsonrpc":"2.0","id":frame["id"],"error":{"code":-32601,"message":"Method not found"}})
                    };
                    reply(
                        &mut socket,
                        "200 OK",
                        "application/json",
                        response.to_string().as_bytes(),
                        matches!(mode, Mode::Modern),
                    )
                    .await;
                    return;
                }
                let mut result = match method {
                    "initialize" => {
                        json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"fetch-fixture","version":"1"}})
                    }
                    "tools/list" => {
                        json!({"tools":[{"name":"echo","inputSchema":{"type":"object"}}]})
                    }
                    "tools/call" => {
                        json!({"content":[{"type":"text","text":frame["params"]["arguments"].to_string()}],"isError":false})
                    }
                    _ => panic!("unexpected fixture method {method}"),
                };
                if matches!(mode, Mode::Modern) {
                    result["resultType"] = json!("complete");
                    if method == "tools/list" {
                        result["ttlMs"] = json!(0);
                        result["cacheScope"] = json!("private");
                    }
                }
                let frame = json!({"jsonrpc":"2.0","id":frame["id"],"result":result});
                if legacy {
                    channel
                        .lock()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .send(frame)
                        .unwrap();
                    reply(&mut socket, "202 Accepted", "text/plain", b"", false).await;
                } else if matches!(mode, Mode::PostSse) {
                    reply(
                        &mut socket,
                        "200 OK",
                        "text/event-stream",
                        format!("event: message\ndata: {frame}\n\n").as_bytes(),
                        method == "initialize",
                    )
                    .await;
                } else {
                    reply(
                        &mut socket,
                        "200 OK",
                        "application/json",
                        frame.to_string().as_bytes(),
                        method == "initialize",
                    )
                    .await;
                }
            });
        }
    });
    Peer {
        url: format!("http://{addr}/{path}"),
        frames,
        headers,
        unauthorized,
        cancel,
        task,
    }
}
async fn config(peer: &Peer) -> McpServerConfig {
    serde_json::from_value(json!({
        "url": peer.url,
        "transport": peer.url.ends_with("/sse").then_some("sse"),
        "headers": {"Authorization":"Bearer fixture-only-bearer"},
        "connect_timeout": 5,
        "read_timeout": 5,
    }))
    .unwrap()
}
async fn real_mode(mode: Mode, name: &str) {
    let _env = crate::test_support::lock_test_env();
    let _proxies: Vec<_> = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .into_iter()
    .map(crate::test_support::EnvVarGuard::remove)
    .collect();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests(name) else {
        return;
    };
    let fixture = FixturePlugins::new(&[]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let peer = peer(mode).await;
    let mut pool = McpPool::new(McpConfig {
        servers: [(name.to_string(), config(&peer).await)].into(),
        ..Default::default()
    })
    .with_backend(McpBackend::Host);
    let connection = pool.get_or_connect(name).await.unwrap();
    assert!(connection.is_ready());
    assert_eq!(connection.tools().len(), 1);
    let result = connection
        .call_tool("echo", json!({"payload":"exact"}), 5)
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "{\"payload\":\"exact\"}");
    assert!(
        peer.headers.lock().unwrap().iter().all(|head| head
            .to_ascii_lowercase()
            .contains("authorization: bearer fixture-only-bearer")),
        "preflight, every POST and the actual SSE GET retain Rust auth"
    );
    let frames = peer.frames.lock().unwrap().clone();
    for method in [
        "initialize",
        "notifications/initialized",
        "tools/list",
        "tools/call",
    ] {
        assert_eq!(
            frames
                .iter()
                .filter(|frame| frame["method"] == method)
                .count(),
            if matches!(mode, Mode::Modern)
                && matches!(method, "initialize" | "notifications/initialized")
            {
                0
            } else if method == "initialize" && matches!(mode, Mode::Negotiate) {
                2
            } else {
                1
            }
        );
    }
    assert!(
        frames
            .iter()
            .filter(|frame| frame.get("id").is_some())
            .all(|frame| frame["id"].is_string())
    );
    for (method, expected) in [
        ("initialize", "1"),
        ("tools/list", "2"),
        ("tools/call", "3"),
    ] {
        if matches!(mode, Mode::Modern) && method == "initialize" {
            continue;
        }
        assert_eq!(
            frames
                .iter()
                .find(|frame| frame["method"] == method)
                .unwrap()["id"],
            expected,
            "the original Rust facade ID must reach the real peer"
        );
    }
    if matches!(mode, Mode::Modern) {
        assert_eq!(
            frames
                .iter()
                .filter(|frame| frame["method"] == "server/discover")
                .count(),
            1
        );
        let headers = peer.headers.lock().unwrap();
        assert_eq!(headers.len(), frames.len());
        for (frame, head) in frames.iter().zip(headers.iter()) {
            assert_eq!(frame["params"]["_meta"], modern_metadata());
            let head = head.to_ascii_lowercase();
            assert!(head.starts_with("post "));
            assert!(!head.contains("mcp-session-id:"));
            assert!(
                head.lines()
                    .any(|line| line == "mcp-protocol-version: 2026-07-28")
            );
            assert!(
                head.lines().any(
                    |line| line == format!("mcp-method: {}", frame["method"].as_str().unwrap())
                )
            );
            assert_eq!(
                head.contains("mcp-name: echo"),
                frame["method"] == "tools/call"
            );
        }
        assert_eq!(result["resultType"], "complete");
    } else if !matches!(mode, Mode::Legacy | Mode::Negotiate) {
        assert!(peer.headers.lock().unwrap().iter().any(|head| {
            head.to_ascii_lowercase()
                .contains("mcp-session-id: observed-fixture-session")
        }));
    }
    let sessions: Vec<_> = manager
        .shared
        .mcp_broker
        .sessions
        .lock()
        .unwrap()
        .values()
        .cloned()
        .collect();
    assert_eq!(sessions.len(), 1);
    assert!(sessions[0].http.is_some());
    assert!(sessions[0].broker.lock().await.is_none());
    pool.shutdown_all().await;
    manager.shutdown().await;
    assert!(
        manager
            .shared
            .mcp_broker
            .sessions
            .lock()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test(flavor = "current_thread")]
async fn host_http_json_uses_real_sdk_guarded_client_and_exact_string_ids() {
    real_mode(Mode::Json, "host_http_json").await;
    real_mode(Mode::Modern, "host_http_modern").await;
}
#[tokio::test(flavor = "current_thread")]
async fn host_http_sse_body_uses_real_sdk_and_shared_bounded_parser() {
    real_mode(Mode::PostSse, "host_http_sse").await;
}
#[tokio::test(flavor = "current_thread")]
async fn host_legacy_sse_observes_endpoint_and_uses_same_guarded_client() {
    real_mode(Mode::Legacy, "host_legacy_sse").await;
}

#[tokio::test(flavor = "current_thread")]
async fn host_http_preflight_session_reaches_initialize_and_subsequent_requests() {
    real_mode(Mode::RequirePreflight, "host_http_preflight").await;
}
#[tokio::test(flavor = "current_thread")]
async fn host_http_incompatible_handshake_negotiates_real_sdk_sse_with_original_id() {
    real_mode(Mode::Negotiate, "host_http_negotiate").await;
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_proxy_refuses_wire_id_mutation_before_any_network_write() {
    let _env = crate::test_support::lock_test_env();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("fetch_proxy_wire_binding") else {
        return;
    };
    let fixture = FixturePlugins::new(&[]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let peer = peer(Mode::Json).await;
    let config = config(&peer).await;
    for invalid_id in [json!("forged-id"), json!(7), Value::Null] {
        let client = McpHttpClient::new(
            &peer.url,
            false,
            false,
            false,
            None,
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        let transport = SdkTransport::connect_with_http(
            "fixture",
            &config,
            CancellationToken::new(),
            Duration::from_secs(5),
            Some(client),
        )
        .await
        .unwrap();
        let grant = transport
            .grant(
                "tools/list",
                json!({}),
                Duration::from_secs(5),
                None,
                Some("rust-id"),
            )
            .unwrap();
        *transport
            .session
            .http
            .as_ref()
            .unwrap()
            .started
            .lock()
            .unwrap() = true;
        let params = NetFetchParams {
            owner: transport.session.owner.clone(),
            session_id: transport.session_id.clone(),
            url: opaque(&transport.session),
            method: "POST".into(),
            headers: McpHttpHeaders::default(),
            frame: Some(json!({"jsonrpc":"2.0","id":invalid_id,"method":"tools/list","params":{}})),
            ticket: Some(grant.ticket),
            operation_id: Some(grant.operation_id),
        };
        let (cx, _, _) = HostRequestContext::for_test(20);
        assert!(
            manager
                .shared
                .mcp_broker
                .serve(
                    &manager.shared,
                    transport.session.host_generation,
                    HostRequest::NetFetch(params),
                    cx
                )
                .await
                .is_err()
        );
        assert!(peer.frames.lock().unwrap().is_empty());
        assert!(transport.session.cancel.is_cancelled());
    }
    manager.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_proxy_foreign_url_cannot_use_session_network_authority() {
    let _env = crate::test_support::lock_test_env();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("fetch_proxy_url_binding") else {
        return;
    };
    let fixture = FixturePlugins::new(&[]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let peer = peer(Mode::Json).await;
    let config = config(&peer).await;
    let client = McpHttpClient::new(
        &peer.url,
        false,
        false,
        false,
        None,
        Duration::from_secs(5),
        Duration::from_secs(5),
    )
    .unwrap();
    let transport = SdkTransport::connect_with_http(
        "fixture",
        &config,
        CancellationToken::new(),
        Duration::from_secs(5),
        Some(client),
    )
    .await
    .unwrap();
    *transport
        .session
        .http
        .as_ref()
        .unwrap()
        .started
        .lock()
        .unwrap() = true;
    let admitted_preflight_count = peer.headers.lock().unwrap().len();
    let params = NetFetchParams {
        owner: transport.session.owner.clone(),
        session_id: transport.session_id.clone(),
        url: peer.url.clone(),
        method: "GET".into(),
        headers: McpHttpHeaders::default(),
        frame: None,
        ticket: None,
        operation_id: None,
    };
    let (cx, _, _) = HostRequestContext::for_test(21);
    assert!(
        manager
            .shared
            .mcp_broker
            .serve(
                &manager.shared,
                transport.session.host_generation,
                HostRequest::NetFetch(params),
                cx
            )
            .await
            .is_err()
    );
    assert_eq!(
        peer.headers.lock().unwrap().len(),
        admitted_preflight_count,
        "forged URL must not make any additional request"
    );
    manager.shutdown().await;
}
#[test]
fn fetch_proxy_wire_is_builtin_only_and_carries_no_credential_or_auth_api() {
    let owner = json!({"plugin_id":"host:mcp","generation":1,"owner_token":"fixture"});
    let params = json!({"owner":owner,"session_id":"s","url":"https://mcp-proxy.invalid/s","method":"GET","headers":{}});
    let frame = json!({"jsonrpc":"2.0","id":1,"method":"net/fetch","params":params});
    assert!(parse_host_message(frame.clone(), HostTier::Builtin).is_ok());
    assert!(parse_host_message(frame.clone(), HostTier::Plugin).is_err());
    let mut forbidden = frame;
    forbidden["params"]["bearer_token"] = json!("forbidden");
    assert!(parse_host_message(forbidden, HostTier::Builtin).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn fetch_proxy_owner_revocation_releases_retained_legacy_body() {
    let _env = crate::test_support::lock_test_env();
    let _proxies: Vec<_> = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .into_iter()
    .map(crate::test_support::EnvVarGuard::remove)
    .collect();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("fetch_proxy_owner_revoke") else {
        return;
    };
    let fixture = FixturePlugins::new(&[]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let peer = peer(Mode::Legacy).await;
    let mut pool = McpPool::new(McpConfig {
        servers: [("fixture".into(), config(&peer).await)].into(),
        ..Default::default()
    })
    .with_backend(McpBackend::Host);
    pool.get_or_connect("fixture").await.unwrap();
    let session = manager
        .shared
        .mcp_broker
        .sessions
        .lock()
        .unwrap()
        .values()
        .next()
        .cloned()
        .unwrap();
    let http = session.http.as_ref().unwrap();
    assert_eq!(
        http.responses.lock().unwrap().len(),
        1,
        "the real SDK is reading the retained legacy GET"
    );
    assert_eq!(http.slots.load(std::sync::atomic::Ordering::SeqCst), 1);
    let count = manager.shared.mcp_broker.revoke_owner("host:mcp");
    manager
        .shared
        .mcp_users
        .fetch_sub(count, std::sync::atomic::Ordering::SeqCst);
    assert!(session.cancel.is_cancelled());
    assert!(http.responses.lock().unwrap().is_empty());
    tokio::time::timeout(Duration::from_secs(5), async {
        while http.slots.load(std::sync::atomic::Ordering::SeqCst) != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(
        session
            .validate(&manager.shared, session.host_generation, &session.owner)
            .is_err()
    );
    assert_eq!(
        peer.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|frame| frame["method"] == "initialize")
            .count(),
        1,
        "withdrawal cannot replay or reconnect"
    );
    pool.shutdown_all().await;
    manager.shutdown().await;
}
#[tokio::test(flavor = "current_thread")]
async fn fetch_proxy_unauthorized_is_safe_auth_recovery_without_operation_replay() {
    let _env = crate::test_support::lock_test_env();
    let _proxies: Vec<_> = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ]
    .into_iter()
    .map(crate::test_support::EnvVarGuard::remove)
    .collect();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("fetch_proxy_auth_recovery") else {
        return;
    };
    let fixture = FixturePlugins::new(&[]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let peer = peer(Mode::Json).await;
    let mut pool = McpPool::new(McpConfig {
        servers: [("fixture".into(), config(&peer).await)].into(),
        ..Default::default()
    })
    .with_backend(McpBackend::Host);
    let connection = pool.get_or_connect("fixture").await.unwrap();
    peer.unauthorized
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let error = connection
        .call_tool("echo", json!({}), 5)
        .await
        .unwrap_err();
    let safe = format!("{error:#}");
    assert!(safe.contains("401"), "{safe}");
    assert!(safe.contains("bearer token"), "{safe}");
    assert!(!safe.contains("untrusted provider error"));
    assert_eq!(
        peer.frames
            .lock()
            .unwrap()
            .iter()
            .filter(|frame| frame["method"] == "tools/call")
            .count(),
        1
    );
    pool.shutdown_all().await;
    manager.shutdown().await;
}

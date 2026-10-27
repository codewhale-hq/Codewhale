//! Selected Native proposals, the real existing pool and local HTTP/SSE peers.
use super::*;
use crate::extension_host::ExtensionHostManager;
use crate::extension_host::supervisor::HostRequestContext;
use serde_json::Value;
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

struct RemotePeer {
    base: String,
    requests: Arc<Mutex<Vec<Value>>>,
    cancel: CancellationToken,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for RemotePeer {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}
fn reply(request: &Value) -> Option<Value> {
    let id = request.get("id")?.clone();
    assert!(
        id.is_string(),
        "both backends must preserve Core's actual string ID"
    );
    let result = match request["method"].as_str().unwrap() {
        "initialize" => {
            json!({"protocolVersion":request["params"]["protocolVersion"],"capabilities":{"tools":{}},"serverInfo":{"name":"reviewed-local-peer","version":"1"}})
        }
        "tools/list" => {
            json!({"tools":[{"name":"remote_echo","description":"local read-only fixture","inputSchema":{"type":"object"},"annotations":{"readOnlyHint":true}}]})
        }
        "tools/call" => {
            json!({"content":[{"type":"text","text":format!("remote:{}",request["params"]["arguments"])}]})
        }
        "ping" => json!({}),
        "resources/list" => json!({"resources":[]}),
        "resources/templates/list" => json!({"resourceTemplates":[]}),
        "prompts/list" => json!({"prompts":[]}),
        "server/discover" => {
            return Some(
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
            );
        }
        other => panic!("unexpected local fixture method {other}"),
    };
    Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
}
async fn peer() -> RemotePeer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let server_cancel = cancel.clone();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&requests);
    let (events, _) = tokio::sync::broadcast::channel::<Value>(32);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = server_cancel.cancelled() => break,
                connection = listener.accept() => {
                    let (socket, _) = connection.unwrap();
                    let cancel = server_cancel.clone();
                    let events = events.clone();
                    let observed = Arc::clone(&observed);
                    connections.spawn(async move {
                        let mut reader = BufReader::new(socket);
                        let mut first = String::new();
                        if reader.read_line(&mut first).await.unwrap() == 0 { return; }
                        let mut length = 0;
                        let mut header_bytes = first.len();
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).await.unwrap() == 0 { return; }
                            header_bytes += line.len();
                            assert!(header_bytes <= 64 * 1024);
                            if line == "\r\n" { break; }
                            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") { length = value.trim().parse::<usize>().unwrap(); }
                        }
                        assert!(length <= 64 * 1024);
                        let mut body = vec![0; length];
                        reader.read_exact(&mut body).await.unwrap();
                        let mut socket = reader.into_inner();
                        if first.starts_with("GET /sse ") {
                            let mut incoming = events.subscribe();
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\nevent: endpoint\ndata: /messages\n\n").await.unwrap();
                            loop {
                                tokio::select! {
                                    biased;
                                    _ = cancel.cancelled() => break,
                                    event = incoming.recv() => {
                                        let Ok(event) = event else { break };
                                        let bytes = format!("event: message\ndata: {event}\n\n");
                                        if socket.write_all(bytes.as_bytes()).await.is_err() { break; }
                                    }
                                }
                            }
                        } else if first.starts_with("POST /mcp ") || first.starts_with("POST /messages ") {
                            let request: Value = serde_json::from_slice(&body).unwrap();
                            {
                                let mut requests = observed.lock().unwrap();
                                assert!(requests.len() < 256);
                                requests.push(request.clone());
                            }
                            let response = reply(&request);
                            if first.starts_with("POST /messages ") {
                                if let Some(response) = response { let _ = events.send(response); }
                                socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                            } else if let Some(response) = response {
                                let body = response.to_string();
                                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                            } else {
                                socket.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                            }
                        } else {
                            socket.write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                        }
                    });
                }
            }
        }
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    });
    RemotePeer {
        base,
        requests,
        cancel,
        task,
    }
}

async fn admit_remote(
    manager: &Arc<ExtensionHostManager>,
    caller: &HostAttachment,
    transport: &str,
    url: &str,
    name: &str,
) -> u64 {
    let current = for_plugins(&caller.plugin_view()).unwrap();
    let owner = current[0].3.registration.owner.clone();
    let scope = current[0].3.registration.scope.clone();
    let mut proposal = params(&owner, scope, name.into());
    proposal.spec.description = json!({"type":transport,"url":url}).to_string();
    let generation = manager.shared.plugin.host_generation.load(Ordering::SeqCst);
    let (cx, _violations, _cancel) = HostRequestContext::for_test(1);
    match admit(&manager.shared, HostTier::Plugin, generation, proposal, &cx).await {
        RegisterResult::Admitted { handle } => handle,
        RegisterResult::Refused { refused } => panic!("remote proposal refused: {refused}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn selected_native_literal_remote_http_and_sse_admit_and_execute_on_both_backends() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("selected_native_remote_http_and_sse") else {
        return;
    };
    let fixture = FixturePlugins::new(&["raw-dsh-mcp"]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let caller = manager.attach(selected(&fixture, "a"));
    caller.reconcile().await.unwrap();
    assert_reviewed_owner_active(&manager, &caller);
    // The wrapper has only a broad reviewed Native receipt; no duplicated
    // declarative endpoint capability inventory is invented for dynamic rows.
    assert!(
        fixture
            .registry()
            .get("raw-dsh-mcp")
            .unwrap()
            .inventory
            .network_hosts
            .is_empty()
    );
    let peer = peer().await;
    for (kind, suffix) in [("streamable-http", "mcp"), ("sse", "sse")] {
        let public_name = format!("remote_{suffix}");
        let handle = admit_remote(
            &manager,
            &caller,
            kind,
            &format!("{}/{suffix}", peer.base),
            &public_name,
        )
        .await;
        for backend in [McpBackend::Rust, McpBackend::Host] {
            let mut active = pool(&fixture, &caller, backend);
            let errors = active.connect_all().await;
            assert!(errors.is_empty(), "{kind} {backend:?}: {errors:?}");
            let name = active
                .all_tools()
                .into_iter()
                .find(|(_, tool)| tool.name == "remote_echo")
                .unwrap()
                .0;
            let result = active
                .call_tool(&name, json!({"backend":format!("{backend:?}")}))
                .await
                .unwrap();
            assert!(
                result["content"][0]["text"]
                    .as_str()
                    .unwrap()
                    .starts_with("remote:")
            );
            assert!(
                peer.requests
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|request| request["method"] == "tools/call" && request["id"].is_string())
            );
            active.shutdown_all().await;
        }
        let definition = for_plugins(&caller.plugin_view())
            .unwrap()
            .into_iter()
            .find(|row| row.0 == public_name)
            .unwrap()
            .3;
        manager
            .shared
            .registry
            .lock()
            .unwrap()
            .unregister(&definition.registration.owner, handle);
        assert!(definition.validate().is_err());
        assert!(definition.registration.cancel.is_cancelled());
    }
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_remote_network_deny_and_credential_refusal_prevent_requests() {
    let _home = crate::test_support::SealedHome::new();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("native_remote_deny_and_credentials") else {
        return;
    };
    let fixture = FixturePlugins::new(&["raw-dsh-mcp"]).await;
    let manager = fixture.manager(node);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let caller = manager.attach(selected(&fixture, "a"));
    caller.reconcile().await.unwrap();
    assert_reviewed_owner_active(&manager, &caller);
    let peer = peer().await;
    admit_remote(
        &manager,
        &caller,
        "streamable-http",
        &format!("{}/mcp", peer.base),
        "remote",
    )
    .await;
    for backend in [McpBackend::Rust, McpBackend::Host] {
        let policy: codewhale_net::network_policy::NetworkPolicy =
            serde_json::from_value(json!({"default":"deny","audit":false})).unwrap();
        let mut denied = pool(&fixture, &caller, backend).with_network_policy(
            codewhale_net::network_policy::NetworkPolicyDecider::new(policy, None),
        );
        assert!(!denied.connect_all().await.is_empty());
        denied.shutdown_all().await;
    }
    assert!(peer.requests.lock().unwrap().is_empty());
    let definition = for_plugins(&caller.plugin_view())
        .unwrap()
        .into_iter()
        .find(|row| row.0 == "remote")
        .unwrap()
        .3;
    for value in [
        json!({"type":"streamable-http","url":format!("{}/mcp",peer.base),"headers":{"Authorization":"literal-fixture-only"}}),
        json!({"type":"sse","url":format!("{}/sse?token=fixture",peer.base)}),
    ] {
        let mut proposal = params(
            &definition.registration.owner,
            definition.registration.scope.clone(),
            "refused".into(),
        );
        proposal.spec.description = value.to_string();
        let (cx, _violations, _cancel) = HostRequestContext::for_test(2);
        assert!(matches!(
            admit(
                &manager.shared,
                HostTier::Plugin,
                definition.registration.host_generation,
                proposal,
                &cx
            )
            .await,
            RegisterResult::Refused { .. }
        ));
    }
    assert!(peer.requests.lock().unwrap().is_empty());
    manager.shutdown().await;
}

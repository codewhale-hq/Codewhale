//! Real committed host bundle, SDK, Rust ticket redemption and contained
//! fixture child. The child is a local MCP peer, never a provider or real app.
use super::*;
use crate::extension_host::tests::{FixturePlugins, node_for_tests};
use crate::extension_host::{ExtensionHostOptions, TestManagerGuard};
use crate::mcp::{McpConnection, McpTimeouts, McpTransport};
use crate::plugins::activation::TestPolicyGuard;
use codewhale_config::McpBackend;
use std::sync::atomic::Ordering;

const PEER: &str = r#"
import readline from 'node:readline'; import fs from 'node:fs';
const record = process.argv[2]; const modern = process.argv[3] === "--modern";
const rl = readline.createInterface({input:process.stdin});
rl.on('line', line => {
  const m=JSON.parse(line); fs.appendFileSync(record, JSON.stringify({method:m.method,wire_id:m.id,params:m.params,ping_reply:m.id==='fixture-ping'&&m.result&&Object.keys(m.result).length===0})+'\n');
  if(m.method==='notifications/initialized') { process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:'fixture-ping',method:'ping',params:{}})+'\n'); return; }
  if (!m.method || !Object.hasOwn(m,'id')) return;
  if(typeof m.id !== 'string') throw new Error('SDK numeric ID escaped the Rust wire adapter');
  if(m.method==='server/discover') { process.stdout.write(JSON.stringify(modern ? {jsonrpc:'2.0',id:m.id,result:{resultType:'complete',ttlMs:0,cacheScope:'private',supportedVersions:['2026-07-28'],capabilities:{tools:{}}}} : {jsonrpc:'2.0',id:m.id,error:{code:-32601,message:'Method not found'}})+'\n'); return; }
  const result=m.method==='initialize'?{protocolVersion:'2025-06-18',serverInfo:{name:'broker-fixture',version:'1'},capabilities:{tools:{}}}
    :m.method==='tools/list'?{tools:[{name:'echo',inputSchema:{type:'object'}}]}
    :{content:[{type:'text',text:JSON.stringify(m.params.arguments)}],isError:false};
  process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,result:modern?{resultType:'complete',ttlMs:0,cacheScope:'private',...result}:result})+'\n');
});
"#;

#[cfg(test)]
async fn fixture(
    test: &str,
) -> Option<(
    FixturePlugins,
    Arc<ExtensionHostManager>,
    std::path::PathBuf,
    McpServerConfig,
)> {
    fixture_with_plugins(test, &[]).await
}
#[cfg(test)]
async fn fixture_with_plugins(
    test: &str,
    names: &[&str],
) -> Option<(
    FixturePlugins,
    Arc<ExtensionHostManager>,
    std::path::PathBuf,
    McpServerConfig,
)> {
    let node = node_for_tests(test)?;
    let fixture = FixturePlugins::new(names).await;
    std::fs::create_dir_all(&fixture.root).unwrap();
    let script = fixture.root.join("mcp-peer.mjs");
    let record = fixture.root.join("mcp-frames.jsonl");
    std::fs::write(&script, PEER).unwrap();
    std::fs::write(&record, "").unwrap();
    let config: McpServerConfig = serde_json::from_value(json!({
        "command": node.to_string_lossy(),
        "args": [script.to_string_lossy(), record.to_string_lossy()],
        "connect_timeout": 5,
    }))
    .unwrap();
    let manager = fixture.manager(node);
    Some((fixture, manager, record, config))
}
async fn transport(config: &McpServerConfig) -> SdkTransport {
    let mut transport = SdkTransport::connect(
        "fixture",
        config,
        CancellationToken::new(),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    transport.send(serde_json::to_vec(&json!({"jsonrpc":"2.0","id":"init","method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"codewhale-tui","version":env!("CARGO_PKG_VERSION")}}})).unwrap()).await.unwrap();
    let init: Value = serde_json::from_slice(&transport.recv().await.unwrap()).unwrap();
    assert_eq!(init["id"], "init");
    transport
}
fn request(
    transport: &SdkTransport,
    grant: &McpOperationGrant,
    params: Value,
    id: u64,
) -> HostRequest {
    let params = ProcWriteParams {
        owner: transport.session.owner.clone(),
        session_id: transport.session_id.clone(),
        ticket: Some(grant.ticket.clone()),
        operation_id: Some(grant.operation_id.clone()),
        frame: json!({"jsonrpc":"2.0","id":grant.wire_id,"method":grant.method,"params":params}),
    };
    let frame = json!({"jsonrpc":"2.0","id":id,"method":"proc/write","params":params});
    match parse_host_message(frame, HostTier::Builtin).unwrap() {
        HostMessage::Request { request, .. } => request,
        _ => panic!("decoded process write must be a request"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn host_sdk_real_connection_keeps_rust_catalog_and_exact_tool_call() {
    for modern in [false, true] {
        let _policy = TestPolicyGuard::extension_host(true);
        let Some((_fixture, manager, record, mut config)) =
            fixture("host_sdk_real_connection").await
        else {
            return;
        };
        if modern {
            config.args.push("--modern".to_string());
        }
        let _manager = TestManagerGuard::install(Arc::clone(&manager));
        let mut connection = McpConnection::connect_with_backend(
            "fixture".into(),
            config,
            &McpTimeouts::default(),
            None,
            McpBackend::Host,
        )
        .await
        .unwrap();
        assert!(connection.is_ready());
        assert_eq!(connection.tools().len(), 1);
        assert_eq!(connection.tools()[0].name, "echo");
        let result = connection
            .call_tool("echo", json!({"payload":"exact"}), 5)
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "{\"payload\":\"exact\"}");
        if !modern {
            tokio::time::timeout(Duration::from_secs(2), async {
                while !std::fs::read_to_string(&record)
                    .unwrap()
                    .contains("\"ping_reply\":true")
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
        }
        let frames = std::fs::read_to_string(record).unwrap();
        assert_eq!(
            frames
                .lines()
                .filter(|line| line.contains("\"method\":\"initialize\""))
                .count(),
            if modern { 0 } else { 1 }
        );
        assert_eq!(
            frames
                .lines()
                .filter(|line| line.contains("\"method\":\"tools/call\""))
                .count(),
            1
        );
        assert!(frames.contains("server/discover"));
        assert_eq!(frames.contains("\"ping_reply\":true"), !modern);
        let frames: Vec<Value> = frames
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        for (method, expected) in [
            ("initialize", "1"),
            ("tools/list", "2"),
            ("tools/call", "3"),
        ] {
            if modern && method == "initialize" {
                continue;
            }
            assert_eq!(
                frames
                    .iter()
                    .find(|frame| frame["method"] == method)
                    .unwrap()["wire_id"],
                expected,
                "the peer must see the original Rust facade ID"
            );
        }
        if modern {
            assert_eq!(
                frames
                    .iter()
                    .filter(|frame| frame["method"] == "server/discover")
                    .count(),
                1
            );
            assert!(
                !frames
                    .iter()
                    .any(|frame| frame["method"] == "notifications/initialized")
            );
            assert!(
                frames
                    .iter()
                    .all(|frame| frame["params"]["_meta"] == modern_metadata())
            );
            assert_eq!(result["resultType"], "complete");
        }
        manager.shutdown().await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn decoded_operation_mismatch_refuses_before_pipe_and_retires_session() {
    let _policy = TestPolicyGuard::extension_host(true);
    let Some((_fixture, manager, record, config)) = fixture("decoded_operation_mismatch").await
    else {
        return;
    };
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let transport = transport(&config).await;
    let grant = transport
        .grant(
            "tools/list",
            json!({"cursor":"admitted"}),
            Duration::from_secs(5),
            None,
            Some("raw-accepted-id"),
        )
        .unwrap();
    let (cx, _, _) = HostRequestContext::for_test(7);
    let result = manager
        .shared
        .mcp_broker
        .serve(
            &manager.shared,
            transport.session.host_generation,
            request(&transport, &grant, json!({"cursor":"rewritten"}), 7),
            cx,
        )
        .await;
    assert!(result.is_err());
    assert!(transport.session.cancel.is_cancelled());
    assert!(
        !std::fs::read_to_string(record)
            .unwrap()
            .contains("rewritten")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn exact_grant_replay_and_stale_owner_host_are_refused() {
    let _policy = TestPolicyGuard::extension_host(true);
    let Some((_fixture, manager, record, config)) = fixture("exact_grant_replay").await else {
        return;
    };
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let transport = transport(&config).await;
    let grant = transport
        .grant(
            "tools/list",
            json!({}),
            Duration::from_secs(5),
            None,
            Some("raw-accepted-id"),
        )
        .unwrap();
    let mut foreign = request(&transport, &grant, json!({}), 8);
    if let HostRequest::ProcWrite(params) = &mut foreign {
        params.owner.generation += 1;
    }
    let (cx, _, _) = HostRequestContext::for_test(8);
    assert!(
        manager
            .shared
            .mcp_broker
            .serve(
                &manager.shared,
                transport.session.host_generation,
                foreign,
                cx
            )
            .await
            .is_err()
    );
    let (cx, _, _) = HostRequestContext::for_test(9);
    assert!(
        manager
            .shared
            .mcp_broker
            .serve(
                &manager.shared,
                transport.session.host_generation + 1,
                request(&transport, &grant, json!({}), 9),
                cx
            )
            .await
            .is_err()
    );
    let (cx, _, _) = HostRequestContext::for_test(10);
    manager
        .shared
        .mcp_broker
        .serve(
            &manager.shared,
            transport.session.host_generation,
            request(&transport, &grant, json!({}), 10),
            cx,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if std::fs::read_to_string(&record)
                .unwrap()
                .contains("tools/list")
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let (cx, _, _) = HostRequestContext::for_test(11);
    assert!(
        manager
            .shared
            .mcp_broker
            .serve(
                &manager.shared,
                transport.session.host_generation,
                request(&transport, &grant, json!({}), 11),
                cx
            )
            .await
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(record)
            .unwrap()
            .lines()
            .filter(|line| line.contains("tools/list"))
            .count(),
        1
    );
    assert!(transport.session.cancel.is_cancelled());
}

#[tokio::test(flavor = "current_thread")]
async fn host_withdrawal_revokes_unwritten_grant_and_broker_lifetime() {
    let _policy = TestPolicyGuard::extension_host(true);
    let Some((_fixture, manager, record, config)) = fixture("host_withdrawal").await else {
        return;
    };
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let transport = transport(&config).await;
    let grant = transport
        .grant(
            "tools/list",
            json!({"cursor":"queued"}),
            Duration::from_secs(5),
            None,
            Some("raw-accepted-id"),
        )
        .unwrap();
    manager
        .shared
        .core_calls
        .revoke_host(HostTier::Builtin, transport.session.host_generation);
    manager.shared.mcp_users.fetch_sub(
        manager
            .shared
            .mcp_broker
            .revoke_host(HostTier::Builtin, transport.session.host_generation),
        std::sync::atomic::Ordering::SeqCst,
    );
    let (cx, _, _) = HostRequestContext::for_test(12);
    assert!(
        manager
            .shared
            .mcp_broker
            .serve(
                &manager.shared,
                transport.session.host_generation,
                request(&transport, &grant, json!({"cursor":"queued"}), 12),
                cx
            )
            .await
            .is_err()
    );
    assert!(transport.probe_dead());
    assert!(!std::fs::read_to_string(record).unwrap().contains("queued"));
}

#[tokio::test(flavor = "current_thread")]
async fn host_http_requires_rust_prepared_client_without_fallback_or_spawn() {
    let _policy = TestPolicyGuard::extension_host(true);
    let Some((_fixture, manager, _, _)) = fixture("host_backend_refuses_http").await else {
        return;
    };
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let config: McpServerConfig =
        serde_json::from_value(json!({"url": "https://example.invalid/mcp"})).unwrap();
    let error = SdkTransport::connect(
        "http",
        &config,
        CancellationToken::new(),
        Duration::from_secs(5),
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("Rust-prepared HTTP client"));
    assert!(
        manager
            .shared
            .mcp_broker
            .sessions
            .lock()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        manager
            .shared
            .mcp_users
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test(flavor = "current_thread")]
async fn computer_use_protocol_bootstrap_and_human_decision_stay_in_rust() {
    if !cfg!(target_os = "macos") {
        return;
    }
    let _env = crate::test_support::lock_test_env();
    let _policy = TestPolicyGuard::extension_host(true);
    let Some(node) = node_for_tests("real_computer_use_host_sdk") else {
        return;
    };
    let (root, _registry, pool, server) = crate::mcp::computer_use_test_fixture();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", root.path());
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let manager = Arc::new(ExtensionHostManager::new(
        super::super::ExtensionHostOptions {
            node_override: Some(node),
            // Keep the bundle under the Codewhale home's readable
            // extension-host entry; a nested `host` entry is sandbox-denied.
            root: Some(root.path().to_path_buf()),
            ..Default::default()
        },
    ));
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let mut pool = pool.with_backend(McpBackend::Host);
    pool.get_or_connect(&server).await.unwrap();
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
    assert!(session.decision_key.lock().unwrap().is_some());
    let input = json!({"action":"allow","scope":"foreground","remember":false});
    let tool = McpPool::mcp_model_tool_name(&server, "consent");
    let decode = |result: Value| {
        serde_json::from_str::<Value>(result["content"][0]["text"].as_str().unwrap()).unwrap()
    };
    let unsigned = decode(
        pool.call_tool_with_decision(&tool, input.clone(), None)
            .await
            .unwrap(),
    );
    assert_eq!(unsigned["error"]["code"], "consent_needs_user");
    let decision = HumanDecision::for_test(&tool, &input);
    let approved = decode(
        pool.call_tool_with_decision(&tool, input.clone(), Some(&decision))
            .await
            .unwrap(),
    );
    assert_eq!(approved["ok"], true);
    assert!(
        pool.call_tool_with_decision(
            &tool,
            json!({"action":"allow","scope":"foreground","remember":true}),
            Some(&decision)
        )
        .await
        .is_err()
    );
    // Ticket-visible params do not contain the attestation added at the pipe.
    let transport = SdkTransport {
        manager: Arc::clone(&manager),
        host: manager.shared.ready_host(HostTier::Builtin).unwrap(),
        session: Arc::clone(&session),
        session_id: session.session_id.clone(),
        replies: Default::default(),
        discovery_timeout: Duration::from_secs(5),
    };
    let grant = transport
        .grant(
            "tools/call",
            json!({"name":"consent","arguments":input}),
            Duration::from_secs(5),
            Some(&decision),
            Some("raw-decision-id"),
        )
        .unwrap();
    assert!(grant.params.get("_meta").is_none());
    pool.shutdown_all().await;
    manager.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn cancelled_or_aborted_partial_pipe_write_retires_contained_child() {
    let _policy = TestPolicyGuard::extension_host(true);
    for abort_handler in [false, true] {
        let Some((_fixture, manager, record, mut config)) = fixture("partial_pipe_write").await
        else {
            return;
        };
        let _manager = TestManagerGuard::install(Arc::clone(&manager));
        let paused = r#"
import fs from 'node:fs'; const record=process.argv[2]; let pending='';
function parse(chunk) { pending+=chunk; while(pending.includes('\n')) {
  const at=pending.indexOf('\n'), m=JSON.parse(pending.slice(0,at)); pending=pending.slice(at+1);
  if(m.method==='server/discover') process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,error:{code:-32601,message:'Method not found'}})+'\n');
  if(m.method==='initialize') process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:m.id,result:{protocolVersion:'2025-06-18',serverInfo:{name:'paused',version:'1'},capabilities:{tools:{}}}})+'\n');
  if(m.method==='notifications/initialized') {process.stdin.off('data',parse); process.stdin.once('data',()=>{fs.appendFileSync(record,'partial-byte-observed\n'); process.stdin.pause()}); return;}
}}
process.stdin.on('data',parse);
"#;
        let path = std::path::PathBuf::from(&config.args[0]);
        std::fs::write(path, paused).unwrap();
        config.connect_timeout = Some(5);
        let transport = transport(&config).await;
        let body = json!({"name":"echo","arguments":{"large":"x".repeat(2*1024*1024)}});
        let grant = transport
            .grant(
                "tools/call",
                body.clone(),
                Duration::from_secs(5),
                None,
                Some("raw-accepted-id"),
            )
            .unwrap();
        let request = request(&transport, &grant, body, 91);
        let (cx, _, cancel) = HostRequestContext::for_test(91);
        let task_manager = Arc::clone(&manager);
        let generation = transport.session.host_generation;
        let task = tokio::spawn(async move {
            task_manager
                .shared
                .mcp_broker
                .serve(&task_manager.shared, generation, request, cx)
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if std::fs::read_to_string(&record)
                    .unwrap()
                    .contains("partial-byte-observed")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        if abort_handler {
            task.abort();
        } else {
            cancel.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap();
        assert!(result.is_err() || result.unwrap().is_err());
        assert!(transport.session.cancel.is_cancelled());
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if transport.session.broker.lock().await.is_none() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(record)
                .unwrap()
                .lines()
                .filter(|line| *line == "partial-byte-observed")
                .count(),
            1
        );
    }
}

#[test]
fn production_process_surface_is_builtin_only_and_strict() {
    let owner = OwnerRef {
        plugin_id: "host:mcp".into(),
        generation: 1,
        owner_token: "token".into(),
    };
    let frame = json!({"jsonrpc":"2.0","id":1,"method":"proc/write","params":{"owner":owner,"session_id":"session","ticket":"ticket","operation_id":"operation","frame":{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}}});
    assert!(parse_host_message(frame.clone(), HostTier::Builtin).is_ok());
    assert!(parse_host_message(frame.clone(), HostTier::Plugin).is_err());
    let mut foreign = frame;
    foreign["params"]["argv"] = json!(["unreviewed"]);
    assert!(parse_host_message(foreign, HostTier::Builtin).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn host_restart_mints_fresh_owner_and_never_revives_old_grant() {
    let _policy = TestPolicyGuard::extension_host(true);
    let Some((_fixture, manager, _, config)) = fixture("host_restart_generation").await else {
        return;
    };
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let first = transport(&config).await;
    let grant = first
        .grant(
            "tools/list",
            json!({}),
            Duration::from_secs(5),
            None,
            Some("raw-accepted-id"),
        )
        .unwrap();
    // Hold the existing reconcile lock so an automatic scheduled restart
    // cannot race this test's explicit retry. Status alone is insufficient:
    // a dead Ready slot projects Restarting before the exit callback runs.
    let reconcile = manager.shared.sync_lock.lock().await;
    first.host.shutdown().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while matches!(
            &*manager
                .shared
                .tier_runtime(HostTier::Builtin)
                .host
                .lock()
                .expect("builtin slot lock"),
            super::super::HostSlot::Ready(_) | super::super::HostSlot::Unresponsive(_)
        ) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("builtin exit callback withdrew the actual slot before retry");
    assert!(first.session.cancel.is_cancelled());
    manager.retry();
    drop(reconcile);
    let second = transport(&config).await;
    assert_ne!(first.session.owner, second.session.owner);
    assert_ne!(
        first.session.host_generation,
        second.session.host_generation
    );
    let (cx, _, _) = HostRequestContext::for_test(51);
    assert!(
        manager
            .shared
            .mcp_broker
            .serve(
                &manager.shared,
                second.session.host_generation,
                request(&first, &grant, json!({}), 51),
                cx
            )
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn host_mcp_without_native_keeps_restart_policy_and_harness_closed() {
    let _review_policy = TestPolicyGuard::extension_host(true);
    let Some((fixture, manager, _, config)) =
        fixture_with_plugins("host_mcp_without_native_restart", &["dsh-workspace-deps"]).await
    else {
        return;
    };
    let plugins = fixture.registry();
    let _native_off = TestPolicyGuard::extension_host(false);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let attachment = manager.attach(plugins);
    attachment.sync().await.unwrap();
    assert_eq!(manager.spawn_attempts(), 0, "config/attachment are lazy");

    // Even a retained harness demand cannot admit a new harness with Native off.
    manager.shared.harness_users.store(1, Ordering::SeqCst);
    let first = transport(&config).await;
    assert_eq!(manager.tier_spawn_attempts(HostTier::Builtin), 1);
    assert_eq!(manager.tier_spawn_attempts(HostTier::Plugin), 0);
    let replay_policy = manager.shared.builtin.supervision.lock().unwrap().policy;
    assert!(!replay_policy, "MCP demand must not become Native policy");
    assert!(
        manager
            .shared
            .registry
            .lock()
            .unwrap()
            .owner("host:harness")
            .is_none()
    );
    assert!(manager.ensure_harness_builtin().await.is_err());

    // Use the real host exit/owner withdrawal before the same replay method
    // that schedule_restart invokes with its captured supervision policy.
    let reconcile = manager.shared.sync_lock.lock().await;
    first.host.shutdown().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        while matches!(
            &*manager.shared.builtin.host.lock().unwrap(),
            super::super::HostSlot::Ready(_) | super::super::HostSlot::Unresponsive(_)
        ) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("actual Builtin exit callback withdraws the slot");
    assert!(first.session.cancel.is_cancelled());
    manager.retry();
    drop(reconcile);
    manager.reconcile_with_policy(replay_policy).await.unwrap();
    assert_eq!(manager.tier_spawn_attempts(HostTier::Plugin), 0);
    assert!(
        !manager
            .shared
            .registry
            .lock()
            .unwrap()
            .owners()
            .any(|owner| owner.tier == HostTier::Plugin)
    );
    let second = transport(&config).await;
    assert_ne!(first.session.owner, second.session.owner);
    assert!(!manager.shared.builtin.supervision.lock().unwrap().policy);
    assert!(
        manager
            .shared
            .registry
            .lock()
            .unwrap()
            .owner("host:harness")
            .is_none()
    );
    manager.shared.harness_users.store(0, Ordering::SeqCst);
}

#[tokio::test(flavor = "current_thread")]
async fn selected_host_without_node_uses_runtime_diagnostic_not_native_gate() {
    assert_eq!(McpBackend::default(), McpBackend::Host);
    let mut selected = codewhale_runtime::config::Config::default();
    assert_eq!(selected.mcp_backend(), McpBackend::Host);
    selected.extension_host = Some(toml::from_str("").unwrap());
    assert_eq!(selected.mcp_backend(), McpBackend::Host);
    selected.extension_host = Some(toml::from_str(r#"mcp_backend = "rust""#).unwrap());
    assert_eq!(selected.mcp_backend(), McpBackend::Rust);
    let _policy = TestPolicyGuard::extension_host(false);
    let root = tempfile::tempdir().unwrap();
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        node_override: Some(root.path().join("missing-node")),
        root: Some(root.path().join("home")),
        ..Default::default()
    }));
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let config: McpServerConfig =
        serde_json::from_value(json!({"command":"missing-peer"})).unwrap();
    let mut pool = crate::mcp::McpPool::new(crate::mcp::McpConfig {
        servers: [("missing-runtime".to_string(), config)].into(),
        ..Default::default()
    });
    assert_eq!(pool.backend(), McpBackend::Host);
    let error = pool
        .get_or_connect("missing-runtime")
        .await
        .err()
        .expect("selected Host must fail without its runtime");
    let diagnostic = format!("{error:#}");
    assert!(
        diagnostic.contains("Node.js ^22.19 || >=24"),
        "{diagnostic}"
    );
    assert!(diagnostic.contains("[extension_host] node"), "{diagnostic}");
    assert!(!diagnostic.contains("requires features.extension_host"));
    assert_eq!(manager.tier_spawn_attempts(HostTier::Plugin), 0);
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
async fn explicit_rust_backend_without_native_never_starts_builtin_runtime() {
    let _setup_policy = TestPolicyGuard::extension_host(true);
    let Some((fixture, _, _, config)) = fixture("rust_backend_no_builtin").await else {
        return;
    };
    let manager = Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
        node_override: Some(fixture.root.join("missing-host-node")),
        root: Some(fixture.root.clone()),
        ..Default::default()
    }));
    let _native_off = TestPolicyGuard::extension_host(false);
    let _manager = TestManagerGuard::install(Arc::clone(&manager));
    let connection = McpConnection::connect_with_backend(
        "fixture".into(),
        config,
        &McpTimeouts::default(),
        None,
        McpBackend::Rust,
    )
    .await
    .unwrap();
    assert!(connection.is_ready());
    assert_eq!(connection.tools().len(), 1);
    assert_eq!(manager.spawn_attempts(), 0);
}

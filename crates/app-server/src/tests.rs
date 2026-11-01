use super::*;
use crate::bridge::*;
use crate::config_rpc::*;
use crate::http::*;
use crate::stdio::*;
use crate::turns::*;
use axum::body::{Body, to_bytes};
use axum::extract::{Path as AxumPath, Query};
use axum::extract::{Request, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use codewhale_protocol::{AppRequest, PromptRequest, PromptResponse};
use std::collections::HashMap;
use std::collections::VecDeque;
use std::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
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
        json!({"allowed_tools": [], "prompt":"must not dispatch"}),
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
    let response = process_app_request(&state, AppRequest::ConfigReload, AppTransport::Stdio).await;
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
    let response = process_app_request(&state, AppRequest::ConfigReload, AppTransport::Stdio).await;
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
        json!({"thread_id":"canonical-1","allowed_tools": [], "input":"next"}),
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
        json!({"thread_id":"never-created","allowed_tools": [], "input":"hello"}),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, THREAD_NOT_FOUND_CODE);
    let error = run_http_thread_message(
        &state,
        "never-created".into(),
        "hello".into(),
        Some(Vec::new()),
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
        json!({"thread_id":"client-1","allowed_tools": [], "input":"first"}),
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
        json!({"thread_id":"client-1","allowed_tools": [], "input":"second"}),
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
        json!({"thread_id":"client-1","allowed_tools": [], "input":"hello"}),
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
        json!({"thread_id":"client-1","allowed_tools": [], "input":"hello"}),
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
        json!({"thread_id":"client-1","allowed_tools": [], "input":"hello"}),
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
        serde_json::to_value(store.snapshot_legacy_thread_history("client-1").unwrap()).unwrap(),
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
        json!({"thread_id":"client-1","allowed_tools": [], "input":"retry"}),
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

    let response = process_app_request(&state, AppRequest::ConfigReload, AppTransport::Stdio).await;
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

    let response = process_app_request(&state, AppRequest::ConfigReload, AppTransport::Stdio).await;
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
    assert_eq!(first.config.provider, second.config.provider);
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

    let err = resolve_auth_token(&options).expect_err("non-loopback generated auth should fail");
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
async fn stdio_thread_goal_methods_refuse_execution_and_read_existing_goal() {
    let (state, _tmp, server) = thread_control::compatibility_fixture().await;
    let capabilities = dispatch_stdio_request(&state, "thread/capabilities", json!({}))
        .await
        .unwrap();
    assert_eq!(capabilities.result["goal_execution"], false);
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
    .unwrap_err();
    assert_eq!(
        set.data.unwrap()["error"],
        "interactive_execution_unsupported"
    );
    thread_control::handle(
        &state,
        serde_json::from_value(json!({"kind":"goal_set","thread_id":id,"objective":"Release 0.10.1","token_budget":59000})).unwrap(),
    )
    .await
    .unwrap();
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
    let (base_url, _notify, server) = spawn_uninterruptible_until_asked_runtime(owner_router).await;
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
              \"params\":{\"thread_id\":\"thr_a\",\"input\":\"go\",\"allowed_tools\":[]}}\n",
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
        Json(json!({"capabilities":{"turn_output_token_limit":f.supported.load(Ordering::SeqCst)}}))
    }
    async fn providers() -> Json<Value> {
        Json(
            json!({"current":"custom","current_provider_id":"fixture-route","providers":[{"id":"custom","model_provider_id":"fixture-route","default_model":"fixture-model"}]}),
        )
    }
    async fn models(
        axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
    ) -> Json<Value> {
        assert_eq!(
            query.get("model_provider_id").map(String::as_str),
            Some("fixture-route")
        );
        Json(
            json!({"provider":"custom","model_provider_id":"fixture-route","total":2,"models":[{"id":"fixture-model","output_token_limit":"supported"},{"id":"uncapped-transport","output_token_limit":"unsupported"}]}),
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
        json!({"allowed_tools": [], "prompt":"review","maxOutputTokens":1500}),
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
                json!({"allowed_tools": [], "prompt":"review","model":model,"maxOutputTokens":1500})
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
            json!({"allowed_tools": [], "prompt":"review","maxOutputTokens":1500}),
        ),
        (
            "thread/message",
            json!({"thread_id":"stdio-cap","allowed_tools": [], "input":"review","maxOutputTokens":1500}),
        ),
        (
            "thread/request",
            json!({"kind":"message","thread_id":"request-cap","allowed_tools": [], "input":"review","maxOutputTokens":1500}),
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
        Some(Vec::new()),
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
                json!({"allowed_tools": [], "prompt":"review","maxOutputTokens":invalid})
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
    static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

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

    async fn interrupt(AxumPath((thread_id, turn_id)): AxumPath<(String, String)>) -> Json<Value> {
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
        json!({ "allowed_tools": [], "prompt": "what is 2+2" }),
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

    let err = dispatch_stdio_request(
        &state,
        "prompt/run",
        json!({ "allowed_tools": [], "prompt": "hello" }),
    )
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
    let err = dispatch_stdio_request(
        &state,
        "prompt/request",
        json!({ "allowed_tools": [], "prompt": "   " }),
    )
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
        Some(Vec::new()),
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
        Some(Vec::new()),
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
    for (method, params) in [
        (
            "thread/goal/set",
            json!({"thread_id":"unknown","objective":"must not start"}),
        ),
        (
            "thread/request",
            json!({"kind":"goal_set","thread_id":"unknown","objective":"must not start"}),
        ),
    ] {
        let error = dispatch_stdio_request(&state, method, params)
            .await
            .unwrap_err();
        let (status, Json(body)) = http_error_from_jsonrpc(error);
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            body["error"]["data"]["error"],
            "interactive_execution_unsupported"
        );
        assert!(state.runtime_thread_map.lock().await.is_empty());
        assert!(state.runtime_bridge.lock().await.is_none());
    }
    for tools in [Value::Null, json!(["shell"])] {
        for (method, params) in [
            (
                "thread/message",
                json!({"thread_id":"unknown","input":"do work","allowed_tools":tools}),
            ),
            (
                "thread/request",
                json!({"kind":"message","thread_id":"unknown","input":"do work","allowed_tools":tools}),
            ),
            (
                "prompt/run",
                json!({"prompt":"do work","allowed_tools":tools}),
            ),
            (
                "prompt/request",
                json!({"prompt":"do work","allowed_tools":tools}),
            ),
        ] {
            let error = dispatch_stdio_request(&state, method, params)
                .await
                .unwrap_err();
            assert_eq!(error.code, -32602);
            assert_eq!(
                error.data.unwrap()["error"],
                "interactive_execution_unsupported"
            );
        }
        let error = run_http_thread_message(
            &state,
            "unknown".into(),
            "do work".into(),
            serde_json::from_value(tools).unwrap(),
            Vec::new(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.data.unwrap()["error"],
            "interactive_execution_unsupported"
        );
        assert!(state.runtime_thread_map.lock().await.is_empty());
        assert!(state.runtime_bridge.lock().await.is_none());
    }
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
            assert_eq!(
                requests[0],
                json!({"allowed_tools": [], "prompt":"look","images":images})
            );
        }
        server.abort();
    }
}

#[test]
fn runtime_image_daemon_all_input_families_preserve_images() {
    let image = json!({"mime":"image/png","dataBase64":"AQ=="});
    let thread: ThreadMessageParams = serde_json::from_value(
        json!({"thread_id":"thr_fixture","allowed_tools": [], "input":"look","images":[image.clone()]}),
    )
    .unwrap();
    let prompt: PromptRequest = serde_json::from_value(
        json!({"allowed_tools": [], "prompt":"look","images":[image.clone()]}),
    )
    .unwrap();
    let generic: ThreadRequest = serde_json::from_value(
        json!({"kind":"message","thread_id":"thr_fixture","allowed_tools": [], "input":"look","images":[image]}),
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

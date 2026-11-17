#![cfg(unix)]

use anyhow::Result;
use codewhale_app_server::{
    AppState, RuntimeOwnerEventFrame, RuntimeOwnerEventStream, RuntimeOwnerFrontend,
    RuntimeOwnerFrontendSelection, RuntimeOwnerRequest,
};
use codewhale_app_server_protocol::runtime::NativeRuntimeRequest;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::Notify;

type Future<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>;

struct Captured {
    workspace: PathBuf,
    admitted: Notify,
    release: tokio::sync::Semaphore,
    completed: AtomicUsize,
    calls: AtomicUsize,
}

impl RuntimeOwnerFrontend for Captured {
    fn execute(&self, request: RuntimeOwnerRequest) -> Future<'_, Value> {
        Box::pin(async move {
            let RuntimeOwnerRequest::Native { workspace, request } = request else {
                anyhow::bail!("ordinary control is not a native projection")
            };
            anyhow::ensure!(workspace == self.workspace, "wrong workspace");
            self.calls.fetch_add(1, Ordering::SeqCst);
            match request {
                NativeRuntimeRequest::StartTurn(params) => {
                    self.admitted.notify_one();
                    self.release.acquire().await?.forget();
                    self.completed.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"thread":{"id":params.thread_id},"turn":{"id":"turn-1"}}))
                }
                NativeRuntimeRequest::Subscribe(_) => Ok(Value::Null),
                NativeRuntimeRequest::Capabilities(_) => Ok(
                    json!({"native_runtime":codewhale_app_server::native_runtime::capabilities()}),
                ),
                _ => Ok(json!({"captured":true})),
            }
        })
    }
    fn events(&self, _thread_id: String, _since_seq: u64) -> Future<'_, RuntimeOwnerEventStream> {
        Box::pin(async {
            let stream: RuntimeOwnerEventStream = Box::pin(futures_util::stream::iter([
                RuntimeOwnerEventFrame {
                    event: "turn.started".into(),
                    data: json!({"turn_id":"turn-1"}),
                    sequence: Some(42),
                },
                RuntimeOwnerEventFrame {
                    event: "stream.end".into(),
                    data: json!({"reason":"owner_stopping","last_seq":42,"retryable":true}),
                    sequence: None,
                },
            ]));
            Ok(stream)
        })
    }
    fn validate_selection<'a>(
        &'a self,
        _selection: &'a RuntimeOwnerFrontendSelection,
    ) -> Future<'a, ()> {
        Box::pin(async { anyhow::bail!("not a scoped fixture") })
    }
    fn serve<'a>(
        &'a self,
        _selection: RuntimeOwnerFrontendSelection,
        _compatibility: AppState,
        _input: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>,
        _output: Box<dyn tokio::io::AsyncWrite + Send + Unpin + 'a>,
    ) -> Future<'a, ()> {
        Box::pin(async { anyhow::bail!("not another frontend") })
    }
}

struct Harness {
    _root: tempfile::TempDir,
    owner: codewhale_protocol::RuntimeOwnerReceipt,
    captured: Arc<Captured>,
    shutdown: codewhale_app_server::daemon_socket::DaemonShutdownHandle,
    server:
        tokio::task::JoinHandle<Result<(), codewhale_app_server::daemon_socket::DaemonSocketError>>,
}

impl Harness {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("cw-native-")
            .tempdir_in("/tmp")
            .unwrap();
        let canonical = root.path().canonicalize().unwrap();
        let config = canonical.join("config.toml");
        std::fs::write(&config, "").unwrap();
        let workspace = canonical.join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let owner = codewhale_protocol::RuntimeOwnerReceipt {
            version: 1,
            data_dir: canonical.join("runtime"),
            execution_scope: "native-fixture".into(),
            lease_generation: uuid::Uuid::new_v4().to_string(),
            pid: std::process::id(),
            process_start: codewhale_app_server::daemon_socket::capture_process_start(
                std::process::id(),
            )
            .await
            .unwrap(),
            principal: codewhale_config::private_directory::PrivateDirectory::current_user_id()
                .to_string(),
            socket_path: canonical.join("run/daemon.sock"),
            config_path: Some(config),
        };
        let captured = Arc::new(Captured {
            workspace,
            admitted: Notify::new(),
            release: tokio::sync::Semaphore::new(0),
            completed: AtomicUsize::new(0),
            calls: AtomicUsize::new(0),
        });
        let (daemon, _) = codewhale_app_server::bind_runtime_frontends(
            owner.config_path.clone(),
            None,
            owner.clone(),
            codewhale_app_server::RuntimeOwnerRouting {
                endpoint: "127.0.0.1:1".parse().unwrap(),
                workspace: Some(captured.workspace.clone()),
                workers: None,
                mobile: false,
                web: false,
                acp: true,
                acp_only: false,
            },
            Some(captured.clone()),
            None,
        )
        .await
        .unwrap();
        let daemon = daemon.unwrap();
        let shutdown = daemon.shutdown_handle();
        let server = tokio::spawn(daemon.serve());
        Self {
            _root: root,
            owner,
            captured,
            shutdown,
            server,
        }
    }
    async fn connect(&self) -> codewhale_app_server::daemon_client::OwnerClient {
        codewhale_app_server::daemon_client::connect_native(
            self.owner.clone(),
            self.captured.workspace.clone(),
        )
        .await
        .unwrap()
    }
    async fn stop(self) {
        self.shutdown.trigger();
        tokio::time::timeout(Duration::from_secs(5), self.server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

async fn receive(client: &mut codewhale_app_server::daemon_client::OwnerClient) -> Value {
    tokio::time::timeout(Duration::from_secs(5), client.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn native_attach_requires_exact_generation_workspace_and_explicit_capability() {
    let harness = Harness::new().await;
    let mut stale = harness.owner.clone();
    stale.lease_generation = "retired".into();
    assert!(
        codewhale_app_server::daemon_client::connect_native(
            stale,
            harness.captured.workspace.clone()
        )
        .await
        .is_err()
    );
    assert!(
        codewhale_app_server::daemon_client::connect_native(
            harness.owner.clone(),
            harness.owner.data_dir.clone()
        )
        .await
        .is_err()
    );
    let mut native = harness.connect().await;
    native
        .send(json!(1), "runtime/capabilities", json!({}))
        .await
        .unwrap();
    assert_eq!(
        receive(&mut native).await["result"]["native_runtime"]["capabilities"],
        json!(["native_runtime_v1"])
    );
    let before = harness.captured.calls.load(Ordering::SeqCst);
    for (id, method, params) in [
        (2, "config/set", json!({})),
        (3, "shutdown", json!({})),
        (
            4,
            "runtime/thread/create",
            json!({"body":{"operation_key":"k","auto_approve":true}}),
        ),
        (
            5,
            "runtime/turn/start",
            json!({"thread_id":"t","body":{"operation_key":"k","prompt":"hi","profile_constitution":{}}}),
        ),
    ] {
        native.send(json!(id), method, params).await.unwrap();
        assert_eq!(
            receive(&mut native).await["error"]["data"]["outcome"],
            "not_started"
        );
    }
    assert_eq!(harness.captured.calls.load(Ordering::SeqCst), before);
    let mut legacy = codewhale_app_server::daemon_client::connect(
        harness.owner.config_path.clone(),
        Some(harness.owner.socket_path.clone()),
    )
    .await
    .unwrap();
    legacy
        .send(json!(9), "runtime/capabilities", json!({}))
        .await
        .unwrap();
    assert!(receive(&mut legacy).await.get("error").is_some());
    drop(native);
    drop(legacy);
    harness.stop().await;
}

#[tokio::test]
async fn native_subscription_ack_precedes_original_frames_and_null_sequence_end() {
    let harness = Harness::new().await;
    let mut client = harness.connect().await;
    client
        .send(
            json!(1),
            "runtime/events/subscribe",
            json!({"thread_id":"t","since_seq":41}),
        )
        .await
        .unwrap();
    let ack = receive(&mut client).await;
    let subscription = ack["result"]["subscription_id"].clone();
    assert_eq!(ack["id"], 1);
    let event = receive(&mut client).await;
    assert_eq!(event["params"]["subscription_id"], subscription);
    assert_eq!(event["params"]["frame"]["sequence"], 42);
    let end = receive(&mut client).await;
    assert_eq!(end["params"]["frame"]["event"], "stream.end");
    assert!(end["params"]["frame"]["sequence"].is_null());
    for id in [2, 3] {
        client
            .send(
                json!(id),
                "runtime/events/unsubscribe",
                json!({"subscription_id":subscription}),
            )
            .await
            .unwrap();
        let response = receive(&mut client).await;
        assert_eq!(response["id"], id);
    }
    drop(client);
    harness.stop().await;
}

#[tokio::test]
async fn native_disconnect_does_not_cancel_or_replay_admitted_mutation() {
    let harness = Harness::new().await;
    let mut client = harness.connect().await;
    client.send(json!(1),"runtime/turn/start",json!({"thread_id":"t","body":{"operation_key":"intent-1","prompt":"hi","allowed_tools":[]}})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), harness.captured.admitted.notified())
        .await
        .unwrap();
    drop(client);
    harness.captured.release.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.captured.completed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let fresh = harness.connect().await;
    drop(fresh);
    assert_eq!(harness.captured.completed.load(Ordering::SeqCst), 1);
    harness.stop().await;
}

#[tokio::test]
async fn native_absent_owner_prelude_never_creates_selected_store() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().canonicalize().unwrap();
    let data_dir = workspace.join("absent-store");
    let request = json!({"jsonrpc":"2.0","id":"attach-1","method":"runtime/attach","params":{"contract_version":1,"target_id":"target-1","selection_revision":1,"workspace":workspace,"expect_owner":{"version":1,"data_dir":data_dir,"execution_scope":"absent","lease_generation":"absent","pid":1,"process_start":"absent","principal":"absent","socket_path":workspace.join("absent/daemon.sock"),"config_path":null}}});
    let input = format!("{request}\n");
    assert!(
        codewhale_app_server::native_runtime::attach_io(
            false,
            tokio::io::BufReader::new(input.as_bytes()),
            tokio::io::sink()
        )
        .await
        .is_err()
    );
    assert!(!data_dir.exists());
    assert!(!workspace.join("absent").exists());
    let oversized = format!("{}\n", " ".repeat(16385));
    assert!(
        codewhale_app_server::native_runtime::attach_io(
            false,
            tokio::io::BufReader::new(oversized.as_bytes()),
            tokio::io::sink()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn native_sixty_fifth_request_is_refused_before_owner_execution() {
    let harness = Harness::new().await;
    let mut client = harness.connect().await;
    for id in 1..=65 {
        client.send(json!(id),"runtime/turn/start",json!({"thread_id":"t","body":{"operation_key":format!("intent-{id}"),"prompt":"hi","allowed_tools":[]}})).await.unwrap();
    }
    let busy = receive(&mut client).await;
    assert_eq!(busy["id"], 65);
    assert_eq!(busy["error"]["data"]["code"], "busy");
    assert_eq!(busy["error"]["data"]["outcome"], "not_started");
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.captured.calls.load(Ordering::SeqCst) < 65 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(harness.captured.calls.load(Ordering::SeqCst), 65);
    drop(client);
    harness.captured.release.add_permits(64);
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.captured.completed.load(Ordering::SeqCst) < 64 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(harness.captured.completed.load(Ordering::SeqCst), 64);
    harness.stop().await;
}

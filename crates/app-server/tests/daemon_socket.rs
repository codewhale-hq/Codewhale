//! Desktop Phase 0 acceptance for the daemon socket: spawn the daemon, connect
//! over the unix socket, complete the attach/claim handshake, round-trip
//! requests through the same JSON-RPC dispatcher the stdio transport uses,
//! and shut down cleanly (socket file removed, listener gone).

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use codewhale_app_server::daemon_socket::{
    DaemonSocketError, DaemonSocketOptions, bind_daemon_socket,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::task::JoinHandle;

static NONCE: AtomicU64 = AtomicU64::new(0);

/// A short, unique socket path: unix socket paths are capped near 100 bytes,
/// so `std::env::temp_dir()` (deep under `/var/folders` on macOS) is too long.
/// `/tmp` is the same choice the hooks crate's socket test makes.
fn short_socket_root(label: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis()
        % 1_000_000;
    let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let root = PathBuf::from("/tmp")
        .canonicalize()
        .expect("selected temporary parent")
        .join(format!("cw-ds-{label}-{pid}-{nonce}-{millis}"));
    assert!(
        root.as_os_str().len() < 60,
        "socket root too long for a unix socket test: {}",
        root.display()
    );
    root
}

struct Harness {
    root: PathBuf,
    socket_path: PathBuf,
    _config_dir: tempfile::TempDir,
}

impl Harness {
    fn new(label: &str) -> Self {
        let root = short_socket_root(label);
        let config_dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(config_dir.path().join("config.toml"), "").expect("config");
        Self {
            socket_path: root.join("run").join("daemon.sock"),
            root,
            _config_dir: config_dir,
        }
    }

    fn options(&self) -> DaemonSocketOptions {
        DaemonSocketOptions {
            socket_path: Some(self.socket_path.clone()),
            config_path: Some(self._config_dir.path().join("config.toml")),
        }
    }

    /// Bind and serve on a background task; returns the serve join handle.
    async fn spawn_daemon(&self) -> JoinHandle<Result<(), DaemonSocketError>> {
        let daemon = bind_daemon_socket(self.options())
            .await
            .expect("bind daemon socket");
        assert_eq!(daemon.local_path(), self.socket_path.as_path());
        tokio::spawn(daemon.serve())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct OwnedBootstrapChild(std::process::Child);
impl Drop for OwnedBootstrapChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

/// Transport-only publication fixture; actual store-lease proof lives in TUI.
struct BootstrapPublicationFixture {
    harness: Harness,
    parent: codewhale_config::private_directory::PrivateDirectory,
    owner: codewhale_protocol::RuntimeOwnerReceipt,
    bytes: Vec<u8>,
}
impl BootstrapPublicationFixture {
    async fn new(label: &str, dead: bool, reused: bool) -> Self {
        use codewhale_config::private_directory::PrivateDirectory;
        let harness = Harness::new(label);
        let parent = PrivateDirectory::admit(harness.socket_path.parent().unwrap()).unwrap();
        let socket = std::os::unix::net::UnixListener::bind(&harness.socket_path).unwrap();
        let identity = parent.socket_identity("daemon.sock").unwrap().unwrap();
        parent.protect_socket("daemon.sock", identity).unwrap();
        drop(socket);
        let (pid, mut process_start) = if dead {
            let mut child = OwnedBootstrapChild(
                std::process::Command::new("/bin/sleep")
                    .arg("60")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let pid = child.0.id();
            let start = codewhale_app_server::daemon_socket::capture_process_start(pid)
                .await
                .unwrap();
            child.0.kill().unwrap();
            child.0.wait().unwrap();
            (pid, start)
        } else {
            (
                std::process::id(),
                codewhale_app_server::daemon_socket::capture_process_start(std::process::id())
                    .await
                    .unwrap(),
            )
        };
        if reused {
            process_start.push_str("-different-generation");
        }
        let owner = codewhale_protocol::RuntimeOwnerReceipt {
            version: 1,
            data_dir: harness.root.join("runtime"),
            execution_scope: "bootstrap-transport-store".into(),
            lease_generation: "bootstrap-dead-generation".into(),
            pid,
            process_start,
            principal: PrivateDirectory::current_user_id().to_string(),
            socket_path: harness.socket_path.clone(),
            config_path: harness.options().config_path,
        };
        let bytes = serde_json::to_vec(&owner).unwrap();
        parent
            .write_owned_file("daemon.sock.owner.json", &bytes, false)
            .unwrap();
        Self {
            harness,
            parent,
            owner,
            bytes,
        }
    }

    async fn capture(
        &self,
    ) -> std::sync::Arc<codewhale_app_server::daemon_client::UnavailablePublication> {
        match codewhale_app_server::daemon_client::probe_owner_for_host_startup(
            self.owner.config_path.clone(),
            Some(self.owner.socket_path.clone()),
        )
        .await
        .unwrap()
        {
            codewhale_app_server::daemon_client::HostOwnerProbe::Unavailable(publication) => {
                publication
            }
            _ => panic!("refused private endpoint must be captured before bootstrap"),
        }
    }

    async fn bind_replacement(
        &self,
        recovery: Option<
            std::sync::Arc<codewhale_app_server::daemon_client::UnavailablePublication>,
        >,
    ) -> anyhow::Result<codewhale_app_server::daemon_socket::DaemonSocket> {
        let mut owner = self.owner.clone();
        owner.pid = std::process::id();
        owner.process_start =
            codewhale_app_server::daemon_socket::capture_process_start(owner.pid).await?;
        owner.lease_generation = "bootstrap-new-generation".into();
        let (daemon, _) = codewhale_app_server::bind_runtime_frontends(
            owner.config_path.clone(),
            None,
            owner,
            codewhale_app_server::RuntimeOwnerRouting {
                endpoint: "127.0.0.1:1".parse().unwrap(),
                workspace: None,
                workers: None,
                mobile: false,
                web: false,
                acp: false,
                acp_only: false,
            },
            None,
            recovery,
        )
        .await?;
        Ok(daemon.expect("a private endpoint directory is available on this host"))
    }

    fn assert_publication(&self, bytes: &[u8]) {
        assert_eq!(
            self.parent
                .read_private_receipt("daemon.sock.owner.json", 16384)
                .unwrap()
                .unwrap()
                .0,
            bytes
        );
    }
}

#[tokio::test]
async fn owner_bootstrap_unavailable_is_host_only_and_read_only() {
    let fixture = BootstrapPublicationFixture::new("boot-read", true, false).await;
    let identity = fixture.parent.socket_identity("daemon.sock").unwrap();
    let captured = fixture.capture().await;
    assert_eq!(captured.receipt(), &fixture.owner);
    assert!(captured.revalidate().await.unwrap());
    assert!(
        codewhale_app_server::daemon_client::connect_if_published(
            fixture.owner.config_path.clone(),
            Some(fixture.owner.socket_path.clone()),
        )
        .await
        .is_err()
    );
    assert!(
        codewhale_app_server::daemon_client::connect(
            fixture.owner.config_path.clone(),
            Some(fixture.owner.socket_path.clone()),
        )
        .await
        .is_err()
    );
    fixture.assert_publication(&fixture.bytes);
    assert_eq!(
        fixture.parent.socket_identity("daemon.sock").unwrap(),
        identity
    );
    assert!(
        !fixture.owner.data_dir.exists(),
        "standalone control must not open or mint a store"
    );
}

#[tokio::test]
async fn owner_bootstrap_live_process_and_reused_pid_refuse_retirement() {
    for reused in [false, true] {
        let fixture = BootstrapPublicationFixture::new("boot-live", false, reused).await;
        let captured = fixture.capture().await;
        let error = captured.revalidate().await.err().unwrap().to_string();
        assert!(
            error.contains(if reused { "reused" } else { "still present" }),
            "{error}"
        );
        assert!(fixture.bind_replacement(Some(captured)).await.is_err());
        fixture.assert_publication(&fixture.bytes);
        assert!(
            fixture
                .parent
                .socket_identity("daemon.sock")
                .unwrap()
                .is_some()
        );
    }
}

#[tokio::test]
async fn owner_bootstrap_equal_bytes_do_not_authorize_replaced_receipt() {
    let fixture = BootstrapPublicationFixture::new("boot-file", true, false).await;
    let captured = fixture.capture().await;
    let identity = fixture.parent.socket_identity("daemon.sock").unwrap();
    std::fs::rename(
        fixture
            .harness
            .socket_path
            .with_file_name("daemon.sock.owner.json"),
        fixture.harness.socket_path.with_file_name("old.owner.json"),
    )
    .unwrap();
    fixture
        .parent
        .write_owned_file("daemon.sock.owner.json", &fixture.bytes, false)
        .unwrap();
    assert!(!captured.revalidate().await.unwrap());
    assert!(fixture.bind_replacement(Some(captured)).await.is_err());
    fixture.assert_publication(&fixture.bytes);
    assert_eq!(
        fixture.parent.socket_identity("daemon.sock").unwrap(),
        identity
    );
}

#[tokio::test]
async fn owner_bootstrap_same_inode_content_change_is_preserved() {
    let fixture = BootstrapPublicationFixture::new("boot-edit", true, false).await;
    let captured = fixture.capture().await;
    let mut edited = fixture.bytes.clone();
    edited.push(b'\n');
    std::fs::write(
        fixture
            .harness
            .socket_path
            .with_file_name("daemon.sock.owner.json"),
        &edited,
    )
    .unwrap();
    assert!(!captured.revalidate().await.unwrap());
    assert!(fixture.bind_replacement(Some(captured)).await.is_err());
    fixture.assert_publication(&edited);
}

#[tokio::test]
async fn owner_bootstrap_replaced_socket_is_preserved() {
    let fixture = BootstrapPublicationFixture::new("boot-sock", true, false).await;
    let captured = fixture.capture().await;
    std::fs::rename(
        &fixture.harness.socket_path,
        fixture.harness.socket_path.with_file_name("old.sock"),
    )
    .unwrap();
    let replacement = std::os::unix::net::UnixListener::bind(&fixture.harness.socket_path).unwrap();
    let identity = fixture.parent.socket_identity("daemon.sock").unwrap();
    drop(replacement);
    assert!(!captured.revalidate().await.unwrap());
    assert!(fixture.bind_replacement(Some(captured)).await.is_err());
    assert_eq!(
        fixture.parent.socket_identity("daemon.sock").unwrap(),
        identity
    );
    fixture.assert_publication(&fixture.bytes);
}

#[tokio::test]
async fn owner_bootstrap_fresh_bind_cannot_retire_uncaptured_publication() {
    let fixture = BootstrapPublicationFixture::new("boot-fresh", true, false).await;
    let identity = fixture.parent.socket_identity("daemon.sock").unwrap();
    assert!(fixture.bind_replacement(None).await.is_err());
    fixture.assert_publication(&fixture.bytes);
    assert_eq!(
        fixture.parent.socket_identity("daemon.sock").unwrap(),
        identity
    );
}

struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    async fn connect(path: &Path) -> Self {
        let stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(path))
            .await
            .expect("connect timeout")
            .expect("connect");
        let (rx, writer) = stream.into_split();
        Self {
            reader: BufReader::new(rx),
            writer,
        }
    }

    async fn call(&mut self, id: u64, method: &str, params: Value) -> Value {
        let line = serde_json::to_string(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .expect("encode");
        self.writer
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write");
        let mut response = String::new();
        let read = tokio::time::timeout(
            Duration::from_secs(10),
            self.reader.read_line(&mut response),
        )
        .await
        .expect("response timeout")
        .expect("read");
        assert!(
            read > 0,
            "daemon closed the connection before answering `{method}`"
        );
        let value: Value = serde_json::from_str(&response).expect("json response");
        assert_eq!(value["id"], json!(id), "response id mismatch: {value}");
        value
    }

    async fn attach(&mut self, id: u64, name: &str, mode: &str) -> Value {
        self.call(
            id,
            "daemon/attach",
            json!({ "client": { "name": name, "version": "0.0.0-test", "pid": std::process::id() }, "mode": mode }),
        )
        .await
    }

    /// Read until EOF; proves the daemon closed the socket.
    async fn wait_for_close(mut self) {
        let mut sink = String::new();
        let read = tokio::time::timeout(Duration::from_secs(10), self.reader.read_line(&mut sink))
            .await
            .expect("close timeout")
            .expect("read");
        assert_eq!(read, 0, "expected EOF, got: {sink}");
    }
}

async fn wait_for_socket_removed(path: &Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while path.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("socket file must be removed on shutdown");
}

#[tokio::test]
async fn owner_attaches_round_trips_and_shuts_down_cleanly() {
    let harness = Harness::new("owner");
    let server = harness.spawn_daemon().await;

    let socket_mode = std::fs::metadata(&harness.socket_path)
        .expect("socket metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(socket_mode, 0o600, "socket must be private to the user");
    let dir_mode = std::fs::metadata(harness.socket_path.parent().expect("parent"))
        .expect("dir metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "runtime dir must be private to the user");

    let mut client = Client::connect(&harness.socket_path).await;

    // Anything but healthz before attaching is refused with a typed error:
    // a read-only probe, a thread/* read, and a prompt run alike.
    for (id, method, params) in [
        (1, "capabilities", json!({})),
        (10, "thread/list", json!({})),
        (
            11,
            "prompt/run",
            json!({ "prompt": "hi", "allowed_tools": [] }),
        ),
    ] {
        let early = client.call(id, method, params).await;
        assert_eq!(early["error"]["code"], json!(-32010), "{method}: {early}");
        assert_eq!(early["error"]["data"]["error"], json!("attach_required"));
        assert_eq!(early["error"]["data"]["method"], json!(method));
    }

    // healthz is allowed pre-attach so a shell can probe liveness first.
    let health = client.call(2, "healthz", json!({})).await;
    assert_eq!(health["result"]["status"], json!("ok"), "{health}");
    assert_eq!(health["result"]["transport"], json!("unix-socket"));

    let attached = client.attach(3, "codewhale-desktop", "claim").await;
    assert_eq!(attached["result"]["attached"], json!(true), "{attached}");
    assert_eq!(attached["result"]["role"], json!("owner"));
    assert_eq!(attached["result"]["transport"], json!("unix-socket"));
    assert_eq!(
        attached["result"]["daemon"]["pid"],
        json!(std::process::id())
    );
    assert_eq!(
        attached["result"]["daemon"]["version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(
        attached["result"]["owner"]["name"],
        json!("codewhale-desktop")
    );
    assert_eq!(attached["result"]["connections"], json!(1));

    // Post-attach, the socket transport advertises its own handshake next to
    // the stdio method set.
    let advertised = client.call(8, "capabilities", json!({})).await;
    let methods = advertised["result"]["methods"]
        .as_array()
        .expect("methods array");
    assert_eq!(methods[0], json!("healthz"), "{advertised}");
    assert_eq!(methods[1], json!("daemon/attach"), "{advertised}");
    assert!(methods.contains(&json!("shutdown")));

    // Round-trip JSON-RPC requests through the shared dispatcher: `app/*`
    // methods in, their JSON results out — byte-for-byte the shapes the
    // stdio transport emits. (No protocol-crate Op/EventMsg envelope is on
    // this wire; the framing is the stdio transport's newline-delimited
    // JSON-RPC.)
    let caps = client.call(4, "app/capabilities", json!({})).await;
    assert_eq!(caps["result"]["ok"], json!(true), "{caps}");
    assert!(caps["result"]["data"]["routes"].is_array());
    let config = client
        .call(5, "app/config/get", json!({ "key": "model" }))
        .await;
    assert_eq!(config["result"]["ok"], json!(true), "{config}");
    assert_eq!(config["result"]["data"]["key"], json!("model"));

    // A second attach on an attached connection is a typed refusal, not
    // method_not_found.
    let again = client.attach(6, "codewhale-desktop", "attach").await;
    assert_eq!(again["error"]["code"], json!(-32014), "{again}");

    let stopped = client.call(7, "shutdown", json!({})).await;
    assert_eq!(stopped["result"]["status"], json!("stopped"), "{stopped}");

    let outcome = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("daemon must exit after the owner's shutdown")
        .expect("join");
    outcome.expect("serve result");
    wait_for_socket_removed(&harness.socket_path).await;
    client.wait_for_close().await;
}

#[tokio::test]
async fn guests_share_the_daemon_but_cannot_stop_it() {
    let harness = Harness::new("guest");
    let server = harness.spawn_daemon().await;

    let mut owner = Client::connect(&harness.socket_path).await;
    let claimed = owner.attach(1, "desktop-window-1", "claim").await;
    assert_eq!(claimed["result"]["role"], json!("owner"), "{claimed}");

    let mut guest = Client::connect(&harness.socket_path).await;
    let lost = guest.attach(1, "desktop-window-2", "claim").await;
    assert_eq!(lost["error"]["code"], json!(-32011), "{lost}");
    assert_eq!(
        lost["error"]["data"]["owner"]["name"],
        json!("desktop-window-1")
    );

    let attached = guest.attach(2, "desktop-window-2", "attach").await;
    assert_eq!(attached["result"]["role"], json!("attached"), "{attached}");
    assert_eq!(
        attached["result"]["owner"]["name"],
        json!("desktop-window-1")
    );
    assert_eq!(attached["result"]["connections"], json!(2));

    let health = guest.call(3, "healthz", json!({})).await;
    assert_eq!(health["result"]["status"], json!("ok"));

    let refused = guest.call(4, "shutdown", json!({})).await;
    assert_eq!(refused["error"]["code"], json!(-32012), "{refused}");
    assert_eq!(refused["error"]["data"]["error"], json!("not_daemon_owner"));
    assert!(
        !server.is_finished(),
        "a guest's shutdown must not stop the daemon"
    );
    assert!(harness.socket_path.exists());

    // Once the owner leaves, the slot frees and a relaunched shell can claim.
    drop(owner);
    let mut relaunched = Client::connect(&harness.socket_path).await;
    let reclaimed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let response = relaunched.attach(1, "desktop-relaunch", "claim").await;
            if response.get("result").is_some() {
                return response;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("owner slot must free when the owner disconnects");
    assert_eq!(reclaimed["result"]["role"], json!("owner"), "{reclaimed}");

    // The guest is still attached and served while the new owner is in.
    let health = guest.call(5, "healthz", json!({})).await;
    assert_eq!(health["result"]["status"], json!("ok"));

    let stopped = relaunched.call(2, "shutdown", json!({})).await;
    assert_eq!(stopped["result"]["status"], json!("stopped"));
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("daemon exits")
        .expect("join")
        .expect("serve result");
    wait_for_socket_removed(&harness.socket_path).await;
    // The owner's shutdown closes every other connection, not just its own.
    guest.wait_for_close().await;
    relaunched.wait_for_close().await;
}

#[tokio::test]
async fn version_skew_is_refused_at_attach() {
    let harness = Harness::new("skew");
    let server = harness.spawn_daemon().await;
    let mut client = Client::connect(&harness.socket_path).await;
    let refused = client
        .call(
            1,
            "daemon/attach",
            json!({ "client": { "name": "old-desktop" }, "expect_daemon_version": "0.0.1-other" }),
        )
        .await;
    assert_eq!(refused["error"]["code"], json!(-32013), "{refused}");
    assert_eq!(
        refused["error"]["data"]["actual"],
        json!(env!("CARGO_PKG_VERSION"))
    );

    let daemon = bind_daemon_socket(harness.options()).await;
    // Meanwhile the original daemon is live, so a second bind must refuse.
    match daemon {
        Err(DaemonSocketError::AlreadyRunning { path }) => {
            assert_eq!(path, harness.socket_path);
        }
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("second daemon must not replace a live socket"),
    }
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn stale_socket_is_cleaned_up_and_foreign_files_are_refused() {
    let harness = Harness::new("stale");
    std::fs::create_dir_all(harness.socket_path.parent().expect("parent")).expect("mkdir");
    std::fs::set_permissions(
        harness.socket_path.parent().unwrap(),
        std::fs::Permissions::from_mode(0o700),
    )
    .expect("private stale fixture parent");

    // A socket file whose listener is gone: bind must reclaim it.
    {
        let dead = tokio::net::UnixListener::bind(&harness.socket_path).expect("bind dead");
        drop(dead);
    }
    assert!(
        harness.socket_path.exists(),
        "dropping a listener leaves the file"
    );
    let daemon = bind_daemon_socket(harness.options())
        .await
        .expect("stale socket must be reclaimed");
    let handle = daemon.shutdown_handle();
    let server = tokio::spawn(daemon.serve());
    let mut client = Client::connect(&harness.socket_path).await;
    let health = client.call(1, "healthz", json!({})).await;
    assert_eq!(health["result"]["status"], json!("ok"));
    handle.trigger();
    tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("daemon exits on handle")
        .expect("join")
        .expect("serve result");
    wait_for_socket_removed(&harness.socket_path).await;

    // A regular file at the path is never deleted.
    std::fs::write(&harness.socket_path, b"not a socket").expect("write file");
    match bind_daemon_socket(harness.options()).await {
        Err(DaemonSocketError::NotASocket { path }) => assert_eq!(path, harness.socket_path),
        Err(other) => panic!("unexpected error: {other}"),
        Ok(_) => panic!("must refuse to replace a non-socket"),
    }
    assert_eq!(
        std::fs::read(&harness.socket_path).expect("file intact"),
        b"not a socket"
    );
}

#[tokio::test]
async fn binding_refuses_public_parent_and_drop_before_serve_retires_exact_socket() {
    let harness = Harness::new("private");
    let parent = harness.socket_path.parent().unwrap();
    std::fs::create_dir_all(parent).unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(bind_daemon_socket(harness.options()).await.is_err());
    assert_eq!(
        std::fs::metadata(parent).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(!harness.socket_path.exists());
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let daemon = bind_daemon_socket(harness.options()).await.unwrap();
    assert!(harness.socket_path.exists());
    drop(daemon);
    tokio::time::timeout(Duration::from_secs(5), async {
        while harness.socket_path.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("cancelled binder retirement finishes off-runtime");
    assert!(!harness.socket_path.exists());
}

#[tokio::test]
async fn dropping_bound_daemon_preserves_a_replacement_at_the_selected_path() {
    let harness = Harness::new("replaced");
    let daemon = bind_daemon_socket(harness.options()).await.unwrap();
    let captured = harness.socket_path.with_extension("captured");
    std::fs::rename(&harness.socket_path, &captured).unwrap();
    std::fs::write(&harness.socket_path, b"operator replacement").unwrap();
    drop(daemon);
    assert_eq!(
        std::fs::read(&harness.socket_path).unwrap(),
        b"operator replacement"
    );
    assert!(captured.exists());
}

#[tokio::test]
async fn captured_owner_frontend_authenticates_generation_without_publishing_bearer() {
    let harness = Harness::new("owner");
    let owner = codewhale_protocol::RuntimeOwnerReceipt {
        version: 1,
        data_dir: harness.root.join("runtime"),
        execution_scope: "captured-test-store".into(),
        lease_generation: "captured-test-generation".into(),
        pid: std::process::id(),
        process_start: codewhale_app_server::daemon_socket::capture_process_start(
            std::process::id(),
        )
        .await
        .unwrap(),
        principal: codewhale_config::private_directory::PrivateDirectory::current_user_id()
            .to_string(),
        socket_path: harness.socket_path.clone(),
        config_path: harness.options().config_path,
    };
    // Transport acceptance: the captured owner is a fixture, not an Engine/store proof.
    let daemon = codewhale_app_server::bind_runtime_owner(
        owner.config_path.clone(),
        "127.0.0.1:1".parse().unwrap(),
        Some("private-fixture-bearer".into()),
        owner.clone(),
    )
    .await
    .unwrap();
    let receipt_path = harness.socket_path.with_file_name("daemon.sock.owner.json");
    let bytes = std::fs::read(&receipt_path).unwrap();
    assert_eq!(
        serde_json::from_slice::<codewhale_protocol::RuntimeOwnerReceipt>(&bytes).unwrap(),
        owner
    );
    assert!(
        !String::from_utf8(bytes)
            .unwrap()
            .contains("private-fixture-bearer")
    );
    assert_eq!(
        std::fs::metadata(&receipt_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let handle = daemon.shutdown_handle();
    let server = tokio::spawn(daemon.serve());
    let mut guest = Client::connect(&harness.socket_path).await;
    let missing = guest.attach(1, "guest", "attach").await;
    assert!(missing["error"].is_object());
    let mut stale = owner.clone();
    stale.lease_generation = "other-generation".into();
    let rejected = guest
        .call(
            2,
            "daemon/attach",
            json!({"client":{"name":"guest"},"mode":"attach","expect_owner":stale}),
        )
        .await;
    assert!(rejected["error"].is_object());
    let claim = guest
        .call(
            3,
            "daemon/attach",
            json!({"client":{"name":"guest"},"mode":"claim","expect_owner":owner}),
        )
        .await;
    assert!(claim["error"].is_object());
    let attached = guest
        .call(
            4,
            "daemon/attach",
            json!({"client":{"name":"guest","pid":1},"mode":"attach","expect_owner":owner}),
        )
        .await;
    assert_eq!(attached["result"]["role"], "attached");
    assert_eq!(attached["result"]["owner_receipt"], json!(owner));
    // The display PID is deliberately false: authorization uses kernel peer credentials.
    let denied = guest.call(5, "shutdown", json!({})).await;
    assert!(denied["error"].is_object());
    assert!(guest.call(6, "healthz", json!({})).await["result"].is_object());
    handle.trigger();
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!harness.socket_path.exists());
    assert!(
        !receipt_path.exists(),
        "normal shutdown awaits exact receipt retirement"
    );
}

/// Native IPC/HTTP transport proof with a fake owned Runtime; no Engine or
/// provider acceptance is inferred from this source fixture.
#[tokio::test]
async fn canonical_cli_control_uses_authenticated_owner_and_same_dispatcher() {
    use axum::{Json, Router, routing::get};
    let harness = Harness::new("thread-control");
    let workspace = harness._config_dir.path().to_path_buf();
    let thread = serde_json::json!({"id":"canonical-transport","created_at":"2026-10-02T00:00:00Z",
        "updated_at":"2026-10-02T00:00:00Z","model":"fixture-model","model_provider":"custom",
        "model_provider_id":"fixture-owner","workspace":workspace,"archived":false});
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let app = Router::new()
        .route(
            "/v1/threads",
            get(move || {
                let thread = thread.clone();
                async move { Json(serde_json::json!([thread])) }
            }),
        )
        .route(
            "/v1/threads/running",
            get(|| async { Json(serde_json::json!([])) }),
        );
    let fake_http = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let owner = codewhale_protocol::RuntimeOwnerReceipt {
        version: 1,
        data_dir: harness.root.join("runtime"),
        execution_scope: "control-fixture".into(),
        lease_generation: "control-generation".into(),
        pid: std::process::id(),
        process_start: codewhale_app_server::daemon_socket::capture_process_start(
            std::process::id(),
        )
        .await
        .unwrap(),
        principal: codewhale_config::private_directory::PrivateDirectory::current_user_id()
            .to_string(),
        socket_path: harness.socket_path.clone(),
        config_path: harness.options().config_path,
    };
    let daemon = codewhale_app_server::bind_runtime_owner(
        owner.config_path.clone(),
        endpoint,
        Some("private-fixture-control".into()),
        owner.clone(),
    )
    .await
    .unwrap();
    let shutdown = daemon.shutdown_handle();
    let socket = tokio::spawn(daemon.serve());
    let result = codewhale_app_server::request_thread_control(
        owner.config_path.clone(),
        Some(harness.socket_path.clone()),
        None,
        codewhale_protocol::ThreadRequest::List(codewhale_protocol::ThreadListParams {
            include_archived: false,
            limit: None,
        }),
    )
    .await
    .unwrap();
    assert_eq!(result.threads.len(), 1);
    assert_eq!(result.threads[0].id, "canonical-transport");
    assert_eq!(result.threads[0].model_provider, "fixture-owner");
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("private-fixture-control")
    );
    let error = codewhale_app_server::request_thread_control(
        owner.config_path.clone(),
        Some(harness.socket_path.clone()),
        Some(codewhale_app_server::ThreadControlSelection {
            workspace: Some(workspace),
            config_profile: None,
            config_source: None,
        }),
        codewhale_protocol::ThreadRequest::List(codewhale_protocol::ThreadListParams {
            include_archived: false,
            limit: None,
        }),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("no captured worker setting"));
    shutdown.trigger();
    tokio::time::timeout(Duration::from_secs(5), socket)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fake_http.abort();
}

/// This only validates the adapter's captured scope transport. Actual Runtime
/// policy/Config admission is covered by the owning Runtime acceptance suite.
struct ScopedControlFixture {
    workers: usize,
    config: PathBuf,
    scopes: std::sync::Arc<tokio::sync::Mutex<Vec<codewhale_app_server::RuntimeFrontendScope>>>,
}
impl codewhale_app_server::RuntimeOwnerFrontend for ScopedControlFixture {
    fn validate_selection<'a>(
        &'a self,
        selection: &'a codewhale_app_server::RuntimeOwnerFrontendSelection,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let codewhale_app_server::RuntimeOwnerFrontendSelection::Control(scope) = selection
            else {
                anyhow::bail!("fixture expects the existing control frontend");
            };
            anyhow::ensure!(
                scope.workers == self.workers,
                "captured scheduler setting changed"
            );
            anyhow::ensure!(
                scope.workspace.is_absolute() && scope.workspace.is_dir(),
                "fixture scope is not selected"
            );
            anyhow::ensure!(
                scope
                    .config_profile
                    .as_deref()
                    .is_none_or(|profile| profile == "reviewed"),
                "profile is not admitted by held owner"
            );
            anyhow::ensure!(
                scope
                    .config_source
                    .as_ref()
                    .is_none_or(|source| source == &self.config),
                "config is not admitted by held owner"
            );
            self.scopes.lock().await.push(scope.clone());
            Ok(())
        })
    }
    fn serve(
        &self,
        selection: codewhale_app_server::RuntimeOwnerFrontendSelection,
        compatibility: codewhale_app_server::AppState,
        input: Box<dyn tokio::io::AsyncBufRead + Send + Unpin>,
        output: Box<dyn tokio::io::AsyncWrite + Send + Unpin>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send + '_>> {
        Box::pin(async move {
            let codewhale_app_server::RuntimeOwnerFrontendSelection::Control(scope) = selection
            else {
                anyhow::bail!("fixture expects control");
            };
            codewhale_app_server::run_guest_control(compatibility, scope.workspace, input, output)
                .await
        })
    }
}

#[tokio::test]
async fn canonical_cli_scoped_resume_fork_keep_exact_owner_workers_and_refuse_wrong_selection() {
    use axum::{Json, Router, response::IntoResponse as _};
    use std::sync::Arc;
    let harness = Harness::new("scoped-control");
    let workspace = harness._config_dir.path().to_path_buf();
    let selected = workspace.join("explicit-target");
    std::fs::create_dir(&selected).unwrap();
    let owner = codewhale_protocol::RuntimeOwnerReceipt {
        version: 1,
        data_dir: harness.root.join("runtime"),
        execution_scope: "scoped-control-store".into(),
        lease_generation: "scoped-control-generation".into(),
        pid: std::process::id(),
        process_start: codewhale_app_server::daemon_socket::capture_process_start(
            std::process::id(),
        )
        .await
        .unwrap(),
        principal: codewhale_config::private_directory::PrivateDirectory::current_user_id()
            .to_string(),
        socket_path: harness.socket_path.clone(),
        config_path: harness.options().config_path,
    };
    let calls = Arc::new(tokio::sync::Mutex::new(Vec::<(String, Value)>::new()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap();
    let captured = owner.clone();
    let http_calls = calls.clone();
    let target = selected.clone();
    let app = Router::new().fallback(move |request: axum::extract::Request| {
        let owner = captured.clone(); let calls = http_calls.clone(); let target = target.clone();
        async move {
            let path = request.uri().path().to_string();
            let bytes = axum::body::to_bytes(request.into_body(), 8 * 1024 * 1024).await.unwrap();
            let body = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
            calls.lock().await.push((path.clone(), body.clone()));
            let record = |id: &str, root: &Path| json!({"id":id,"created_at":"2026-10-02T00:00:00Z",
                "updated_at":"2026-10-02T00:00:00Z","model":"fixture-model","model_provider":"custom",
                "model_provider_id":"fixture-owner","workspace":root,"archived":false});
            let value = if path.ends_with("/operations/lookup") { json!({"state":"absent"}) }
                else if path.ends_with("/mutate") {
                    assert_eq!(body["expected_data_dir"], json!(owner.data_dir));
                    assert_eq!(body["expected_execution_scope"], owner.execution_scope);
                    assert_eq!(body["workspace"], json!(target));
                    assert_eq!(body["mutation"]["options"]["overrides"], json!({"model":"explicit-model", "model_provider":"fixture-owner", "approval_policy":"on-request", "sandbox":"workspace-write"}));
                    let fork = body["mutation"]["action"] == "fork";
                    json!({"version":1,"data_dir":owner.data_dir,"execution_scope":owner.execution_scope,
                        "operation_key":body["operation_key"],"request_digest":"1".repeat(64),"history_digest":"2".repeat(64),
                        "runtime_thread_id":if fork {"canonical-scoped-fork"} else {"canonical-source"},
                        "session_id":if fork {"session-scoped-fork"} else {"session-source"}})
                } else if path.ends_with("/history") {
                    json!({"version":1,"data_dir":owner.data_dir,"execution_scope":owner.execution_scope,
                        "runtime_thread_id":"canonical-source","saved_session_id":"session-source",
                        "saved_document_digest":"a".repeat(64),"session_goal_digest":"74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b",
                        "document_digest":"b".repeat(64),"session":{"metadata":{"id":"session-source"},"messages":[],"journal":{"entries":[]}}})
                } else if path == "/v1/threads/running" { json!([]) }
                else if path == "/v1/threads" { json!([record("canonical-source", &target), record("other-workspace", Path::new("/recorded-other-workspace"))]) }
                else if path == "/v1/threads/canonical-scoped-fork" { record("canonical-scoped-fork", &target) }
                else if path == "/v1/threads/canonical-source" { record("canonical-source", &target) }
                else { return (axum::http::StatusCode::NOT_FOUND, Json(json!({"error":"fixture route missing"}))).into_response(); };
            Json(value).into_response()
        }
    });
    let fake_http = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let scopes = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let frontend = Arc::new(ScopedControlFixture {
        workers: 7,
        config: owner.config_path.clone().unwrap(),
        scopes: scopes.clone(),
    });
    let (daemon, _) = codewhale_app_server::bind_runtime_frontends(
        owner.config_path.clone(),
        Some("fixture-private-scope-token".into()),
        owner.clone(),
        codewhale_app_server::RuntimeOwnerRouting {
            endpoint,
            workspace: Some(workspace),
            workers: Some(7),
            mobile: false,
            web: false,
            acp: true,
            acp_only: false,
        },
        Some(frontend),
        None,
    )
    .await
    .unwrap();
    let daemon = daemon.expect("a private endpoint directory is available on this host");
    let shutdown = daemon.shutdown_handle();
    let socket = tokio::spawn(daemon.serve());
    let selection = codewhale_app_server::ThreadControlSelection {
        workspace: Some(selected.clone()),
        config_profile: Some("reviewed".into()),
        config_source: owner.config_path.clone(),
    };
    for (kind, operation, expected_id) in [
        ("resume", "scoped-resume", "canonical-source"),
        ("fork", "scoped-fork", "canonical-scoped-fork"),
    ] {
        let request = serde_json::from_value(
            json!({"kind":kind,"thread_id":"canonical-source","operation_key":operation,
                "model":"explicit-model", "model_provider":"fixture-owner", "approval_policy":"on-request", "sandbox":"workspace-write"}),
        )
        .unwrap();
        let response = codewhale_app_server::request_thread_control(
            owner.config_path.clone(),
            Some(harness.socket_path.clone()),
            Some(selection.clone()),
            request,
        )
        .await
        .unwrap();
        assert_eq!(response.data["receipt"]["operation_key"], operation);
        assert_eq!(response.data["receipt"]["runtime_thread_id"], expected_id);
        assert_eq!(response.thread.unwrap().id, expected_id);
    }
    let list = codewhale_app_server::request_thread_control(
        owner.config_path.clone(),
        Some(harness.socket_path.clone()),
        Some(selection.clone()),
        codewhale_protocol::ThreadRequest::List(codewhale_protocol::ThreadListParams {
            include_archived: false,
            limit: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        list.threads
            .iter()
            .any(|thread| thread.id == "other-workspace"),
        "scoped execution does not filter owner-store-wide read-only list"
    );
    assert!(scopes.lock().await.iter().all(|scope| scope.workers == 7
        && scope.workspace == selected
        && scope.config_profile.as_deref() == Some("reviewed")));
    let before = calls.lock().await.len();
    for wrong in [
        codewhale_app_server::ThreadControlSelection {
            config_profile: Some("wrong".into()),
            ..selection.clone()
        },
        codewhale_app_server::ThreadControlSelection {
            config_source: Some(selected.join("other.toml")),
            ..selection.clone()
        },
    ] {
        let request = serde_json::from_value(json!({"kind":"resume","thread_id":"canonical-source","operation_key":"never-dispatched"})).unwrap();
        assert!(
            codewhale_app_server::request_thread_control(
                owner.config_path.clone(),
                Some(harness.socket_path.clone()),
                Some(wrong),
                request
            )
            .await
            .is_err()
        );
    }
    assert_eq!(
        calls.lock().await.len(),
        before,
        "selection refusal happens before HTTP effect dispatch"
    );
    shutdown.trigger();
    tokio::time::timeout(Duration::from_secs(5), socket)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    fake_http.abort();
}

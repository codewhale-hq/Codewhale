//! Actual manager/store/lease bootstrap, without any provider turn. Transport
//! fixtures do not substitute for the separate rebuilt two-process proof.
use super::*;
use codewhale_config::private_directory::PrivateDirectory;
use codewhale_engine::runtime_threads::{
    RuntimeProcessOwnerLock, RuntimeStoreBinding, manager::ThreadListFilter,
};
use codewhale_protocol::RuntimeOwnerReceipt;

struct OwnedChild(std::process::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

struct Rig {
    _temporary: tempfile::TempDir,
    root: PathBuf,
    store: PathBuf,
    socket: PathBuf,
    config_path: PathBuf,
    config: Config,
}
impl Rig {
    fn new() -> Result<Self> {
        let temporary = tempfile::Builder::new()
            .prefix("cw-ob-")
            .tempdir_in("/tmp")?;
        let root = temporary.path().canonicalize()?;
        let config_path = root.join("config.toml");
        fs::write(&config_path, "")?;
        fs::create_dir(root.join("workspace"))?;
        let config = Config::default().with_legacy_root(
            Some("bootstrap-no-provider-fixture".into()),
            Some("http://127.0.0.1:1/v1".into()),
        );
        Ok(Self {
            store: root.join("runtime"),
            socket: root.join("run/daemon.sock"),
            _temporary: temporary,
            root,
            config_path,
            config,
        })
    }
    fn manager(
        &self,
        lock: Option<RuntimeProcessOwnerLock>,
        binding: Option<&RuntimeStoreBinding>,
    ) -> Result<SharedRuntimeThreadManager> {
        let (manager, _) = open_runtime_threads_for_host(
            &self.config,
            self.root.join("workspace"),
            RuntimeThreadManagerConfig {
                data_dir: self.store.clone(),
                task_data_dir: self.root.join("tasks"),
                sessions_dir: Some(self.root.join("sessions")),
                max_active_threads: 8,
            },
            Arc::new(codewhale_plugins::PluginRegistry::new()),
            false,
            lock,
            binding,
        )?;
        Ok(manager)
    }
    async fn receipt(&self, manager: &SharedRuntimeThreadManager) -> Result<RuntimeOwnerReceipt> {
        let (binding, generation) = manager.capture_control_owner()?;
        Ok(RuntimeOwnerReceipt {
            version: 1,
            data_dir: binding.data_dir,
            execution_scope: binding.execution_scope,
            lease_generation: generation,
            pid: std::process::id(),
            process_start: codewhale_app_server::daemon_socket::capture_process_start(
                std::process::id(),
            )
            .await?,
            principal: PrivateDirectory::current_user_id().to_string(),
            socket_path: self.socket.clone(),
            config_path: Some(self.config_path.clone()),
        })
    }
    fn publish(&self, owner: &RuntimeOwnerReceipt) -> Result<()> {
        let parent = PrivateDirectory::admit(self.socket.parent().unwrap())?;
        let listener = std::os::unix::net::UnixListener::bind(&self.socket)?;
        let identity = parent
            .socket_identity("daemon.sock")?
            .context("owned socket")?;
        parent.protect_socket("daemon.sock", identity)?;
        drop(listener);
        parent.write_owned_file("daemon.sock.owner.json", &serde_json::to_vec(owner)?, false)?;
        Ok(())
    }
    async fn dead_publication(&self) -> Result<(RuntimeOwnerReceipt, String, Value)> {
        let manager = self.manager(None, None)?;
        let draft = manager
            .create_thread(CreateThreadRequest {
                allow_shell: Some(false),
                system_prompt: Some("Preserve this unrelated saved draft".into()),
                ..CreateThreadRequest::default()
            })
            .await?;
        let detail = serde_json::to_value(manager.get_thread_detail(&draft.id).await?)?;
        let mut owner = self.receipt(&manager).await?;
        manager.shutdown_and_wait().await?;
        drop(manager);
        let mut child = OwnedChild(
            std::process::Command::new("/bin/sleep")
                .arg("60")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()?,
        );
        owner.pid = child.0.id();
        owner.process_start =
            codewhale_app_server::daemon_socket::capture_process_start(owner.pid).await?;
        child.0.kill()?;
        child.0.wait()?;
        self.publish(&owner)?;
        Ok((owner, draft.id, detail))
    }
    async fn admit(&self) -> Result<RuntimeHostAdmission> {
        admit_runtime_host(
            Some(self.config_path.clone()),
            Some(self.socket.clone()),
            self.store.clone(),
        )
        .await
    }
}

#[tokio::test]
async fn preheld_bootstrap_lease_stays_with_real_manager_until_drop() -> Result<()> {
    let _env = lock_test_env();
    let rig = Rig::new()?;
    let RuntimeHostAdmission::Bootstrap {
        lock,
        binding,
        recovery,
    } = rig.admit().await?
    else {
        bail!("fresh host must bootstrap")
    };
    assert!(binding.is_none() && recovery.is_none());
    assert!(RuntimeProcessOwnerLock::try_acquire_for_host(&rig.store, false)?.is_none());
    let manager = rig.manager(Some(lock), binding.as_ref())?;
    assert!(RuntimeProcessOwnerLock::try_acquire_for_host(&rig.store, false)?.is_none());
    let (owner, generation) = manager.capture_control_owner()?;
    assert_eq!(owner.data_dir, rig.store);
    assert!(!generation.is_empty());
    manager.shutdown_and_wait().await?;
    assert!(
        RuntimeProcessOwnerLock::try_acquire_for_host(&rig.store, false)?.is_none(),
        "drain alone must not release retained ownership"
    );
    drop(manager);
    assert!(RuntimeProcessOwnerLock::try_acquire_for_host(&rig.store, false)?.is_some());
    Ok(())
}

#[tokio::test]
async fn two_crash_reentries_converge_on_one_owner_and_preserve_saved_draft() -> Result<()> {
    let _env = lock_test_env();
    let rig = Rig::new()?;
    let (old, draft, detail) = rig.dead_publication().await?;
    let gate = Arc::new(tokio::sync::Barrier::new(3));
    let start = || {
        let gate = gate.clone();
        let config = rig.config_path.clone();
        let socket = rig.socket.clone();
        let store = rig.store.clone();
        tokio::spawn(async move {
            gate.wait().await;
            admit_runtime_host(Some(config), Some(socket), store).await
        })
    };
    let mut first = start();
    let mut second = start();
    gate.wait().await;
    let (admission, loser) = tokio::select! {
        result = &mut first => (result??, second),
        result = &mut second => (result??, first),
    };
    let RuntimeHostAdmission::Bootstrap {
        lock,
        binding,
        recovery,
    } = admission
    else {
        bail!("one crash reentry must win the existing lease")
    };
    let recovery = recovery.context("exact stale publication")?;
    assert_eq!(recovery.receipt(), &old);
    assert!(RuntimeProcessOwnerLock::try_acquire_for_host(&rig.store, false)?.is_none());
    let manager = rig.manager(Some(lock), binding.as_ref())?;
    let owner = rig.receipt(&manager).await?;
    assert_ne!(owner.lease_generation, old.lease_generation);
    assert_eq!(owner.execution_scope, old.execution_scope);
    let (daemon, _) = codewhale_app_server::bind_runtime_frontends(
        Some(rig.config_path.clone()),
        None,
        owner.clone(),
        codewhale_app_server::RuntimeOwnerRouting {
            endpoint: "127.0.0.1:1".parse()?,
            workspace: None,
            workers: None,
            mobile: false,
            web: false,
            acp: false,
            acp_only: false,
        },
        None,
        Some(recovery),
    )
    .await?;
    let daemon = daemon.expect("a private endpoint directory is available on this host");
    let stop = daemon.shutdown_handle();
    let serving = tokio::spawn(daemon.serve());
    let RuntimeHostAdmission::Attached(client) = loser.await?? else {
        bail!("loser must attach to the winner, never open another manager")
    };
    assert_eq!(client.receipt(), &owner);
    validate_selected_owner(&client, rig.store.clone()).await?;
    assert_eq!(
        serde_json::to_value(manager.get_thread_detail(&draft).await?)?,
        detail
    );
    assert_eq!(
        manager
            .list_threads(ThreadListFilter::IncludeArchived, None)
            .await?
            .len(),
        1
    );
    drop(client);
    stop.trigger();
    serving.await??;
    manager.shutdown_and_wait().await?;
    drop(manager);
    assert!(RuntimeProcessOwnerLock::try_acquire_for_host(&rig.store, false)?.is_some());
    Ok(())
}

#[tokio::test]
async fn dead_publication_with_missing_existing_lease_refuses_without_recreation() -> Result<()> {
    let _env = lock_test_env();
    let rig = Rig::new()?;
    let (owner, _, _) = rig.dead_publication().await?;
    let lease = rig.store.join("runtime-process.owner.lock");
    fs::remove_file(&lease)?;
    assert!(rig.admit().await.is_err());
    assert!(
        !lease.exists(),
        "dead publication cannot manufacture a replacement lease"
    );
    assert_eq!(
        serde_json::from_slice::<RuntimeOwnerReceipt>(&fs::read(
            rig.socket.with_file_name("daemon.sock.owner.json")
        )?)?,
        owner
    );
    Ok(())
}

#[tokio::test]
async fn owner_bootstrap_attach_response_loss_never_falls_back_or_changes_store() -> Result<()> {
    use tokio::io::AsyncBufReadExt as _;
    let _env = lock_test_env();
    let rig = Rig::new()?;
    let manager = rig.manager(None, None)?;
    let owner = rig.receipt(&manager).await?;
    manager.shutdown_and_wait().await?;
    drop(manager);
    let store_owner = fs::read(rig.store.join("owner.json"))?;
    let parent = PrivateDirectory::admit(rig.socket.parent().unwrap())?;
    let listener = tokio::net::UnixListener::bind(&rig.socket)?;
    let identity = parent.socket_identity("daemon.sock")?.context("socket")?;
    parent.protect_socket("daemon.sock", identity)?;
    let bytes = serde_json::to_vec(&owner)?;
    parent.write_owned_file("daemon.sock.owner.json", &bytes, false)?;
    let peer = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut line = String::new();
        tokio::io::BufReader::new(stream)
            .read_line(&mut line)
            .await?;
        let request: Value = serde_json::from_str(&line)?;
        anyhow::ensure!(
            request["method"] == "daemon/attach",
            "one real attach write"
        );
        // Simulate lost acknowledgement after the valid peer saw attach.
        Ok::<_, anyhow::Error>(())
    });
    let error = rig.admit().await.err().context("lost attach must refuse")?;
    assert!(error.to_string().contains("closed before attach"));
    peer.await??;
    assert_eq!(fs::read(rig.store.join("owner.json"))?, store_owner);
    assert_eq!(
        parent
            .read_private_receipt("daemon.sock.owner.json", 16384)?
            .context("original receipt")?
            .0,
        bytes
    );
    assert_eq!(parent.socket_identity("daemon.sock")?, Some(identity));
    assert!(
        RuntimeProcessOwnerLock::try_acquire_for_host(&rig.store, false)?.is_some(),
        "no owner manager was opened after attach uncertainty"
    );
    Ok(())
}

#[tokio::test]
async fn stale_publication_cannot_select_another_store_or_execution_scope() -> Result<()> {
    let _env = lock_test_env();
    for wrong_store in [false, true] {
        let rig = Rig::new()?;
        let (mut owner, _, _) = rig.dead_publication().await?;
        if wrong_store {
            owner.data_dir = rig.root.join("other-store");
        } else {
            owner.execution_scope.push_str("-wrong");
        }
        fs::write(
            rig.socket.with_file_name("daemon.sock.owner.json"),
            serde_json::to_vec(&owner)?,
        )?;
        assert!(rig.admit().await.is_err());
        assert!(!rig.root.join("other-store").exists());
        assert_eq!(
            serde_json::from_slice::<RuntimeOwnerReceipt>(&fs::read(
                rig.socket.with_file_name("daemon.sock.owner.json")
            )?)?,
            owner
        );
    }
    Ok(())
}

#[tokio::test]
async fn moved_preheld_lease_refuses_before_store_recovery() -> Result<()> {
    let _env = lock_test_env();
    let rig = Rig::new()?;
    let RuntimeHostAdmission::Bootstrap { lock, .. } = rig.admit().await? else {
        bail!("fresh bootstrap")
    };
    let lease = rig.store.join("runtime-process.owner.lock");
    fs::rename(&lease, rig.store.join("moved.owner.lock"))?;
    fs::write(&lease, b"")?;
    assert!(rig.manager(Some(lock), None).is_err());
    assert!(
        !rig.store.join("owner.json").exists(),
        "invalid held lease must not recover/mint the store"
    );
    Ok(())
}

#[tokio::test]
async fn busy_unpublished_owner_has_bounded_refusal_without_store_recovery() -> Result<()> {
    let _env = lock_test_env();
    let rig = Rig::new()?;
    let held = RuntimeProcessOwnerLock::try_acquire_for_host(&rig.store, true)?
        .context("first owner lease")?;
    let started = tokio::time::Instant::now();
    let error = rig
        .admit()
        .await
        .err()
        .context("busy host must remain bounded")?;
    assert!(error.to_string().contains("deadline expired"));
    assert!(started.elapsed() < Duration::from_secs(8));
    assert!(!rig.store.join("owner.json").exists());
    assert!(!rig.socket.with_file_name("daemon.sock.owner.json").exists());
    drop(held);
    Ok(())
}

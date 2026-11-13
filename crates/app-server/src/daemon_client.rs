//! Authenticated local control attachment to the already held Runtime owner.
//! The private receipt routes; the connected kernel process authenticates it.
//! This forwards the existing bounded dispatcher protocol, never HTTP bearers.

use anyhow::Result;
use std::path::PathBuf;

#[cfg(any(unix, windows))]
mod platform {
    use super::*;
    use crate::daemon_socket::AuthorizedPeer;
    #[cfg(windows)]
    use crate::daemon_socket::default_socket_path;
    use crate::stdio::{
        BoundedLines, ParsedStdioLine, is_frame_refusal, parse_stdio_line, serialize_control_frame,
        write_control_line, write_stdio_line,
    };
    use anyhow::{Context as _, bail};
    use codewhale_app_server_protocol::lifecycle::InitializeResponse;
    use codewhale_app_server_protocol::rpc::{
        RequestId, RpcCall, RpcErrorResponse, RpcNotification, RpcResponse,
    };
    use codewhale_config::private_directory::PrivateDirectory;
    #[cfg(unix)]
    use codewhale_paths::daemon_socket::default_socket_path;
    use codewhale_protocol::RuntimeOwnerReceipt;
    use codewhale_runtime::runtime_control::owner_work;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::BufReader;
    #[cfg(unix)]
    use tokio::net::UnixStream;
    #[cfg(unix)]
    type Connection = UnixStream;
    #[cfg(windows)]
    type Connection = tokio::net::windows::named_pipe::NamedPipeClient;

    type OwnerTransport = crate::websocket::LocalWebSocket<
        tokio::io::ReadHalf<Connection>,
        tokio::io::WriteHalf<Connection>,
    >;

    fn resolved_store_form(path: PathBuf) -> Option<PathBuf> {
        let checked = codewhale_engine::runtime_threads::checked_runtime_store_root(path).ok()?;
        let mut tail = Vec::new();
        let mut probe = checked.as_path();
        loop {
            match probe.canonicalize() {
                Ok(real) => {
                    return Some(tail.iter().rev().fold(real, |acc, part| acc.join(part)));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    tail.push(probe.file_name()?.to_owned());
                    probe = probe.parent()?;
                }
                Err(_) => return None,
            }
        }
    }

    fn resolved_store_form_of(path: &std::path::Path) -> PathBuf {
        resolved_store_form(path.to_path_buf()).unwrap_or_else(|| path.to_path_buf())
    }

    fn same_store_path(left: &std::path::Path, right: &std::path::Path) -> bool {
        left == right
            || (cfg!(any(target_os = "macos", windows))
                && left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase())
    }

    fn same_socket_selection(left: &std::path::Path, right: &std::path::Path) -> bool {
        if left == right {
            return true;
        }
        #[cfg(unix)]
        {
            if left.file_name().is_none() || left.file_name() != right.file_name() {
                return false;
            }
            match (
                left.parent()
                    .map(codewhale_paths::resolve_root_owned_ancestors),
                right
                    .parent()
                    .map(codewhale_paths::resolve_root_owned_ancestors),
            ) {
                (Some(left), Some(right)) => left == right,
                _ => false,
            }
        }
        #[cfg(windows)]
        {
            false
        }
    }

    pub struct OwnerClient {
        read: BoundedLines<BufReader<tokio::io::ReadHalf<OwnerTransport>>>,
        write: tokio::io::WriteHalf<OwnerTransport>,
        peer: Arc<AuthorizedPeer>,
        receipt: RuntimeOwnerReceipt,
        routing: Option<crate::RuntimeOwnerRouting>,
        frontend: crate::daemon_socket::AttachFrontend,
        output_failed: bool,
        native_initialized: bool,
        pending_initialize: Option<RequestId>,
    }

    /// Host startup is the only caller allowed to acquire the Runtime lease.
    /// Unavailable is pre-write transport evidence, never ownership or death.
    pub enum HostOwnerProbe {
        Absent,
        Attached(Box<OwnerClient>),
        Unavailable(Arc<UnavailablePublication>),
    }

    #[derive(Debug, thiserror::Error)]
    #[error("selected owner publication changed before attach; re-observe")]
    pub struct PublicationChangedBeforeAttach;

    fn owner_receipt_error(error: anyhow::Error) -> anyhow::Error {
        #[cfg(unix)]
        if error.is::<codewhale_config::private_directory::UnlinkedPrivateFile>() {
            return PublicationChangedBeforeAttach.into();
        }
        error
    }

    pub(crate) struct OwnerPublication {
        pub(crate) parent: Arc<PrivateDirectory>,
        pub(crate) receipt_name: String,
        #[cfg(unix)]
        pub(crate) socket_path: PathBuf,
        pub(crate) bytes: Vec<u8>,
        pub(crate) receipt: RuntimeOwnerReceipt,
        pub(crate) file: std::fs::File,
        #[cfg(unix)]
        pub(crate) socket_identity:
            Option<codewhale_config::private_directory::PrivateSocketIdentity>,
    }
    impl OwnerPublication {
        pub(crate) fn receipt_is_current(&self) -> Result<bool> {
            if !self.parent.is_at_selected_path()? {
                return Ok(false);
            }
            let current = match self
                .parent
                .read_private_receipt(&self.receipt_name, 16384)
                .map_err(owner_receipt_error)
            {
                Err(error) if error.is::<PublicationChangedBeforeAttach>() => return Ok(false),
                result => result?,
            };
            let Some((bytes, file)) = current else {
                return Ok(false);
            };
            Ok(bytes == self.bytes && PrivateDirectory::same_file_identity(&self.file, &file)?)
        }
        pub(crate) fn is_current(&self) -> Result<bool> {
            if !self.receipt_is_current()? {
                return Ok(false);
            }
            #[cfg(unix)]
            {
                let name = self
                    .socket_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("invalid owner endpoint basename")?;
                if self.parent.socket_identity(name)? != self.socket_identity {
                    return Ok(false);
                }
            }
            Ok(true)
        }
    }

    /// Opaque retained publication. No public constructor or path-based cleanup.
    pub struct UnavailablePublication {
        pub(crate) publication: Arc<OwnerPublication>,
        cause: std::io::Error,
    }
    impl UnavailablePublication {
        pub fn receipt(&self) -> &RuntimeOwnerReceipt {
            &self.publication.receipt
        }
        fn strict_error(&self) -> anyhow::Error {
            let cause = match self.cause.raw_os_error() {
                Some(code) => std::io::Error::from_raw_os_error(code),
                None => std::io::Error::new(self.cause.kind(), self.cause.to_string()),
            };
            anyhow::Error::new(cause)
                .context("selected owner publication unavailable; canonical host recovery required")
        }
        fn require_dead_process(&self) -> Result<()> {
            let receipt = self.receipt();
            #[cfg(unix)]
            let principal = PrivateDirectory::current_user_id().to_string();
            #[cfg(windows)]
            let principal =
                codewhale_config::windows_identity::CurrentWindowsUser::open()?.sid_string()?;
            anyhow::ensure!(
                receipt.principal == principal && !receipt.process_start.is_empty(),
                "selected owner process identity is invalid"
            );
            #[cfg(unix)]
            let (running, start) = {
                use codewhale_config::private_directory::{UnixProcessStatus, unix_process_status};
                #[cfg(target_os = "linux")]
                {
                    use codewhale_config::private_directory::linux_process_start_predates_boot;
                    if linux_process_start_predates_boot(&receipt.process_start)? {
                        return Ok(());
                    }
                }
                match unix_process_status(receipt.pid)? {
                    UnixProcessStatus::Absent => return Ok(()),
                    UnixProcessStatus::Present { start } => (true, start),
                }
            };
            #[cfg(windows)]
            let (running, start) = {
                use codewhale_config::windows_identity::{
                    WindowsProcessStatus, windows_process_status,
                };
                match windows_process_status(receipt.pid)? {
                    WindowsProcessStatus::Absent => return Ok(()),
                    WindowsProcessStatus::Running { start } => (true, start),
                    WindowsProcessStatus::Exited { start } => (false, start),
                }
            };
            if start != receipt.process_start {
                bail!("selected owner PID was reused; refusing recovery")
            }
            if running {
                bail!("selected owner process is still present; refusing recovery")
            }
            Ok(())
        }
        pub(crate) fn require_same_selection(
            &self,
            owner: &RuntimeOwnerReceipt,
            endpoint: &std::path::Path,
        ) -> Result<()> {
            let old = self.receipt();
            anyhow::ensure!(
                old.version == owner.version
                    && old.data_dir == owner.data_dir
                    && old.execution_scope == owner.execution_scope
                    && same_socket_selection(&old.socket_path, endpoint)
                    && old.config_path == owner.config_path
                    && old.principal == owner.principal,
                "captured stale owner belongs to another selected store/config/principal"
            );
            Ok(())
        }
        fn discard_if_store_gone_blocking(&self, selected_store: PathBuf) -> Result<bool> {
            let gone = resolved_store_form(selected_store).is_some_and(|selected| {
                same_store_path(&selected, &resolved_store_form_of(&self.receipt().data_dir))
                    && matches!(
                        selected.symlink_metadata(),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound
                    )
            });
            if !gone || !matches!(self.revalidate_receipt(), Ok(true)) {
                return Ok(false);
            }
            anyhow::ensure!(
                self.publication.parent.retire_private_receipt(
                    &self.publication.receipt_name,
                    &self.publication.file
                )?,
                PublicationChangedBeforeAttach
            );
            Ok(true)
        }
        async fn discard_if_store_gone(
            self: &Arc<Self>,
            selected_store: &std::path::Path,
        ) -> Result<bool> {
            let held = self.clone();
            let selected_store = selected_store.to_path_buf();
            owner_work(move || held.discard_if_store_gone_blocking(selected_store)).await
        }
        /// Caller retains the real Runtime lease; this checks evidence only.
        pub(crate) fn revalidate_receipt(&self) -> Result<bool> {
            if !self.publication.receipt_is_current()? {
                return Ok(false);
            }
            self.require_dead_process()?;
            Ok(true)
        }
        pub(crate) fn revalidate_all(&self) -> Result<bool> {
            if !self.publication.is_current()? {
                return Ok(false);
            }
            self.require_dead_process()?;
            Ok(true)
        }
        pub async fn revalidate(self: &Arc<Self>) -> Result<bool> {
            let held = self.clone();
            owner_work(move || held.revalidate_all()).await
        }
    }

    async fn observe_publication(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
    ) -> Result<Option<Arc<OwnerPublication>>> {
        let socket_path = owner_work(move || match selected {
            Some(path) => Ok(path),
            None => default_socket_path().map_err(Into::into),
        })
        .await?;
        #[cfg(unix)]
        let directory = socket_path
            .parent()
            .context("owner endpoint has no private parent")?
            .to_path_buf();
        #[cfg(windows)]
        let directory = owner_work(crate::daemon_windows::owner_directory).await?;
        #[cfg(unix)]
        let name = format!(
            "{}.owner.json",
            socket_path
                .file_name()
                .and_then(|name| name.to_str())
                .context("invalid owner endpoint basename")?
        );
        #[cfg(windows)]
        let name = "daemon.owner.json".to_string();
        owner_work(move || {
            // This runs before the engine has anywhere to log, so a refusal
            // here is the first thing a user sees and the last thing they can
            // diagnose. A mount that reports its own ownership (HarmonyOS
            // hmdfs) makes the strict proof impossible for every object on it,
            // so discovering an owner — not authenticating one — takes the
            // measured exception. The receipt is still read through the
            // retained handle, must be a single-link file, and the socket
            // identity is still compared.
            let parent = match PrivateDirectory::inspect_trusting_mount(&directory) {
                Ok(parent) => Arc::new(parent),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    return Ok(None);
                }
                Err(error) => return Err(error),
            };
            let Some((bytes, file)) = parent
                .read_private_receipt(&name, 16384)
                .map_err(owner_receipt_error)?
            else {
                return Ok(None);
            };
            let receipt: RuntimeOwnerReceipt = serde_json::from_slice(&bytes)?;
            anyhow::ensure!(
                receipt.version == 1
                    && receipt.pid > 0
                    && same_socket_selection(&receipt.socket_path, &socket_path)
                    && !receipt.lease_generation.is_empty(),
                "selected owner receipt is invalid"
            );
            anyhow::ensure!(
                receipt.config_path == config_path,
                "selected owner config does not match; refusing another store"
            );
            #[cfg(unix)]
            let socket_identity = parent.socket_identity(
                socket_path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .context("invalid owner endpoint basename")?,
            )?;
            Ok(Some(Arc::new(OwnerPublication {
                parent,
                receipt_name: name,
                bytes,
                receipt,
                file,
                #[cfg(unix)]
                socket_path,
                #[cfg(unix)]
                socket_identity,
            })))
        })
        .await
    }

    /// Read-only absence recheck after the actual Runtime lease was acquired.
    pub async fn publication_is_absent(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
    ) -> Result<bool> {
        Ok(observe_publication(config_path, selected).await?.is_none())
    }

    pub async fn probe_owner_for_host_startup(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
        selected_store: &std::path::Path,
    ) -> Result<HostOwnerProbe> {
        match probe_frontend(
            config_path,
            selected,
            crate::daemon_socket::AttachFrontend::Control,
            None,
            None,
            None,
            None,
        )
        .await?
        {
            HostOwnerProbe::Unavailable(publication)
                if publication.discard_if_store_gone(selected_store).await? =>
            {
                Ok(HostOwnerProbe::Absent)
            }
            probe => Ok(probe),
        }
    }

    pub async fn connect(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
    ) -> Result<OwnerClient> {
        connect_if_published(config_path, selected).await?.context(
            "the selected Runtime has no authenticated live owner; start its canonical host",
        )
    }

    pub async fn connect_if_published(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
    ) -> Result<Option<OwnerClient>> {
        connect_frontend(
            config_path,
            selected,
            crate::daemon_socket::AttachFrontend::Control,
            None,
            None,
            None,
            None,
        )
        .await
    }
    pub async fn connect_acp_if_published(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
    ) -> Result<Option<OwnerClient>> {
        connect_frontend(
            config_path,
            selected,
            crate::daemon_socket::AttachFrontend::Acp,
            None,
            None,
            None,
            None,
        )
        .await
    }
    pub async fn connect_listener_if_published(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
        listener: crate::RuntimeListenerSelection,
        expected_owner: RuntimeOwnerReceipt,
    ) -> Result<Option<OwnerClient>> {
        listener.validate_bounds()?;
        connect_frontend(
            config_path,
            selected,
            crate::daemon_socket::AttachFrontend::Listener,
            Some(listener),
            Some(expected_owner),
            None,
            None,
        )
        .await
    }
    pub async fn connect_scoped_control_if_published(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
        scope: crate::RuntimeFrontendScope,
        owner: RuntimeOwnerReceipt,
    ) -> Result<Option<OwnerClient>> {
        scope.validate_bounds()?;
        connect_frontend(
            config_path,
            selected,
            crate::daemon_socket::AttachFrontend::Control,
            None,
            Some(owner),
            Some(scope),
            None,
        )
        .await
    }
    pub async fn connect_selected_acp_if_published(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
        scope: crate::RuntimeFrontendScope,
        model: String,
        owner: RuntimeOwnerReceipt,
    ) -> Result<Option<OwnerClient>> {
        scope.validate_bounds()?;
        anyhow::ensure!(
            !model.trim().is_empty() && model.len() <= 1024,
            "invalid selected ACP model"
        );
        connect_frontend(
            config_path,
            selected,
            crate::daemon_socket::AttachFrontend::Acp,
            None,
            Some(owner),
            Some(scope),
            Some(model),
        )
        .await
    }
    async fn connect_frontend(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
        frontend: crate::daemon_socket::AttachFrontend,
        listener: Option<crate::RuntimeListenerSelection>,
        expected_owner: Option<RuntimeOwnerReceipt>,
        scope: Option<crate::RuntimeFrontendScope>,
        acp_model: Option<String>,
    ) -> Result<Option<OwnerClient>> {
        match probe_frontend(
            config_path,
            selected,
            frontend,
            listener,
            expected_owner,
            scope,
            acp_model,
        )
        .await?
        {
            HostOwnerProbe::Absent => Ok(None),
            HostOwnerProbe::Attached(client) => Ok(Some(*client)),
            HostOwnerProbe::Unavailable(publication) => Err(publication.strict_error()),
        }
    }

    async fn probe_frontend(
        config_path: Option<PathBuf>,
        selected: Option<PathBuf>,
        frontend: crate::daemon_socket::AttachFrontend,
        listener: Option<crate::RuntimeListenerSelection>,
        expected_owner: Option<RuntimeOwnerReceipt>,
        scope: Option<crate::RuntimeFrontendScope>,
        acp_model: Option<String>,
    ) -> Result<HostOwnerProbe> {
        let Some(publication) = observe_publication(config_path, selected).await? else {
            return Ok(HostOwnerProbe::Absent);
        };
        #[cfg(unix)]
        let socket_path = publication.socket_path.clone();
        let receipt = publication.receipt.clone();
        #[cfg(unix)]
        let (stream, peer) = {
            let stream = match tokio::time::timeout(
                Duration::from_secs(5),
                UnixStream::connect(&socket_path),
            )
            .await
            .context("owner connect deadline expired")?
            {
                Ok(stream) => stream,
                Err(cause) => {
                    let held = publication.clone();
                    anyhow::ensure!(
                        owner_work(move || held.is_current()).await?,
                        PublicationChangedBeforeAttach
                    );
                    if cause.kind() == std::io::ErrorKind::ConnectionRefused
                        || (cause.kind() == std::io::ErrorKind::NotFound
                            && publication.socket_identity.is_none())
                    {
                        return Ok(HostOwnerProbe::Unavailable(Arc::new(
                            UnavailablePublication { publication, cause },
                        )));
                    }
                    return Err(cause.into());
                }
            };
            let credential = stream
                .peer_cred()
                .context("owner peer credentials unavailable")?;
            let pid = credential
                .pid()
                .and_then(|pid| u32::try_from(pid).ok())
                .context("owner peer PID unavailable")?;
            if credential.uid() != PrivateDirectory::current_user_id()
                || receipt.principal != credential.uid().to_string()
                || pid != receipt.pid
            {
                let held = publication.clone();
                anyhow::ensure!(
                    owner_work(move || held.is_current()).await?,
                    PublicationChangedBeforeAttach
                );
                bail!("connected process is not the selected Runtime owner");
            }
            (
                stream,
                Arc::new(AuthorizedPeer {
                    pid,
                    start: receipt.process_start.clone(),
                }),
            )
        };
        #[cfg(windows)]
        let (stream, peer) = {
            let (stream, process) = match crate::daemon_windows::connect_owner(&receipt).await {
                Ok(connected) => connected,
                Err(error) => {
                    let held = publication.clone();
                    anyhow::ensure!(
                        owner_work(move || held.is_current()).await?,
                        PublicationChangedBeforeAttach
                    );
                    let missing = error
                        .downcast_ref::<std::io::Error>()
                        .filter(|cause| cause.kind() == std::io::ErrorKind::NotFound)
                        .map(|cause| match cause.raw_os_error() {
                            Some(code) => std::io::Error::from_raw_os_error(code),
                            None => std::io::Error::from(cause.kind()),
                        });
                    if let Some(cause) = missing {
                        return Ok(HostOwnerProbe::Unavailable(Arc::new(
                            UnavailablePublication { publication, cause },
                        )));
                    }
                    return Err(error);
                }
            };
            let peer = AuthorizedPeer {
                pid: process.pid(),
                start: process.start().to_string(),
                process,
            };
            (stream, Arc::new(peer))
        };
        let held_publication = publication.clone();
        let check = peer.clone();
        owner_work(move || {
            check.check()?;
            anyhow::ensure!(
                held_publication.is_current()?,
                PublicationChangedBeforeAttach
            );
            Ok(())
        })
        .await?;
        anyhow::ensure!(
            expected_owner
                .as_ref()
                .is_none_or(|expected| expected == &receipt),
            "selected owner changed before frontend admission"
        );
        let (read, write) = tokio::io::split(stream);
        let transport = crate::websocket::connect(read, write).await?;
        let (read, mut write) = tokio::io::split(transport);
        let mut replies = BoundedLines::new(BufReader::new(read));
        let attach_id = uuid::Uuid::new_v4().to_string();
        let attach = json!(RpcCall {
            id: Some(RequestId::String(attach_id.clone())),
            method: "daemon/attach".into(),
            params: json!({
                "client":{"name":"codewhale-stdio","version":env!("CARGO_PKG_VERSION"),"pid":std::process::id()},
                "mode":"attach","frontend":frontend,"listener":listener,"scope":scope,"acp_model":acp_model,"expect_daemon_version":env!("CARGO_PKG_VERSION"),"expect_owner":receipt
            }),
            trace: None
        });
        tokio::time::timeout(
            Duration::from_secs(5),
            write_stdio_line(&mut write, &attach),
        )
        .await
        .context("owner attach write deadline expired; outcome uncertain")??;
        let line = tokio::time::timeout(Duration::from_secs(5), replies.next_line())
            .await
            .context("owner attach response deadline expired")??
            .context("owner closed before attach")?;
        anyhow::ensure!(line.len() <= 16384, "oversized owner attach response");
        let response: Value = serde_json::from_str(&line)?;
        anyhow::ensure!(
            response["id"] == attach_id
                && response["error"].is_null()
                && response["result"]["role"] == "attached",
            "selected owner refused guest attachment"
        );
        let returned: RuntimeOwnerReceipt =
            serde_json::from_value(response["result"]["owner_receipt"].clone())
                .context("owner attach receipt unavailable")?;
        anyhow::ensure!(
            returned == receipt,
            "connected owner did not confirm the exact captured generation/store"
        );
        let routing = response["result"]
            .get("runtime_routing")
            .filter(|value| !value.is_null())
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()?;
        Ok(HostOwnerProbe::Attached(Box::new(OwnerClient {
            read: replies,
            write,
            peer,
            receipt,
            routing,
            frontend,
            output_failed: false,
            native_initialized: false,
            pending_initialize: None,
        })))
    }

    impl OwnerClient {
        pub fn routing(&self) -> Option<&crate::RuntimeOwnerRouting> {
            self.routing.as_ref()
        }

        pub fn receipt(&self) -> &RuntimeOwnerReceipt {
            &self.receipt
        }
        pub async fn send(&mut self, id: Value, method: &str, params: Value) -> Result<()> {
            anyhow::ensure!(
                !self.output_failed,
                "owner transport write previously failed; outcomes uncertain, not replayed"
            );
            if self.frontend == crate::daemon_socket::AttachFrontend::Acp {
                return self
                    .send_frame(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                    .await;
            }
            let id: RequestId = serde_json::from_value(id)?;
            anyhow::ensure!(
                self.pending_initialize.is_none(),
                "receive the pending initialize acknowledgement before sending another request"
            );
            if self.frontend == crate::daemon_socket::AttachFrontend::Control
                && !self.native_initialized
                && method != "initialize"
            {
                let initialize_id = RequestId::String(format!(
                    "codewhale-native-initialize-{}",
                    uuid::Uuid::new_v4()
                ));
                self.send_call(RpcCall { id: Some(initialize_id.clone()), method: "initialize".into(), params: json!({"clientInfo":{"name":"codewhale-native-control","version":env!("CARGO_PKG_VERSION")}}), trace: None }).await?;
                self.pending_initialize = Some(initialize_id.clone());
                let acknowledged: Result<()> = async {
                    let response = tokio::time::timeout(Duration::from_secs(5), self.recv()).await.context("owner initialize response deadline expired; connection outcome uncertain")??.context("owner input closed before initialize acknowledgement")?;
                    anyhow::ensure!(response.get("id") == Some(&json!(initialize_id)) && response.get("error").is_none() && response.get("result").is_some(), "owner initialize acknowledgement mismatch or refusal");
                    Ok(())
                }.await;
                if let Err(error) = acknowledged {
                    self.output_failed = true;
                    return Err(error);
                }
                self.native_initialized = true;
            }
            let pending = (self.frontend == crate::daemon_socket::AttachFrontend::Control
                && method == "initialize"
                && !self.native_initialized)
                .then(|| id.clone());
            self.send_call(RpcCall {
                id: Some(id),
                method: method.to_string(),
                params,
                trace: None,
            })
            .await?;
            self.pending_initialize = pending;
            Ok(())
        }
        async fn send_call(&mut self, request: RpcCall) -> Result<()> {
            self.send_frame(json!(request)).await
        }
        async fn send_frame(&mut self, frame: Value) -> Result<()> {
            anyhow::ensure!(
                !self.output_failed,
                "owner transport write previously failed; outcomes uncertain, not replayed"
            );
            let bytes = serialize_control_frame(&frame)?;
            let check = self.peer.clone();
            owner_work(move || check.check()).await?;
            self.output_failed = true;
            tokio::time::timeout(
                Duration::from_secs(5),
                write_control_line(&mut self.write, &bytes),
            )
            .await
            .context("owner write deadline expired; outcome uncertain, not replayed")??;
            self.output_failed = false;
            Ok(())
        }
        pub async fn recv(&mut self) -> Result<Option<Value>> {
            anyhow::ensure!(
                !self.output_failed,
                "owner transport write previously failed; outcomes uncertain, not replayed"
            );
            let Some(line) = self.read.next_line().await? else {
                return Ok(None);
            };
            let check = self.peer.clone();
            owner_work(move || check.check()).await?;
            let value: Value = serde_json::from_str(&line)?;
            if let Some(id) = self.pending_initialize.as_ref()
                && value.get("id") == Some(&json!(id))
            {
                let response = serde_json::from_value::<RpcResponse>(value.clone());
                let accepted = response
                    .ok()
                    .filter(|_| value.get("error").is_none())
                    .and_then(|response| {
                        serde_json::from_value::<InitializeResponse>(response.result).ok()
                    })
                    .is_some_and(|response| response.codex_home.is_absolute());
                let rejected = serde_json::from_value::<RpcErrorResponse>(value.clone()).is_ok();
                if !accepted && !rejected {
                    self.output_failed = true;
                    anyhow::bail!("initialize acknowledgement does not match its typed response");
                }
                self.native_initialized = accepted;
                self.pending_initialize = None;
                if accepted {
                    self.send_call(RpcCall {
                        id: None,
                        method: "initialized".into(),
                        params: Value::Null,
                        trace: None,
                    })
                    .await?;
                }
            }
            Ok(Some(value))
        }
        pub async fn forward<I, O>(mut self, input: I, output: O) -> Result<()>
        where
            I: tokio::io::AsyncRead + Unpin,
            O: tokio::io::AsyncWrite + Unpin,
        {
            anyhow::ensure!(
                !self.output_failed,
                "owner transport write previously failed; outcomes uncertain, not replayed"
            );
            let mut input = BoundedLines::new(BufReader::new(input));
            let mut output = tokio::io::BufWriter::new(output);
            let mut input_open = true;
            loop {
                tokio::select! {
                    line = input.next_line(), if input_open => match match line {
                        Ok(line) => line,
                        Err(error) if self.frontend == crate::daemon_socket::AttachFrontend::Acp && is_frame_refusal(&error) => {
                            write_stdio_line(&mut output, &json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":error.to_string()}})).await?;
                            continue;
                        }
                        Err(error) => return Err(error.into()),
                    } {
                        None => {
                            input_open=false;
                            tokio::time::timeout(Duration::from_secs(5), async {
                                write_stdio_line(&mut self.write,&json!(RpcNotification { method:"daemon/input_closed".into(), params:Some(json!({})) })).await?;
                                Ok::<(), anyhow::Error>(())
                            }).await.context("owner input-close write outcome uncertain, not replayed")??;
                        }
                        Some(line) if self.frontend==crate::daemon_socket::AttachFrontend::Acp => {
                            let check=self.peer.clone();
                            owner_work(move || check.check()).await?;
                            // Preserve response IDs/permission replies verbatim.
                            // Actual ACP authority/parser stays in AcpServer.
                            tokio::time::timeout(Duration::from_secs(5),write_control_line(&mut self.write,line.as_bytes())).await.context("ACP write outcome uncertain; not replayed")??;
                        }
                        Some(line) => match parse_stdio_line(&line) {
                            ParsedStdioLine::Blank => {},
                            ParsedStdioLine::Ignored => {},
                            ParsedStdioLine::Response(response) => self.send_frame(response).await?,
                            ParsedStdioLine::Request(request) => {
                                self.send_call(request).await?;
                            }
                        }
                    },
                    line = self.read.next_line() => match line? {
                        None if !input_open => return Ok(()),
                        None if self.frontend==crate::daemon_socket::AttachFrontend::Acp => bail!("The Codewhale Runtime this ACP session attached to (pid {}) has exited, so the session has ended. Restart the agent to continue; pending operation outcomes may be uncertain.", self.receipt.pid),
                        None => bail!("selected owner connection closed; pending operation outcomes may be uncertain"),
                        Some(line) => {
                            let _: Value = serde_json::from_str(&line).context("invalid owner response")?;
                            write_control_line(&mut output, line.as_bytes()).await?;
                        }
                    }
                }
            }
        }
    }

    #[cfg(all(test, unix))]
    mod tests {
        use super::*;

        #[test]
        fn owner_receipt_reobserves_only_typed_unlinked_file_refusals() {
            let retired =
                anyhow::Error::new(codewhale_config::private_directory::UnlinkedPrivateFile)
                    .context("bounded owner receipt read");
            assert!(owner_receipt_error(retired).is::<PublicationChangedBeforeAttach>());
            for refused in [
                anyhow::anyhow!("private file was unlinked before validation"),
                anyhow::anyhow!("xAI OAuth file must not have multiple filesystem links"),
                std::io::Error::from(std::io::ErrorKind::PermissionDenied).into(),
                anyhow::anyhow!("owner attach write deadline expired; outcome uncertain"),
            ] {
                assert!(!owner_receipt_error(refused).is::<PublicationChangedBeforeAttach>());
            }
        }
    }
}

#[cfg(any(unix, windows))]
pub use platform::{
    HostOwnerProbe, OwnerClient, PublicationChangedBeforeAttach, UnavailablePublication, connect,
    connect_acp_if_published, connect_if_published, connect_listener_if_published,
    connect_scoped_control_if_published, connect_selected_acp_if_published,
    probe_owner_for_host_startup, publication_is_absent,
};

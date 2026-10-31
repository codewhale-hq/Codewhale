//! Windows kernel pipe frontend for the existing authenticated control loop.
//! Pipe names route; private receipts plus held kernel process/token identity
//! select the actual owner. No bearer or Engine authority crosses this wire.

use crate::daemon_client::UnavailablePublication;
use crate::daemon_socket::{
    AuthorizedPeer, ConnectionTasks, DaemonShutdownHandle, DaemonSocketError, DaemonSocketOptions,
    SocketPathInputs, connection_context, owner_work, run_authorized_connection, socket_owner_work,
};
use crate::{AppState, AppTransport, build_state_off_runtime};
use anyhow::{Context as _, Result};
use codewhale_config::private_directory::PrivateDirectory;
use codewhale_config::windows_identity::{CurrentWindowsUser, OwnerOnlyAcl, WindowsPeerProcess};
use codewhale_protocol::RuntimeOwnerReceipt;
use std::fs::File;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
use tokio::sync::watch;

pub(crate) fn owner_directory() -> Result<PathBuf> {
    let input = SocketPathInputs::from_environment(None)?;
    let home = input
        .codewhale_home_override
        .or_else(|| {
            input
                .user_home
                .map(|p| p.join(codewhale_paths::CODEWHALE_APP_DIR))
        })
        .context("selected Windows owner home unavailable")?;
    anyhow::ensure!(
        home.is_absolute(),
        "selected Windows owner home must be absolute"
    );
    Ok(home.join("run"))
}

pub(crate) fn selected_pipe_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        let text = path.to_str().context("invalid Windows pipe name")?;
        anyhow::ensure!(
            text.starts_with(r"\\.\pipe\")
                && text.len() < 240
                && !text[9..].contains(['\\', '/', '\0']),
            "control pipe must be one local named-pipe component"
        );
        return Ok(path);
    }
    use sha2::{Digest, Sha256};
    let home = owner_directory()?;
    let principal = CurrentWindowsUser::open()?.sid_string()?;
    let mut digest = Sha256::new();
    digest.update(b"Codewhale/owner-pipe/v1\0");
    digest.update(home.as_os_str().as_encoded_bytes());
    digest.update(b"\0");
    digest.update(principal.as_bytes());
    let suffix: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(PathBuf::from(format!(r"\\.\pipe\codewhale-owner-{suffix}")))
}

fn create_pipe(path: &Path, first: bool) -> Result<NamedPipeServer> {
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
    let acl = OwnerOnlyAcl::new(FILE_ALL_ACCESS, false)?;
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .max_instances(65)
        .in_buffer_size(16384)
        .out_buffer_size(16384);
    // Security is supplied at creation. There is never a default-DACL window.
    acl.with_security_attributes(|attributes| unsafe {
        options
            .create_with_security_attributes_raw(path, attributes)
            .map_err(Into::into)
    })
}

struct ReceiptGuard {
    parent: Arc<PrivateDirectory>,
    file: Option<File>,
    owner_state: Option<AppState>,
}
impl ReceiptGuard {
    async fn retire(&mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            let parent = self.parent.clone();
            let owner_state = self.owner_state.take();
            tokio::spawn(async move {
                socket_owner_work(owner_state, move || {
                    parent.retire_private_receipt("daemon.owner.json", &file)?;
                    Ok(())
                })
                .await
            })
            .await??;
        }
        Ok(())
    }
}
impl Drop for ReceiptGuard {
    fn drop(&mut self) {
        let Some(file) = self.file.take() else { return };
        let parent = self.parent.clone();
        let owner_state = self.owner_state.take();
        let retire = move || -> Result<()> {
            let _held_owner = owner_state;
            parent.retire_private_receipt("daemon.owner.json", &file)?;
            Ok(())
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Err(error) = owner_work(retire).await {
                    tracing::warn!(%error,"private Windows owner retirement uncertain");
                }
            });
        } else if let Err(error) = retire() {
            tracing::warn!(%error,"private Windows owner retirement uncertain");
        }
    }
}

pub struct DaemonSocket {
    pipe: NamedPipeServer,
    path: PathBuf,
    state: AppState,
    shutdown: Arc<watch::Sender<bool>>,
    owner: Option<RuntimeOwnerReceipt>,
    receipt: Option<ReceiptGuard>,
}
impl DaemonSocket {
    pub fn local_path(&self) -> &Path {
        &self.path
    }
    pub fn shutdown_handle(&self) -> DaemonShutdownHandle {
        DaemonShutdownHandle(self.shutdown.clone())
    }
    pub async fn serve(self) -> Result<(), DaemonSocketError> {
        let Self {
            mut pipe,
            path,
            state,
            shutdown,
            owner,
            mut receipt,
        } = self;
        let context = connection_context(
            state,
            path.clone(),
            DaemonShutdownHandle(shutdown.clone()),
            owner,
        )
        .await
        .map_err(DaemonSocketError::State)?;
        let mut stopping = shutdown.subscribe();
        let mut connections = ConnectionTasks::default();
        loop {
            if *stopping.borrow() {
                break;
            }
            tokio::select! {
                result=pipe.connect()=>{
                    result.map_err(|error|DaemonSocketError::State(error.into()))?;
                    if connections.at_capacity() {pipe.disconnect().map_err(|error|DaemonSocketError::State(error.into()))?;continue;}
                    // Retain the connected instance until the successor exists;
                    // the named endpoint is never released between accepts.
                    let successor_path=path.clone();
                    let successor=owner_work(move ||create_pipe(&successor_path,false)).await.map_err(DaemonSocketError::State)?;
                    let accepted=std::mem::replace(&mut pipe,successor);
                    let context=context.clone();
                    connections.spawn(async move {
                        use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
                        let mut pid=0;
                        if unsafe {GetNamedPipeClientProcessId(accepted.as_raw_handle(),&mut pid)}==0 {return}
                        let process=match owner_work(move ||WindowsPeerProcess::open_current_user(pid)).await {Ok(process)=>process,Err(_)=>return};
                        let start=process.start().to_string();let (read,write)=tokio::io::split(accepted);
                        run_authorized_connection(context,read,write,AuthorizedPeer {pid,start,process}).await;
                    });
                }
                changed=stopping.changed()=>{if changed.is_err() || *stopping.borrow() {break}}
            }
        }
        connections.shutdown().await;
        drop(pipe);
        if let Some(receipt) = receipt.as_mut() {
            receipt.retire().await.map_err(DaemonSocketError::State)?;
        }
        Ok(())
    }
}

pub async fn bind_daemon_socket(
    options: DaemonSocketOptions,
) -> Result<DaemonSocket, DaemonSocketError> {
    bind(options, None, None).await
}
pub(crate) async fn bind_captured_owner(
    state: AppState,
    owner: RuntimeOwnerReceipt,
    recovery: Option<Arc<UnavailablePublication>>,
) -> Result<Option<DaemonSocket>, DaemonSocketError> {
    // The named-pipe endpoint is bounded by the ACL the process applies, which
    // NTFS always stores, so there is nothing a filesystem can fail to express
    // here and nothing to degrade: `Some` is the only reachable answer. The
    // shape matches the Unix one so the caller has a single code path.
    bind(
        DaemonSocketOptions {
            socket_path: Some(owner.socket_path.clone()),
            config_path: None,
        },
        Some((state, owner)),
        recovery,
    )
    .await
    .map(Some)
}
async fn bind(
    options: DaemonSocketOptions,
    captured: Option<(AppState, RuntimeOwnerReceipt)>,
    recovery: Option<Arc<UnavailablePublication>>,
) -> Result<DaemonSocket, DaemonSocketError> {
    let owner_state = captured.as_ref().map(|(state, _)| state.clone());
    let explicit = options.socket_path;
    let path = owner_work(move || selected_pipe_path(explicit))
        .await
        .map_err(DaemonSocketError::State)?;
    let recovered = match recovery {
        Some(previous) => {
            let owner = &captured
                .as_ref()
                .ok_or_else(|| {
                    DaemonSocketError::State(anyhow::anyhow!(
                        "stale publication requires the actual captured Runtime owner"
                    ))
                })?
                .1;
            previous
                .require_same_selection(owner, &path)
                .map_err(DaemonSocketError::State)?;
            Some(
                socket_owner_work(owner_state.clone(), move || {
                    let parent = previous.publication.parent.clone();
                    anyhow::ensure!(
                        parent.is_at_selected_path()?,
                        "selected Windows owner parent changed"
                    );
                    anyhow::ensure!(
                        previous.revalidate_receipt()?,
                        "captured stale owner receipt changed before retirement"
                    );
                    anyhow::ensure!(
                        parent.retire_private_receipt(
                            &previous.publication.receipt_name,
                            &previous.publication.file
                        )?,
                        "captured stale owner receipt changed during retirement"
                    );
                    drop(previous);
                    Ok(parent)
                })
                .await
                .map_err(DaemonSocketError::State)?,
            )
        }
        None => None,
    };
    let selected = path.clone();
    let pipe = socket_owner_work(owner_state, move || create_pipe(&selected, true))
        .await
        .map_err(DaemonSocketError::State)?;
    let (state, owner) = match captured {
        Some((state, owner)) => (state, Some(owner)),
        None => (
            build_state_off_runtime(options.config_path, None, AppTransport::Socket)
                .await
                .map_err(DaemonSocketError::State)?,
            None,
        ),
    };
    let receipt = if let Some(owner) = owner.as_ref() {
        let owner = owner.clone();
        let owner_state = state.clone();
        Some(
            socket_owner_work(Some(owner_state.clone()), move || {
                let parent = match recovered {
                    Some(parent) => parent,
                    None => Arc::new(PrivateDirectory::admit(&owner_directory()?)?),
                };
                let mut guard = ReceiptGuard {
                    parent: parent.clone(),
                    file: None,
                    owner_state: Some(owner_state),
                };
                anyhow::ensure!(
                    parent.is_at_selected_path()?,
                    "selected Windows owner parent changed"
                );
                anyhow::ensure!(
                    parent
                        .read_private_receipt("daemon.owner.json", 16384)?
                        .is_none(),
                    "owner publication appeared during bootstrap; refusing replacement"
                );
                let bytes = serde_json::to_vec(&owner)?;
                anyhow::ensure!(bytes.len() <= 16384, "Windows owner receipt exceeds bound");
                parent.write_owned_file("daemon.owner.json", &bytes, false)?;
                guard.file = Some(
                    parent
                        .read_private_receipt("daemon.owner.json", 16384)?
                        .context("Windows owner publication unavailable")?
                        .1,
                );
                anyhow::ensure!(
                    parent.is_at_selected_path()?,
                    "selected Windows owner parent changed during publication"
                );
                Ok(guard)
            })
            .await
            .map_err(DaemonSocketError::State)?,
        )
    } else {
        None
    };
    let (shutdown, _) = watch::channel(false);
    Ok(DaemonSocket {
        pipe,
        path,
        state,
        shutdown: Arc::new(shutdown),
        owner,
        receipt,
    })
}

pub(crate) async fn connect_owner(
    owner: &RuntimeOwnerReceipt,
) -> Result<(
    tokio::net::windows::named_pipe::NamedPipeClient,
    WindowsPeerProcess,
)> {
    let path = owner.socket_path.clone();
    let expected = owner.clone();
    owner_work(move || {
        let client = ClientOptions::new()
            .open(&path)
            .context("opening selected Windows owner; refusing fallback")?;
        use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
        let mut pid = 0;
        anyhow::ensure!(
            unsafe { GetNamedPipeServerProcessId(client.as_raw_handle(), &mut pid) } != 0
                && pid == expected.pid,
            "connected Windows server is not the selected owner"
        );
        let process = WindowsPeerProcess::open_current_user(pid)?;
        anyhow::ensure!(
            process.start() == expected.process_start && process.principal()? == expected.principal,
            "selected Windows server generation/principal changed"
        );
        Ok((client, process))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn first_instance_pipe_refuses_competing_listener_and_reopens_after_close() {
        let path = PathBuf::from(format!(
            r"\\.\pipe\codewhale-owner-test-{}",
            uuid::Uuid::new_v4()
        ));
        let first = create_pipe(&path, true).expect("current-user protected first pipe");
        assert!(
            create_pipe(&path, true).is_err(),
            "never join/substitute another selected owner instance"
        );
        drop(first);
        let reopened =
            create_pipe(&path, true).expect("closed instance releases exact kernel endpoint");
        drop(reopened);
    }

    #[test]
    fn local_pipe_selector_refuses_remote_and_nested_names() {
        for name in [
            r"\\remote\pipe\codewhale",
            r"\\.\pipe\nested\owner",
            r"\\.\pipe\owner/name",
        ] {
            assert!(selected_pipe_path(Some(PathBuf::from(name))).is_err());
        }
    }

    #[test]
    fn held_peer_captures_actual_current_user_process_generation() {
        assert!(WindowsPeerProcess::open_current_user(0).is_err());
        let peer =
            WindowsPeerProcess::open_current_user(std::process::id()).expect("held process/token");
        assert_eq!(peer.pid(), std::process::id());
        assert!(peer.start().starts_with("windows:"));
        assert_eq!(
            peer.principal().unwrap(),
            CurrentWindowsUser::open().unwrap().sid_string().unwrap()
        );
        peer.check_current_user()
            .expect("held generation remains current");
    }
}

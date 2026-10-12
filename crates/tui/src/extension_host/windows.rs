//! Native-only Windows launch boundary. LPAC with only the `registryRead`
//! capability (Winsock cannot initialize without it) supplies
//! filesystem/network denial; the existing Job supplies lifetime and memory.
//! A fresh profile never inherits ACLs from a retired host. Profiles/assets
//! created here are disposed by their exact owner after the process ends;
//! crash leftovers are not guessed at or swept. Windows may also provide its
//! private AppContainer scratch/registry, separate from Core-owned state.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Read};
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows_sys::Win32::Foundation::{
    ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE, LocalFree, WAIT_ABANDONED,
    WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::Authorization::{GRANT_ACCESS, REVOKE_ACCESS};
use windows_sys::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile,
};
use windows_sys::Win32::Security::{
    ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, FreeSid, GetTokenInformation,
    OBJECT_INHERIT_ACE, PSID, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
    TOKEN_APPCONTAINER_INFORMATION, TOKEN_GROUPS, TOKEN_QUERY, TOKEN_STATISTICS,
    TokenAppContainerSid, TokenCapabilities, TokenIsAppContainer,
    TokenIsLessPrivilegedAppContainer, TokenStatistics,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAG_OVERLAPPED,
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_READ_ATTRIBUTES,
    OPEN_EXISTING, PIPE_ACCESS_INBOUND, PIPE_ACCESS_OUTBOUND, READ_CONTROL, WRITE_DAC,
};
use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_WAIT};
use windows_sys::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateMutexW, CreateProcessW,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, OpenProcessToken,
    PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY,
    PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, ReleaseMutex, ResumeThread,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows_sys::Win32::System::WindowsProgramming::PROCESS_CREATION_CHILD_PROCESS_OVERRIDE;

use crate::dependencies::{HostRuntime, HostRuntimeKind};
use crate::fleet::files::WindowsDirectory;
use crate::process_tree::ProcessTree;

const PROBE_SOURCE: &str = include_str!("../../extension-host/src/windows-sandbox-probe.mjs");
const PROBE_DEADLINE: Duration = Duration::from_secs(15);
// Windows SDK winnt.h; the documented LPAC startup attribute opts out of the
// ambient ALL APPLICATION PACKAGES group. Not a fabricated token status.
const PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT: u32 = 0x1;
const MAX_GRANT_ENTRIES: usize = 65_536;
const MAX_GRANT_SCOPES: usize = 1024;
/// Empty Bun config in the granted runtime-copy directory (see sandbox_args).
const EMPTY_BUN_CONFIG: &str = "empty-bunfig.toml";
const ACL_EDIT_WAIT_MS: u32 = 30_000;

// A kernel mutex covers admission and retirement in every Core process in
// this Windows logon/session. A process-local mutex cannot protect the shared
// host home's read/merge/write DACL transactions from another Core process.
struct AclEditGuard {
    mutex: OwnedHandle,
    // Windows mutex ownership belongs to the acquiring thread.
    _thread_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl AclEditGuard {
    fn acquire() -> io::Result<Self> {
        let mut token = null_mut();
        // SAFETY: the process pseudo-handle is valid and token is an out pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut statistics = TOKEN_STATISTICS::default();
        let mut read = 0;
        // SAFETY: the aligned SDK struct and size match this fixed token query.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenStatistics,
                (&mut statistics as *mut TOKEN_STATISTICS).cast(),
                size_of::<TOKEN_STATISTICS>() as u32,
                &mut read,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if read != size_of::<TOKEN_STATISTICS>() as u32 {
            return Err(io::Error::other("invalid ACL lock logon identity"));
        }
        let logon = statistics.AuthenticationId;
        let name = wide(OsStr::new(&format!(
            r"Local\Codewhale.Native.AclEdits.{:08x}{:08x}",
            logon.HighPart, logon.LowPart,
        )))?;
        // Default creator DACL, non-inheritable handle: never add a Native
        // profile or AppContainer-group grant to Core's synchronization.
        let mutex = unsafe { CreateMutexW(null(), 0, name.as_ptr()) };
        if mutex.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mutex = unsafe { OwnedHandle::from_raw_handle(mutex) };
        match unsafe { WaitForSingleObject(mutex.as_raw_handle(), ACL_EDIT_WAIT_MS) } {
            // Abandonment also acquires ownership. No cached transaction state
            // is trusted: callers pin each object and reread/validate its actual
            // kernel DACL before editing, including after a prior owner crashed.
            WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(Self {
                mutex,
                _thread_bound: std::marker::PhantomData,
            }),
            WAIT_TIMEOUT => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Native ACL edit lock timed out",
            )),
            _ => Err(io::Error::last_os_error()),
        }
    }
}

impl Drop for AclEditGuard {
    fn drop(&mut self) {
        // SAFETY: this thread acquired the mutex and the handle is still owned.
        if unsafe { ReleaseMutex(self.mutex.as_raw_handle()) } == 0 {
            tracing::warn!(
                "Native ACL edit lock release failed: {}",
                io::Error::last_os_error()
            );
        }
    }
}

#[derive(Clone)]
pub(crate) struct NativeSandbox {
    profile: Arc<Profile>,
    _assets: Arc<tempfile::TempDir>,
    pub program: PathBuf,
    data: PathBuf,
}

impl std::fmt::Debug for NativeSandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSandbox")
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

struct Profile {
    name: Vec<u16>,
    sid: PSID,
    grants: Mutex<BTreeMap<PathBuf, GrantScope>>,
    cleanup_runtime: Option<tokio::runtime::Handle>,
}
struct GrantScope {
    identity: (u32, u64),
    kind: GrantKind,
}
/// What one recorded ACE covers, so retirement edits exactly that object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GrantKind {
    /// One regular file.
    File,
    /// One directory's own entry list: no inheritance, no children.
    Directory,
    /// A directory and everything below it (inherited and explicit ACEs).
    Tree,
}
// A retained exact profile SID, never a parsed/guessed orphan identity.
struct RetiredProfile {
    name: Vec<u16>,
    sid: PSID,
    grants: BTreeMap<PathBuf, GrantScope>,
}
// SAFETY: retired state is exclusively owned; the SID is freed only after the
// existing runtime's blocking cleanup worker has finished its exact grants.
unsafe impl Send for RetiredProfile {}
// SAFETY: SID/name are immutable after successful creation. Win32 consumes
// borrowed SID memory synchronously; only the final Arc drop frees it.
unsafe impl Send for Profile {}
unsafe impl Sync for Profile {}

impl Profile {
    fn create() -> io::Result<Self> {
        let name = wide(OsStr::new(&format!(
            "Codewhale.Native.{}",
            uuid::Uuid::new_v4()
        )))?;
        let mut sid = null_mut();
        // SAFETY: nul-terminated owned strings and an initialized out pointer.
        let result = unsafe {
            CreateAppContainerProfile(
                name.as_ptr(),
                name.as_ptr(),
                name.as_ptr(),
                null(),
                0,
                &mut sid,
            )
        };
        if result < 0 {
            return Err(io::Error::other(format!(
                "CreateAppContainerProfile HRESULT {result:#x}"
            )));
        }
        if sid.is_null() {
            // No existing/user profile is adopted or removed on a collision.
            unsafe {
                DeleteAppContainerProfile(name.as_ptr());
            }
            return Err(io::Error::other("AppContainer profile has no SID"));
        }
        Ok(Self {
            name,
            sid,
            grants: Mutex::new(BTreeMap::new()),
            cleanup_runtime: tokio::runtime::Handle::try_current().ok(),
        })
    }
}

impl Profile {
    fn remember(&self, path: &Path, file: &File, kind: GrantKind) -> io::Result<()> {
        let value = crate::plugins::windows_file_identity(file)?;
        let mut grants = self
            .grants
            .lock()
            .map_err(|_| io::Error::other("profile grant accounting poisoned"))?;
        if let Some(old) = grants.get(path) {
            if old.identity != (value.volume, value.index) || old.kind != kind {
                return Err(io::Error::other(
                    "recorded profile grant identity changed; refusing overwrite",
                ));
            }
            return Ok(());
        }
        if grants.len() == MAX_GRANT_SCOPES {
            return Err(io::Error::other("profile exceeds 1024 exact grant scopes"));
        }
        grants.insert(
            path.to_path_buf(),
            GrantScope {
                identity: (value.volume, value.index),
                kind,
            },
        );
        Ok(())
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let grants = std::mem::take(
            self.grants
                .get_mut()
                .unwrap_or_else(|error| error.into_inner()),
        );
        let retired = RetiredProfile {
            name: std::mem::take(&mut self.name),
            sid: self.sid,
            grants,
        };
        // Same existing Tokio scheduler; no new runtime/authority. The worker
        // owns the SID until retirement completes, even during cancellation.
        if let Some(runtime) = self.cleanup_runtime.take() {
            runtime.spawn_blocking(move || retired.dispose());
        } else {
            // Synchronous platform callers have no runtime; work remains
            // bounded to the exact recorded roots, never a profile sweep.
            retired.dispose();
        }
    }
}
impl RetiredProfile {
    fn dispose(self) {
        drop(self);
    }
}
// Drop also retires a task cancelled before its blocking worker starts. Once
// started, spawn_blocking cannot abandon this exclusively owned cleanup.
impl Drop for RetiredProfile {
    fn drop(&mut self) {
        for (path, scope) in &self.grants {
            if let Err(error) = retire_scope(path, scope, self.sid) {
                tracing::warn!(path = %path.display(), "Native exact-profile ACL retirement failed: {error}");
            }
        }
        // The OS profile and SID stay alive until exact grant retirement ends.
        // Crash remnants or refused/replaced roots are not guessed at/swept.
        unsafe {
            let result = DeleteAppContainerProfile(self.name.as_ptr());
            if result < 0 {
                tracing::warn!("Native AppContainer profile disposal failed ({result:#x})");
            }
            FreeSid(self.sid);
        }
    }
}

fn retire_scope(root: &Path, scope: &GrantScope, sid: PSID) -> io::Result<()> {
    let _serial = AclEditGuard::acquire()?;
    if !root.try_exists()? {
        return Ok(());
    }
    let directory = scope.kind != GrantKind::File;
    let root_pin = if directory {
        WindowsDirectory::open_acl(root)?
    } else {
        WindowsDirectory::open(
            root.parent()
                .ok_or_else(|| io::Error::other("grant has no parent"))?,
        )?
    };
    let root_file;
    let file = if directory {
        root_pin.acl_handle()?
    } else {
        root_file = acl_file_with_share(root, 1 | 2 | 4)?;
        &root_file
    };
    let value = crate::plugins::windows_file_identity(file)?;
    if (value.volume, value.index) != scope.identity {
        return Err(io::Error::other(
            "recorded grant object was replaced; refusing cleanup",
        ));
    }
    // Remove the parent's inheritable grant before walking, so newly created
    // children cannot inherit this retired SID. The kernel write never
    // propagates; every child is independently fenced and updated.
    edit_acl(file, sid, 0, 0, REVOKE_ACCESS)?;
    if scope.kind != GrantKind::Tree {
        return Ok(());
    }
    let mut pending = Vec::new();
    let mut count = 1;
    enqueue_pinned_children(root, &root_pin, &mut pending, count)?;
    let mut failure = None;
    while let Some((path, parent_pin)) = pending.pop() {
        count += 1;
        if count > MAX_GRANT_ENTRIES {
            return Err(io::Error::other(
                "profile retirement exceeds 65536 entries in a recorded root",
            ));
        }
        let mut work = || -> io::Result<()> {
            let metadata = fs::symlink_metadata(&path)?;
            if crate::plugins::metadata_is_link_or_reparse(&metadata) {
                return Err(io::Error::other(
                    "profile retirement refuses links/reparse points",
                ));
            }
            if metadata.is_dir() {
                let directory = parent_pin.open_acl_child(
                    path.file_name()
                        .ok_or_else(|| io::Error::other("grant has no filename"))?,
                )?;
                edit_acl(directory.acl_handle()?, sid, 0, 0, REVOKE_ACCESS)?;
                enqueue_pinned_children(&path, &directory, &mut pending, count)
            } else if metadata.is_file() {
                if parent_pin.child_path(
                    path.file_name()
                        .ok_or_else(|| io::Error::other("grant has no filename"))?,
                )? != path
                {
                    return Err(io::Error::other("grant left its pinned parent"));
                }
                // Retiring this exact SID may overlap a new host's data
                // writes. The handle edits this exact object while the pinned
                // parent chain fences its path, so allow those legitimate
                // writers/renames; never restore an old whole ACL.
                let file = acl_file_with_share(&path, 1 | 2 | 4)?;
                edit_acl(&file, sid, 0, 0, REVOKE_ACCESS)?;
                Ok(())
            } else {
                Err(io::Error::other(
                    "profile retirement refuses a nonregular entry",
                ))
            }
        };
        match work() {
            Ok(()) => {}
            Err(error) => {
                failure = Some(error);
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
fn enqueue_children(path: &Path, pending: &mut Vec<PathBuf>, visited: usize) -> io::Result<()> {
    for entry in fs::read_dir(path)? {
        // Bound materialized pending entries, not only already visited paths.
        if visited + pending.len() >= MAX_GRANT_ENTRIES {
            return Err(io::Error::other(
                "profile directory traversal exceeds 65536 entries",
            ));
        }
        pending.push(entry?.path());
    }
    Ok(())
}
fn enqueue_pinned_children(
    path: &Path,
    pin: &WindowsDirectory,
    pending: &mut Vec<(PathBuf, WindowsDirectory)>,
    visited: usize,
) -> io::Result<()> {
    let mut children = Vec::new();
    // Reuse the exact admission/retirement bound, including already pending
    // entries. Each child keeps its actual direct parent pinned until visited.
    enqueue_children(path, &mut children, visited + pending.len())?;
    pending.extend(children.into_iter().map(|child| (child, pin.clone())));
    Ok(())
}
/// Name the admission step in an error, so a refusal says where it stopped.
fn in_step<T>(step: &str, result: io::Result<T>) -> io::Result<T> {
    result.map_err(|error| io::Error::new(error.kind(), format!("{step}: {error}")))
}
fn acl_file(path: &Path) -> io::Result<File> {
    acl_file_with_share(path, 1)
}
fn acl_file_with_share(path: &Path, share: u32) -> io::Result<File> {
    let file = fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC | FILE_READ_ATTRIBUTES)
        .share_mode(share)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || crate::plugins::metadata_is_link_or_reparse(&metadata)
        || crate::plugins::windows_file_identity(&file)?.links != 1
    {
        return Err(io::Error::other(
            "profile ACL operation refuses a linked/nonregular file",
        ));
    }
    Ok(file)
}

impl NativeSandbox {
    /// Blocking, invoked by the existing manager's bounded launch worker.
    pub(crate) fn prepare(
        runtime: &HostRuntime,
        bundle: &Path,
        home: &Path,
        data: &Path,
        memory_cap: u64,
    ) -> Result<Self, String> {
        let work = || -> io::Result<Self> {
            let profile = Arc::new(Profile::create()?);
            let parent = home.join("extension-host").join("native-launch");
            fs::create_dir_all(&parent)?;
            let _parent = in_step("pin launch parent", WindowsDirectory::open(&parent))?;
            let assets = Arc::new(
                tempfile::Builder::new()
                    .prefix("host-")
                    .tempdir_in(&parent)?,
            );
            let program = assets.path().join("runtime.exe");
            fs::write(assets.path().join(EMPTY_BUN_CONFIG), b"")?;
            // The selected runtime is copied, never granted access in an
            // installation/user directory. Pin its opened bytes while copying.
            let original = runtime.path.canonicalize()?;
            let source_pin = in_step(
                "pin runtime directory",
                WindowsDirectory::open(
                    original
                        .parent()
                        .ok_or_else(|| io::Error::other("runtime has no parent"))?,
                ),
            )?;
            let mut source = in_step(
                "open runtime",
                crate::plugins::manifest::open_bundle_file(&original),
            )?;
            if source.metadata()?.len() > 512 * 1024 * 1024 {
                return Err(io::Error::other(
                    "selected runtime exceeds 512 MiB launch limit",
                ));
            }
            let mut target = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&program)?;
            io::copy(&mut source, &mut target)?;
            target.sync_all()?;
            drop(target);
            drop(source_pin);
            let sandbox = Self {
                profile,
                _assets: assets,
                program,
                data: data.to_path_buf(),
            };
            in_step(
                "grant runtime copy",
                sandbox.grant_tree(sandbox._assets.path(), false),
            )?;
            // Exact canonical bundle file only: never recursively grant its
            // parent, which also contains the Builtin's private data.
            in_step(
                "grant host bundle",
                sandbox.grant_file(bundle, FILE_GENERIC_READ | FILE_GENERIC_EXECUTE, true),
            )?;
            // Bun's resolver lists the directory holding its entry file and
            // refuses the entry when it cannot ("Module not found"); it treats
            // unreadable ancestors as opaque. List that one directory only:
            // no inheritance, so its other files (embedded builtin modules,
            // notices) stay denied, and the Builtin's data beside it too.
            // Node loads the entry by path and needs no listing.
            if runtime.kind == HostRuntimeKind::Bun && !runtime.compiled {
                in_step(
                    "list host bundle directory",
                    sandbox.grant_directory(
                        bundle
                            .parent()
                            .ok_or_else(|| io::Error::other("host bundle has no parent"))?,
                    ),
                )?;
            }
            fs::create_dir_all(data.join("tmp"))?;
            in_step("grant data directory", sandbox.grant_tree(data, true))?;
            in_step("isolation probe", sandbox.probe(runtime, memory_cap))?;
            Ok(sandbox)
        };
        work().map_err(|error| format!("Windows Native isolation could not be verified: {error}"))
    }

    /// Only call after the existing Rust Native receipt/hash check. The root
    /// is a reviewed staged snapshot, never the mutable source or a link.
    pub(crate) fn admit_root(&self, root: &Path) -> Result<(), String> {
        self.grant_tree(root, false)
            .map_err(|error| format!("cannot admit reviewed Windows bundle: {error}"))
    }

    fn grant_tree(&self, root: &Path, writable: bool) -> io::Result<()> {
        let _serial = AclEditGuard::acquire()?;
        let root_pin = WindowsDirectory::open_acl(root)?;
        self.profile
            .remember(root, root_pin.acl_handle()?, GrantKind::Tree)?;
        let access = FILE_GENERIC_READ
            | FILE_GENERIC_EXECUTE
            | if writable {
                FILE_GENERIC_WRITE | windows_sys::Win32::Storage::FileSystem::DELETE
            } else {
                0
            };
        set_acl(
            root_pin.acl_handle()?,
            self.profile.sid,
            access,
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
        )?;
        let mut pending = Vec::new();
        let mut count = 1;
        enqueue_pinned_children(root, &root_pin, &mut pending, count)?;
        while let Some((path, parent_pin)) = pending.pop() {
            count += 1;
            if count > MAX_GRANT_ENTRIES {
                return Err(io::Error::other("sandbox grant tree exceeds 65536 entries"));
            }
            let metadata = fs::symlink_metadata(&path)?;
            if crate::plugins::metadata_is_link_or_reparse(&metadata) {
                return Err(io::Error::other(
                    "sandbox grant refuses a link/reparse point",
                ));
            }
            if metadata.is_dir() {
                let pin = parent_pin.open_acl_child(
                    path.file_name()
                        .ok_or_else(|| io::Error::other("grant has no filename"))?,
                )?;
                set_acl(
                    pin.acl_handle()?,
                    self.profile.sid,
                    access,
                    OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
                )?;
                enqueue_pinned_children(&path, &pin, &mut pending, count)?;
            } else if metadata.is_file() {
                self.grant_pinned_file(&parent_pin, &path, access, false)?;
            } else {
                return Err(io::Error::other(
                    "sandbox grant refuses a non-regular entry",
                ));
            }
        }
        Ok(())
    }

    /// Read/list/traverse on exactly one directory object. Not inherited:
    /// each child keeps only the grants it is given explicitly.
    fn grant_directory(&self, directory: &Path) -> io::Result<()> {
        let _serial = AclEditGuard::acquire()?;
        let pin = WindowsDirectory::open_acl(directory)?;
        self.profile
            .remember(directory, pin.acl_handle()?, GrantKind::Directory)?;
        set_acl(
            pin.acl_handle()?,
            self.profile.sid,
            FILE_GENERIC_READ | FILE_GENERIC_EXECUTE,
            0,
        )
    }

    fn grant_file(&self, path: &Path, access: u32, remember: bool) -> io::Result<()> {
        let _serial = AclEditGuard::acquire()?;
        let pin = WindowsDirectory::open(
            path.parent()
                .ok_or_else(|| io::Error::other("file has no parent"))?,
        )?;
        self.grant_pinned_file(&pin, path, access, remember)
    }

    // The owning grant entrypoint holds AclEditGuard. Tree files reuse the parent
    // chain rather than reopen pinned directory objects. The
    // child_path comparison is only a lexical invariant; the protection is the
    // held no-write/no-delete parent chain plus acl_file's no-follow,
    // regular, single-link checks.
    fn grant_pinned_file(
        &self,
        parent_pin: &WindowsDirectory,
        path: &Path,
        access: u32,
        remember: bool,
    ) -> io::Result<()> {
        if parent_pin.child_path(
            path.file_name()
                .ok_or_else(|| io::Error::other("file has no filename"))?,
        )? != path
        {
            return Err(io::Error::other("grant left its pinned parent"));
        }
        let file = acl_file(path)?;
        if remember {
            self.profile.remember(path, &file, GrantKind::File)?;
        }
        set_acl(&file, self.profile.sid, access, 0)
    }

    fn probe(&self, runtime: &HostRuntime, memory_cap: u64) -> io::Result<()> {
        let outside = tempfile::tempdir()?;
        let mut reads = Vec::new();
        for name in [
            "codex-auth.json",
            "dsh-credentials.yaml",
            "builtin-private-state.json",
        ] {
            let path = outside.path().join(name);
            fs::write(&path, b"non-secret-denial-control")?;
            reads.push(path);
        }
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let overrides = [
            (
                "CODEWHALE_WINDOWS_PROBE_INSIDE".to_string(),
                self.data
                    .join(format!(".probe-{}", uuid::Uuid::new_v4()))
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "CODEWHALE_WINDOWS_PROBE_OUTSIDE".to_string(),
                outside
                    .path()
                    .join("forbidden-write")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "CODEWHALE_WINDOWS_PROBE_READS".to_string(),
                serde_json::to_string(&reads)?,
            ),
            (
                "CODEWHALE_WINDOWS_PROBE_PORT".to_string(),
                listener.local_addr()?.port().to_string(),
            ),
        ];
        let runtime_env = super::supervisor::runtime_env(runtime.kind);
        let mut args = super::supervisor::runtime_args(runtime);
        if runtime.compiled {
            args.extend(["--tier=plugin".into(), "--windows-sandbox-probe".into()]);
        } else {
            args.extend([
                "--input-type=module".into(),
                "-e".into(),
                format!("{PROBE_SOURCE}\nconsole.log(JSON.stringify(await windowsSandboxProbe()))"),
            ]);
        }
        // Serialize the same sandbox-safe argv used to launch the parent.
        let args = sandbox_args(&args, self._assets.path());
        let child_args = (
            "CODEWHALE_WINDOWS_PROBE_CHILD_ARGS".to_string(),
            serde_json::to_string(&args)?,
        );
        let env = crate::child_env::sanitized_plugin_mcp_env_from(
            std::env::vars_os(),
            runtime_env
                .iter()
                .chain(&overrides)
                .chain(std::iter::once(&child_args))
                .map(|(k, v)| (k.as_str(), v.as_str())),
        );
        let mut probe = self.spawn_inner(&args, &env, memory_cap, false)?;
        // Close input immediately. Fixed output is under 512 bytes; a runtime
        // substitution that floods a pipe cannot evade the wait deadline.
        drop(probe.stdin);
        let status = match probe.child.wait_timeout(PROBE_DEADLINE) {
            Ok(status) => status,
            Err(error) => {
                let _ = probe.child.tree.kill();
                let _ = probe.child.wait_timeout(Duration::from_secs(2));
                return Err(error);
            }
        };
        let mut stdout = Vec::new();
        File::from(probe.stdout)
            .take(4097)
            .read_to_end(&mut stdout)?;
        let mut stderr = Vec::new();
        File::from(probe.stderr)
            .take(4097)
            .read_to_end(&mut stderr)?;
        if !status.success() || stdout.len() > 4096 {
            return Err(io::Error::other(format!(
                "isolation probe failed ({status}): {}",
                String::from_utf8_lossy(&stderr)
            )));
        }
        let expected = serde_json::json!({"version":1,"data_roundtrip":true,"outside_read_denied":true,"outside_write_denied":true,"network_denied":true,"descendant_denied":true});
        if serde_json::from_slice::<serde_json::Value>(&stdout)? != expected
            || listener.accept().is_ok()
        {
            return Err(io::Error::other(
                "sandbox probe returned no exact allow/deny receipt",
            ));
        }
        Ok(())
    }

    pub(crate) fn spawn(
        &self,
        args: &[String],
        env: &[(OsString, OsString)],
        memory_cap: u64,
    ) -> io::Result<Spawned> {
        let args = sandbox_args(args, self._assets.path());
        self.spawn_inner(&args, env, memory_cap, true)
    }

    fn spawn_inner(
        &self,
        args: &[String],
        env: &[(OsString, OsString)],
        memory_cap: u64,
        overlapped: bool,
    ) -> io::Result<Spawned> {
        let (stdin, child_stdin) = pipe(false, overlapped)?;
        let (stdout, child_stdout) = pipe(true, overlapped)?;
        let (stderr, child_stderr) = pipe(true, overlapped)?;
        let mut attrs = Attributes::new(4)?;
        let registry_read = CapabilitySid::registry_read()?;
        let capability_sid = registry_read.sid();
        let mut capability = SID_AND_ATTRIBUTES {
            Sid: capability_sid,
            Attributes: windows_sys::Win32::System::SystemServices::SE_GROUP_ENABLED as u32,
        };
        let capabilities = SECURITY_CAPABILITIES {
            AppContainerSid: self.profile.sid,
            Capabilities: &mut capability,
            CapabilityCount: 1,
            Reserved: 0,
        };
        let lpac = PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT;
        // Core creates this LPAC from an unrestricted process, so it can opt
        // the runtime into spawning descendants. Windows gives those children
        // the parent's AppContainer token and, by default, the same Job; the
        // startup probe verifies that they keep the same file/network limits.
        let child_process_policy = PROCESS_CREATION_CHILD_PROCESS_OVERRIDE;
        let handles = [
            child_stdin.as_raw_handle(),
            child_stdout.as_raw_handle(),
            child_stderr.as_raw_handle(),
        ];
        attrs.set(PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, &capabilities)?;
        attrs.set(PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY, &lpac)?;
        attrs.set(
            PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY,
            &child_process_policy,
        )?;
        attrs.set_slice(PROC_THREAD_ATTRIBUTE_HANDLE_LIST, &handles)?;
        let mut startup: STARTUPINFOEXW = unsafe { zeroed() };
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = child_stdin.as_raw_handle();
        startup.StartupInfo.hStdOutput = child_stdout.as_raw_handle();
        startup.StartupInfo.hStdError = child_stderr.as_raw_handle();
        startup.lpAttributeList = attrs.ptr();
        let application = wide(self.program.as_os_str())?;
        let mut command = command_line(self.program.as_os_str(), args)?;
        let directory = wide(self.data.as_os_str())?;
        let mut env = env.to_vec();
        let temp = self.data.join("tmp").into_os_string();
        for key in ["TEMP", "TMP", "TMPDIR"] {
            env.retain(|(name, _)| !name.to_string_lossy().eq_ignore_ascii_case(key));
            env.push((key.into(), temp.clone()));
        }
        let environment = environment_block(&env)?;
        let mut info: PROCESS_INFORMATION = unsafe { zeroed() };
        // SAFETY: owned attribute backing/storage/argv/environment outlive
        // this call; HANDLE_LIST is precisely the three inheritable pipe ends.
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                null(),
                null(),
                1,
                CREATE_SUSPENDED
                    | CREATE_NO_WINDOW
                    | CREATE_UNICODE_ENVIRONMENT
                    | EXTENDED_STARTUPINFO_PRESENT,
                environment.as_ptr().cast(),
                directory.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            )
        };
        if created == 0 {
            return Err(io::Error::last_os_error());
        }
        let process = unsafe { OwnedHandle::from_raw_handle(info.hProcess) };
        let thread = unsafe { OwnedHandle::from_raw_handle(info.hThread) };
        let setup = || -> io::Result<Arc<ProcessTree>> {
            let tree = Arc::new(ProcessTree::attach_windows_handle(
                info.dwProcessId,
                &process,
            )?);
            tree.limit_process_memory(memory_cap)?;
            verify_token(&process, self.profile.sid, capability_sid)?;
            // No Native byte executes until Job, memory and actual LPAC token
            // identity/capabilities have all been checked by Rust.
            if unsafe { ResumeThread(thread.as_raw_handle()) } != 1 {
                return Err(io::Error::other(
                    "Native main thread did not resume from its exact suspended state",
                ));
            }
            Ok(tree)
        };
        let tree = match setup() {
            Ok(tree) => tree,
            Err(error) => {
                unsafe {
                    windows_sys::Win32::System::Threading::TerminateProcess(
                        process.as_raw_handle(),
                        70,
                    );
                    WaitForSingleObject(process.as_raw_handle(), 2000);
                }
                return Err(error);
            }
        };
        drop((child_stdin, child_stdout, child_stderr, thread));
        Ok(Spawned {
            child: Child {
                process: Arc::new(process),
                tree,
                pid: info.dwProcessId,
                _sandbox: self.clone(),
                reaped: false,
            },
            stdin,
            stdout,
            stderr,
        })
    }
}

pub(crate) struct Spawned {
    pub child: Child,
    pub stdin: OwnedHandle,
    pub stdout: OwnedHandle,
    pub stderr: OwnedHandle,
}

pub(crate) struct Child {
    process: Arc<OwnedHandle>,
    pub tree: Arc<ProcessTree>,
    pub pid: u32,
    _sandbox: NativeSandbox,
    reaped: bool,
}

impl Child {
    pub(crate) async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        let process = Arc::clone(&self.process);
        let sandbox = self._sandbox.clone();
        let status = tokio::task::spawn_blocking(move || {
            let _sandbox = sandbox;
            wait(&process, u32::MAX)
        })
        .await
        .map_err(io::Error::other)??;
        self.reaped = true;
        Ok(status)
    }
    pub(crate) async fn kill(&mut self) -> io::Result<()> {
        self.tree.kill()?;
        self.wait().await.map(|_| ())
    }
    fn wait_timeout(&mut self, after: Duration) -> io::Result<std::process::ExitStatus> {
        let status = wait(
            &self.process,
            after.as_millis().min(u32::MAX as u128 - 1) as u32,
        )?;
        self.reaped = true;
        // The fixed probe must not retain background descendants/pipe handles.
        let _ = self.tree.kill();
        Ok(status)
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.tree.kill();
        }
    }
}

fn wait(process: &OwnedHandle, timeout: u32) -> io::Result<std::process::ExitStatus> {
    use std::os::windows::process::ExitStatusExt;
    match unsafe { WaitForSingleObject(process.as_raw_handle(), timeout) } {
        WAIT_OBJECT_0 => {
            let mut status = 0;
            if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut status) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(std::process::ExitStatus::from_raw(status))
        }
        WAIT_TIMEOUT => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Windows sandbox probe exceeded launch deadline",
        )),
        _ => Err(io::Error::last_os_error()),
    }
}

fn verify_token(process: &OwnedHandle, sid: PSID, capability: PSID) -> io::Result<()> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    // AppContainer must be confirmed, and LPAC through the token flag or, when
    // that query fails (hosted Windows Server returned ERROR_INVALID_PARAMETER;
    // the cause is unknown), through the `WIN://NOALLAPPPKG` security
    // attribute the LPAC opt-out adds (ntdoc; NtObjectManager's
    // LowPrivilegeAppContainer). Report each observation, so a refusal says
    // which property the kernel did not confirm.
    let query = |class| -> io::Result<u32> {
        let mut value = 0_u32;
        let mut read = 0;
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                class,
                (&mut value as *mut u32).cast(),
                size_of::<u32>() as u32,
                &mut read,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(value)
    };
    let mut observed = Vec::new();
    let app_container = match query(TokenIsAppContainer) {
        Ok(value) => {
            observed.push(format!("TokenIsAppContainer={value}"));
            value == 1
        }
        Err(error) => {
            observed.push(format!("TokenIsAppContainer query failed: {error}"));
            false
        }
    };
    let less_privileged = match query(TokenIsLessPrivilegedAppContainer) {
        Ok(value) => {
            observed.push(format!("TokenIsLessPrivilegedAppContainer={value}"));
            value == 1
        }
        Err(error) => {
            observed.push(format!(
                "TokenIsLessPrivilegedAppContainer query failed: {error}"
            ));
            match token_has_lpac_attribute(&token) {
                Ok(true) => {
                    observed.push("WIN://NOALLAPPPKG=1".to_string());
                    true
                }
                Ok(false) => {
                    observed.push("WIN://NOALLAPPPKG absent".to_string());
                    false
                }
                Err(error) => {
                    observed.push(format!("WIN://NOALLAPPPKG unreadable: {error}"));
                    false
                }
            }
        }
    };
    if !(app_container && less_privileged) {
        return Err(io::Error::other(format!(
            "Windows refused the required LPAC token ({})",
            observed.join(", ")
        )));
    }
    // Variable-size token buffers are aligned for their SDK structs.
    for class in [TokenAppContainerSid, TokenCapabilities] {
        let mut size = 0;
        unsafe {
            GetTokenInformation(token.as_raw_handle(), class, null_mut(), 0, &mut size);
        }
        if size == 0 || size > 64 * 1024 {
            return Err(io::Error::other("invalid sandbox token size"));
        }
        let mut buffer = vec![0_usize; (size as usize).div_ceil(size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                class,
                buffer.as_mut_ptr().cast(),
                size,
                &mut size,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let valid = unsafe {
            if class == TokenAppContainerSid {
                let value = &*buffer.as_ptr().cast::<TOKEN_APPCONTAINER_INFORMATION>();
                !value.TokenAppContainer.is_null() && EqualSid(value.TokenAppContainer, sid) != 0
            } else {
                // Exactly the one granted capability, and nothing else.
                let groups = &*buffer.as_ptr().cast::<TOKEN_GROUPS>();
                groups.GroupCount == 1 && EqualSid(groups.Groups[0].Sid, capability) != 0
            }
        };
        if !valid {
            return Err(io::Error::other(
                "sandbox token has unexpected identity/capabilities",
            ));
        }
    }
    Ok(())
}

/// Whether the token carries the LPAC opt-out attribute `WIN://NOALLAPPPKG`
/// with a nonzero integer value. `TokenSecurityAttributes` (Windows 8+)
/// returns TOKEN_SECURITY_ATTRIBUTES_INFORMATION whose pointers are absolute
/// addresses into the returned buffer; each is checked to be aligned and to
/// stay inside it before it is read.
fn token_has_lpac_attribute(token: &OwnedHandle) -> io::Result<bool> {
    use windows_sys::Win32::Security::TokenSecurityAttributes;
    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *const u16,
    }
    #[repr(C)]
    struct AttributeV1 {
        name: UnicodeString,
        value_type: u16,
        reserved: u16,
        flags: u32,
        value_count: u32,
        values: *const u64,
    }
    #[repr(C)]
    struct AttributesInformation {
        version: u16,
        reserved: u16,
        attribute_count: u32,
        attributes: *const AttributeV1,
    }
    const VERSION_V1: u16 = 1;
    const TYPE_INT64: u16 = 1;
    const TYPE_UINT64: u16 = 2;
    let mut size = 0;
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenSecurityAttributes,
            null_mut(),
            0,
            &mut size,
        );
    }
    if (size as usize) < size_of::<AttributesInformation>() || size > 64 * 1024 {
        return Err(io::Error::other("invalid token security attribute size"));
    }
    let mut buffer = vec![0_usize; (size as usize).div_ceil(size_of::<usize>())];
    if unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenSecurityAttributes,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let start = buffer.as_ptr() as usize;
    let end = start + buffer.len() * size_of::<usize>();
    let inside = |pointer: usize, bytes: usize, align: usize| {
        pointer % align == 0
            && pointer >= start
            && pointer.checked_add(bytes).is_some_and(|last| last <= end)
    };
    let information = unsafe { &*buffer.as_ptr().cast::<AttributesInformation>() };
    if information.version != VERSION_V1 {
        return Err(io::Error::other("unknown token security attribute version"));
    }
    let count = information.attribute_count as usize;
    if count == 0 {
        return Ok(false);
    }
    if !count
        .checked_mul(size_of::<AttributeV1>())
        .is_some_and(|bytes| {
            inside(
                information.attributes as usize,
                bytes,
                align_of::<AttributeV1>(),
            )
        })
    {
        return Err(io::Error::other(
            "token security attributes exceed their buffer",
        ));
    }
    for index in 0..count {
        let attribute = unsafe { &*information.attributes.add(index) };
        let name_bytes = usize::from(attribute.name.length);
        if name_bytes % 2 != 0
            || !inside(
                attribute.name.buffer as usize,
                name_bytes,
                align_of::<u16>(),
            )
        {
            return Err(io::Error::other(
                "token security attribute name exceeds its buffer",
            ));
        }
        let name = unsafe { std::slice::from_raw_parts(attribute.name.buffer, name_bytes / 2) };
        if !String::from_utf16_lossy(name).eq_ignore_ascii_case("WIN://NOALLAPPPKG") {
            continue;
        }
        let values = attribute.value_count as usize;
        if !matches!(attribute.value_type, TYPE_INT64 | TYPE_UINT64)
            || values == 0
            || !values
                .checked_mul(size_of::<u64>())
                .is_some_and(|bytes| inside(attribute.values as usize, bytes, align_of::<u64>()))
        {
            return Ok(false);
        }
        return Ok(unsafe { *attribute.values } != 0);
    }
    Ok(false)
}

/// The only capability a Native LPAC host receives: `registryRead`. Without
/// it an LPAC cannot open any registry key, so Winsock initialization, which
/// reads its catalog under HKLM, fails and Node aborts at startup. It grants
/// no file, network or COM access; socket creation stays denied, which the
/// isolation probe checks.
struct CapabilitySid {
    sid: PSID,
    sids: *mut PSID,
    count: u32,
    groups: *mut PSID,
    group_count: u32,
}

impl CapabilitySid {
    fn registry_read() -> io::Result<Self> {
        use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
        use windows_sys::core::BOOL;
        type DeriveCapabilitySidsFromName = unsafe extern "system" fn(
            *const u16,
            *mut *mut PSID,
            *mut u32,
            *mut *mut PSID,
            *mut u32,
        ) -> BOOL;
        let module = wide(OsStr::new("kernelbase.dll"))?;
        // SAFETY: nul-terminated module name; the module stays loaded for the process.
        let handle = unsafe { GetModuleHandleW(module.as_ptr()) };
        let address = if handle.is_null() {
            None
        } else {
            // SAFETY: nul-terminated export name and a live module handle.
            unsafe { GetProcAddress(handle, c"DeriveCapabilitySidsFromName".as_ptr().cast()) }
        };
        let Some(address) = address else {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this Windows build cannot derive LPAC capability SIDs (DeriveCapabilitySidsFromName is unavailable), so Native extension hosts are not supported here",
            ));
        };
        // SAFETY: the export has exactly this documented signature.
        let derive: DeriveCapabilitySidsFromName = unsafe { std::mem::transmute(address) };
        let name = wide(OsStr::new("registryRead"))?;
        let mut value = Self {
            sid: null_mut(),
            sids: null_mut(),
            count: 0,
            groups: null_mut(),
            group_count: 0,
        };
        // SAFETY: nul-terminated name and initialized out pointers; the
        // returned arrays and SIDs are freed by Drop with LocalFree.
        if unsafe {
            derive(
                name.as_ptr(),
                &mut value.groups,
                &mut value.group_count,
                &mut value.sids,
                &mut value.count,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        if value.count != 1 || value.sids.is_null() {
            return Err(io::Error::other(
                "registryRead did not derive exactly one capability SID",
            ));
        }
        // SAFETY: DeriveCapabilitySidsFromName succeeded, and the checked
        // count says its owned array contains exactly one PSID. The array and
        // SID stay alive in `value` until its Drop implementation frees them.
        value.sid = unsafe { std::slice::from_raw_parts(value.sids, 1)[0] };
        Ok(value)
    }

    fn sid(&self) -> PSID {
        self.sid
    }
}

impl Drop for CapabilitySid {
    fn drop(&mut self) {
        // SAFETY: each SID and each array came from DeriveCapabilitySidsFromName.
        unsafe {
            for (array, count) in [(self.sids, self.count), (self.groups, self.group_count)] {
                if array.is_null() {
                    continue;
                }
                for index in 0..count as usize {
                    LocalFree(*array.add(index));
                }
                LocalFree(array.cast());
            }
        }
    }
}

fn set_acl(file: &File, sid: PSID, access: u32, inheritance: u32) -> io::Result<()> {
    edit_acl(file, sid, access, inheritance, GRANT_ACCESS)
}
/// Edit exactly one SID's allow entries on this pinned handle. Read the stored
/// descriptor with GetKernelObjectSecurity: GetSecurityInfo can normalize a
/// child's inherited ACEs/control while its parent has a grant. Writing that
/// view back freezes the normalized state. The exact kernel reader/writer pair
/// preserves every other ACE and never propagates changes to child objects.
fn edit_acl(
    file: &File,
    sid: PSID,
    access: u32,
    inheritance: u32,
    mode: windows_sys::Win32::Security::Authorization::ACCESS_MODE,
) -> io::Result<()> {
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, AddAccessAllowedAceEx, AddAce, GetAce, GetLengthSid,
        INHERITED_ACE, InitializeAcl, InitializeSecurityDescriptor, SE_DACL_AUTO_INHERIT_REQ,
        SE_DACL_AUTO_INHERITED, SE_DACL_PROTECTED, SECURITY_DESCRIPTOR, SetKernelObjectSecurity,
        SetSecurityDescriptorControl, SetSecurityDescriptorDacl,
    };
    use windows_sys::Win32::System::SystemServices::{
        ACCESS_ALLOWED_ACE_TYPE, SECURITY_DESCRIPTOR_REVISION,
    };
    // The owning grant/retirement entrypoint holds AclEditGuard while opening
    // and editing its exact objects, including all shared pinned ancestors.
    let grant = mode == GRANT_ACCESS;
    if !grant && mode != REVOKE_ACCESS {
        return Err(io::Error::other("unsupported ACL edit mode"));
    }
    let descriptor = RawDacl::read(file)?;
    let old_acl = descriptor.acl();
    let preserved = descriptor.control & (SE_DACL_AUTO_INHERITED | SE_DACL_PROTECTED);
    let old = std::ptr::NonNull::new(old_acl)
        .ok_or_else(|| io::Error::other("refusing to replace an unrestricted DACL"))?;
    let (acl_revision, acl_size, ace_count) = {
        // SAFETY: RawDacl validated this DACL inside its owned descriptor.
        let acl = unsafe { old.as_ref() };
        (
            u32::from(acl.AclRevision),
            usize::from(acl.AclSize),
            acl.AceCount,
        )
    };
    let added = if grant {
        size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + unsafe { GetLengthSid(sid) } as usize
    } else {
        0
    };
    let size = acl_size + added;
    if size > 0xFFFC {
        return Err(io::Error::other("edited DACL exceeds the ACL size limit"));
    }
    // ACLs must be DWORD aligned.
    let mut storage = vec![0_u32; size.div_ceil(size_of::<u32>())];
    let new_acl = storage.as_mut_ptr().cast::<ACL>();
    if unsafe {
        InitializeAcl(
            new_acl,
            (storage.len() * size_of::<u32>()) as u32,
            acl_revision,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut merged = access;
    let mut inserted = !grant;
    for index in 0..u32::from(ace_count) {
        let mut ace = null_mut();
        if unsafe { GetAce(old_acl, index, &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let ace = std::ptr::NonNull::new(ace)
            .ok_or_else(|| io::Error::other("GetAce returned no entry"))?;
        // SAFETY: GetAce returned a pointer to an entry inside the live DACL.
        let header = unsafe { ace.cast::<ACE_HEADER>().as_ref() };
        let flags = u32::from(header.AceFlags);
        let inherited = flags & INHERITED_ACE != 0;
        // Only ACCESS_ALLOWED_ACE carries its SID at SidStart.
        let ours = u32::from(header.AceType) == ACCESS_ALLOWED_ACE_TYPE
            && usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>()
            && unsafe {
                EqualSid(
                    std::ptr::addr_of_mut!((*ace.cast::<ACCESS_ALLOWED_ACE>().as_ptr()).SidStart)
                        .cast(),
                    sid,
                )
            } != 0;
        if ours && grant && !inherited && flags == inheritance {
            // GRANT_ACCESS semantics: combine with the existing explicit grant.
            merged |= unsafe { ace.cast::<ACCESS_ALLOWED_ACE>().as_ref().Mask };
            continue;
        }
        if ours && !grant {
            // Retirement removes explicit entries and the stale inherited
            // copies left after the parent's grant was revoked first.
            continue;
        }
        if !inserted && inherited {
            // Explicit entries precede inherited ones.
            if unsafe { AddAccessAllowedAceEx(new_acl, acl_revision, inheritance, merged, sid) }
                == 0
            {
                return Err(io::Error::last_os_error());
            }
            inserted = true;
        }
        if unsafe {
            AddAce(
                new_acl,
                acl_revision,
                u32::MAX,
                ace.as_ptr(),
                u32::from(header.AceSize),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    if !inserted
        && unsafe { AddAccessAllowedAceEx(new_acl, acl_revision, inheritance, merged, sid) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut security: SECURITY_DESCRIPTOR = unsafe { zeroed() };
    let security_ptr = (&mut security as *mut SECURITY_DESCRIPTOR).cast();
    // NTFS stores this descriptor as given, with no inheritance merge. The
    // kernel keeps SE_DACL_AUTO_INHERITED only when SE_DACL_AUTO_INHERIT_REQ
    // accompanies it, so request it exactly when the object already had it;
    // otherwise the object would silently become a legacy DACL.
    let requested = preserved
        | if preserved & SE_DACL_AUTO_INHERITED != 0 {
            SE_DACL_AUTO_INHERIT_REQ
        } else {
            0
        };
    if unsafe { InitializeSecurityDescriptor(security_ptr, SECURITY_DESCRIPTOR_REVISION) } == 0
        || unsafe { SetSecurityDescriptorDacl(security_ptr, 1, new_acl, 0) } == 0
        || unsafe {
            SetSecurityDescriptorControl(
                security_ptr,
                SE_DACL_AUTO_INHERIT_REQ | SE_DACL_AUTO_INHERITED | SE_DACL_PROTECTED,
                requested,
            )
        } == 0
        || unsafe {
            SetKernelObjectSecurity(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION,
                security_ptr,
            )
        } == 0
    {
        return Err(io::Error::last_os_error());
    }
    drop(descriptor);
    let (still_granted, control) = read_back_dacl(file, sid)?;
    // The raw descriptor and exact kernel write preserve both bits on files
    // and directories, without re-deriving or propagating inherited entries.
    let checked = SE_DACL_AUTO_INHERITED | SE_DACL_PROTECTED;
    if control & checked != preserved & checked {
        return Err(io::Error::other(
            "edited DACL changed its inheritance control bits",
        ));
    }
    if !grant && still_granted {
        return Err(io::Error::other(
            "retired profile SID is still present after ACL retirement",
        ));
    }
    Ok(())
}

/// Read back the object's DACL: whether any allow entry names `sid`, and the
/// descriptor's control bits.
fn read_back_dacl(file: &File, sid: PSID) -> io::Result<(bool, u16)> {
    use windows_sys::Win32::Security::{ACCESS_ALLOWED_ACE, ACE_HEADER, GetAce};
    use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
    let descriptor = RawDacl::read(file)?;
    let acl = descriptor.acl();
    let control = descriptor.control;
    let dacl = std::ptr::NonNull::new(acl)
        .ok_or_else(|| io::Error::other("refusing to inspect an unrestricted DACL"))?;
    // SAFETY: RawDacl validated this DACL inside its owned descriptor.
    for index in 0..u32::from(unsafe { dacl.as_ref().AceCount }) {
        let mut ace = null_mut();
        if unsafe { GetAce(acl, index, &mut ace) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let ace = std::ptr::NonNull::new(ace)
            .ok_or_else(|| io::Error::other("GetAce returned no entry"))?;
        // SAFETY: GetAce returned a pointer to an entry inside the live DACL.
        let header = unsafe { ace.cast::<ACE_HEADER>().as_ref() };
        if u32::from(header.AceType) == ACCESS_ALLOWED_ACE_TYPE
            && usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>()
            && unsafe {
                EqualSid(
                    std::ptr::addr_of_mut!((*ace.cast::<ACCESS_ALLOWED_ACE>().as_ptr()).SidStart)
                        .cast(),
                    sid,
                )
            } != 0
        {
            return Ok((true, control));
        }
    }
    Ok((false, control))
}

/// Owned DWORD-aligned storage for a bounded self-relative kernel descriptor.
/// The ACL offset is validated once and never outlives this allocation.
struct RawDacl {
    storage: Vec<u32>,
    acl_offset: usize,
    control: u16,
}
impl RawDacl {
    fn read(file: &File) -> io::Result<Self> {
        use windows_sys::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
        use windows_sys::Win32::Security::{GetKernelObjectSecurity, SECURITY_DESCRIPTOR_RELATIVE};
        let mut required = 0_u32;
        // SAFETY: a zero-length size query writes only `required`.
        if unsafe {
            GetKernelObjectSecurity(
                file.as_raw_handle(),
                DACL_SECURITY_INFORMATION,
                null_mut(),
                0,
                &mut required,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
                return Err(error);
            }
        }
        // An external ACL writer can change the required size between calls.
        // Bound both the allocation and the number of attempts; fail closed.
        for _ in 0..3 {
            if !(size_of::<SECURITY_DESCRIPTOR_RELATIVE>()..=1024 * 1024)
                .contains(&(required as usize))
            {
                return Err(io::Error::other(
                    "kernel security descriptor exceeds size bounds",
                ));
            }
            let capacity = required;
            let mut storage = vec![0_u32; (capacity as usize).div_ceil(size_of::<u32>())];
            // SAFETY: storage is aligned, owned, and at least capacity bytes.
            if unsafe {
                GetKernelObjectSecurity(
                    file.as_raw_handle(),
                    DACL_SECURITY_INFORMATION,
                    storage.as_mut_ptr().cast(),
                    capacity,
                    &mut required,
                )
            } == 0
            {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32)
                    && required > capacity
                {
                    continue;
                }
                return Err(error);
            }
            if required > capacity {
                return Err(io::Error::other(
                    "kernel security descriptor exceeds its buffer",
                ));
            }
            return Self::from_storage(storage, required as usize);
        }
        Err(io::Error::other(
            "kernel security descriptor repeatedly changed size",
        ))
    }

    fn from_storage(mut storage: Vec<u32>, length: usize) -> io::Result<Self> {
        use windows_sys::Win32::Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, GetAce, GetSecurityDescriptorControl,
            GetSecurityDescriptorDacl, IsValidAcl, IsValidSid, SE_SELF_RELATIVE,
            SECURITY_DESCRIPTOR_RELATIVE, SID,
        };
        use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
        if length < size_of::<SECURITY_DESCRIPTOR_RELATIVE>()
            || length > storage.len() * size_of::<u32>()
        {
            return Err(io::Error::other("kernel security descriptor is truncated"));
        }
        let descriptor = storage.as_mut_ptr().cast();
        let mut control = 0_u16;
        let mut revision = 0_u32;
        // SAFETY: the aligned allocation contains the complete descriptor header.
        if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if control & SE_SELF_RELATIVE == 0 {
            return Err(io::Error::other(
                "kernel security descriptor is not self-relative",
            ));
        }
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl: *mut ACL = null_mut();
        // SAFETY: only the validated self-relative header is read here. Bound
        // the returned DACL pointer before dereferencing any of its bytes.
        if unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        if present == 0 || acl.is_null() {
            return Err(io::Error::other("refusing an absent or unrestricted DACL"));
        }
        let acl_offset = (acl as usize)
            .checked_sub(descriptor as usize)
            .filter(|offset| {
                *offset >= size_of::<SECURITY_DESCRIPTOR_RELATIVE>()
                    && *offset % size_of::<u32>() == 0
                    && offset
                        .checked_add(size_of::<ACL>())
                        .is_some_and(|end| end <= length)
            })
            .ok_or_else(|| io::Error::other("DACL header is outside its descriptor"))?;
        // Read through owned storage at the checked offset, not through the
        // out-pointer Windows returned: same address, owned provenance.
        // SAFETY: acl_offset + size_of::<ACL>() <= length <= storage bytes.
        let acl = unsafe { storage.as_ptr().cast::<u8>().add(acl_offset) }
            .cast_mut()
            .cast::<ACL>();
        // SAFETY: the aligned DACL header lies completely inside storage.
        let acl_size = usize::from(unsafe { (*acl).AclSize });
        if acl_size < size_of::<ACL>()
            || acl_offset
                .checked_add(acl_size)
                .is_none_or(|end| end > length)
        {
            return Err(io::Error::other("DACL is outside its descriptor"));
        }
        // SAFETY: the entire claimed ACL extent is bounded by owned storage.
        // IsValidAcl checks revision and whether its ACEs fit that extent.
        if unsafe { IsValidAcl(acl) } == 0 {
            return Err(io::Error::other(
                "kernel descriptor contains an invalid ACL",
            ));
        }
        for index in 0..u32::from(unsafe { (*acl).AceCount }) {
            let mut entry = null_mut();
            if unsafe { GetAce(acl, index, &mut entry) } == 0 {
                return Err(io::Error::last_os_error());
            }
            let entry_offset = (entry as usize)
                .checked_sub(acl as usize)
                .filter(|offset| {
                    *offset >= size_of::<ACL>()
                        && *offset % size_of::<u32>() == 0
                        && offset
                            .checked_add(size_of::<ACE_HEADER>())
                            .is_some_and(|end| end <= acl_size)
                })
                .ok_or_else(|| io::Error::other("ACE header is outside its DACL"))?;
            // Same rule: address the ACE from the storage-derived ACL.
            // SAFETY: entry_offset + size_of::<ACE_HEADER>() <= acl_size.
            let entry = unsafe { acl.cast::<u8>().add(entry_offset) };
            // SAFETY: the complete, aligned ACE header is inside the DACL.
            let header = unsafe { &*entry.cast::<ACE_HEADER>() };
            let entry_size = usize::from(header.AceSize);
            if entry_size < size_of::<ACE_HEADER>() || entry_offset + entry_size > acl_size {
                return Err(io::Error::other("ACE is outside its DACL"));
            }
            if u32::from(header.AceType) == ACCESS_ALLOWED_ACE_TYPE {
                // IsValidAcl does not validate SIDs. Bound the whole SID before
                // IsValidSid or either caller's EqualSid can inspect it.
                let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
                let sid_header = std::mem::offset_of!(SID, SubAuthority);
                if entry_size < sid_offset + sid_header {
                    return Err(io::Error::other("allow ACE has a truncated SID header"));
                }
                // SAFETY: the complete SID header is inside this ACE.
                let sid = unsafe { entry.cast::<u8>().add(sid_offset) };
                let count = usize::from(unsafe { *sid.add(1) });
                if sid_header + count * size_of::<u32>() > entry_size - sid_offset
                    || unsafe { IsValidSid(sid.cast()) } == 0
                {
                    return Err(io::Error::other(
                        "allow ACE has an invalid or truncated SID",
                    ));
                }
            }
        }
        Ok(Self {
            storage,
            acl_offset,
            control,
        })
    }

    fn acl(&self) -> *mut ACL {
        // SAFETY: from_storage validated the offset, alignment, and ACL extent;
        // storage stays owned and unchanged while the returned pointer is used.
        unsafe {
            self.storage
                .as_ptr()
                .cast::<u8>()
                .add(self.acl_offset)
                .cast_mut()
                .cast()
        }
    }
}

#[cfg(test)]
struct LocalAllocation(*mut core::ffi::c_void);
#[cfg(test)]
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        unsafe {
            LocalFree(self.0);
        }
    }
}

struct Attributes {
    bytes: Vec<usize>,
    initialized: bool,
}
impl Attributes {
    fn new(count: u32) -> io::Result<Self> {
        let mut size = 0;
        unsafe {
            InitializeProcThreadAttributeList(null_mut(), count, 0, &mut size);
        }
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut value = Self {
            bytes: vec![0; size.div_ceil(size_of::<usize>())],
            initialized: false,
        };
        if unsafe { InitializeProcThreadAttributeList(value.ptr(), count, 0, &mut size) } == 0 {
            return Err(io::Error::last_os_error());
        }
        value.initialized = true;
        Ok(value)
    }
    fn ptr(&mut self) -> *mut core::ffi::c_void {
        self.bytes.as_mut_ptr().cast()
    }
    fn set<T>(&mut self, attribute: u32, value: &T) -> io::Result<()> {
        self.set_raw(attribute, (value as *const T).cast(), size_of::<T>())
    }
    fn set_slice<T>(&mut self, attribute: u32, values: &[T]) -> io::Result<()> {
        self.set_raw(
            attribute,
            values.as_ptr().cast(),
            std::mem::size_of_val(values),
        )
    }
    fn set_raw(
        &mut self,
        attribute: u32,
        value: *const core::ffi::c_void,
        size: usize,
    ) -> io::Result<()> {
        if unsafe {
            UpdateProcThreadAttribute(
                self.ptr(),
                0,
                attribute as usize,
                value,
                size,
                null_mut(),
                null(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
impl Drop for Attributes {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.ptr());
            }
        }
    }
}

fn pipe(output: bool, overlapped: bool) -> io::Result<(OwnedHandle, OwnedHandle)> {
    let name = wide(OsStr::new(&format!(
        r"\\.\pipe\Codewhale.Native.{}",
        uuid::Uuid::new_v4()
    )))?;
    // Parent endpoints are overlapped/noninheritable; exactly the synchronous
    // child endpoints are included in the process attribute HANDLE_LIST.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            (if output {
                PIPE_ACCESS_INBOUND
            } else {
                PIPE_ACCESS_OUTBOUND
            }) | if overlapped { FILE_FLAG_OVERLAPPED } else { 0 }
                | FILE_FLAG_FIRST_PIPE_INSTANCE,
            PIPE_WAIT,
            1,
            8192,
            8192,
            0,
            null(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let parent = unsafe { OwnedHandle::from_raw_handle(handle) };
    let security = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: null_mut(),
        bInheritHandle: 1,
    };
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            if output {
                FILE_GENERIC_WRITE
            } else {
                FILE_GENERIC_READ
            },
            0,
            &security,
            OPEN_EXISTING,
            0,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let child = unsafe { OwnedHandle::from_raw_handle(handle) };
    if unsafe { ConnectNamedPipe(parent.as_raw_handle(), null_mut()) } == 0
        && unsafe { GetLastError() } != ERROR_PIPE_CONNECTED
    {
        return Err(io::Error::last_os_error());
    }
    Ok((parent, child))
}

fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut value: Vec<_> = value.encode_wide().collect();
    if value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows argv/env contains NUL",
        ));
    }
    value.push(0);
    Ok(value)
}

fn sandbox_args(args: &[String], runtime_dir: &Path) -> Vec<String> {
    // An LPAC cannot open the NUL device. Both the host and its probe
    // descendant read the empty config in the already-granted runtime copy.
    let empty_config = format!("--config={}", runtime_dir.join(EMPTY_BUN_CONFIG).display());
    args.iter()
        .map(|arg| {
            if arg == "--config=NUL" {
                empty_config.clone()
            } else {
                arg.clone()
            }
        })
        .collect()
}

fn command_line(program: &OsStr, args: &[String]) -> io::Result<Vec<u16>> {
    // The documented CommandLineToArgvW/MS CRT quote+backslash rules. An
    // explicit application path means PATH/first-token parsing is never authority.
    let mut result = Vec::new();
    for arg in std::iter::once(program).chain(args.iter().map(OsStr::new)) {
        let units = wide(arg)?;
        if !result.is_empty() {
            result.push(b' ' as u16);
        }
        result.push(b'"' as u16);
        let mut slashes = 0;
        for unit in units.into_iter().take_while(|unit| *unit != 0) {
            if unit == b'\\' as u16 {
                slashes += 1;
                continue;
            }
            if unit == b'"' as u16 {
                result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2 + 1));
            } else {
                result.extend(std::iter::repeat_n(b'\\' as u16, slashes));
            }
            slashes = 0;
            result.push(unit);
        }
        result.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
        result.push(b'"' as u16);
    }
    result.push(0);
    Ok(result)
}

fn environment_block(env: &[(OsString, OsString)]) -> io::Result<Vec<u16>> {
    let mut entries = env.iter().collect::<Vec<_>>();
    entries.sort_by_key(|(key, _)| key.to_string_lossy().to_uppercase());
    let mut result = Vec::new();
    for (key, value) in entries {
        let key = wide(key)?;
        if key.len() == 1 || key.contains(&(b'=' as u16)) {
            return Err(io::Error::other("invalid Windows environment key"));
        }
        result.extend(&key[..key.len() - 1]);
        result.push(b'=' as u16);
        result.extend(wide(value)?);
    }
    if result.is_empty() {
        result.push(0);
    }
    result.push(0);
    Ok(result)
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;

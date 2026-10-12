//! Private directory/file mechanism extracted from xai_credentials.
//! Retained handles, no-follow component traversal, owner/type/link checks,
//! atomic publication and Windows owner-only security preserve existing policy.

#[cfg(windows)]
use crate::windows_identity::{CurrentWindowsUser, WindowsLocalAllocation};
use anyhow::{Context, Result, bail};
#[cfg(unix)]
use std::ffi::CString;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
#[cfg(all(windows, test))]
use std::sync::Mutex;
#[cfg(not(windows))]
use std::time::{SystemTime, UNIX_EPOCH};

/// Anchored current-user-owned directory and file mechanism. Credential and
/// Runtime policies remain with their callers; this type owns no authority.
#[derive(Debug)]
pub struct PrivateDirectory {
    pub(crate) directory: PathBuf,
    #[cfg(unix)]
    pub(crate) directory_handle: File,
    #[cfg(windows)]
    pub(crate) _component_handles: Vec<File>,
}

/// A retained file was retired before it could be validated. This is still a
/// refusal; an owner observer may re-observe, but must never accept this file.
#[cfg(unix)]
#[derive(Debug)]
pub struct UnlinkedPrivateFile;

#[cfg(unix)]
impl std::fmt::Display for UnlinkedPrivateFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("private file was unlinked before validation")
    }
}

#[cfg(unix)]
impl std::error::Error for UnlinkedPrivateFile {}

impl PrivateDirectory {
    pub fn open(directory: &Path) -> Result<Self> {
        open_owned_directory(directory, true, true)
    }
    /// Read-only admission of an existing private directory, without repair.
    pub fn inspect(directory: &Path) -> Result<Self> {
        open_owned_directory(directory, false, false)
    }

    /// Create missing owned directories; refuse existing non-private permissions.
    pub fn admit(directory: &Path) -> Result<Self> {
        open_owned_directory(directory, false, true)
    }

    /// Check the retained directory against the lexical selection without
    /// following links, creating directories, or changing permissions.
    #[cfg(unix)]
    pub fn is_at_selected_path(&self) -> Result<bool> {
        use std::os::unix::fs::MetadataExt as _;
        let current = Self::inspect(&self.directory)?;
        let held = self.directory_handle.metadata()?;
        let now = current.directory_handle.metadata()?;
        Ok(held.dev() == now.dev() && held.ino() == now.ino())
    }

    #[cfg(unix)]
    pub fn current_user_id() -> u32 {
        // SAFETY: geteuid has no pointer arguments.
        unsafe { libc::geteuid() }
    }
}

fn validate_private_basename(name: &str) -> Result<()> {
    let path = Path::new(name);
    anyhow::ensure!(
        path.components().count() == 1
            && matches!(path.components().next(), Some(Component::Normal(_)))
            && path.file_name().and_then(|value| value.to_str()) == Some(name),
        "xAI OAuth private basename must be one UTF-8 path component"
    );
    Ok(())
}

#[cfg(unix)]
fn resolve_ancestors(directory: &Path) -> std::path::PathBuf {
    use std::os::unix::fs::MetadataExt as _;

    let mut resolved = std::path::PathBuf::new();
    for component in directory.components() {
        resolved.push(component);
        let root_owned_link = std::fs::symlink_metadata(&resolved)
            .is_ok_and(|meta| meta.file_type().is_symlink() && meta.uid() == 0);
        if root_owned_link && let Ok(real) = resolved.canonicalize() {
            resolved = real;
        }
    }
    resolved
}

#[cfg(unix)]
fn open_owned_directory(directory: &Path, protect: bool, create: bool) -> Result<PrivateDirectory> {
    use std::os::fd::FromRawFd as _;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    anyhow::ensure!(
        directory.is_absolute(),
        "xAI OAuth credentials directory must be absolute"
    );
    let resolved = resolve_ancestors(directory);
    let directory = resolved.as_path();
    // SAFETY: the literal root path contains no interior NUL and the returned
    // descriptor is immediately owned by `File`.
    let root_fd = unsafe {
        libc::open(
            c"/".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if root_fd < 0 {
        return Err(std::io::Error::last_os_error()).context("opening filesystem root");
    }
    // SAFETY: `root_fd` is a newly owned descriptor on the success path above.
    let mut current = unsafe { File::from_raw_fd(root_fd) };
    for component in directory.components() {
        let Component::Normal(name) = component else {
            if matches!(component, Component::RootDir) {
                continue;
            }
            bail!(
                "Codewhale credentials directory has an unsupported component: {}",
                crate::quote_os_path(directory)
            );
        };
        let name = cstring_from_os_str(name)?;
        // SAFETY: parent borrowed from live `current`; `name` outlives the call.
        let mut fd = unsafe {
            libc::openat(
                std::os::fd::AsRawFd::as_raw_fd(&current),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if create
            && fd < 0
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
        {
            // SAFETY: both the parent descriptor and component pointer remain
            // valid for this call. `mkdirat` cannot follow the missing leaf.
            let created = unsafe {
                libc::mkdirat(
                    std::os::fd::AsRawFd::as_raw_fd(&current),
                    name.as_ptr(),
                    0o700,
                )
            };
            if created != 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error).with_context(|| {
                        format!(
                            "creating a component of Codewhale credentials directory {}",
                            crate::quote_os_path(directory)
                        )
                    });
                }
            }
            // SAFETY: same stable parent/component arguments as above.
            fd = unsafe {
                libc::openat(
                    std::os::fd::AsRawFd::as_raw_fd(&current),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
        }
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "opening Codewhale credentials directory without following links: {}",
                    crate::quote_os_path(directory)
                )
            });
        }
        // SAFETY: `fd` is a newly owned descriptor on the success path above.
        current = unsafe { File::from_raw_fd(fd) };
    }
    let metadata = current.metadata().with_context(|| {
        format!(
            "inspecting Codewhale credentials directory {}",
            crate::quote_os_path(directory)
        )
    })?;
    anyhow::ensure!(
        metadata.is_dir(),
        "Codewhale credentials path must be a directory"
    );
    // SAFETY: geteuid(2) dereferences no pointers.
    anyhow::ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "Codewhale credentials directory must be owned by the current user"
    );
    if protect {
        current.set_permissions(fs::Permissions::from_mode(0o700))?;
    } else {
        anyhow::ensure!(
            metadata.mode() & 0o077 == 0,
            "private endpoint directory must already be current-user-only"
        );
    }
    Ok(PrivateDirectory {
        directory: directory.to_path_buf(),
        directory_handle: current,
    })
}

#[cfg(unix)]
fn cstring_from_os_str(value: &std::ffi::OsStr) -> Result<CString> {
    use std::os::unix::ffi::OsStrExt as _;
    CString::new(value.as_bytes()).context("owned xAI OAuth path contains an interior NUL")
}

#[cfg(unix)]
impl PrivateDirectory {
    fn open_at(&self, name: &str, flags: i32, mode: libc::mode_t) -> Result<Option<File>> {
        use std::os::fd::AsRawFd as _;
        use std::os::fd::FromRawFd as _;

        validate_private_basename(name)?;
        let name = CString::new(name).context("xAI OAuth basename contains an interior NUL")?;
        // SAFETY: the stable directory descriptor and component pointer remain
        // valid for the call; a successful descriptor is transferred to File.
        let fd = unsafe {
            libc::openat(
                self.directory_handle.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                libc::c_uint::from(mode),
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error).with_context(|| {
                format!(
                    "opening Codewhale-owned xAI OAuth path {}",
                    crate::quote_os_path(&self.directory.join(name.to_string_lossy().as_ref()))
                )
            });
        }
        // SAFETY: `fd` is newly owned on the success path above.
        Ok(Some(unsafe { File::from_raw_fd(fd) }))
    }

    pub fn open_owned_file_for_read(&self, name: &str) -> Result<Option<File>> {
        self.open_at(name, libc::O_RDONLY, 0)
    }

    pub fn open_internal_file(&self, name: &str) -> Result<File> {
        use std::os::unix::fs::PermissionsExt as _;
        let file = self
            .open_at(name, libc::O_RDWR | libc::O_CREAT, 0o600)?
            .context("xAI OAuth lifecycle lock disappeared while opening")?;
        validate_owned_file_handle(&file, &self.directory.join(name))?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }

    pub fn write_owned_file(&self, name: &str, bytes: &[u8], allow_replace: bool) -> Result<()> {
        use std::os::fd::AsRawFd as _;
        use std::os::unix::fs::PermissionsExt as _;

        let temp_name = format!(
            ".xai-oauth-write-{}-{}.tmp",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let mut temp = self
            .open_at(
                &temp_name,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )?
            .context("creating private xAI OAuth temporary file")?;
        let result = (|| -> Result<()> {
            temp.write_all(bytes)
                .context("writing xAI OAuth temporary file")?;
            temp.flush().context("flushing xAI OAuth temporary file")?;
            temp.set_permissions(fs::Permissions::from_mode(0o600))?;
            temp.sync_all()
                .context("syncing xAI OAuth temporary file")?;

            let target =
                CString::new(name).context("xAI OAuth basename contains an interior NUL")?;
            let temporary =
                CString::new(temp_name.as_str()).context("temporary basename contains NUL")?;
            if allow_replace {
                if let Some(existing) = self.open_owned_file_for_read(name)? {
                    validate_owned_file_handle(&existing, &self.directory.join(name))?;
                }
                // SAFETY: both names are relative to the same stable directory
                // handle; rename is atomic and cannot escape that directory.
                if unsafe {
                    libc::renameat(
                        self.directory_handle.as_raw_fd(),
                        temporary.as_ptr(),
                        self.directory_handle.as_raw_fd(),
                        target.as_ptr(),
                    )
                } != 0
                {
                    return Err(std::io::Error::last_os_error())
                        .context("atomically replacing xAI OAuth credentials");
                }
            } else {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                // Exclusive rename publishes one link atomically. A hard-link
                // publication briefly exposes two links to concurrent readers.
                self.move_no_replace(&temp_name, name)?;
                #[cfg(not(any(target_os = "macos", target_os = "linux")))]
                {
                    // `linkat` installs the unique generation without clobbering an
                    // existing path. The temporary link is removed immediately.
                    // SAFETY: all descriptors/names remain valid for both calls.
                    if unsafe {
                        libc::linkat(
                            self.directory_handle.as_raw_fd(),
                            temporary.as_ptr(),
                            self.directory_handle.as_raw_fd(),
                            target.as_ptr(),
                            0,
                        )
                    } != 0
                    {
                        return Err(std::io::Error::last_os_error())
                            .context("installing a new xAI OAuth generation without replacement");
                    }
                    // SAFETY: same descriptor and staging name as the `linkat` above.
                    if unsafe {
                        libc::unlinkat(self.directory_handle.as_raw_fd(), temporary.as_ptr(), 0)
                    } != 0
                    {
                        let error = std::io::Error::last_os_error();
                        // The target and staging name still reference the same
                        // inode. Remove the just-installed target so the generic
                        // error cleanup can safely retire the single remaining
                        // staging link instead of leaving an inert secret with
                        // link count two.
                        // SAFETY: same descriptor; `target` was just installed above.
                        unsafe {
                            libc::unlinkat(self.directory_handle.as_raw_fd(), target.as_ptr(), 0)
                        };
                        return Err(error).context("removing xAI OAuth generation staging link");
                    }
                }
            }
            self.directory_handle
                .sync_all()
                .context("syncing Codewhale credentials directory")?;
            Ok(())
        })();
        drop(temp);
        if result.is_err() {
            let _ = self.remove_raw(&temp_name);
        }
        result
    }

    pub fn remove_raw(&self, name: &str) -> Result<bool> {
        use std::os::fd::AsRawFd as _;
        validate_private_basename(name)?;
        let Some(file) = self.open_owned_file_for_read(name)? else {
            return Ok(false);
        };
        validate_owned_file_handle(&file, &self.directory.join(name))?;
        drop(file);
        let name = CString::new(name).context("xAI OAuth basename contains an interior NUL")?;
        // SAFETY: the name is one component relative to the stable credentials
        // directory descriptor and was validated immediately above.
        if unsafe { libc::unlinkat(self.directory_handle.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(false);
            }
            return Err(error).context("removing Codewhale-owned xAI OAuth file");
        }
        Ok(true)
    }

    pub fn rename_raw(&self, from: &str, to: &str) -> Result<()> {
        use std::os::fd::AsRawFd as _;
        validate_private_basename(from)?;
        validate_private_basename(to)?;
        let source = self
            .open_owned_file_for_read(from)?
            .context("xAI OAuth source disappeared before retirement")?;
        validate_owned_file_handle(&source, &self.directory.join(from))?;
        anyhow::ensure!(
            self.open_owned_file_for_read(to)?.is_none(),
            "refusing to replace an existing xAI OAuth retirement path"
        );
        drop(source);
        let from = CString::new(from).context("xAI OAuth basename contains an interior NUL")?;
        let to = CString::new(to).context("xAI OAuth basename contains an interior NUL")?;
        // SAFETY: both names are one component relative to the same pinned
        // directory descriptor.
        if unsafe {
            libc::renameat(
                self.directory_handle.as_raw_fd(),
                from.as_ptr(),
                self.directory_handle.as_raw_fd(),
                to.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error()).context("retiring xAI OAuth file");
        }
        Ok(())
    }
}

#[cfg(unix)]
pub(crate) fn validate_owned_file_handle(file: &File, path: &Path) -> Result<fs::Metadata> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = file.metadata().with_context(|| {
        format!(
            "inspecting Codewhale-owned xAI OAuth file {}",
            crate::quote_os_path(path)
        )
    })?;
    anyhow::ensure!(metadata.is_file(), "xAI OAuth path must be a regular file");
    // SAFETY: geteuid(2) dereferences no pointers.
    anyhow::ensure!(
        metadata.uid() == unsafe { libc::geteuid() },
        "xAI OAuth file must be owned by the current user"
    );
    anyhow::ensure!(metadata.nlink() != 0, UnlinkedPrivateFile);
    anyhow::ensure!(
        metadata.nlink() == 1,
        "xAI OAuth file must not have multiple filesystem links"
    );
    Ok(metadata)
}

#[cfg(windows)]
fn open_owned_directory(directory: &Path, protect: bool, create: bool) -> Result<PrivateDirectory> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_SHARE_READ, FILE_SHARE_WRITE, WRITE_DAC, WRITE_OWNER,
    };

    anyhow::ensure!(
        directory.is_absolute(),
        "xAI OAuth credentials directory must be absolute"
    );
    let mut current = PathBuf::new();
    let mut handles = Vec::new();
    let mut created_final = false;
    for component in directory.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(Path::new(r"\")),
            Component::Normal(name) => {
                current.push(name);
                created_final = false;
                if create {
                    match fs::create_dir(&current) {
                        Ok(()) => {
                            created_final = true;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => {
                            return Err(error).with_context(|| {
                                format!(
                                    "creating a component of Codewhale credentials directory {}",
                                    crate::quote_os_path(directory)
                                )
                            });
                        }
                    }
                }
                let mut options = fs::OpenOptions::new();
                options
                    .read(true)
                    .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
                let handle = options.open(&current).with_context(|| {
                    format!(
                        "opening Codewhale credentials directory component {}",
                        crate::quote_os_path(&current)
                    )
                })?;
                validate_windows_handle_path(&handle, &current, true)?;
                handles.push(handle);
            }
            Component::CurDir | Component::ParentDir => bail!(
                "Codewhale credentials directory must be lexically normalized: {}",
                crate::quote_os_path(directory)
            ),
        }
    }
    anyhow::ensure!(
        !handles.is_empty(),
        "Codewhale credentials directory cannot be a volume root"
    );
    let mut secure_options = fs::OpenOptions::new();
    secure_options
        .access_mode(FILE_GENERIC_READ | WRITE_DAC | WRITE_OWNER)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let final_directory = secure_options.open(directory).with_context(|| {
        format!(
            "opening Codewhale credentials directory for owner-only security: {}",
            crate::quote_os_path(directory)
        )
    })?;
    validate_windows_handle_path(&final_directory, directory, true)?;
    if protect || created_final {
        secure_windows_owner_only_handle(&final_directory, true)
            .context("securing the current-user private directory")?;
    }
    verify_windows_owner_only_handle(&final_directory)
        .context("verifying Codewhale credentials directory ownership")?;
    handles.push(final_directory);
    Ok(PrivateDirectory {
        directory: directory.to_path_buf(),
        _component_handles: handles,
    })
}

#[cfg(windows)]
impl PrivateDirectory {
    fn open_windows_file(&self, name: &str, read: bool, write: bool) -> Result<Option<File>> {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        validate_private_basename(name)?;
        let path = self.directory.join(name);
        let mut options = fs::OpenOptions::new();
        options
            .read(read)
            .write(write)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        match options.open(&path) {
            Ok(file) => {
                validate_owned_file_handle(&file, &path)?;
                Ok(Some(file))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error).with_context(|| {
                format!(
                    "opening Codewhale-owned xAI OAuth path {}",
                    crate::quote_os_path(&path)
                )
            }),
        }
    }

    pub fn open_owned_file_for_read(&self, name: &str) -> Result<Option<File>> {
        self.open_windows_file(name, true, false)
    }

    pub fn open_internal_file(&self, name: &str) -> Result<File> {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, WRITE_DAC, WRITE_OWNER,
        };

        validate_private_basename(name)?;
        let path = self.directory.join(name);
        for _ in 0..8 {
            if let Some(existing) = self.open_windows_file(name, true, true)? {
                return Ok(existing);
            }

            let mut options = fs::OpenOptions::new();
            options
                // `access_mode` supplies the exact Win32 access mask below,
                // while Rust still requires the portable write intent to be
                // set before it permits `create_new`.
                .write(true)
                .access_mode(
                    FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC | WRITE_OWNER | DELETE,
                )
                .create_new(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
            let file = match options.open(&path) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "creating Codewhale-owned xAI OAuth lifecycle lock {}",
                            crate::quote_os_path(&path)
                        )
                    });
                }
            };
            let secured = (|| -> Result<()> {
                validate_windows_file_shape(&file, &path)?;
                secure_windows_owner_only_handle(&file, false)
                    .context("securing a new xAI OAuth lifecycle lock")?;
                validate_owned_file_handle(&file, &path)?;
                Ok(())
            })();
            if let Err(error) = secured {
                let cleanup = mark_windows_file_handle_for_deletion(&file);
                return match cleanup {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(error).context(format!(
                        "also failed to delete the empty lifecycle lock: {cleanup:#}"
                    )),
                };
            }
            return Ok(file);
        }
        bail!("xAI OAuth lifecycle lock changed repeatedly while opening")
    }

    pub fn write_owned_file(&self, name: &str, bytes: &[u8], allow_replace: bool) -> Result<()> {
        let path = self.directory.join(name);
        if let Some(existing) = self.open_owned_file_for_read(name)? {
            anyhow::ensure!(
                allow_replace,
                "refusing to replace an existing xAI OAuth generation"
            );
            drop(existing);
        }
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)
            .context("creating private xAI OAuth temporary file")?;
        let temporary_path = temporary.path().to_path_buf();
        let security_handle =
            reopen_windows_file_for_owner_security(temporary.as_file(), &temporary_path)?;
        secure_windows_owner_only_handle(&security_handle, false)
            .context("securing a new xAI OAuth temporary file before writing credentials")?;
        validate_owned_file_handle(&security_handle, &temporary_path)
            .context("verifying a new xAI OAuth temporary file before writing credentials")?;
        let write_result = (|| -> Result<()> {
            temporary
                .write_all(bytes)
                .context("writing xAI OAuth temporary file")?;
            temporary
                .flush()
                .context("flushing xAI OAuth temporary file")?;
            temporary
                .as_file()
                .sync_all()
                .context("syncing xAI OAuth temporary file")?;
            Ok(())
        })();
        if let Err(error) = write_result {
            return Err(cleanup_windows_secret_after_error(
                &security_handle,
                error,
                "temporary file",
            ));
        }
        let persisted = if allow_replace {
            match temporary.persist(&path) {
                Ok(file) => file,
                Err(error) => {
                    let tempfile::PersistError { error, file } = error;
                    let persistence_error = anyhow::Error::new(error)
                        .context("atomically replacing xAI OAuth credentials");
                    let error = cleanup_windows_secret_after_error(
                        &security_handle,
                        persistence_error,
                        "temporary file",
                    );
                    drop(file);
                    return Err(error);
                }
            }
        } else {
            match temporary.persist_noclobber(&path) {
                Ok(file) => file,
                Err(error) => {
                    let tempfile::PersistError { error, file } = error;
                    let persistence_error = anyhow::Error::new(error)
                        .context("installing a new xAI OAuth generation without replacement");
                    let error = cleanup_windows_secret_after_error(
                        &security_handle,
                        persistence_error,
                        "temporary file",
                    );
                    drop(file);
                    return Err(error);
                }
            }
        };
        if let Err(error) = validate_persisted_windows_owned_file(&persisted, &path) {
            // MoveFileEx has already published this exact object. Delete it by
            // handle rather than trusting the pathname again. For a refresh
            // replacement this can leave the unchanged config pointer missing;
            // that fail-closed availability outcome is safer than retaining a
            // generation that failed the post-publication invariant check.
            return Err(cleanup_windows_secret_after_error(
                &security_handle,
                error,
                "rejected generation",
            ));
        }
        Ok(())
    }

    pub fn remove_raw(&self, name: &str) -> Result<bool> {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        validate_private_basename(name)?;
        let path = self.directory.join(name);
        let mut options = fs::OpenOptions::new();
        options
            .access_mode(FILE_GENERIC_READ | DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error).context("opening xAI OAuth file for exact deletion"),
        };
        validate_owned_file_handle(&file, &path)?;
        mark_windows_file_handle_for_deletion(&file)?;
        drop(file);
        Ok(true)
    }
}

#[cfg(windows)]
fn reopen_windows_file_for_owner_security(file: &File, path: &Path) -> Result<File> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _};
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, ReOpenFile, WRITE_DAC, WRITE_OWNER,
    };

    // ReOpenFile derives a new handle from the already-created temporary file,
    // so no pathname can be substituted between creation and hardening.
    // SAFETY: `file` is live; no output pointers passed.
    let handle = unsafe {
        ReOpenFile(
            file.as_raw_handle(),
            FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC | WRITE_OWNER | DELETE,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            FILE_FLAG_OPEN_REPARSE_POINT,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error())
            .context("reopening a new xAI OAuth temporary file for owner-only security");
    }
    // SAFETY: ReOpenFile returned a newly owned handle on the success path.
    let reopened = unsafe { File::from_raw_handle(handle) };
    validate_windows_file_shape(&reopened, path)?;
    Ok(reopened)
}

#[cfg(windows)]
fn mark_windows_file_handle_for_deletion(file: &File) -> Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };

    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the disposition buffer has the documented structure and the
    // handle remains owned until after the call. Windows marks this exact file
    // object delete-pending rather than resolving the path again.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("marking exact xAI OAuth file handle for deletion");
    }
    Ok(())
}

#[cfg(windows)]
fn cleanup_windows_secret_after_error(
    file: &File,
    error: anyhow::Error,
    label: &str,
) -> anyhow::Error {
    match mark_windows_file_handle_for_deletion(file) {
        Ok(()) => error,
        Err(cleanup) => error.context(format!(
            "also failed to delete the xAI OAuth {label} by exact handle: {cleanup:#}"
        )),
    }
}

#[cfg(all(windows, test))]
static WINDOWS_POST_PERSIST_VALIDATION_FAILURE: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(windows)]
fn validate_persisted_windows_owned_file(file: &File, path: &Path) -> Result<fs::Metadata> {
    #[cfg(test)]
    {
        let mut injected = WINDOWS_POST_PERSIST_VALIDATION_FAILURE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if injected.as_deref() == Some(path) {
            *injected = None;
            bail!("injected post-persistence xAI OAuth validation failure");
        }
    }
    validate_owned_file_handle(file, path)
}

#[cfg(all(windows, test))]
pub(crate) fn fail_next_windows_post_persist_validation(path: &Path) {
    *WINDOWS_POST_PERSIST_VALIDATION_FAILURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path.to_path_buf());
}

#[cfg(windows)]
pub(crate) fn validate_owned_file_handle(file: &File, path: &Path) -> Result<fs::Metadata> {
    let metadata = validate_windows_file_shape(file, path)?;
    verify_windows_owner_only_handle(file)
        .context("Codewhale-owned xAI OAuth file is not current-user-only")?;
    Ok(metadata)
}

#[cfg(windows)]
pub(crate) fn validate_windows_file_shape(file: &File, path: &Path) -> Result<fs::Metadata> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let metadata = validate_windows_handle_path(file, path, false)?;
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: both pointers remain valid for the duration of the call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("inspecting xAI OAuth file link count");
    }
    anyhow::ensure!(
        information.nNumberOfLinks == 1,
        "xAI OAuth file must not have multiple filesystem links"
    );
    Ok(metadata)
}

#[cfg(windows)]
fn validate_windows_handle_path(
    file: &File,
    expected: &Path,
    expect_directory: bool,
) -> Result<fs::Metadata> {
    use std::os::windows::fs::MetadataExt as _;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let metadata = file.metadata().with_context(|| {
        format!(
            "inspecting Codewhale-owned path {}",
            crate::quote_os_path(expected)
        )
    })?;
    anyhow::ensure!(
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
        "Codewhale-owned xAI OAuth path must not be a reparse point"
    );
    anyhow::ensure!(
        if expect_directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        },
        "Codewhale-owned xAI OAuth path has the wrong filesystem type"
    );

    // Compare the current selection to the captured object, not its lexical
    // spelling: Windows can select the same object through an 8.3 short name.
    // Reparse points are refused at every component. These attribute-only
    // handles do not pin the path (they bypass share checks); a component
    // swapped mid-walk either fails to open or selects a different identity,
    // which this comparison refuses.
    let components = open_windows_selected_components(expected, expect_directory)?;
    let selected = components
        .last()
        .context("private selected path unavailable")?;
    anyhow::ensure!(
        windows_file_identity(file)? == windows_file_identity(selected)?
            && normalize_windows_path_for_comparison(&windows_final_handle_path(file)?)?
                == normalize_windows_path_for_comparison(&windows_final_handle_path(selected)?)?,
        "Codewhale-owned xAI OAuth path was redirected while opening"
    );
    Ok(metadata)
}

#[cfg(windows)]
fn open_windows_selected_components(expected: &Path, expect_directory: bool) -> Result<Vec<File>> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    anyhow::ensure!(
        expected.is_absolute(),
        "private selected path must be absolute"
    );
    let mut current = PathBuf::new();
    let mut handles = Vec::new();
    let mut components = expected.components().peekable();
    while let Some(component) = components.next() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(Path::new(r"\")),
            Component::Normal(name) => {
                current.push(name);
                let directory = components.peek().is_some() || expect_directory;
                let handle = fs::OpenOptions::new()
                    .access_mode(FILE_READ_ATTRIBUTES)
                    .share_mode(
                        FILE_SHARE_READ
                            | FILE_SHARE_WRITE
                            | if directory { 0 } else { FILE_SHARE_DELETE },
                    )
                    .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
                    .open(&current)
                    .context("opening the current private path selection")?;
                let metadata = handle.metadata()?;
                anyhow::ensure!(
                    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT == 0,
                    "Codewhale-owned xAI OAuth path must not be a reparse point"
                );
                anyhow::ensure!(
                    if directory {
                        metadata.is_dir()
                    } else {
                        metadata.is_file()
                    },
                    "Codewhale-owned xAI OAuth path has the wrong filesystem type"
                );
                handles.push(handle);
            }
            Component::CurDir | Component::ParentDir => {
                bail!("private selected path must be lexically normalized")
            }
        }
    }
    anyhow::ensure!(
        !handles.is_empty(),
        "private selected path cannot be a volume root"
    );
    Ok(handles)
}

#[cfg(windows)]
fn windows_final_handle_path(file: &File) -> Result<PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    let flags = FILE_NAME_NORMALIZED | VOLUME_NAME_DOS;
    let handle = file.as_raw_handle();
    // SAFETY: null output asks only for the required UTF-16 length.
    let needed = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, flags) };
    if needed == 0 {
        return Err(std::io::Error::last_os_error())
            .context("resolving Codewhale-owned xAI OAuth handle path");
    }
    let mut buffer = vec![0u16; needed as usize + 1];
    // SAFETY: the buffer is writable and the handle remains valid.
    let written = unsafe {
        GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
    };
    if written == 0 || written as usize >= buffer.len() {
        return Err(std::io::Error::last_os_error())
            .context("resolving Codewhale-owned xAI OAuth handle path");
    }
    Ok(PathBuf::from(OsString::from_wide(
        &buffer[..written as usize],
    )))
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    #[test]
    fn selected_path_alias_keeps_identity_and_replacement_is_refused() {
        let root = tempfile::tempdir().expect("temporary root");
        let selected = root.path().join("credentials");
        fs::create_dir(&selected).expect("selected directory");
        // Hosted Windows temporary roots can contain RUNNER~1 while the kernel
        // reports the long spelling. Both must select the same retained object.
        let canonical = selected
            .canonicalize()
            .expect("canonical selected directory");
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&selected)
            .expect("captured selected directory");
        validate_windows_handle_path(&held, &selected, true).expect("selected spelling");
        validate_windows_handle_path(&held, &canonical, true).expect("canonical spelling");

        let retained = root.path().join("retained");
        fs::rename(&selected, &retained).expect("move captured object");
        fs::create_dir(&selected).expect("replacement at the same selected path");
        assert!(
            validate_windows_handle_path(&held, &selected, true).is_err(),
            "a replacement at the selected path must not inherit the captured identity"
        );
        validate_windows_handle_path(&held, &retained, true).expect("actual retained object");
        fs::create_dir(root.path().join("other")).expect("existing unnormalized path component");
        assert!(
            validate_windows_handle_path(&held, &root.path().join("other/../retained"), true)
                .is_err(),
            "lexically unnormalized paths remain inadmissible"
        );
    }
}

#[cfg(windows)]
pub(crate) fn normalize_windows_path_for_comparison(path: &Path) -> Result<String> {
    let text = path.to_str().ok_or_else(|| {
        anyhow::anyhow!(
            "xAI OAuth path {} contains invalid Unicode and cannot be compared safely",
            crate::quote_os_path(path)
        )
    })?;
    let without_device_prefix = text.strip_prefix(r"\\?\").unwrap_or(text);
    let normalized_prefix = without_device_prefix.strip_prefix("UNC\\").map_or_else(
        || without_device_prefix.to_string(),
        |rest| format!(r"\\{rest}"),
    );
    Ok(normalized_prefix
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase())
}

#[cfg(windows)]
fn secure_windows_owner_only_handle(file: &File, inherit_to_children: bool) -> Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    let acl = crate::windows_identity::OwnerOnlyAcl::new(FILE_ALL_ACCESS, inherit_to_children)?;
    // SAFETY: the file handle remains owned by `file`, and the ACL remains
    // allocated for the duration of the call. The owner and protected DACL are
    // committed together so the verifier never observes a half-secured file.
    let result = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            acl.user_sid(),
            std::ptr::null_mut(),
            acl.acl(),
            std::ptr::null(),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(result as i32))
            .context("applying a current-user-only DACL to Codewhale-owned xAI OAuth storage");
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn verify_windows_owner_only_handle(file: &File) -> Result<()> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, GRANT_ACCESS, GetExplicitEntriesFromAclW, GetSecurityInfo,
        SE_FILE_OBJECT, SET_ACCESS, TRUSTEE_IS_SID,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, EqualSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    let user = CurrentWindowsUser::open()?;
    let mut owner: PSID = std::ptr::null_mut();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle remains valid and all output pointers are writable.
    let result = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if result != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(result as i32))
            .context("reading Codewhale-owned xAI OAuth security descriptor");
    }
    let _descriptor = WindowsLocalAllocation(descriptor.cast());
    // SAFETY: `owner` is non-null; `user.sid()` is owned by `user`.
    anyhow::ensure!(
        !owner.is_null() && unsafe { EqualSid(owner, user.sid()) } != 0,
        "Codewhale-owned xAI OAuth storage owner is not the current user"
    );
    anyhow::ensure!(
        !dacl.is_null(),
        "Codewhale-owned xAI OAuth storage must have an owner-only DACL"
    );
    let mut count = 0;
    let mut entries: *mut EXPLICIT_ACCESS_W = std::ptr::null_mut();
    // SAFETY: `dacl` belongs to the live descriptor; Windows allocates the
    // returned entry array, released by the guard below.
    let result = unsafe { GetExplicitEntriesFromAclW(dacl, &mut count, &mut entries) };
    if result != ERROR_SUCCESS {
        return Err(std::io::Error::from_raw_os_error(result as i32))
            .context("reading Codewhale-owned xAI OAuth DACL entries");
    }
    let _entries = WindowsLocalAllocation(entries.cast());
    anyhow::ensure!(
        count == 1 && !entries.is_null(),
        "Codewhale-owned xAI OAuth DACL must grant only one user"
    );
    // SAFETY: `count == 1` proves the first returned entry is initialized.
    let entry = unsafe { &*entries };
    let trustee_sid: PSID = entry.Trustee.ptstrName.cast();
    // SAFETY: form and null checked in this expression; sid owned by `user`.
    anyhow::ensure!(
        entry.Trustee.TrusteeForm == TRUSTEE_IS_SID
            && !trustee_sid.is_null()
            && unsafe { EqualSid(trustee_sid, user.sid()) } != 0
            && matches!(entry.grfAccessMode, SET_ACCESS | GRANT_ACCESS)
            && entry.grfAccessPermissions == FILE_ALL_ACCESS,
        "Codewhale-owned xAI OAuth DACL is not current-user-only"
    );
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn open_owned_directory(directory: &Path, protect: bool, create: bool) -> Result<PrivateDirectory> {
    anyhow::ensure!(
        protect,
        "private endpoint authentication is unsupported on this platform"
    );
    if create {
        fs::create_dir_all(directory)?;
    }
    let metadata = fs::symlink_metadata(directory)?;
    anyhow::ensure!(
        metadata.is_dir(),
        "Codewhale credentials path must be a directory"
    );
    Ok(PrivateDirectory {
        directory: directory.to_path_buf(),
    })
}

#[cfg(not(any(unix, windows)))]
impl PrivateDirectory {
    pub fn open_owned_file_for_read(&self, name: &str) -> Result<Option<File>> {
        validate_private_basename(name)?;
        match File::open(self.directory.join(name)) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn open_internal_file(&self, name: &str) -> Result<File> {
        validate_private_basename(name)?;
        Ok(fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(self.directory.join(name))?)
    }

    pub fn write_owned_file(&self, name: &str, bytes: &[u8], allow_replace: bool) -> Result<()> {
        validate_private_basename(name)?;
        let path = self.directory.join(name);
        anyhow::ensure!(
            allow_replace || !path.exists(),
            "refusing to replace xAI OAuth generation"
        );
        crate::persistence::atomic_write(&path, bytes)
    }

    pub fn remove_raw(&self, name: &str) -> Result<bool> {
        validate_private_basename(name)?;
        match fs::remove_file(self.directory.join(name)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn rename_raw(&self, from: &str, to: &str) -> Result<()> {
        validate_private_basename(from)?;
        validate_private_basename(to)?;
        fs::rename(self.directory.join(from), self.directory.join(to))?;
        Ok(())
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn validate_owned_file_handle(file: &File, _path: &Path) -> Result<fs::Metadata> {
    let metadata = file.metadata()?;
    anyhow::ensure!(metadata.is_file(), "xAI OAuth path must be a regular file");
    Ok(metadata)
}

/// Filesystem identity of an owned Unix socket below a retained private parent.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivateSocketIdentity {
    device: libc::dev_t,
    inode: libc::ino_t,
}

#[cfg(unix)]
impl PrivateDirectory {
    pub fn socket_identity(&self, name: &str) -> Result<Option<PrivateSocketIdentity>> {
        use std::os::fd::AsRawFd as _;
        validate_private_basename(name)?;
        let name = CString::new(name)?;
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: one basename below the retained directory; output is writable.
        if unsafe {
            libc::fstatat(
                self.directory_handle.as_raw_fd(),
                name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(None);
            }
            return Err(error.into());
        }
        // SAFETY: successful fstatat initialized the entire stat object.
        let metadata = unsafe { metadata.assume_init() };
        if metadata.st_mode & libc::S_IFMT != libc::S_IFSOCK {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "private endpoint exists and is not a socket",
            )
            .into());
        }
        anyhow::ensure!(
            metadata.st_uid == Self::current_user_id(),
            "private socket is not owned by the current user"
        );
        Ok(Some(PrivateSocketIdentity {
            device: metadata.st_dev,
            inode: metadata.st_ino,
        }))
    }

    /// Never follows a substituted leaf to change another object's permissions.
    pub fn protect_socket(&self, name: &str, identity: PrivateSocketIdentity) -> Result<()> {
        use std::os::fd::AsRawFd as _;
        anyhow::ensure!(
            self.socket_identity(name)? == Some(identity),
            "private socket changed before protection"
        );
        let name_c = CString::new(name)?;
        // SAFETY: retained directory, one component; no-follow is mandatory.
        if unsafe {
            libc::fchmodat(
                self.directory_handle.as_raw_fd(),
                name_c.as_ptr(),
                0o600,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("protecting private socket without following links");
        }
        anyhow::ensure!(
            self.socket_identity(name)? == Some(identity),
            "private socket changed during protection"
        );
        Ok(())
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn move_no_replace(&self, from: &str, to: &str) -> Result<()> {
        use std::os::fd::AsRawFd as _;
        validate_private_basename(from)?;
        validate_private_basename(to)?;
        let from = CString::new(from)?;
        let to = CString::new(to)?;
        #[cfg(target_os = "macos")]
        // SAFETY: both basenames and retained descriptors remain valid; EXCL refuses replacement.
        let result = unsafe {
            libc::renameatx_np(
                self.directory_handle.as_raw_fd(),
                from.as_ptr(),
                self.directory_handle.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        #[cfg(target_os = "linux")]
        // Use the kernel syscall because musl need not export a renameat2 wrapper.
        // SAFETY: same retained directory; NOREPLACE is required, never emulated by a check.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                self.directory_handle.as_raw_fd(),
                from.as_ptr(),
                self.directory_handle.as_raw_fd(),
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("moving private entry without replacement");
        }
        Ok(())
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn move_no_replace(&self, _from: &str, _to: &str) -> Result<()> {
        bail!("exclusive endpoint retirement is unsupported on this platform")
    }

    pub fn retire_socket(&self, name: &str, expected: PrivateSocketIdentity) -> Result<bool> {
        use std::os::fd::AsRawFd as _;
        if self.socket_identity(name)? != Some(expected) {
            return Ok(false);
        }
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let retired = format!(
            ".cw-endpoint-{}-{}-{}.retired",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        self.move_no_replace(name, &retired)?;
        if self.socket_identity(&retired)? != Some(expected) {
            let restored = self.move_no_replace(&retired, name);
            return Err(anyhow::anyhow!(
                "endpoint identity changed; replacement preserved (restore: {restored:?})"
            ));
        }
        let retired_c = CString::new(retired)?;
        // SAFETY: the exclusively retired, matching socket is below the retained private parent.
        if unsafe { libc::unlinkat(self.directory_handle.as_raw_fd(), retired_c.as_ptr(), 0) } != 0
        {
            return Err(std::io::Error::last_os_error())
                .context("removing exact retired private socket");
        }
        self.directory_handle.sync_all()?;
        Ok(true)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::os::unix::net::UnixListener;

    fn root() -> tempfile::TempDir {
        let selected = Path::new("/tmp").canonicalize().unwrap();
        tempfile::Builder::new()
            .prefix("cw-pd-")
            .tempdir_in(selected)
            .unwrap()
    }

    #[test]
    fn owner_receipt_read_is_bounded_and_refuses_links_or_public_permissions() {
        let root = root();
        let parent = PrivateDirectory::admit(&root.path().join("run")).unwrap();
        parent
            .write_owned_file("owner.json", b"12345678", false)
            .unwrap();
        assert!(parent.read_private_receipt("owner.json", 7).is_err());
        assert_eq!(
            parent
                .read_private_receipt("owner.json", 8)
                .unwrap()
                .unwrap()
                .0,
            b"12345678"
        );
        let path = parent.directory.join("owner.json");
        fs::hard_link(&path, parent.directory.join("linked.json")).unwrap();
        assert!(parent.read_private_receipt("owner.json", 8).is_err());
        fs::remove_file(parent.directory.join("linked.json")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(parent.read_private_receipt("owner.json", 8).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&path, parent.directory.join("alias.json")).unwrap();
        assert!(parent.read_private_receipt("alias.json", 8).is_err());
    }

    #[test]
    fn owner_receipt_opened_before_retirement_is_rejected_as_unlinked_not_hardlinked() {
        let root = root();
        let parent = PrivateDirectory::admit(&root.path().join("run")).unwrap();
        parent
            .write_owned_file("owner.json", b"receipt", false)
            .unwrap();
        let file = parent
            .open_owned_file_for_read("owner.json")
            .unwrap()
            .unwrap();
        assert!(parent.remove_raw("owner.json").unwrap());
        let error = parent
            .read_private_receipt_file("owner.json", 8, file)
            .unwrap_err();
        assert!(error.is::<UnlinkedPrivateFile>());

        parent
            .write_owned_file("owner.json", b"receipt", false)
            .unwrap();
        fs::hard_link(
            parent.directory.join("owner.json"),
            parent.directory.join("alias.json"),
        )
        .unwrap();
        let error = parent.read_private_receipt("owner.json", 8).unwrap_err();
        assert!(
            !error.is::<UnlinkedPrivateFile>(),
            "a linked receipt must remain unsafe, not retryable"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn private_exclusive_publication_keeps_one_link_and_preserves_existing_destination() {
        use std::os::unix::fs::MetadataExt as _;
        let root = root();
        let parent = PrivateDirectory::admit(&root.path().join("run")).unwrap();
        let mut staging = parent.open_internal_file("staging").unwrap();
        staging.write_all(b"receipt").unwrap();
        staging.sync_all().unwrap();
        parent.move_no_replace("staging", "owner.json").unwrap();
        assert_eq!(staging.metadata().unwrap().nlink(), 1);
        assert!(
            parent
                .open_owned_file_for_read("staging")
                .unwrap()
                .is_none()
        );
        assert!(
            parent
                .write_owned_file("owner.json", b"replacement", false)
                .is_err()
        );
        assert_eq!(
            parent
                .read_private_receipt("owner.json", 8)
                .unwrap()
                .unwrap()
                .0,
            b"receipt"
        );
        assert_eq!(
            fs::read_dir(&parent.directory).unwrap().count(),
            1,
            "failed publication must clean only its own staging file"
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn owner_receipt_retirement_preserves_a_replacement_and_uses_captured_file() {
        let root = root();
        let parent = PrivateDirectory::admit(&root.path().join("run")).unwrap();
        parent
            .write_owned_file("owner.json", b"captured", false)
            .unwrap();
        let (_, captured) = parent
            .read_private_receipt("owner.json", 8)
            .unwrap()
            .unwrap();
        fs::rename(
            parent.directory.join("owner.json"),
            parent.directory.join("old.json"),
        )
        .unwrap();
        parent
            .write_owned_file("owner.json", b"replaced", false)
            .unwrap();
        assert!(
            !parent
                .retire_private_receipt("owner.json", &captured)
                .unwrap()
        );
        assert_eq!(
            parent
                .read_private_receipt("owner.json", 8)
                .unwrap()
                .unwrap()
                .0,
            b"replaced"
        );
        assert!(
            parent
                .retire_private_receipt("old.json", &captured)
                .unwrap()
        );
        assert!(
            parent
                .read_private_receipt("old.json", 8)
                .unwrap()
                .is_none()
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn local_process_generation_is_kernel_bound_and_refuses_invalid_pid() {
        let first = unix_process_start(std::process::id()).unwrap();
        assert!(!first.is_empty());
        assert_eq!(unix_process_start(std::process::id()).unwrap(), first);
        assert!(unix_process_start(0).is_err());
        assert!(unix_process_start(u32::MAX).is_err());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn unix_process_status_requires_positive_kernel_absence() {
        assert_eq!(
            unix_process_status(std::process::id()).unwrap(),
            UnixProcessStatus::Present {
                start: unix_process_start(std::process::id()).unwrap()
            }
        );
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(unix_process_status(pid).unwrap(), UnixProcessStatus::Absent);
        assert!(unix_process_status(0).is_err());
        assert!(unix_process_status(u32::MAX).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_process_status_never_converts_uncertainty_to_death() {
        assert!(unix_pid_is_absent(
            -1,
            &std::io::Error::from_raw_os_error(libc::ESRCH)
        ));
        for code in [libc::EPERM, libc::EACCES, libc::EINVAL] {
            assert!(!unix_pid_is_absent(
                -1,
                &std::io::Error::from_raw_os_error(code)
            ));
        }
        assert!(!unix_pid_is_absent(
            0,
            &std::io::Error::from_raw_os_error(libc::ESRCH)
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_generation_survives_nondumpable_hardening() {
        const CHILD: &str = "CODEWHALE_PROCESS_IDENTITY_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let pid = std::process::id();
            let before = unix_process_start(pid).unwrap();
            // SAFETY: changes only this isolated child test process.
            assert_eq!(unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) }, 0);
            // SAFETY: queries only the calling process with no pointer arguments.
            assert_eq!(unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) }, 0);
            if PrivateDirectory::current_user_id() != 0 {
                use std::os::unix::fs::MetadataExt as _;
                assert_eq!(fs::metadata(format!("/proc/{pid}/stat")).unwrap().uid(), 0);
            }
            assert_eq!(unix_process_start(pid).unwrap(), before);
            assert!(unix_process_start(0).is_err());
            assert!(unix_process_start(u32::MAX).is_err());
            println!("HARDENED_PROCESS_GENERATION_VERIFIED");
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "private_directory::tests::linux_process_generation_survives_nondumpable_hardening",
                "--exact",
                "--nocapture",
            ])
            .env_clear()
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated hardened process failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("HARDENED_PROCESS_GENERATION_VERIFIED")
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_status_refuses_foreign_or_malformed_principals() {
        let valid = "Name:\tfixture\nPid:\t42\nUid:\t100\t101\t102\t103\n";
        assert!(validate_linux_process_status(valid, 42, 101).is_ok());
        assert!(validate_linux_process_status(valid, 42, 100).is_err());
        assert!(validate_linux_process_status(valid, 43, 101).is_err());
        for status in [
            "Uid:\t100 101 102 103\n",
            "Pid:\t42\n",
            "Pid:\t42\nUid:\t100 101 102\n",
            "Pid:\t42\nUid:\t100 101 102 103 104\n",
            "Pid:\t42\nUid:\t100 invalid 102 103\n",
            "Pid:\t42\nPid:\t42\nUid:\t100 101 102 103\n",
            "Pid:\t42\nUid:\t100 101 102 103\nUid:\t100 101 102 103\n",
        ] {
            assert!(validate_linux_process_status(status, 42, 101).is_err());
        }
    }

    #[test]
    fn endpoint_admission_refuses_public_existing_directory_without_repair() {
        let root = root();
        let selected = root.path().join("public");
        fs::create_dir(&selected).unwrap();
        fs::set_permissions(&selected, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(PrivateDirectory::admit(&selected).is_err());
        assert_eq!(
            fs::metadata(&selected).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let credentials = PrivateDirectory::open(&selected).unwrap();
        assert_eq!(
            credentials
                .directory_handle
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn endpoint_admission_refuses_linked_component_and_changed_selected_parent() {
        let root = root();
        let original = root.path().join("original");
        let held = PrivateDirectory::admit(&original).unwrap();
        symlink(&original, root.path().join("alias")).unwrap();
        assert!(PrivateDirectory::admit(&root.path().join("alias")).is_err());
        fs::rename(&original, root.path().join("retained")).unwrap();
        PrivateDirectory::admit(&original).unwrap();
        assert!(!held.is_at_selected_path().unwrap());
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn exact_endpoint_retirement_preserves_replacement_and_removes_captured_socket() {
        let root = root();
        let parent = PrivateDirectory::admit(&root.path().join("run")).unwrap();
        let path = parent.directory.join("owner.sock");
        let original = UnixListener::bind(&path).unwrap();
        let identity = parent.socket_identity("owner.sock").unwrap().unwrap();
        fs::rename(&path, parent.directory.join("old.sock")).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        assert!(!parent.retire_socket("owner.sock", identity).unwrap());
        assert!(path.exists());
        assert_eq!(parent.socket_identity("old.sock").unwrap(), Some(identity));
        assert!(parent.retire_socket("old.sock", identity).unwrap());
        assert!(!parent.directory.join("old.sock").exists());
        assert!(path.exists());
        drop((original, replacement));
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn endpoint_move_refuses_to_replace_existing_socket() {
        let root = root();
        let parent = PrivateDirectory::admit(&root.path().join("run")).unwrap();
        let original = UnixListener::bind(parent.directory.join("owner.sock")).unwrap();
        let destination = UnixListener::bind(parent.directory.join("retired.sock")).unwrap();
        let original_identity = parent.socket_identity("owner.sock").unwrap().unwrap();
        let destination_identity = parent.socket_identity("retired.sock").unwrap().unwrap();

        let error = parent
            .move_no_replace("owner.sock", "retired.sock")
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            parent.socket_identity("owner.sock").unwrap(),
            Some(original_identity)
        );
        assert_eq!(
            parent.socket_identity("retired.sock").unwrap(),
            Some(destination_identity)
        );
        drop((original, destination));
    }

    #[test]
    fn endpoint_leaf_symlink_is_never_followed_for_protection_or_retirement() {
        let root = root();
        let parent = PrivateDirectory::admit(&root.path().join("run")).unwrap();
        let path = parent.directory.join("owner.sock");
        let original = UnixListener::bind(&path).unwrap();
        let identity = parent.socket_identity("owner.sock").unwrap().unwrap();
        fs::remove_file(&path).unwrap();
        let other = root.path().join("operator-file");
        fs::write(&other, b"preserve").unwrap();
        fs::set_permissions(&other, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&other, &path).unwrap();
        assert!(parent.protect_socket("owner.sock", identity).is_err());
        assert!(parent.retire_socket("owner.sock", identity).is_err());
        assert_eq!(fs::read(&other).unwrap(), b"preserve");
        assert_eq!(
            fs::metadata(&other).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(
            fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        drop(original);
    }
}

/// A current-user process identity or positive kernel absence. Query failures
/// remain errors; an unreadable process record never establishes death.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
pub enum UnixProcessStatus {
    Present { start: String },
    Absent,
}

#[cfg(unix)]
fn unix_pid_is_absent(result: i32, error: &std::io::Error) -> bool {
    result == -1 && error.raw_os_error() == Some(libc::ESRCH)
}

/// Read on the existing bounded owner worker. Signal zero never terminates a
/// process; only ESRCH after validating a positive PID proves absence.
#[cfg(unix)]
pub fn unix_process_status(pid: u32) -> Result<UnixProcessStatus> {
    let kernel_pid = i32::try_from(pid).context("invalid local process PID")?;
    anyhow::ensure!(kernel_pid > 0, "invalid local process PID");
    match unix_process_start(pid) {
        Ok(start) => Ok(UnixProcessStatus::Present { start }),
        Err(identity_error) => {
            // SAFETY: a validated positive PID and signal zero only query
            // existence. No process-group selection or signal is possible.
            let result = unsafe { libc::kill(kernel_pid, 0) };
            let error = std::io::Error::last_os_error();
            if unix_pid_is_absent(result, &error) {
                Ok(UnixProcessStatus::Absent)
            } else {
                Err(identity_error).context("selected process identity/liveness is uncertain")
            }
        }
    }
}

/// Kernel process creation identity for the authenticated local Unix peer.
/// Run on the existing bounded owner worker; PID alone is never sufficient.
#[cfg(unix)]
pub fn unix_process_start(pid: u32) -> Result<String> {
    anyhow::ensure!(pid > 0, "invalid local process PID");
    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
        let size = std::mem::size_of::<libc::proc_bsdinfo>();
        // SAFETY: kernel writes the initialized fixed-size BSD process buffer.
        let read = unsafe {
            libc::proc_pidinfo(
                i32::try_from(pid)?,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                i32::try_from(size)?,
            )
        };
        anyhow::ensure!(
            usize::try_from(read).ok() == Some(size),
            "local process creation identity unavailable"
        );
        // SAFETY: exact-size successful kernel query initialized the buffer.
        let info = unsafe { info.assume_init() };
        anyhow::ensure!(
            info.pbi_pid == pid && info.pbi_uid == PrivateDirectory::current_user_id(),
            "local process principal changed"
        );
        Ok(format!(
            "macos:{}:{}",
            info.pbi_start_tvsec, info.pbi_start_tvusec
        ))
    }
    #[cfg(target_os = "linux")]
    {
        use std::io::Read as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _};
        use std::os::unix::fs::OpenOptionsExt as _;

        // Keep stat and credentials on one kernel process object. A retained
        // proc directory cannot be redirected to a reused PID after exit.
        let directory = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(format!("/proc/{pid}"))?;
        let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: the live directory descriptor and output buffer are valid.
        let result = unsafe { libc::fstatfs(directory.as_raw_fd(), filesystem.as_mut_ptr()) };
        if result != 0 {
            return Err(std::io::Error::last_os_error())
                .context("inspecting kernel process directory");
        }
        // SAFETY: successful fstatfs initialized the output buffer.
        anyhow::ensure!(
            i128::from(unsafe { filesystem.assume_init() }.f_type)
                == i128::from(libc::PROC_SUPER_MAGIC),
            "local process identity is not on procfs"
        );
        let open = |name: &std::ffi::CStr| -> Result<File> {
            // SAFETY: the retained directory and constant basename remain valid.
            let fd = unsafe {
                libc::openat(
                    directory.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if fd < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("opening kernel process identity");
            }
            // SAFETY: openat returned a newly owned descriptor.
            Ok(unsafe { File::from_raw_fd(fd) })
        };
        let mut bytes = Vec::new();
        open(c"stat")?.take(8193).read_to_end(&mut bytes)?;
        anyhow::ensure!(bytes.len() <= 8192, "oversized kernel process identity");
        let stat = std::str::from_utf8(&bytes)?;
        anyhow::ensure!(
            stat.split_once(' ')
                .is_some_and(|(actual, _)| actual.parse::<u32>().ok() == Some(pid)),
            "kernel process PID changed"
        );
        let end = stat.rfind(')').context("invalid kernel process identity")?;
        let start = stat[end + 1..]
            .split_whitespace()
            .nth(19)
            .context("kernel process start field unavailable")?
            .parse::<u64>()?;
        // PR_SET_DUMPABLE=0 makes proc inode ownership root even for a local
        // user's process. Authenticate the effective UID in the kernel status
        // header instead; do not disable hardening or trust the inode owner.
        // Pid/Uid precede potentially large supplementary-group lists.
        let mut status = String::new();
        open(c"status")?.take(8192).read_to_string(&mut status)?;
        validate_linux_process_status(&status, pid, PrivateDirectory::current_user_id())?;
        let mut boot = String::new();
        File::open("/proc/sys/kernel/random/boot_id")?
            .take(65)
            .read_to_string(&mut boot)?;
        let boot = boot.trim();
        anyhow::ensure!(
            boot.len() == 36 && boot.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'),
            "kernel boot identity unavailable"
        );
        Ok(format!("linux:{boot}:{start}"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    bail!("local process generation authentication unsupported on this platform")
}

#[cfg(target_os = "linux")]
fn validate_linux_process_status(status: &str, pid: u32, uid: u32) -> Result<()> {
    let mut actual_pid = None;
    let mut actual_uid = None;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("Pid:") {
            anyhow::ensure!(actual_pid.is_none(), "duplicate kernel process PID");
            actual_pid = Some(value.trim().parse::<u32>()?);
        } else if let Some(value) = line.strip_prefix("Uid:") {
            anyhow::ensure!(actual_uid.is_none(), "duplicate kernel process credentials");
            let values = value
                .split_whitespace()
                .map(str::parse::<u32>)
                .collect::<std::result::Result<Vec<_>, _>>()?;
            anyhow::ensure!(values.len() == 4, "invalid kernel process credentials");
            actual_uid = Some(values[1]);
        }
    }
    anyhow::ensure!(actual_pid == Some(pid), "kernel process PID changed");
    anyhow::ensure!(actual_uid == Some(uid), "local process principal changed");
    Ok(())
}

#[cfg(unix)]
impl PrivateDirectory {
    /// The same anchored owned-file opener used by credentials, with a bounded
    /// read and retained object for exact receipt retirement.
    pub fn read_private_receipt(&self, name: &str, max: usize) -> Result<Option<(Vec<u8>, File)>> {
        let Some(file) = self.open_owned_file_for_read(name)? else {
            return Ok(None);
        };
        self.read_private_receipt_file(name, max, file).map(Some)
    }

    fn read_private_receipt_file(
        &self,
        name: &str,
        max: usize,
        mut file: File,
    ) -> Result<(Vec<u8>, File)> {
        use std::io::Read as _;
        use std::os::unix::fs::MetadataExt as _;
        let before = validate_owned_file_handle(&file, &self.directory.join(name))?;
        anyhow::ensure!(
            before.mode() & 0o077 == 0 && before.len() <= u64::try_from(max)?,
            "private owner receipt permissions or size refused"
        );
        let mut bytes = Vec::new();
        (&mut file)
            .take(
                u64::try_from(max)?
                    .checked_add(1)
                    .context("receipt limit overflow")?,
            )
            .read_to_end(&mut bytes)?;
        let after = validate_owned_file_handle(&file, &self.directory.join(name))?;
        anyhow::ensure!(
            bytes.len() <= max
                && u64::try_from(bytes.len())? == before.len()
                && before.len() == after.len()
                && before.mtime() == after.mtime()
                && before.mtime_nsec() == after.mtime_nsec(),
            "private owner receipt changed while reading"
        );
        Ok((bytes, file))
    }

    pub fn retire_private_receipt(&self, name: &str, captured: &File) -> Result<bool> {
        use std::os::unix::fs::MetadataExt as _;
        let expected = validate_owned_file_handle(captured, &self.directory.join(name))?;
        let Some(named) = self.open_owned_file_for_read(name)? else {
            return Ok(false);
        };
        let actual = validate_owned_file_handle(&named, &self.directory.join(name))?;
        if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
            return Ok(false);
        }
        let retired = format!(
            ".cw-owner-{}-{}.retired",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        self.move_no_replace(name, &retired)?;
        let Some(moved) = self.open_owned_file_for_read(&retired)? else {
            bail!("private receipt retirement is uncertain")
        };
        let moved_metadata = validate_owned_file_handle(&moved, &self.directory.join(&retired))?;
        if (expected.dev(), expected.ino()) != (moved_metadata.dev(), moved_metadata.ino()) {
            let restored = self.move_no_replace(&retired, name);
            bail!("private receipt changed; retaining replacement (restore: {restored:?})");
        }
        self.remove_raw(&retired)?;
        self.directory_handle.sync_all()?;
        Ok(true)
    }
}

#[cfg(windows)]
impl PrivateDirectory {
    pub fn is_at_selected_path(&self) -> Result<bool> {
        let current = Self::inspect(&self.directory)?;
        let held = self
            ._component_handles
            .last()
            .context("private directory handle unavailable")?;
        let now = current
            ._component_handles
            .last()
            .context("private directory handle unavailable")?;
        validate_windows_handle_path(held, &self.directory, true)?;
        Ok(windows_file_identity(held)? == windows_file_identity(now)?)
    }
    pub fn read_private_receipt(&self, name: &str, max: usize) -> Result<Option<(Vec<u8>, File)>> {
        use std::io::Read as _;
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        validate_private_basename(name)?;
        let path = self.directory.join(name);
        let file = match fs::OpenOptions::new()
            .access_mode(FILE_GENERIC_READ | DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("opening private owner receipt"),
        };
        let before = validate_owned_file_handle(&file, &path)?;
        anyhow::ensure!(
            before.len() <= max as u64,
            "private owner receipt exceeds bound"
        );
        let mut bytes = Vec::new();
        (&file).take(max as u64 + 1).read_to_end(&mut bytes)?;
        let after = validate_owned_file_handle(&file, &path)?;
        anyhow::ensure!(
            bytes.len() as u64 == before.len()
                && before.len() == after.len()
                && before.last_write_time() == after.last_write_time(),
            "private owner receipt changed during bounded read"
        );
        Ok(Some((bytes, file)))
    }
    pub fn retire_private_receipt(&self, name: &str, captured: &File) -> Result<bool> {
        let Some((_, current)) = self.read_private_receipt(name, 16384)? else {
            return Ok(false);
        };
        if !Self::same_file_identity(captured, &current)? {
            return Ok(false);
        }
        anyhow::ensure!(
            self.is_at_selected_path()?,
            "private owner parent changed before retirement"
        );
        validate_owned_file_handle(captured, &self.directory.join(name))?;
        // Exact retained handle deletion cannot unlink a substituted pathname.
        mark_windows_file_handle_for_deletion(captured)?;
        Ok(true)
    }
}

#[cfg(windows)]
fn windows_file_identity(file: &File) -> Result<(u32, u32, u32)> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    anyhow::ensure!(
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } != 0,
        "private file identity unavailable"
    );
    Ok((
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ))
}

impl PrivateDirectory {
    pub fn same_file_identity(left: &File, right: &File) -> Result<bool> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let a = left.metadata()?;
            let b = right.metadata()?;
            Ok(a.dev() == b.dev() && a.ino() == b.ino())
        }
        #[cfg(windows)]
        {
            Ok(windows_file_identity(left)? == windows_file_identity(right)?)
        }
        #[cfg(not(any(unix, windows)))]
        bail!("private file identity unsupported on this platform")
    }
}

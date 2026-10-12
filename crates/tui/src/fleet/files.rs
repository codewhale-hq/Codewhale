//! Workspace-confined file operations shared by Fleet artifacts and its ledger.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Component, Path};

pub(crate) fn path_is_confined(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.components().all(|component| match component {
            Component::Normal(name) => !cfg!(windows) || !name.as_encoded_bytes().contains(&b':'),
            _ => false,
        })
}

/// Refuse `path` when any existing component below `root` is a link. The
/// walk stops at the first component that does not exist yet, so a path that
/// is about to be created through a real directory chain passes. This is a
/// lexical check made before the use; prefer [`WorkspaceFile`] where the
/// caller can open through it, and use this for paths a library call (a
/// directory listing, a removal) takes by name.
pub(crate) fn reject_linked_path(root: &Path, path: &Path) -> io::Result<()> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} must stay within {}", path.display(), root.display()),
        )
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if crate::plugins::metadata_is_link_or_reparse(&metadata) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Refusing symlinked path {}", current.display()),
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn invalid_path() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "Fleet artifact path must stay within the workspace",
    )
}

#[cfg(unix)]
#[derive(Debug)]
pub(crate) struct WorkspaceFile {
    directory: File,
    filename: std::ffi::CString,
    /// A user's own workspace file rather than a private store entry: new
    /// entries take the process umask and replacement keeps the existing
    /// permission bits instead of narrowing them to owner-only.
    shared: bool,
}

#[cfg(unix)]
impl WorkspaceFile {
    /// Private store entry: created files are 0600, created parents 0700, and
    /// every replacement is owner-only again.
    pub(crate) fn open(workspace: &Path, relative: &Path, create: bool) -> io::Result<Self> {
        Self::open_confined(workspace, relative, create, false, true)
    }

    /// A user's workspace file, edited in place: created entries follow the
    /// umask and replacement preserves the file's permission bits (an
    /// executable script stays executable). Confinement is unchanged.
    pub(crate) fn open_shared(workspace: &Path, relative: &Path, create: bool) -> io::Result<Self> {
        Self::open_confined(workspace, relative, create, true, true)
    }

    /// An already captured absolute root: reject later root/ancestor links.
    pub(crate) fn open_delivery(workspace: &Path, relative: &Path) -> io::Result<Self> {
        Self::open_confined(workspace, relative, false, false, false)
    }

    /// Open under a root that may itself be reached through user-owned links:
    /// the application's own state directory, which a user may have relocated
    /// behind a symlink. The Unix opener already resolves the root and still
    /// refuses every link below it, so this is the same call under a name that
    /// says why a caller wants it. Not for a caller-supplied workspace root.
    pub(crate) fn open_resolved_root(
        workspace: &Path,
        relative: &Path,
        create: bool,
    ) -> io::Result<Self> {
        Self::open(workspace, relative, create)
    }

    fn open_confined(
        workspace: &Path,
        relative: &Path,
        create: bool,
        shared: bool,
        resolve_root: bool,
    ) -> io::Result<Self> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::ffi::OsStrExt;
        if !path_is_confined(relative) {
            return Err(invalid_path());
        }
        let workspace = if resolve_root {
            workspace.canonicalize()?
        } else {
            if !workspace.is_absolute() {
                return Err(invalid_path());
            }
            workspace.to_path_buf()
        };
        // Use the established credential/artifact openat pattern, without
        // touching credentials or creating a second filesystem store.
        // SAFETY: static path and immediate ownership of a successful fd.
        let fd = unsafe {
            libc::open(
                c"/".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: this fd was just created and has no other owner.
        let mut directory = unsafe { File::from_raw_fd(fd) };
        let parents = relative.parent().ok_or_else(invalid_path)?;
        for (path, may_create) in [(workspace.as_path(), false), (parents, create)] {
            for component in path.components() {
                let Component::Normal(name) = component else {
                    if component == Component::RootDir {
                        continue;
                    }
                    return Err(invalid_path());
                };
                let name = std::ffi::CString::new(name.as_bytes())?;
                let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
                // SAFETY: directory pins the parent; name is one component.
                let mut fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
                if fd < 0
                    && may_create
                    && io::Error::last_os_error().kind() == io::ErrorKind::NotFound
                {
                    // SAFETY: directory and relative basename remain valid.
                    let mode = if shared { 0o777 } else { 0o700 };
                    if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), mode) } != 0
                        && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists
                    {
                        return Err(io::Error::last_os_error());
                    }
                    // SAFETY: reject a symlink inserted after mkdirat.
                    fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
                }
                if fd < 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: fd is freshly owned.
                directory = unsafe { File::from_raw_fd(fd) };
            }
        }
        Ok(Self {
            directory,
            filename: std::ffi::CString::new(
                relative.file_name().ok_or_else(invalid_path)?.as_bytes(),
            )?,
            shared,
        })
    }

    fn create_mode(&self) -> libc::c_uint {
        if self.shared { 0o666 } else { 0o600 }
    }

    pub(crate) fn sibling(&self, name: &str) -> io::Result<Self> {
        if !path_is_confined(Path::new(name)) || Path::new(name).components().count() != 1 {
            return Err(invalid_path());
        }
        Ok(Self {
            directory: self.directory.try_clone()?,
            filename: std::ffi::CString::new(name)?,
            shared: self.shared,
        })
    }

    pub(crate) fn open_update(&self, create: bool, append: bool) -> io::Result<File> {
        self.open_with_flags(
            libc::O_RDWR
                | if create { libc::O_CREAT } else { 0 }
                | if append { libc::O_APPEND } else { 0 },
        )
    }

    pub(crate) fn open_file(&self) -> io::Result<File> {
        self.open_with_flags(libc::O_RDONLY)
    }

    /// Reads a file another process may hold open for writing. Unix opens do
    /// not exclude writers, so this is [`Self::open_file`].
    pub(crate) fn open_file_shared(&self) -> io::Result<File> {
        self.open_file()
    }

    pub(crate) fn open_write(&self, append: bool) -> io::Result<File> {
        self.open_with_flags(
            libc::O_WRONLY | libc::O_CREAT | if append { libc::O_APPEND } else { 0 },
        )
    }

    fn open_with_flags(&self, flags: libc::c_int) -> io::Result<File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        use std::os::unix::fs::MetadataExt;
        // macOS can fail an `openat(O_CREAT)` with ENOENT while another thread
        // is creating the same name through the same pinned directory, even
        // though the parent exists. The loser of that race only has to ask
        // again: the file is there by then. The retry is bounded and applies
        // only when creating, so a vanished parent still fails.
        let mut attempts = 0;
        let fd = loop {
            // SAFETY: a pinned parent and validated basename; never follows links.
            let fd = unsafe {
                libc::openat(
                    self.directory.as_raw_fd(),
                    self.filename.as_ptr(),
                    flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                    self.create_mode(),
                )
            };
            if fd >= 0 {
                break fd;
            }
            let error = io::Error::last_os_error();
            attempts += 1;
            if flags & libc::O_CREAT != 0 && error.kind() == io::ErrorKind::NotFound && attempts < 4
            {
                std::thread::yield_now();
                continue;
            }
            return Err(error);
        };
        // SAFETY: fd is freshly owned.
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Fleet file must be a regular, non-hard-linked file",
            ));
        }
        Ok(file)
    }

    pub(crate) fn publish(&self, bytes: &[u8]) -> io::Result<()> {
        self.atomic_write(bytes, false)
    }

    pub(crate) fn replace(&self, bytes: &[u8]) -> io::Result<()> {
        self.atomic_write(bytes, true)
    }

    fn atomic_write(&self, bytes: &[u8], replace: bool) -> io::Result<()> {
        self.atomic_write_retained(bytes, replace, None).map(drop)
    }

    pub(crate) fn publish_retained(&self, bytes: &[u8], executable: bool) -> io::Result<File> {
        self.atomic_write_retained(bytes, false, Some(if executable { 0o755 } else { 0o644 }))
    }

    fn atomic_write_retained(
        &self,
        bytes: &[u8],
        replace: bool,
        mode: Option<u32>,
    ) -> io::Result<File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let temporary =
            std::ffi::CString::new(format!(".fleet-write-{}.tmp", uuid::Uuid::new_v4()))
                .expect("generated basename");
        // SAFETY: parent is pinned; exclusive creation cannot follow a link.
        let fd = unsafe {
            libc::openat(
                self.directory.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                self.create_mode(),
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is freshly owned.
        let mut file = unsafe { File::from_raw_fd(fd) };
        let result = (|| {
            if self.shared && replace {
                self.copy_permissions_to(&file)?;
            }
            if let Some(mode) = mode {
                use std::os::unix::fs::PermissionsExt;
                file.set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
            file.write_all(bytes)?;
            file.sync_all()?;
            if !replace && rename_exclusive(self.directory.as_raw_fd(), &temporary, &self.filename)?
            {
                return Ok(true);
            }
            // SAFETY: both basenames are anchored to the same open parent.
            // Replacement changes the directory entry, never a symlink target.
            let published = unsafe {
                if replace {
                    libc::renameat(
                        self.directory.as_raw_fd(),
                        temporary.as_ptr(),
                        self.directory.as_raw_fd(),
                        self.filename.as_ptr(),
                    )
                } else {
                    libc::linkat(
                        self.directory.as_raw_fd(),
                        temporary.as_ptr(),
                        self.directory.as_raw_fd(),
                        self.filename.as_ptr(),
                        0,
                    )
                }
            };
            if published != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(replace)
        })();
        // Successful rename already consumed this temporary entry. Never
        // unlink the vacant old name, which another writer could now reuse.
        if !matches!(result, Ok(true)) {
            // SAFETY: unlink this call's exclusive temporary basename.
            if unsafe { libc::unlinkat(self.directory.as_raw_fd(), temporary.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        result?;
        self.directory.sync_all()?;
        Ok(file)
    }

    pub(crate) fn open_retained(&self) -> io::Result<File> {
        self.open_file()
    }

    pub(crate) fn identity_probe(&self) -> io::Result<File> {
        self.open_file()
    }

    pub(crate) fn move_opened_to_sibling(
        &self,
        _file: &File,
        destination: &Self,
    ) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        if !same_file(&self.directory, &destination.directory)? {
            return Err(invalid_path());
        }
        // Retirement must be one no-clobber rename. The link/unlink fallback
        // used by immutable publication is not a conditional move of an old entry.
        if !rename_exclusive(
            self.directory.as_raw_fd(),
            &self.filename,
            &destination.filename,
        )? {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "exclusive retirement is unavailable",
            ));
        }
        Ok(())
    }

    pub(crate) fn sync_parent(&self) -> io::Result<()> {
        self.directory.sync_all()
    }
}

#[cfg(unix)]
impl WorkspaceFile {
    /// Give the replacement the permission bits of the regular file it
    /// replaces. Set-id bits are never carried over; an absent target keeps
    /// the umask-derived creation mode.
    fn copy_permissions_to(&self, replacement: &File) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        // SAFETY: zeroed plain-data struct, filled by fstatat on success.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: pinned parent, validated basename, and no link following.
        let found = unsafe {
            libc::fstatat(
                self.directory.as_raw_fd(),
                self.filename.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if found != 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            };
        }
        if (stat.st_mode & libc::S_IFMT) != libc::S_IFREG {
            return Ok(());
        }
        // SAFETY: an owned, open descriptor; fchmod never follows a path.
        if unsafe { libc::fchmod(replacement.as_raw_fd(), stat.st_mode & 0o777) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// Publish `from` as `to` without replacing an existing entry, as the Windows
/// rename below does. `linkat` then `unlinkat` leaves the published file with
/// two links for a moment, and `open_with_flags` rejects a link count other
/// than 1, so a concurrent reader failed with InvalidData. `Ok(false)` sends
/// the caller back to `linkat`, which keeps that window: other Unixes, and any
/// kernel, filesystem or sandbox that refuses the exclusive rename.
#[cfg(target_os = "linux")]
fn rename_exclusive(
    directory: libc::c_int,
    from: &std::ffi::CStr,
    to: &std::ffi::CStr,
) -> io::Result<bool> {
    // SAFETY: both basenames are anchored to the same open parent.
    let renamed = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            directory,
            from.as_ptr(),
            directory,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    exclusive_rename_outcome(renamed == 0)
}

#[cfg(target_vendor = "apple")]
fn rename_exclusive(
    directory: libc::c_int,
    from: &std::ffi::CStr,
    to: &std::ffi::CStr,
) -> io::Result<bool> {
    // SAFETY: both basenames are anchored to the same open parent.
    let renamed = unsafe {
        libc::renameatx_np(
            directory,
            from.as_ptr(),
            directory,
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    exclusive_rename_outcome(renamed == 0)
}

#[cfg(all(unix, not(any(target_os = "linux", target_vendor = "apple"))))]
fn rename_exclusive(_: libc::c_int, _: &std::ffi::CStr, _: &std::ffi::CStr) -> io::Result<bool> {
    Ok(false)
}

/// Only an existing target is final. Any other refusal falls back to `linkat`,
/// so publication is never worse off than before the exclusive rename.
#[cfg(any(target_os = "linux", target_vendor = "apple"))]
fn exclusive_rename_outcome(renamed: bool) -> io::Result<bool> {
    if renamed {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::AlreadyExists {
        return Err(error);
    }
    Ok(false)
}

/// Windows path fence shared by Fleet publication and Native sandbox ACL
/// admission. Every original absolute ancestor is opened without delete/write
/// sharing and without following reparse points before path-based calls.
#[cfg(windows)]
#[derive(Clone, Debug)]
pub(crate) struct WindowsDirectory {
    ancestors: Vec<std::sync::Arc<File>>,
    directory: std::path::PathBuf,
}

#[cfg(windows)]
impl WindowsDirectory {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        Self::open_inner(path, false, false)
    }

    fn open_inner(path: &Path, create: bool, acl_target: bool) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid_path());
        }
        let mut directory = std::path::PathBuf::new();
        let mut ancestors = Vec::new();
        let count = path.components().count();
        for (index, component) in path.components().enumerate() {
            if matches!(component, Component::ParentDir | Component::CurDir) {
                return Err(invalid_path());
            }
            directory.push(component.as_os_str());
            if matches!(component, Component::Prefix(_)) {
                continue;
            }
            let open = || Self::open_component(&directory, acl_target && index + 1 == count);
            let file = match open() {
                Err(error) if create && error.kind() == io::ErrorKind::NotFound => {
                    match std::fs::create_dir(&directory) {
                        Ok(()) => {}
                        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error),
                    }
                    open()?
                }
                result => result?,
            };
            ancestors.push(file);
        }
        Ok(Self {
            ancestors,
            directory,
        })
    }

    fn open_component(path: &Path, acl_target: bool) -> io::Result<std::sync::Arc<File>> {
        use std::os::windows::fs::OpenOptionsExt;
        let mut options = std::fs::OpenOptions::new();
        options.read(true).share_mode(1).custom_flags(0x0220_0000);
        if acl_target {
            // The Native ACL writer uses SetKernelObjectSecurity, which never
            // propagates to descendants; each child is fenced and granted
            // explicitly. So the target needs only READ_CONTROL|WRITE_DAC plus a
            // read right that records share access: read-only sharing still
            // blocks rename, delete and data writes while it is edited. Unlike
            // MAXIMUM_ALLOWED, which includes DELETE, it does not collide with a
            // live host whose current directory is this directory.
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, READ_CONTROL, WRITE_DAC,
            };
            options
                .access_mode(READ_CONTROL | WRITE_DAC | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES);
        }
        let file = options.open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_dir() || crate::plugins::metadata_is_link_or_reparse(&metadata) {
            return Err(invalid_path());
        }
        Ok(std::sync::Arc::new(file))
    }

    pub(crate) fn child_path(&self, name: &std::ffi::OsStr) -> io::Result<std::path::PathBuf> {
        let name = Path::new(name);
        if !path_is_confined(name) || name.components().count() != 1 {
            return Err(invalid_path());
        }
        Ok(self.directory.join(name))
    }

    /// Extend the actual held direct-parent chain instead of reopening pinned
    /// ancestors by path.
    pub(crate) fn open_acl_child(&self, name: &std::ffi::OsStr) -> io::Result<Self> {
        let directory = self.child_path(name)?;
        let file = Self::open_component(&directory, true)?;
        let mut ancestors = self.ancestors.clone();
        ancestors.push(file);
        Ok(Self {
            ancestors,
            directory,
        })
    }

    pub(crate) fn open_acl(path: &Path) -> io::Result<Self> {
        Self::open_inner(path, false, true)
    }

    pub(crate) fn acl_handle(&self) -> io::Result<&File> {
        self.ancestors
            .last()
            .map(std::sync::Arc::as_ref)
            .ok_or_else(invalid_path)
    }
}

#[cfg(windows)]
#[derive(Debug)]
pub(crate) struct WorkspaceFile {
    // Retaining every ancestor without delete/write sharing prevents a path
    // swap or junction replacement while path-based Windows calls are running.
    _ancestors: Vec<std::sync::Arc<File>>,
    directory: std::path::PathBuf,
    filename: std::ffi::OsString,
}

#[cfg(windows)]
impl WorkspaceFile {
    pub(crate) fn open(workspace: &Path, relative: &Path, create: bool) -> io::Result<Self> {
        if !path_is_confined(relative) {
            return Err(invalid_path());
        }
        // Preserve the original spelling until links/reparse points have been
        // refused. Canonicalization must not turn a linked root into authority.
        let workspace = if workspace.is_absolute() {
            workspace.to_path_buf()
        } else {
            std::env::current_dir()?.join(workspace)
        };
        let root = WindowsDirectory::open(&workspace)?;
        let target = workspace.join(relative.parent().ok_or_else(invalid_path)?);
        let mut pinned = WindowsDirectory::open_inner(&target, create, false)?;
        pinned.ancestors.extend(root.ancestors);
        let ancestors = pinned.ancestors;
        let directory = pinned.directory;
        Ok(Self {
            _ancestors: ancestors,
            directory,
            filename: relative.file_name().ok_or_else(invalid_path)?.to_owned(),
        })
    }

    /// Open under a root that may itself be reached through user-owned links:
    /// the application's own state directory, which a user may have relocated
    /// behind a junction. The root is resolved once to its real directory and
    /// the same link-refusing walk then runs, so links below the root are still
    /// refused. The caller must have created the root already. Not for a
    /// caller-supplied workspace root.
    pub(crate) fn open_resolved_root(
        workspace: &Path,
        relative: &Path,
        create: bool,
    ) -> io::Result<Self> {
        let resolved = workspace.canonicalize()?;
        Self::open(&resolved, relative, create)
    }

    pub(crate) fn open_delivery(workspace: &Path, relative: &Path) -> io::Result<Self> {
        Self::open(workspace, relative, false)
    }

    /// Windows has no Unix permission bits to preserve; see the Unix opener.
    pub(crate) fn open_shared(workspace: &Path, relative: &Path, create: bool) -> io::Result<Self> {
        Self::open(workspace, relative, create)
    }

    pub(crate) fn sibling(&self, name: &str) -> io::Result<Self> {
        if !path_is_confined(Path::new(name)) || Path::new(name).components().count() != 1 {
            return Err(invalid_path());
        }
        Ok(Self {
            _ancestors: self._ancestors.clone(),
            directory: self.directory.clone(),
            filename: name.into(),
        })
    }

    pub(crate) fn open_update(&self, create: bool, append: bool) -> io::Result<File> {
        self.open_with_access(create, append, true, true)
    }

    pub(crate) fn open_write(&self, append: bool) -> io::Result<File> {
        self.open_with_access(true, append, false, true)
    }

    /// Reads a file another process may hold open for writing (a running
    /// worker's log). [`Self::open_file`] denies concurrent writers, so it
    /// fails with a sharing violation while the writer is alive; this opens
    /// read-only with full sharing and applies the same regular, unlinked
    /// checks to the handle.
    pub(crate) fn open_file_shared(&self) -> io::Result<File> {
        self.open_with_access(false, false, true, false)
    }

    fn open_with_access(
        &self,
        create: bool,
        append: bool,
        read: bool,
        write: bool,
    ) -> io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(read)
            .write(write)
            .append(append)
            .create(create)
            .truncate(false)
            .share_mode(0x0000_0007)
            .custom_flags(0x0020_0000)
            .open(self.directory.join(&self.filename))?;
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || crate::plugins::metadata_is_link_or_reparse(&metadata)
            || crate::plugins::windows_file_identity(&file)?.links != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Fleet file must be regular and not linked",
            ));
        }
        Ok(file)
    }

    pub(crate) fn open_file(&self) -> io::Result<File> {
        // Existing protected reader rejects reparse points, hard links and
        // non-regular files, and denies concurrent writes/replacement.
        crate::plugins::manifest::open_bundle_file(&self.directory.join(&self.filename))
    }

    pub(crate) fn publish(&self, bytes: &[u8]) -> io::Result<()> {
        self.atomic_write(bytes, false)
    }

    pub(crate) fn replace(&self, bytes: &[u8]) -> io::Result<()> {
        self.atomic_write(bytes, true)
    }

    fn atomic_write(&self, bytes: &[u8], replace: bool) -> io::Result<()> {
        self.atomic_write_retained(bytes, replace).map(drop)
    }

    pub(crate) fn publish_retained(&self, bytes: &[u8], _executable: bool) -> io::Result<File> {
        self.atomic_write_retained(bytes, false)
    }

    pub(crate) fn open_retained(&self) -> io::Result<File> {
        crate::plugins::manifest::open_bundle_file_for_retirement(
            &self.directory.join(&self.filename),
        )
    }

    pub(crate) fn identity_probe(&self) -> io::Result<File> {
        crate::plugins::manifest::open_bundle_identity_probe(
            &self.directory.join(&self.filename),
            false,
        )
    }

    pub(crate) fn move_opened_to_sibling(&self, file: &File, destination: &Self) -> io::Result<()> {
        if self.directory != destination.directory {
            return Err(invalid_path());
        }
        rename_windows_opened(file, &destination.filename, false)
    }

    pub(crate) fn sync_parent(&self) -> io::Result<()> {
        Ok(())
    }

    fn atomic_write_retained(&self, bytes: &[u8], replace: bool) -> io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
        };

        let mut temporary =
            tempfile::Builder::new()
                .prefix(".fleet-write-")
                .make_in(&self.directory, |path| {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE)
                        .share_mode(FILE_SHARE_READ)
                        .open(path)
                })?;
        let result = (|| {
            temporary.write_all(bytes)?;
            temporary.as_file().sync_all()?;

            rename_windows_opened(temporary.as_file(), &self.filename, replace)
        })();
        match result {
            Ok(()) => {
                // The old name is vacant after the rename; do not unlink an
                // entry another process might create there afterwards.
                temporary.disable_cleanup(true);
                Ok(temporary.into_file())
            }
            Err(error) => {
                // Close the source's delete-denying handle before cleanup.
                temporary.into_temp_path().close()?;
                Err(error)
            }
        }
    }
}

#[cfg(windows)]
pub(crate) fn rename_windows_opened(
    file: &File,
    filename: &std::ffi::OsStr,
    replace: bool,
) -> io::Result<()> {
    use std::mem::{offset_of, size_of};
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Wdk::Storage::FileSystem::{
        FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
    };
    use windows_sys::Win32::Foundation::RtlNtStatusToDosError;
    use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
    // MoveFileExW (including tempfile::persist) reopens the destination
    // directory with FILE_ADD_FILE, conflicting with our ancestor pins.
    // A native rename with no root handle and a single basename uses
    // the source file's existing parent instead. Keep all ancestor and
    // source handles pinned; never relax their write/delete guards.
    // https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information
    let name = filename.encode_wide().collect::<Vec<_>>();
    let name_bytes = name.len() * size_of::<u16>();
    let buffer_size = (offset_of!(FILE_RENAME_INFORMATION, FileName) + name_bytes)
        .max(size_of::<FILE_RENAME_INFORMATION>());
    let mut buffer = vec![0_usize; buffer_size.div_ceil(size_of::<usize>())];
    let rename = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: the zeroed buffer is aligned and covers the struct plus
    // the complete UTF-16 basename; no root means the source's parent.
    unsafe {
        (*rename).Anonymous.ReplaceIfExists = replace;
        (*rename).FileNameLength = name_bytes as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*rename).FileName.as_mut_ptr(), name.len());
    }
    let mut attempt = 0;
    loop {
        let mut status = IO_STATUS_BLOCK::default();
        // SAFETY: this synchronously opened file has DELETE access;
        // every handle and buffer remains live throughout the call.
        let result = unsafe {
            NtSetInformationFile(
                file.as_raw_handle(),
                &mut status,
                rename.cast(),
                buffer_size as u32,
                FileRenameInformation,
            )
        };
        if result >= 0 {
            return Ok(());
        }
        // SAFETY: converts the returned NTSTATUS without dereferencing.
        let error = io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(result) as i32 });
        let Some(backoff) = crate::utils::windows_publish_retry_delay(&error, attempt) else {
            return Err(error);
        };
        std::thread::sleep(backoff);
        attempt += 1;
    }
}

#[cfg(all(test, unix))]
mod unix_publication_tests {
    use super::*;
    use std::io::Read;

    /// Several threads creating one new name through their own pinned handles
    /// must all succeed. macOS can fail the losers of that race with ENOENT
    /// unless the open asks again.
    #[test]
    fn racing_creates_of_one_name_all_succeed() {
        let workspace = tempfile::tempdir().unwrap();
        for round in 0..40 {
            let relative = std::path::PathBuf::from(format!("locks-{round}/manager.lock"));
            std::thread::scope(|scope| {
                let workers: Vec<_> = (0..4)
                    .map(|_| {
                        scope.spawn(|| {
                            WorkspaceFile::open(workspace.path(), &relative, true)
                                .and_then(|file| file.open_update(true, false))
                        })
                    })
                    .collect();
                for worker in workers {
                    worker
                        .join()
                        .unwrap()
                        .expect("every racing create succeeds");
                }
            });
        }
    }

    #[test]
    fn a_racing_reader_never_sees_a_publication_half_done() {
        // Identical replays publish the same handle concurrently and the loser
        // reads the winner's file back. A reader that lands between linkat and
        // the temporary's unlink sees two links, which open_file rejects.
        let workspace = tempfile::tempdir().unwrap();
        for round in 0..50 {
            let relative = std::path::PathBuf::from(format!("receipt-{round}.json"));
            let writer = WorkspaceFile::open(workspace.path(), &relative, true).unwrap();
            let reader = WorkspaceFile::open(workspace.path(), &relative, false).unwrap();
            std::thread::scope(|scope| {
                let read = scope.spawn(|| {
                    loop {
                        match reader.open_file() {
                            Ok(mut file) => {
                                let mut bytes = Vec::new();
                                file.read_to_end(&mut bytes).unwrap();
                                break bytes;
                            }
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                                std::hint::spin_loop();
                            }
                            Err(error) => panic!("round {round}: {error}"),
                        }
                    }
                });
                writer.publish(b"receipt").unwrap();
                assert_eq!(read.join().unwrap(), b"receipt");
            });
        }
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 50);
    }

    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777
    }

    #[test]
    fn shared_replacement_keeps_the_existing_permission_bits() {
        use std::os::unix::fs::PermissionsExt;
        let workspace = tempfile::tempdir().unwrap();
        let script = workspace.path().join("deploy.sh");
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        WorkspaceFile::open_shared(workspace.path(), Path::new("deploy.sh"), false)
            .unwrap()
            .replace(b"#!/bin/sh\necho edited\n")
            .unwrap();

        assert_eq!(std::fs::read(&script).unwrap(), b"#!/bin/sh\necho edited\n");
        assert_eq!(
            mode(&script),
            0o755,
            "an edit must not drop the executable bit"
        );
    }

    #[test]
    fn shared_replacement_drops_set_id_bits_and_keeps_owner_only_files_private() {
        use std::os::unix::fs::PermissionsExt;
        let workspace = tempfile::tempdir().unwrap();
        // Set-group-id on a file of one's own group is allowed unprivileged;
        // the sticky bit on a regular file is not (EFTYPE on macOS).
        for (name, before, after) in [("tool", 0o6750, 0o750), ("key.pem", 0o600, 0o600)] {
            let path = workspace.path().join(name);
            std::fs::write(&path, b"old").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(before)).unwrap();
            WorkspaceFile::open_shared(workspace.path(), Path::new(name), false)
                .unwrap()
                .replace(b"new")
                .unwrap();
            assert_eq!(std::fs::read(&path).unwrap(), b"new");
            assert_eq!(mode(&path), after, "{name}");
        }
    }

    #[test]
    fn shared_creation_follows_the_umask_for_the_file_and_its_parents() {
        let workspace = tempfile::tempdir().unwrap();
        // std creates files as 0o666 and directories as 0o777 under the
        // process umask; read that back instead of changing the umask.
        let probe_file = workspace.path().join("probe-file");
        std::fs::write(&probe_file, b"").unwrap();
        let probe_dir = workspace.path().join("probe-dir");
        std::fs::create_dir(&probe_dir).unwrap();

        WorkspaceFile::open_shared(workspace.path(), Path::new("new/nested/notes.md"), true)
            .unwrap()
            .replace(b"notes")
            .unwrap();

        let created = workspace.path().join("new/nested/notes.md");
        assert_eq!(std::fs::read(&created).unwrap(), b"notes");
        assert_eq!(mode(&created), mode(&probe_file));
        assert_eq!(mode(&workspace.path().join("new")), mode(&probe_dir));
        assert_eq!(mode(&workspace.path().join("new/nested")), mode(&probe_dir));
    }

    #[test]
    fn private_replacement_and_creation_stay_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let workspace = tempfile::tempdir().unwrap();
        let ledger = workspace.path().join("fleet.jsonl");
        std::fs::write(&ledger, b"old").unwrap();
        std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o644)).unwrap();

        WorkspaceFile::open(workspace.path(), Path::new("fleet.jsonl"), false)
            .unwrap()
            .replace(b"compacted")
            .unwrap();
        assert_eq!(mode(&ledger), 0o600);

        WorkspaceFile::open(workspace.path(), Path::new("private/receipt.json"), true)
            .unwrap()
            .publish(b"receipt")
            .unwrap();
        assert_eq!(mode(&workspace.path().join("private")), 0o700);
        assert_eq!(mode(&workspace.path().join("private/receipt.json")), 0o600);
    }
}

#[cfg(all(test, windows))]
mod windows_publication_tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn publication_and_replacement_keep_ancestor_write_and_delete_guards() {
        let workspace = tempfile::tempdir().unwrap();
        let parent = workspace.path().join("private");
        let ledger =
            WorkspaceFile::open(workspace.path(), Path::new("private/fleet.jsonl"), true).unwrap();
        let forbidden = std::fs::OpenOptions::new()
            .write(true)
            .custom_flags(0x0220_0000)
            .open(&parent)
            .unwrap_err();
        assert_eq!(forbidden.raw_os_error(), Some(32));
        assert!(std::fs::rename(&parent, workspace.path().join("swapped")).is_err());

        // Both fail with MoveFileExW while the destination parent is pinned.
        ledger.publish(b"first").unwrap();
        ledger.replace(b"compacted").unwrap();
        assert_eq!(
            std::fs::read(parent.join("fleet.jsonl")).unwrap(),
            b"compacted"
        );
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);
    }

    #[test]
    fn replacement_survives_a_short_lived_reader_without_delete_sharing() {
        let workspace = tempfile::tempdir().unwrap();
        let relative = Path::new("fleet.jsonl");
        let ledger = WorkspaceFile::open(workspace.path(), relative, true).unwrap();
        ledger.publish(b"first").unwrap();
        let held = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x1 | 0x2)
            .open(workspace.path().join(relative))
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(held);
        });
        ledger.replace(b"compacted").unwrap();
        release.join().unwrap();
        assert_eq!(
            std::fs::read(workspace.path().join(relative)).unwrap(),
            b"compacted"
        );
    }

    #[test]
    fn immutable_publication_preserves_existing_bytes_and_removes_its_temporary() {
        let workspace = tempfile::tempdir().unwrap();
        let artifact =
            WorkspaceFile::open(workspace.path(), Path::new("receipt.json"), true).unwrap();
        artifact.publish(b"receipt").unwrap();
        assert_eq!(
            artifact.publish(b"replacement").unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read(workspace.path().join("receipt.json")).unwrap(),
            b"receipt"
        );
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 1);
    }

    #[test]
    fn artifact_paths_cannot_name_windows_alternate_data_streams() {
        for path in ["receipt.json:private", "dir/receipt:private", ":stream"] {
            assert!(
                !path_is_confined(Path::new(path)),
                "accepted stream path {path}"
            );
        }
    }
}

#[cfg(all(test, windows))]
mod windows_root_link_tests {
    use super::*;

    fn make_junction(link: &Path, target: &Path) {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("invoke Windows junction creation");
        assert!(
            output.status.success(),
            "failed to create junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A state root relocated behind a junction: the strict opener refuses the
    /// root link, `open_resolved_root` resolves it once and writes through,
    /// and a link below the root stays refused either way.
    #[test]
    fn linked_state_root_resolves_once_and_keeps_the_inner_rule() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = temp.path().join("link");
        make_junction(&link, &real);

        let relative = Path::new("state/entry.txt");
        assert!(
            WorkspaceFile::open(&link, relative, true).is_err(),
            "the strict opener must refuse a linked root"
        );

        WorkspaceFile::open_resolved_root(&link, relative, true)
            .unwrap()
            .publish(b"payload")
            .unwrap();
        assert_eq!(
            std::fs::read(real.join("state").join("entry.txt")).unwrap(),
            b"payload"
        );

        let elsewhere = temp.path().join("elsewhere");
        std::fs::create_dir(&elsewhere).unwrap();
        make_junction(&real.join("linked-child"), &elsewhere);
        assert!(
            WorkspaceFile::open_resolved_root(&link, Path::new("linked-child/x.txt"), true)
                .is_err(),
            "a link below the resolved root must stay refused"
        );
    }
}

#[cfg(all(not(unix), not(windows)))]
#[derive(Debug)]
pub(crate) struct WorkspaceFile;
#[cfg(all(not(unix), not(windows)))]
impl WorkspaceFile {
    pub(crate) fn open(_: &Path, _: &Path, _: bool) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Confined Fleet artifact I/O is unavailable on this platform",
        ))
    }
    pub(crate) fn open_shared(workspace: &Path, relative: &Path, create: bool) -> io::Result<Self> {
        Self::open(workspace, relative, create)
    }
    pub(crate) fn open_resolved_root(
        workspace: &Path,
        relative: &Path,
        create: bool,
    ) -> io::Result<Self> {
        Self::open(workspace, relative, create)
    }
    pub(crate) fn sibling(&self, _: &str) -> io::Result<Self> {
        unreachable!()
    }
    pub(crate) fn open_update(&self, _: bool, _: bool) -> io::Result<File> {
        unreachable!()
    }
    pub(crate) fn open_write(&self, _: bool) -> io::Result<File> {
        unreachable!()
    }
    pub(crate) fn replace(&self, _: &[u8]) -> io::Result<()> {
        unreachable!()
    }
    pub(crate) fn open_file(&self) -> io::Result<File> {
        unreachable!()
    }
    pub(crate) fn open_file_shared(&self) -> io::Result<File> {
        unreachable!()
    }
    pub(crate) fn publish(&self, _: &[u8]) -> io::Result<()> {
        unreachable!()
    }
    pub(crate) fn open_delivery(workspace: &Path, relative: &Path) -> io::Result<Self> {
        Self::open(workspace, relative, false)
    }
    pub(crate) fn open_retained(&self) -> io::Result<File> {
        self.open_file()
    }
    pub(crate) fn identity_probe(&self) -> io::Result<File> {
        self.open_file()
    }
    pub(crate) fn publish_retained(&self, _: &[u8], _: bool) -> io::Result<File> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(crate) fn move_opened_to_sibling(&self, _: &File, _: &Self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    pub(crate) fn sync_parent(&self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Compare already opened lock handles; a replaced lock must never create two
/// independent critical sections for the same live ledger.
pub(crate) fn same_file(left: &File, right: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let a = left.metadata()?;
        let b = right.metadata()?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(windows)]
    {
        let a = crate::plugins::windows_file_identity(left)?;
        let b = crate::plugins::windows_file_identity(right)?;
        Ok(a.volume == b.volume && a.index == b.index)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = (left, right);
        unreachable!()
    }
}

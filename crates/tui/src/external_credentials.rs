//! Capability-gated I/O for credentials owned by another CLI.
//!
//! Every external open/read stays behind an opaque grant. Consumption opens
//! one absolute regular file through a no-follow traversal, validates that
//! same handle, and reads a bounded payload from it. This prevents a consented
//! path from being redirected through a leaf or parent symlink/reparse point
//! and avoids the old exists-then-read race.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use codewhale_config::ExternalCredentialReadGrant;

/// Credential JSON is expected to be tiny. Bound reads so a replaced regular
/// file cannot turn read-only consent into unbounded memory consumption.
const MAX_EXTERNAL_CREDENTIAL_BYTES: u64 = 1024 * 1024;

#[cfg(all(test, unix))]
thread_local! {
    static BEFORE_LEAF_OPEN_HOOK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
thread_local! {
    /// Per-test-thread real sink counters. Keeping the trap thread-local makes
    /// parallel tests unable to contaminate one another while still counting
    /// the exact production functions reached by the code under test.
    static SIDE_EFFECT_TRAP: std::cell::Cell<[usize; 5]> = const {
        std::cell::Cell::new([0; 5])
    };
}

#[cfg(test)]
fn increment_side_effect(index: usize) {
    SIDE_EFFECT_TRAP.with(|trap| {
        let mut counts = trap.get();
        counts[index] += 1;
        trap.set(counts);
    });
}

/// Open and read the exact granted file once. Missing files are reported as
/// `Ok(None)`; every other unsafe or malformed filesystem shape fails closed.
pub(crate) fn read_to_string(grant: &ExternalCredentialReadGrant) -> Result<Option<String>> {
    #[cfg(test)]
    increment_side_effect(0);

    let mut file = match open_secure_regular_file(grant.path(), false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "securely opening external {} credential file {}",
                    grant.source().as_str(),
                    codewhale_config::quote_os_path(grant.path())
                )
            });
        }
    };

    #[cfg(test)]
    increment_side_effect(1);

    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_EXTERNAL_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| {
            format!(
                "reading external {} credential file {}",
                grant.source().as_str(),
                codewhale_config::quote_os_path(grant.path())
            )
        })?;
    if bytes.len() as u64 > MAX_EXTERNAL_CREDENTIAL_BYTES {
        bail!(
            "external {} credential file {} exceeds the {} byte safety limit",
            grant.source().as_str(),
            codewhale_config::quote_os_path(grant.path()),
            MAX_EXTERNAL_CREDENTIAL_BYTES
        );
    }
    let contents = String::from_utf8(bytes).with_context(|| {
        format!(
            "external {} credential file {} is not valid UTF-8",
            grant.source().as_str(),
            codewhale_config::quote_os_path(grant.path())
        )
    })?;
    Ok(Some(contents))
}

/// Read one Codewhale-owned credential file through the same no-follow,
/// bounded I/O boundary used for external grants. On Unix the opened handle
/// must belong to the effective user and have no group/other permission bits.
/// The caller is responsible for constraining `path` to a validated basename
/// below Codewhale's credentials directory before invoking this function.
pub(crate) fn read_codewhale_owned_to_string(path: &Path) -> Result<Option<String>> {
    let mut file = match open_secure_regular_file(path, true) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "securely opening Codewhale-owned credential file {}",
                    codewhale_config::quote_os_path(path)
                )
            });
        }
    };
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_EXTERNAL_CREDENTIAL_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| {
            format!(
                "reading Codewhale-owned credential file {}",
                codewhale_config::quote_os_path(path)
            )
        })?;
    if bytes.len() as u64 > MAX_EXTERNAL_CREDENTIAL_BYTES {
        bail!(
            "Codewhale-owned credential file {} exceeds the {} byte safety limit",
            codewhale_config::quote_os_path(path),
            MAX_EXTERNAL_CREDENTIAL_BYTES
        );
    }
    String::from_utf8(bytes).map(Some).with_context(|| {
        format!(
            "Codewhale-owned credential file {} is not valid UTF-8",
            codewhale_config::quote_os_path(path)
        )
    })
}

#[cfg(unix)]
pub(crate) fn open_secure_regular_file(path: &Path, require_owner_only: bool) -> io::Result<File> {
    use std::ffi::CString;
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;

    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "external credential path must be absolute",
        ));
    }

    let root = CString::new("/").expect("static root contains no NUL");
    // SAFETY: `root` is a valid C string and flags require no variadic mode.
    let root_fd = unsafe {
        libc::open(
            root.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `root_fd` is newly owned after the successful `open`.
    let mut current = unsafe { File::from_raw_fd(root_fd) };
    let mut normals = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(Ok(part)),
            Component::RootDir => None,
            Component::Prefix(_) | Component::CurDir | Component::ParentDir => {
                Some(Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "external credential path must be lexically normalized",
                )))
            }
        })
        .peekable();

    let mut opened_leaf = false;
    while let Some(component) = normals.next() {
        let component = component?;
        let component = CString::new(component.as_bytes()).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "external credential path contains a NUL byte",
            )
        })?;
        let leaf = normals.peek().is_none();
        #[cfg(test)]
        if leaf {
            BEFORE_LEAF_OPEN_HOOK.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
        }
        let flags = if leaf {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK
        } else {
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_DIRECTORY
        };
        use std::os::fd::AsRawFd;
        // SAFETY: the directory fd and component C string are valid for this
        // call and flags require no variadic mode.
        let fd = unsafe { libc::openat(current.as_raw_fd(), component.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `fd` is newly owned after the successful `openat`.
        current = unsafe { File::from_raw_fd(fd) };
        opened_leaf = leaf;
    }

    if !opened_leaf {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "external credential path must name a file",
        ));
    }
    let metadata = current.metadata()?;
    if !metadata.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "external credential path must name a regular file",
        ));
    }
    if require_owner_only {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: geteuid(2) dereferences no pointers.
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Codewhale-owned credential file must be singly linked, owned by this user, and mode 0600 or stricter",
            ));
        }
    }
    Ok(current)
}

#[cfg(windows)]
pub(crate) fn open_secure_regular_file(path: &Path, require_owner_only: bool) -> io::Result<File> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use std::path::Component;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_NAME_OPENED,
        GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
    };

    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "external credential path must be absolute and lexically normalized",
        ));
    }

    // Reject every reparse-point component before the final open. The final
    // handle is opened as the reparse point itself, checked again, and its
    // kernel-resolved path is compared below. A second component pass catches
    // replacement during the open window.
    reject_windows_reparse_components(path)?;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !metadata.file_type().is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "external credential path must name a non-reparse regular file",
        ));
    }
    reject_windows_reparse_components(path)?;

    let handle = file.as_raw_handle();
    // Compare the spelling Windows actually opened rather than asking it to
    // expand the path into its normalized long form. A valid caller path can
    // contain an 8.3 component such as `RUNNER~1`; normalizing only the handle
    // side would make that exact path look redirected. FILE_NAME_OPENED keeps
    // the comparison handle-relative while the pre/post component checks above
    // continue to reject reparse points and swaps.
    let flags = FILE_NAME_OPENED | VOLUME_NAME_DOS;
    // SAFETY: the handle remains owned by `file`; null output asks Windows for
    // the required UTF-16 buffer length.
    let needed = unsafe { GetFinalPathNameByHandleW(handle, std::ptr::null_mut(), 0, flags) };
    if needed == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u16; needed as usize + 1];
    // SAFETY: `buffer` is writable for its declared length and `handle` is
    // valid for the duration of the call.
    let written = unsafe {
        GetFinalPathNameByHandleW(handle, buffer.as_mut_ptr(), buffer.len() as u32, flags)
    };
    if written == 0 || written as usize >= buffer.len() {
        return Err(io::Error::last_os_error());
    }
    let final_path = OsString::from_wide(&buffer[..written as usize]);
    let actual = normalize_windows_path_for_comparison(Path::new(&final_path))?;
    let expected = normalize_windows_path_for_comparison(path)?;
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "external credential path was redirected while opening",
        ));
    }
    if require_owner_only {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut information = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the opened credential handle and output pointer remain valid
        // for the duration of the call.
        if unsafe { GetFileInformationByHandle(handle, &mut information) } == 0 {
            return Err(io::Error::last_os_error());
        }
        if information.nNumberOfLinks != 1 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Codewhale-owned credential file must be singly linked",
            ));
        }
        verify_windows_owner_only_handle(handle)?;
    }
    Ok(file)
}

/// Normalize a Windows path without replacement characters. Unpaired UTF-16
/// is rejected so two distinct paths can never compare equal after a lossy
/// conversion. This is intentionally stricter than filesystem display.
#[cfg(windows)]
fn normalize_windows_path_for_comparison(path: &Path) -> io::Result<String> {
    let text = path.to_str().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            "credential path contains invalid Unicode and cannot be compared safely",
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

/// Apply a protected DACL granting only the current Windows user full access.
/// Directories propagate that owner-only policy to newly staged generations.
#[cfg(all(windows, test))]
pub(crate) fn secure_codewhale_owned_windows_path(
    path: &Path,
    inherit_to_children: bool,
) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Security::Authorization::{
        EXPLICIT_ACCESS_W, SE_FILE_OBJECT, SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW,
        TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, NO_INHERITANCE, OWNER_SECURITY_INFORMATION,
        PROTECTED_DACL_SECURITY_INFORMATION, SUB_CONTAINERS_AND_OBJECTS_INHERIT,
    };
    use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

    let user = CurrentWindowsUser::open()?;
    let entry = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_ALL_ACCESS,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if inherit_to_children {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: 0,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: user.sid().cast::<u16>(),
        },
    };
    let mut acl = std::ptr::null_mut();
    // SAFETY: `entry` and the returned ACL stay live through the security-info
    // update; the ACL is released with LocalFree below.
    let result = unsafe { SetEntriesInAclW(1, &entry, std::ptr::null(), &mut acl) };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let _acl = WindowsLocalAllocation(acl.cast());
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
    // SAFETY: `wide` is NUL terminated and `acl` remains allocated for the
    // duration of this call. Owner and DACL are applied together to match the
    // production current-user-only verifier.
    let result = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            user.sid(),
            std::ptr::null_mut(),
            acl,
            std::ptr::null(),
        )
    };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    Ok(())
}

#[cfg(windows)]
fn verify_windows_owner_only_handle(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> io::Result<()> {
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
    // SAFETY: the opened file handle remains valid and all output pointers are
    // writable. Windows allocates `descriptor`, released below.
    let result = unsafe {
        GetSecurityInfo(
            handle,
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
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let _descriptor = WindowsLocalAllocation(descriptor.cast());
    // SAFETY: `owner` is non-null; `user.sid()` is owned by `user`.
    if owner.is_null() || unsafe { EqualSid(owner, user.sid()) } == 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Codewhale-owned credential file owner is not the current user",
        ));
    }
    if dacl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Codewhale-owned credential file must have an owner-only DACL",
        ));
    }
    let mut count = 0;
    let mut entries: *mut EXPLICIT_ACCESS_W = std::ptr::null_mut();
    // SAFETY: `dacl` is owned by the live security descriptor; Windows
    // allocates the returned entries, released below.
    let result = unsafe { GetExplicitEntriesFromAclW(dacl, &mut count, &mut entries) };
    if result != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(result as i32));
    }
    let _entries = WindowsLocalAllocation(entries.cast());
    if count != 1 || entries.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Codewhale-owned credential file DACL must grant only one user",
        ));
    }
    // SAFETY: `count == 1` proves the first returned entry is initialized.
    let entry = unsafe { &*entries };
    let trustee_sid: PSID = entry.Trustee.ptstrName.cast();
    // SAFETY: form and null checked in this expression; sid owned by `user`.
    let current_user_only = entry.Trustee.TrusteeForm == TRUSTEE_IS_SID
        && !trustee_sid.is_null()
        && unsafe { EqualSid(trustee_sid, user.sid()) } != 0
        && matches!(entry.grfAccessMode, SET_ACCESS | GRANT_ACCESS)
        && entry.grfAccessPermissions == FILE_ALL_ACCESS;
    if !current_user_only {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Codewhale-owned credential file DACL is not current-user-only",
        ));
    }
    Ok(())
}

#[cfg(windows)]
struct CurrentWindowsUser {
    token: windows_sys::Win32::Foundation::HANDLE,
    token_info: Vec<usize>,
}

#[cfg(windows)]
impl CurrentWindowsUser {
    fn open() -> io::Result<Self> {
        use windows_sys::Win32::Foundation::{GetLastError, HANDLE};
        use windows_sys::Win32::Security::{
            GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser,
        };
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        let mut token: HANDLE = std::ptr::null_mut();
        // SAFETY: the pseudo-process handle is valid and `token` is writable.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut needed = 0;
        // SAFETY: the null buffer/zero length call obtains the required size.
        let _ =
            unsafe { GetTokenInformation(token, TokenUser, std::ptr::null_mut(), 0, &mut needed) };
        if needed == 0 {
            // SAFETY: reads thread-local error state only.
            let error = io::Error::from_raw_os_error(unsafe { GetLastError() } as i32);
            // SAFETY: `token` is owned here and not stored on this path.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(token) };
            return Err(error);
        }
        let words = (needed as usize).div_ceil(std::mem::size_of::<usize>());
        let mut token_info = vec![0usize; words];
        // SAFETY: the word buffer is aligned and contains at least `needed`
        // writable bytes; `token` remains open.
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                token_info.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            // SAFETY: `token` is owned here and not stored on this path.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(token) };
            return Err(error);
        }
        // SAFETY: initialized by GetTokenInformation; buffer outlives use.
        let user = unsafe { &*token_info.as_ptr().cast::<TOKEN_USER>() };
        if user.User.Sid.is_null() {
            // SAFETY: `token` is owned here and not stored on this path.
            unsafe { windows_sys::Win32::Foundation::CloseHandle(token) };
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "current Windows user token has no SID",
            ));
        }
        Ok(Self { token, token_info })
    }

    fn sid(&self) -> windows_sys::Win32::Security::PSID {
        use windows_sys::Win32::Security::TOKEN_USER;
        // SAFETY: `token_info` is aligned, initialized by GetTokenInformation,
        // and remains owned by `self` while the returned SID is used.
        unsafe { (*self.token_info.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

#[cfg(windows)]
impl Drop for CurrentWindowsUser {
    fn drop(&mut self) {
        // SAFETY: `token` is owned by this guard and closed exactly once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.token) };
    }
}

#[cfg(windows)]
struct WindowsLocalAllocation(*mut core::ffi::c_void);

#[cfg(windows)]
impl Drop for WindowsLocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Windows returned this allocation to a caller documented
            // to release it with LocalFree; the guard frees it exactly once.
            unsafe { windows_sys::Win32::Foundation::LocalFree(self.0) };
        }
    }
}

#[cfg(windows)]
fn reject_windows_reparse_components(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    let mut current = std::path::PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(
            component,
            std::path::Component::Prefix(_) | std::path::Component::RootDir
        ) {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&current)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "external credential path contains reparse point {}",
                    codewhale_config::quote_os_path(&current)
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn open_secure_regular_file(
    _path: &Path,
    _require_owner_only: bool,
) -> io::Result<File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "secure external credential reads are unsupported on this platform",
    ))
}

#[cfg(test)]
pub(crate) fn reset_side_effect_trap() {
    SIDE_EFFECT_TRAP.with(|trap| trap.set([0; 5]));
}

#[cfg(test)]
#[must_use]
pub(crate) fn side_effect_trap_counts() -> (usize, usize) {
    SIDE_EFFECT_TRAP.with(|trap| {
        let counts = trap.get();
        (counts[0], counts[1])
    })
}

#[cfg(test)]
#[must_use]
pub(crate) fn complete_side_effect_trap_counts() -> (usize, usize, usize, usize, usize) {
    SIDE_EFFECT_TRAP.with(|trap| {
        let counts = trap.get();
        (counts[0], counts[1], counts[2], counts[3], counts[4])
    })
}

#[cfg(test)]
pub(crate) fn record_owned_credential_write() {
    increment_side_effect(2);
}

#[cfg(test)]
pub(crate) fn record_oauth_refresh() {
    increment_side_effect(3);
}

#[cfg(test)]
pub(crate) fn record_oauth_network() {
    increment_side_effect(4);
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::{ExternalCredentialConsentToml, ExternalCredentialSource, ProviderKind};

    fn grant(path: &Path) -> ExternalCredentialReadGrant {
        ExternalCredentialConsentToml::read_only(
            ProviderKind::OpenaiCodex,
            ExternalCredentialSource::CodexCli,
            path.to_path_buf(),
        )
        .read_grant(
            ProviderKind::OpenaiCodex,
            ExternalCredentialSource::CodexCli,
            path,
        )
        .expect("test grant")
    }

    #[test]
    fn secure_read_accepts_one_bounded_regular_file() {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("auth.json");
        std::fs::write(&path, "{\"token\":\"ok\"}").expect("fixture");
        assert_eq!(
            read_to_string(&grant(&path))
                .expect("secure read")
                .as_deref(),
            Some("{\"token\":\"ok\"}")
        );
    }

    #[cfg(unix)]
    #[test]
    fn secure_read_rejects_leaf_and_parent_symlinks_and_non_regular_files() {
        let _env = crate::test_support::lock_test_env();
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical temp root");
        let real_dir = root.join("real");
        std::fs::create_dir(&real_dir).expect("real dir");
        let real = real_dir.join("auth.json");
        std::fs::write(&real, "secret").expect("fixture");

        let leaf = root.join("leaf.json");
        symlink(&real, &leaf).expect("leaf symlink");
        assert!(read_to_string(&grant(&leaf)).is_err());

        let parent = root.join("linked-parent");
        symlink(&real_dir, &parent).expect("parent symlink");
        assert!(read_to_string(&grant(&parent.join("auth.json"))).is_err());

        assert!(read_to_string(&grant(&real_dir)).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn secure_read_rejects_a_leaf_swapped_after_grant_before_open() {
        let _env = crate::test_support::lock_test_env();
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical temp root");
        let path = root.join("auth.json");
        let moved = root.join("auth-before-swap.json");
        let attacker = root.join("attacker.json");
        std::fs::write(&path, "owner-a").expect("owner fixture");
        std::fs::write(&attacker, "attacker").expect("attacker fixture");
        let grant = grant(&path);
        let hook_path = path.clone();
        BEFORE_LEAF_OPEN_HOOK.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move || {
                std::fs::rename(&hook_path, &moved).expect("move original");
                symlink(&attacker, &hook_path).expect("swap leaf to symlink");
            }));
        });
        assert!(
            read_to_string(&grant).is_err(),
            "a swap to a symlink must fail before any bytes are read"
        );
    }

    #[test]
    fn secure_read_rejects_oversized_regular_file() {
        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .canonicalize()
            .expect("canonical temp root")
            .join("oversized.json");
        let file = File::create(&path).expect("fixture");
        file.set_len(MAX_EXTERNAL_CREDENTIAL_BYTES + 1)
            .expect("oversize fixture");
        let error = read_to_string(&grant(&path)).expect_err("oversized file");
        assert!(error.to_string().contains("safety limit"), "{error:#}");
    }

    #[cfg(unix)]
    #[test]
    fn owned_read_requires_owner_only_regular_file_and_never_follows_symlinks() {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical temp root");
        let path = root.join("owned.json");
        std::fs::write(&path, "owned-secret").expect("fixture");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
            .expect("loose mode");
        assert!(
            read_codewhale_owned_to_string(&path).is_err(),
            "group/other-readable owned credentials must fail closed"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("owner-only mode");
        assert_eq!(
            read_codewhale_owned_to_string(&path)
                .expect("secure owned read")
                .as_deref(),
            Some("owned-secret")
        );

        let hardlink = root.join("owned-hardlink.json");
        std::fs::hard_link(&path, &hardlink).expect("hardlink fixture");
        assert!(
            read_codewhale_owned_to_string(&path).is_err(),
            "owned reads must reject multiply-linked files"
        );
        std::fs::remove_file(hardlink).expect("remove hardlink fixture");

        let link = root.join("owned-link.json");
        symlink(&path, &link).expect("symlink");
        assert!(read_codewhale_owned_to_string(&link).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn owned_read_is_bounded() {
        use std::os::unix::fs::PermissionsExt as _;

        let _env = crate::test_support::lock_test_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir
            .path()
            .canonicalize()
            .unwrap()
            .join("oversized-owned.json");
        let file = File::create(&path).expect("fixture");
        file.set_len(MAX_EXTERNAL_CREDENTIAL_BYTES + 1).unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .unwrap();
        let error = read_codewhale_owned_to_string(&path).expect_err("oversized owned file");
        assert!(error.to_string().contains("safety limit"), "{error:#}");
    }

    #[cfg(windows)]
    fn secured_owned_windows_fixture(contents: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        secure_codewhale_owned_windows_path(dir.path(), true).expect("owner-only directory");
        let path = dir.path().join("owned.json");
        std::fs::write(&path, contents).expect("fixture");
        secure_codewhale_owned_windows_path(&path, false).expect("owner-only file");
        (dir, path)
    }

    #[cfg(windows)]
    #[test]
    fn owned_read_accepts_current_user_only_dacl_with_opened_path_spelling() {
        let _env = crate::test_support::lock_test_env();
        let (_dir, path) = secured_owned_windows_fixture(b"owned-secret");
        assert_eq!(
            read_codewhale_owned_to_string(&path)
                .expect("secure owned read")
                .as_deref(),
            Some("owned-secret")
        );
    }

    #[cfg(windows)]
    #[test]
    fn owned_read_rejects_hardlinks_on_windows() {
        let _env = crate::test_support::lock_test_env();
        let (dir, path) = secured_owned_windows_fixture(b"owned-secret");
        let hardlink = dir.path().join("owned-hardlink.json");
        std::fs::hard_link(&path, &hardlink).expect("hardlink fixture");
        assert!(
            read_codewhale_owned_to_string(&path).is_err(),
            "owned reads must reject multiply-linked files"
        );
        std::fs::remove_file(hardlink).expect("remove hardlink fixture");
    }

    #[cfg(windows)]
    #[test]
    fn owned_read_is_bounded_on_windows() {
        let _env = crate::test_support::lock_test_env();
        let (_dir, path) = secured_owned_windows_fixture(b"owned-secret");
        let file = File::options()
            .write(true)
            .open(&path)
            .expect("reopen fixture");
        file.set_len(MAX_EXTERNAL_CREDENTIAL_BYTES + 1)
            .expect("oversize fixture");
        let error = read_codewhale_owned_to_string(&path).expect_err("oversized owned file");
        assert!(error.to_string().contains("safety limit"), "{error:#}");
    }

    #[cfg(windows)]
    #[test]
    fn owned_read_rejects_leaf_reparse_points_on_windows() {
        let _env = crate::test_support::lock_test_env();
        let (dir, path) = secured_owned_windows_fixture(b"owned-secret");
        let link = dir.path().join("owned-link.json");
        if std::os::windows::fs::symlink_file(&path, &link).is_ok() {
            assert!(
                read_codewhale_owned_to_string(&link).is_err(),
                "owned reads must reject leaf reparse points"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_handle_path_comparison_is_lossless_and_fails_closed() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt as _;
        use std::path::PathBuf;

        let expected = PathBuf::from(r"C:\Users\Alice\credential.json");
        let kernel = PathBuf::from(r"\\?\C:\Users\Alice\credential.json");
        assert_eq!(
            normalize_windows_path_for_comparison(&expected).unwrap(),
            normalize_windows_path_for_comparison(&kernel).unwrap()
        );
        assert_eq!(
            normalize_windows_path_for_comparison(Path::new(r"C:\Users\Alice\A\credential.json"))
                .unwrap(),
            normalize_windows_path_for_comparison(Path::new(r"C:\Users\Alice\a\credential.json"))
                .unwrap(),
            "Windows credential path identity must compare case-insensitively"
        );

        let invalid = PathBuf::from(OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            0xd800,
        ]));
        assert!(normalize_windows_path_for_comparison(&invalid).is_err());
    }
}

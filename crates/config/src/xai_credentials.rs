//! Naming and cleanup policy for Codewhale-owned xAI OAuth generations.
//!
//! Config stores only a validated basename. Callers can therefore never turn
//! the generation pointer into an arbitrary path read or deletion primitive.

#[cfg(any(not(unix), test))]
use std::fs;
use std::fs::File;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
#[cfg(not(windows))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
#[cfg(not(windows))]
use std::time::{SystemTime, UNIX_EPOCH};

use crate::private_directory::validate_owned_file_handle;
#[cfg(all(windows, test))]
use crate::private_directory::{
    fail_next_windows_post_persist_validation, normalize_windows_path_for_comparison,
    verify_windows_owner_only_handle,
};
use anyhow::{Context, Result, bail};

pub const XAI_OAUTH_GENERATION_PREFIX: &str = "xai-auth-";
pub const XAI_OAUTH_GENERATION_SUFFIX: &str = ".json";
pub const LEGACY_XAI_OAUTH_FILE_NAME: &str = "xai-auth.json";
pub const CHATGPT_OAUTH_GENERATION_PREFIX: &str = "chatgpt-auth-";
pub const CHATGPT_OAUTH_GENERATION_SUFFIX: &str = ".json";
pub const LEGACY_CHATGPT_OAUTH_FILE_NAME: &str = "chatgpt-oauth.json";
pub const CLAUDE_OAUTH_GENERATION_PREFIX: &str = "claude-auth-";
pub const CLAUDE_OAUTH_GENERATION_SUFFIX: &str = ".json";
pub const LEGACY_CLAUDE_OAUTH_FILE_NAME: &str = "claude-oauth.json";
/// Stable local host and registration metadata, retained across token logout.
pub const CHATGPT_HOST_FILE_NAME: &str = "chatgpt-host.json";
const XAI_OAUTH_LIFECYCLE_LOCK_FILE_NAME: &str = ".xai-oauth.lock";
const XAI_OAUTH_FILE_LIMIT: u64 = 1024 * 1024;

/// Stable handle to Codewhale's private xAI OAuth directory.
///
/// The lexical `$CODEWHALE_HOME/credentials` boundary is retained verbatim.
/// Unix opens every component relative to the preceding directory with
/// `O_NOFOLLOW`; Windows keeps non-delete-shared handles to every component and
/// rejects reparse points. Holding this value therefore pins the directory
/// identity for the duration of one lifecycle operation.
#[derive(Debug)]
pub struct XaiOAuthCredentialStore {
    directory: PathBuf,
    private: crate::private_directory::PrivateDirectory,
}

/// Files retired from an active xAI OAuth epoch before a mode switch commits.
///
/// Unix hides original names behind private tombstones immediately. Windows
/// relies on the lifecycle lock, then deletes exact handles after commit. A
/// failed config mutation restores or retains the prior files; a successful
/// mutation removes them.
#[derive(Debug)]
pub struct XaiOAuthRevocation {
    retired: Vec<(String, String)>,
}

#[must_use]
pub fn is_valid_xai_oauth_generation(value: &str) -> bool {
    is_valid_owned_oauth_generation(
        value,
        XAI_OAUTH_GENERATION_PREFIX,
        XAI_OAUTH_GENERATION_SUFFIX,
    )
}

pub fn validate_xai_oauth_generation(value: &str) -> Result<&str> {
    if !is_valid_xai_oauth_generation(value) {
        bail!(
            "invalid Codewhale-owned xAI OAuth generation; expected xai-auth-<32 lowercase hex>.json"
        );
    }
    Ok(value)
}

#[must_use]
pub fn is_valid_chatgpt_oauth_generation(value: &str) -> bool {
    is_valid_owned_oauth_generation(
        value,
        CHATGPT_OAUTH_GENERATION_PREFIX,
        CHATGPT_OAUTH_GENERATION_SUFFIX,
    )
}

pub fn validate_chatgpt_oauth_generation(value: &str) -> Result<&str> {
    if !is_valid_chatgpt_oauth_generation(value) {
        bail!(
            "invalid Codewhale-owned ChatGPT OAuth generation; expected chatgpt-auth-<32 lowercase hex>.json"
        );
    }
    Ok(value)
}

pub fn is_valid_claude_oauth_generation(value: &str) -> bool {
    is_valid_owned_oauth_generation(
        value,
        CLAUDE_OAUTH_GENERATION_PREFIX,
        CLAUDE_OAUTH_GENERATION_SUFFIX,
    )
}

pub fn validate_claude_oauth_generation(value: &str) -> Result<&str> {
    anyhow::ensure!(
        is_valid_claude_oauth_generation(value),
        "invalid Codewhale-owned Claude OAuth generation"
    );
    Ok(value)
}

pub fn claude_oauth_generation_path(value: &str) -> Result<PathBuf> {
    Ok(xai_oauth_credentials_dir()?.join(validate_claude_oauth_generation(value)?))
}

fn is_valid_owned_oauth_generation(value: &str, prefix: &str, suffix: &str) -> bool {
    let path = Path::new(value);
    if path.components().count() != 1
        || !matches!(path.components().next(), Some(Component::Normal(_)))
        || path.file_name().and_then(|name| name.to_str()) != Some(value)
    {
        return false;
    }
    let Some(id) = value
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(suffix))
    else {
        return false;
    };
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn xai_oauth_credentials_dir() -> Result<PathBuf> {
    lexical_absolute_path(&crate::codewhale_home()?.join("credentials"))
}

/// Make an owned path absolute without resolving any filesystem component.
/// Canonicalization is deliberately forbidden here: following an existing
/// `credentials` symlink would erase the lexical Codewhale-owned boundary and
/// turn an external directory into an apparently valid destination.
fn lexical_absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("resolving the Codewhale credentials directory")?
            .join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        bail!(
            "Codewhale credentials directory must be lexically normalized: {}",
            crate::quote_os_path(&absolute)
        );
    }
    Ok(absolute)
}

pub fn xai_oauth_generation_path(generation: &str) -> Result<PathBuf> {
    Ok(xai_oauth_credentials_dir()?.join(validate_xai_oauth_generation(generation)?))
}

pub fn legacy_xai_oauth_path() -> Result<PathBuf> {
    Ok(xai_oauth_credentials_dir()?.join(LEGACY_XAI_OAUTH_FILE_NAME))
}

pub fn chatgpt_oauth_generation_path(generation: &str) -> Result<PathBuf> {
    Ok(xai_oauth_credentials_dir()?.join(validate_chatgpt_oauth_generation(generation)?))
}

pub fn legacy_chatgpt_oauth_path() -> Result<PathBuf> {
    Ok(xai_oauth_credentials_dir()?.join(LEGACY_CHATGPT_OAUTH_FILE_NAME))
}

/// Serialize every Codewhale-owned xAI OAuth lifecycle mutation across threads
/// and processes while pinning the lexical credentials directory.
///
/// Lock order is always xAI lifecycle first, then config document. Callers must
/// not invoke this function recursively.
pub fn with_xai_oauth_lifecycle_lock<T>(
    operation: impl FnOnce(&XaiOAuthCredentialStore) -> Result<T>,
) -> Result<T> {
    static PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _process_guard = PROCESS_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| anyhow::anyhow!("xAI OAuth lifecycle lock was poisoned"))?;
    let store = XaiOAuthCredentialStore::open()?;
    let lock_file = store.open_lock_file()?;
    let mut lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock.write().with_context(|| {
        format!(
            "failed to acquire xAI OAuth lifecycle lock in {}",
            crate::quote_os_path(store.directory())
        )
    })?;
    operation(&store)
}

/// Run an authority mode switch while the prior owned OAuth epoch is hidden
/// from concurrent Codewhale readers. A failed authority mutation restores the
/// old files; a successful mutation permanently removes them.
pub fn with_xai_oauth_revocation_transaction<T>(
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    with_xai_oauth_lifecycle_lock(|store| {
        let revocation = store.stage_revocation()?;
        match operation() {
            Ok(value) => {
                revocation.commit(store).context(
                    "xAI OAuth authority changed, but retired owned credentials could not be removed",
                )?;
                Ok(value)
            }
            Err(error) => {
                if let Err(rollback) = revocation.rollback(store) {
                    return Err(error).context(format!(
                        "also failed to restore the prior xAI OAuth epoch: {rollback:#}"
                    ));
                }
                Err(error)
            }
        }
    })
}

impl XaiOAuthCredentialStore {
    fn open() -> Result<Self> {
        let directory = xai_oauth_credentials_dir()?;
        open_owned_credentials_directory(&directory)
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn path_for(&self, name: &str) -> Result<PathBuf> {
        validate_owned_auth_name(name)?;
        Ok(self.directory.join(name))
    }

    pub fn read_to_string(&self, name: &str) -> Result<Option<String>> {
        validate_owned_auth_name(name)?;
        let Some(mut file) = self.open_owned_file_for_read(name)? else {
            return Ok(None);
        };
        let metadata =
            validate_owned_file_handle(&file, &self.directory.join(name), self.private.guard())?;
        if metadata.len() > XAI_OAUTH_FILE_LIMIT {
            bail!(
                "Codewhale-owned xAI OAuth file {} exceeds the {} byte limit",
                crate::quote_os_path(&self.directory.join(name)),
                XAI_OAUTH_FILE_LIMIT
            );
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        (&mut file)
            .take(XAI_OAUTH_FILE_LIMIT + 1)
            .read_to_end(&mut bytes)
            .with_context(|| {
                format!(
                    "reading Codewhale-owned xAI OAuth file {}",
                    crate::quote_os_path(&self.directory.join(name))
                )
            })?;
        if bytes.len() as u64 > XAI_OAUTH_FILE_LIMIT {
            bail!(
                "Codewhale-owned xAI OAuth file {} exceeds the {} byte limit",
                crate::quote_os_path(&self.directory.join(name)),
                XAI_OAUTH_FILE_LIMIT
            );
        }
        String::from_utf8(bytes).map(Some).map_err(|_| {
            anyhow::anyhow!(
                "Codewhale-owned xAI OAuth file {} is not valid UTF-8",
                crate::quote_os_path(&self.directory.join(name))
            )
        })
    }

    pub fn write(&self, name: &str, bytes: &[u8], allow_replace: bool) -> Result<()> {
        validate_owned_auth_name(name)?;
        anyhow::ensure!(
            bytes.len() as u64 <= XAI_OAUTH_FILE_LIMIT,
            "refusing oversized xAI OAuth credential payload"
        );
        self.write_owned_file(name, bytes, allow_replace)
    }

    pub fn remove(&self, name: &str) -> Result<bool> {
        validate_owned_auth_name(name)?;
        self.remove_raw(name)
    }

    pub fn clear_all(&self) -> Result<usize> {
        let mut removed = 0;
        for name in self.owned_auth_names()? {
            if self.remove(&name)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn clear_chatgpt(&self) -> Result<usize> {
        let mut removed = 0;
        for name in owned_auth_names_for(self, is_chatgpt_owned_auth_name)? {
            if self.remove(&name)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub fn clear_claude(&self) -> Result<usize> {
        let mut removed = 0;
        for name in owned_auth_names_for(self, |name| {
            name == LEGACY_CLAUDE_OAUTH_FILE_NAME || is_valid_claude_oauth_generation(name)
        })? {
            if self.remove(&name)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Stage every active owned credential before a config mode switch. The
    /// generation basename is the OAuth epoch; the lifecycle lock prevents a
    /// stale Codewhale reader from using it while authority changes.
    pub fn stage_revocation(&self) -> Result<XaiOAuthRevocation> {
        #[cfg(windows)]
        {
            // Every Codewhale reader/writer takes the lifecycle lock, so a
            // Windows mode switch can retain the exact active basenames until
            // the config commit succeeds. `commit` then opens each leaf with
            // DELETE access and marks that exact handle for deletion. This
            // avoids path-based rename races and makes rollback a no-op.
            Ok(XaiOAuthRevocation {
                retired: self
                    .owned_auth_names()?
                    .into_iter()
                    .map(|name| (name, String::new()))
                    .collect(),
            })
        }

        #[cfg(not(windows))]
        {
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let mut retired = Vec::new();
            for (index, name) in self.owned_auth_names()?.into_iter().enumerate() {
                let tombstone = format!(
                    ".xai-oauth-retired-{}-{nonce}-{}-{index}.tmp",
                    std::process::id(),
                    COUNTER.fetch_add(1, Ordering::Relaxed)
                );
                if let Err(error) = self.rename_raw(&name, &tombstone) {
                    let rollback = XaiOAuthRevocation { retired };
                    if let Err(rollback_error) = rollback.rollback(self) {
                        return Err(error).context(format!(
                        "also failed to restore previously retired xAI OAuth files: {rollback_error:#}"
                    ));
                    }
                    return Err(error);
                }
                retired.push((name, tombstone));
            }
            Ok(XaiOAuthRevocation { retired })
        }
    }

    fn owned_auth_names(&self) -> Result<Vec<String>> {
        owned_auth_names_in_store(self)
    }

    fn open_lock_file(&self) -> Result<File> {
        self.open_internal_file(XAI_OAUTH_LIFECYCLE_LOCK_FILE_NAME)
    }
}

#[cfg(unix)]
fn owned_auth_names_in_store(store: &XaiOAuthCredentialStore) -> Result<Vec<String>> {
    use std::ffi::CStr;
    use std::os::fd::AsRawFd as _;

    // `fdopendir` consumes its descriptor, so enumerate through a duplicate of
    // the pinned directory handle. No pathname is resolved after the store is
    // opened, even if the lexical directory is renamed or replaced.
    // SAFETY: duplicates the live store descriptor; CLOEXEC keeps it out of children.
    let duplicated = unsafe {
        libc::fcntl(
            store.private.directory_handle.as_raw_fd(),
            libc::F_DUPFD_CLOEXEC,
            0,
        )
    };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error())
            .context("duplicating Codewhale credentials directory handle");
    }
    // SAFETY: `duplicated` is an owned directory descriptor. `closedir` below
    // assumes ownership on the successful conversion.
    let stream = unsafe { libc::fdopendir(duplicated) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `fdopendir` failed and therefore did not consume the fd.
        unsafe { libc::close(duplicated) };
        return Err(error).context("enumerating Codewhale credentials directory");
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `stream` remains live until `closedir`; each returned entry
        // is valid until the next call and copied before then.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: POSIX `dirent::d_name` is NUL terminated.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let Ok(name) = name.to_str() else {
            continue;
        };
        if name == LEGACY_XAI_OAUTH_FILE_NAME || is_valid_xai_oauth_generation(name) {
            names.push(name.to_string());
        }
    }
    // SAFETY: `stream` is still owned and has not previously been closed.
    if unsafe { libc::closedir(stream) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("closing Codewhale credentials directory enumeration");
    }
    names.sort();
    Ok(names)
}

#[cfg(unix)]
fn owned_auth_names_for(
    store: &XaiOAuthCredentialStore,
    accept: fn(&str) -> bool,
) -> Result<Vec<String>> {
    use std::ffi::CStr;
    use std::os::fd::AsRawFd as _;

    // SAFETY: duplicates the live store descriptor; CLOEXEC keeps it out of children.
    let duplicated = unsafe {
        libc::fcntl(
            store.private.directory_handle.as_raw_fd(),
            libc::F_DUPFD_CLOEXEC,
            0,
        )
    };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error())
            .context("duplicating Codewhale credentials directory handle");
    }
    // SAFETY: `duplicated` is an owned directory descriptor. `closedir` below
    // assumes ownership on the successful conversion.
    let stream = unsafe { libc::fdopendir(duplicated) };
    if stream.is_null() {
        let error = std::io::Error::last_os_error();
        // SAFETY: `fdopendir` failed and therefore did not consume the fd.
        unsafe { libc::close(duplicated) };
        return Err(error).context("enumerating Codewhale credentials directory");
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `stream` remains live until `closedir`; each returned entry
        // is valid until the next call and copied before then.
        let entry = unsafe { libc::readdir(stream) };
        if entry.is_null() {
            break;
        }
        // SAFETY: POSIX `dirent::d_name` is NUL terminated.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let Ok(name) = name.to_str() else {
            continue;
        };
        if accept(name) {
            names.push(name.to_string());
        }
    }
    // SAFETY: `stream` is still owned and has not previously been closed.
    if unsafe { libc::closedir(stream) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("closing Codewhale credentials directory enumeration");
    }
    names.sort();
    Ok(names)
}

#[cfg(not(unix))]
fn owned_auth_names_in_store(store: &XaiOAuthCredentialStore) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let entries = fs::read_dir(&store.directory).with_context(|| {
        format!(
            "failed to inspect Codewhale credentials directory {}",
            crate::quote_os_path(&store.directory)
        )
    })?;
    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect Codewhale credentials directory {}",
                crate::quote_os_path(&store.directory)
            )
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if name == LEGACY_XAI_OAUTH_FILE_NAME || is_valid_xai_oauth_generation(name) {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

#[cfg(not(unix))]
fn owned_auth_names_for(
    store: &XaiOAuthCredentialStore,
    accept: fn(&str) -> bool,
) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let entries = fs::read_dir(&store.directory).with_context(|| {
        format!(
            "failed to inspect Codewhale credentials directory {}",
            crate::quote_os_path(&store.directory)
        )
    })?;
    for entry in entries {
        let entry = entry.with_context(|| {
            format!(
                "failed to inspect Codewhale credentials directory {}",
                crate::quote_os_path(&store.directory)
            )
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if accept(name) {
            names.push(name.to_string());
        }
    }
    names.sort();
    Ok(names)
}

impl XaiOAuthRevocation {
    /// Restore the old epoch after a config mutation fails. Restoration is
    /// fail-closed: an unexpected replacement at an original name is never
    /// overwritten.
    pub fn rollback(self, store: &XaiOAuthCredentialStore) -> Result<()> {
        #[cfg(windows)]
        {
            let _ = store;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let mut first_error = None;
            for (original, tombstone) in self.retired.into_iter().rev() {
                let result = store.rename_raw(&tombstone, &original).with_context(|| {
                    format!(
                        "restoring retired xAI OAuth file {}",
                        crate::quote_os_path(&store.directory.join(original))
                    )
                });
                if result.is_err() && first_error.is_none() {
                    first_error = result.err();
                }
            }
            if let Some(error) = first_error {
                return Err(error);
            }
            Ok(())
        }
    }

    /// Permanently remove retired bytes after the replacement config commits.
    pub fn commit(self, store: &XaiOAuthCredentialStore) -> Result<usize> {
        let mut removed = 0;
        for (_original, _tombstone) in self.retired {
            #[cfg(windows)]
            let target = _original;
            #[cfg(not(windows))]
            let target = _tombstone;
            if store.remove_raw(&target)? {
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn validate_owned_auth_name(name: &str) -> Result<()> {
    anyhow::ensure!(
        name == LEGACY_XAI_OAUTH_FILE_NAME
            || name == LEGACY_CHATGPT_OAUTH_FILE_NAME
            || name == CHATGPT_HOST_FILE_NAME
            || name == LEGACY_CLAUDE_OAUTH_FILE_NAME
            || is_valid_claude_oauth_generation(name)
            || is_valid_xai_oauth_generation(name)
            || is_valid_chatgpt_oauth_generation(name),
        "invalid Codewhale-owned OAuth basename"
    );
    Ok(())
}

fn is_chatgpt_owned_auth_name(name: &str) -> bool {
    name == LEGACY_CHATGPT_OAUTH_FILE_NAME || is_valid_chatgpt_oauth_generation(name)
}

fn open_owned_credentials_directory(directory: &Path) -> Result<XaiOAuthCredentialStore> {
    // Sign-in has to work on a filesystem that cannot express ownership, or it
    // cannot work there at all: HarmonyOS hmdfs reports the logged-in user's
    // uid and a group-writable mode for every object an app creates and refuses
    // `chmod`, so the strict proof can never hold. The trusting admission keeps
    // the no-follow walk, the single-link check and atomic publication, and
    // relies on the mount's own access control for the rest — the same tradeoff
    // the file-backed secret store already makes on that mount, with the same
    // warning.
    Ok(XaiOAuthCredentialStore {
        directory: directory.to_path_buf(),
        private: crate::private_directory::PrivateDirectory::open_trusting_mount(directory)?,
    })
}

impl XaiOAuthCredentialStore {
    fn open_owned_file_for_read(&self, name: &str) -> Result<Option<File>> {
        self.private.open_owned_file_for_read(name)
    }
    fn open_internal_file(&self, name: &str) -> Result<File> {
        self.private.open_internal_file(name)
    }
    fn write_owned_file(&self, name: &str, bytes: &[u8], allow_replace: bool) -> Result<()> {
        self.private.write_owned_file(name, bytes, allow_replace)
    }
    fn remove_raw(&self, name: &str) -> Result<bool> {
        self.private.remove_raw(name)
    }
    #[cfg(not(windows))]
    fn rename_raw(&self, from: &str, to: &str) -> Result<()> {
        self.private.rename_raw(from, to)
    }
}

/// Delete one superseded generation after its replacement pointer committed.
/// The basename is validated before any filesystem access.
pub fn remove_xai_oauth_generation(generation: &str) -> Result<bool> {
    let generation = validate_xai_oauth_generation(generation)?;
    with_xai_oauth_lifecycle_lock(|store| store.remove(generation))
}

/// Explicit logout policy: remove the legacy Codewhale-owned file and every
/// valid generated xAI OAuth file. Unknown files in the credentials directory
/// are never touched.
pub fn clear_all_xai_oauth_credentials() -> Result<usize> {
    with_xai_oauth_lifecycle_lock(XaiOAuthCredentialStore::clear_all)
}

pub fn clear_all_chatgpt_oauth_credentials() -> Result<usize> {
    with_xai_oauth_lifecycle_lock(XaiOAuthCredentialStore::clear_chatgpt)
}

/// Clears every Codewhale-owned ChatGPT OAuth file without re-entering the
/// lifecycle lock. Only for code already running inside
/// [`with_xai_oauth_lifecycle_lock`] or [`with_xai_oauth_revocation_transaction`];
/// everyone else must call [`clear_all_chatgpt_oauth_credentials`], which
/// takes the lock. The lifecycle lock is a non-reentrant in-process mutex, so
/// a nested call would deadlock the logout path (seen as CI hangs on the
/// `logout_*` tests).
pub fn clear_all_chatgpt_oauth_credentials_locked() -> Result<usize> {
    let store = XaiOAuthCredentialStore::open()?;
    XaiOAuthCredentialStore::clear_chatgpt(&store)
}

pub fn clear_all_claude_oauth_credentials_locked() -> Result<usize> {
    XaiOAuthCredentialStore::clear_claude(&XaiOAuthCredentialStore::open()?)
}

pub fn remove_chatgpt_oauth_generation(generation: &str) -> Result<bool> {
    let generation = validate_chatgpt_oauth_generation(generation)?;
    with_xai_oauth_lifecycle_lock(|store| store.remove(generation))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_directory_preserves_lexical_identity() {
        let directory = tempfile::tempdir().expect("temp dir");
        let lexical = directory.path().join("missing").join("credentials");
        assert_eq!(
            lexical_absolute_path(&lexical).expect("preserve lexical path"),
            lexical
        );
        assert!(!lexical.exists());
        assert!(
            lexical_absolute_path(&directory.path().join("missing/../escape")).is_err(),
            "owned credential roots must reject traversal components"
        );
    }

    #[test]
    fn generation_names_are_strict_basenames() {
        let valid = "xai-auth-0123456789abcdef0123456789abcdef.json";
        assert!(is_valid_xai_oauth_generation(valid));
        for invalid in [
            "../xai-auth-0123456789abcdef0123456789abcdef.json",
            "/tmp/xai-auth-0123456789abcdef0123456789abcdef.json",
            "xai-auth-0123456789ABCDEF0123456789ABCDEF.json",
            "xai-auth-short.json",
            "xai-auth.json",
        ] {
            assert!(!is_valid_xai_oauth_generation(invalid), "{invalid}");
        }
        let chatgpt = "chatgpt-auth-0123456789abcdef0123456789abcdef.json";
        assert!(is_valid_chatgpt_oauth_generation(chatgpt));
        assert!(!is_valid_chatgpt_oauth_generation(valid));
        assert!(!is_valid_xai_oauth_generation(chatgpt));
        assert!(!is_valid_chatgpt_oauth_generation(
            "../chatgpt-auth-0123456789abcdef0123456789abcdef.json"
        ));
    }

    #[test]
    fn logout_cleanup_removes_only_owned_xai_files() {
        let directory = tempfile::tempdir().expect("temp dir");
        let directory = directory.path().canonicalize().expect("canonical temp dir");
        let store = open_owned_credentials_directory(&directory).expect("open store");
        let generation = "xai-auth-0123456789abcdef0123456789abcdef.json";
        store
            .write(generation, b"secret", false)
            .expect("generation");
        store
            .write(LEGACY_XAI_OAUTH_FILE_NAME, b"legacy", false)
            .expect("legacy");
        fs::write(directory.join("other-provider.json"), "keep").expect("other provider");

        assert_eq!(store.clear_all().expect("clear"), 2);
        assert!(directory.join("other-provider.json").exists());
        assert!(!directory.join(generation).exists());
        assert!(!directory.join("xai-auth.json").exists());
    }

    #[test]
    fn logout_cleanup_removes_chatgpt_files_without_touching_xai() {
        let directory = tempfile::tempdir().expect("temp dir");
        let directory = directory.path().canonicalize().expect("canonical temp dir");
        let store = open_owned_credentials_directory(&directory).expect("open store");
        let chatgpt = "chatgpt-auth-0123456789abcdef0123456789abcdef.json";
        let xai = "xai-auth-0123456789abcdef0123456789abcdef.json";
        store.write(chatgpt, b"chatgpt", false).expect("chatgpt");
        store.write(xai, b"xai", false).expect("xai");
        store
            .write(CHATGPT_HOST_FILE_NAME, b"host", false)
            .expect("host");
        store
            .write(LEGACY_CHATGPT_OAUTH_FILE_NAME, b"legacy", false)
            .expect("legacy chatgpt");

        assert_eq!(store.clear_chatgpt().expect("clear chatgpt"), 2);
        assert!(directory.join(xai).exists());
        assert!(directory.join(CHATGPT_HOST_FILE_NAME).exists());
        assert!(!directory.join(chatgpt).exists());
        assert!(!directory.join(LEGACY_CHATGPT_OAUTH_FILE_NAME).exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_store_pins_directory_identity_across_lexical_path_swap() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path().canonicalize().expect("canonical temp root");
        let credentials = root.join("credentials");
        fs::create_dir(&credentials).expect("credentials");
        let store = open_owned_credentials_directory(&credentials).expect("open pinned store");
        let parked = root.join("parked-credentials");
        let external = root.join("external-owner");
        fs::create_dir(&external).expect("external directory");
        fs::rename(&credentials, &parked).expect("park credentials");
        symlink(&external, &credentials).expect("replace lexical path with symlink");

        let generation = "xai-auth-0123456789abcdef0123456789abcdef.json";
        store
            .write(generation, b"pinned bytes", false)
            .expect("write through pinned directory handle");

        assert_eq!(fs::read(parked.join(generation)).unwrap(), b"pinned bytes");
        assert!(!external.join(generation).exists());
        assert_eq!(
            store.read_to_string(generation).unwrap().as_deref(),
            Some("pinned bytes")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_store_rejects_symlinked_root_component_and_hardlinked_leaf() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path().canonicalize().expect("canonical temp root");
        let real = root.join("real-home");
        fs::create_dir(&real).expect("real home");
        let linked = root.join("linked-home");
        symlink(&real, &linked).expect("home symlink");
        assert!(
            open_owned_credentials_directory(&linked.join("credentials")).is_err(),
            "owned roots must reject every symlink component"
        );

        let credentials = real.join("credentials");
        let store = open_owned_credentials_directory(&credentials).expect("safe store");
        let generation = "xai-auth-fedcba9876543210fedcba9876543210.json";
        store
            .write(generation, b"secret", false)
            .expect("seed generation");
        fs::hard_link(
            credentials.join(generation),
            credentials.join("attacker-hardlink"),
        )
        .expect("hardlink fixture");
        assert!(
            store.read_to_string(generation).is_err(),
            "owned reads must reject multiply-linked credential files"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_owned_path_identity_is_case_insensitive_and_lossless() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt as _;

        assert_eq!(
            normalize_windows_path_for_comparison(Path::new(r"C:\Users\Alice\Credentials"))
                .unwrap(),
            normalize_windows_path_for_comparison(Path::new(r"\\?\c:\users\ALICE\credentials"))
                .unwrap()
        );
        let invalid = PathBuf::from(OsString::from_wide(&[
            b'C' as u16,
            b':' as u16,
            b'\\' as u16,
            0xd800,
        ]));
        assert!(normalize_windows_path_for_comparison(&invalid).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn windows_post_persist_failure_exact_deletes_new_and_replacement_generations() {
        fn assert_directory_empty(path: &Path) {
            let entries = fs::read_dir(path)
                .expect("read credentials directory")
                .collect::<std::io::Result<Vec<_>>>()
                .expect("read credential entries");
            assert!(
                entries.is_empty(),
                "rejected credential bytes must leave no durable file: {entries:?}"
            );
        }

        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path().canonicalize().expect("canonical temp root");
        let credentials = root.join("new-credentials");
        let store = open_owned_credentials_directory(&credentials).expect("open secure store");
        let generation = "xai-auth-0123456789abcdef0123456789abcdef.json";
        let generation_path = credentials.join(generation);
        fail_next_windows_post_persist_validation(&generation_path);
        let error = store
            .write(generation, b"new credential bytes", false)
            .expect_err("post-persist validation must fail");
        assert!(error.to_string().contains("injected post-persistence"));
        assert_directory_empty(&credentials);

        let replacement_credentials = root.join("replacement-credentials");
        let replacement_store = open_owned_credentials_directory(&replacement_credentials)
            .expect("open replacement store");
        let replacement_path = replacement_credentials.join(generation);
        replacement_store
            .write(generation, b"prior credential bytes", false)
            .expect("seed prior generation");
        fail_next_windows_post_persist_validation(&replacement_path);
        let error = replacement_store
            .write(generation, b"replacement credential bytes", true)
            .expect_err("replacement validation must fail");
        assert!(error.to_string().contains("injected post-persistence"));
        assert!(
            !replacement_path.exists(),
            "a rejected replacement deliberately fails closed instead of restoring by path"
        );
        assert_directory_empty(&replacement_credentials);
    }

    #[cfg(windows)]
    #[test]
    fn windows_store_secures_every_new_owned_object_for_the_current_user() {
        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path().canonicalize().expect("canonical temp root");
        let credentials = root.join("credentials");
        let store = open_owned_credentials_directory(&credentials).expect("open secure store");

        let directory = store
            .private
            ._component_handles
            .last()
            .expect("final credentials directory handle");
        verify_windows_owner_only_handle(directory).expect("current-user-only directory");

        let lock = store.open_lock_file().expect("create lifecycle lock");
        validate_owned_file_handle(
            &lock,
            &credentials.join(XAI_OAUTH_LIFECYCLE_LOCK_FILE_NAME),
            store.private.guard(),
        )
        .expect("current-user-only lifecycle lock");

        let generation = "xai-auth-0123456789abcdef0123456789abcdef.json";
        store
            .write(generation, b"credential bytes", false)
            .expect("write secure generation");
        let generation_file = store
            .open_owned_file_for_read(generation)
            .expect("open generation")
            .expect("generation exists");
        validate_owned_file_handle(
            &generation_file,
            &credentials.join(generation),
            store.private.guard(),
        )
        .expect("current-user-only generation");
    }

    #[cfg(windows)]
    #[test]
    fn windows_store_rejects_reparse_component_and_hardlinked_leaf() {
        let root = tempfile::tempdir().expect("temp dir");
        let root = root.path().canonicalize().expect("canonical temp root");
        let real = root.join("real-home");
        fs::create_dir(&real).expect("real home");
        let linked = root.join("linked-home");
        if std::os::windows::fs::symlink_dir(&real, &linked).is_ok() {
            assert!(
                open_owned_credentials_directory(&linked.join("credentials")).is_err(),
                "owned roots must reject junctions and directory reparse points"
            );
        }

        let credentials = real.join("credentials");
        let store = open_owned_credentials_directory(&credentials).expect("safe store");
        let generation = "xai-auth-fedcba9876543210fedcba9876543210.json";
        store
            .write(generation, b"secret", false)
            .expect("seed generation");
        fs::hard_link(
            credentials.join(generation),
            credentials.join("attacker-hardlink"),
        )
        .expect("hardlink fixture");
        assert!(
            store.read_to_string(generation).is_err(),
            "owned reads must reject multiply-linked credential files"
        );
    }
}

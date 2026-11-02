//! # Endpoint resolution
//!
//! In precedence order (see [`resolve_socket_path`]):
//!
//! 1. an explicit path (`--socket-path`);
//! 2. `$CODEWHALE_HOME/run/daemon.sock` when `CODEWHALE_HOME` is set — an
//!    explicit home is an isolation boundary, so its daemon must not collide
//!    with the default one;
//! 3. `$XDG_RUNTIME_DIR/codewhale/daemon.sock`;
//! 4. macOS: `~/Library/Application Support/codewhale/daemon.sock`;
//! 5. `~/.codewhale/run/daemon.sock`.
//!

#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;

/// Basename of the daemon socket inside the Codewhale runtime directory.
pub const DAEMON_SOCKET_FILE_NAME: &str = "daemon.sock";

/// Historical reserved endpoint used in unsupported-platform diagnostics.
/// Actual Windows owner endpoints are scoped to the selected home/principal.
pub const WINDOWS_NAMED_PIPE: &str = r"\\.\pipe\codewhale-daemon";

/// Longest socket path the kernel accepts (`sun_path` minus the NUL).
pub const MAX_SOCKET_PATH_BYTES: usize = if cfg!(any(target_os = "macos", target_os = "ios")) {
    103
} else {
    107
};

/// Typed failures of the daemon socket transport.
#[derive(Debug, thiserror::Error)]
#[expect(
    missing_docs,
    reason = "Preserve the relocated public error variant fields"
)]
pub enum DaemonSocketError {
    /// The platform has no daemon socket implementation. Never a silent
    /// fallback: the caller must pick another transport explicitly.
    #[error(
        "the daemon socket transport is not supported on {platform}; the reserved endpoint \
         there is the named pipe {planned_endpoint}, which is not implemented yet"
    )]
    UnsupportedPlatform {
        platform: &'static str,
        planned_endpoint: &'static str,
    },
    /// No home directory (or runtime directory) to derive a default path from.
    #[error(
        "cannot resolve the Codewhale runtime directory for the daemon socket: no home directory"
    )]
    RuntimeDirUnavailable,
    /// `CODEWHALE_HOME` is set but not a usable absolute path.
    #[error("invalid CODEWHALE_HOME override: {0}")]
    InvalidHomeOverride(String),
    /// Unix socket paths are limited to roughly one hundred bytes.
    #[error("daemon socket path {} is {len} bytes; this platform allows at most {max}", path.display())]
    PathTooLong {
        path: PathBuf,
        len: usize,
        max: usize,
    },
    /// Something other than a socket already sits at the path. Refused so a
    /// misconfigured path can never delete a user's file.
    #[error("{} exists and is not a unix socket; refusing to remove it", path.display())]
    NotASocket { path: PathBuf },
    /// A daemon answered on the socket: this one must not replace it.
    #[error(
        "a live listener already answers on {}; refusing to replace it (another codewhale daemon, or something else bound to this path)",
        path.display()
    )]
    AlreadyRunning { path: PathBuf },
    /// The liveness probe neither connected nor was refused within the
    /// budget. Refused rather than clobbered; remove the file by hand if the
    /// old daemon is truly gone.
    #[error("liveness probe of {} timed out; refusing to replace a socket that may be live", path.display())]
    ProbeTimedOut { path: PathBuf },
    /// Filesystem or socket I/O failed.
    #[error("{context} ({})", path.display())]
    Io {
        context: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The app-server state (config, state store, runtime) failed to build.
    ///
    /// The source carries the real cause; a caller that prints only this
    /// message hides it. `serve --http` on a mount where the endpoint
    /// directory cannot be private used to surface here as a bare
    /// "failed to build daemon state", which named neither the directory nor
    /// the reason — see [`Self::EndpointDirectoryUnsupported`].
    #[error("failed to build daemon state")]
    State(#[source] anyhow::Error),
    /// The filesystem cannot hold the private directory an endpoint needs.
    ///
    /// A private endpoint directory must be current-user-owned and closed to
    /// group and other. HarmonyOS hmdfs — the 2in1 user directory — reports the
    /// logged-in user's uid and a group-writable mode for every object created
    /// on it, so `chmod` answers `EPERM` and no directory there can satisfy that
    /// check.
    /// This is a missing capability, not a refusal: an owner with an HTTP
    /// frontend can run without its control socket, and does.
    #[error(
        "the filesystem cannot hold a private endpoint directory at {}: {reason}",
        path.display()
    )]
    EndpointDirectoryUnsupported { path: PathBuf, reason: String },
}

/// Inputs to [`resolve_socket_path`], separated from the environment so the
/// precedence rules are a pure, testable function.
#[derive(Debug, Clone, Default)]
pub struct SocketPathInputs {
    /// `--socket-path`.
    pub explicit: Option<PathBuf>,
    /// A valid explicit `CODEWHALE_HOME`.
    pub codewhale_home_override: Option<PathBuf>,
    /// `$XDG_RUNTIME_DIR`, when set and non-empty.
    pub xdg_runtime_dir: Option<PathBuf>,
    /// The user's home directory.
    pub user_home: Option<PathBuf>,
    /// Whether the macOS Application Support layout applies.
    pub macos: bool,
}

impl SocketPathInputs {
    /// Capture the live environment.
    pub fn from_environment(explicit: Option<PathBuf>) -> Result<Self, DaemonSocketError> {
        let codewhale_home_override = crate::codewhale_home_override()
            .map_err(|err| DaemonSocketError::InvalidHomeOverride(err.to_string()))?;
        let xdg_runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Ok(Self {
            explicit,
            codewhale_home_override,
            xdg_runtime_dir,
            user_home: crate::user_home(),
            macos: cfg!(target_os = "macos"),
        })
    }
}

/// Apply the precedence rules documented at the module level and enforce the
/// kernel's path-length limit.
pub fn resolve_socket_path(inputs: &SocketPathInputs) -> Result<PathBuf, DaemonSocketError> {
    let path = if let Some(explicit) = inputs.explicit.clone() {
        explicit
    } else if let Some(home) = inputs.codewhale_home_override.clone() {
        home.join("run").join(DAEMON_SOCKET_FILE_NAME)
    } else if let Some(runtime_dir) = inputs.xdg_runtime_dir.clone() {
        runtime_dir.join("codewhale").join(DAEMON_SOCKET_FILE_NAME)
    } else {
        let user_home = inputs
            .user_home
            .clone()
            .ok_or(DaemonSocketError::RuntimeDirUnavailable)?;
        if inputs.macos {
            user_home
                .join("Library")
                .join("Application Support")
                .join("codewhale")
                .join(DAEMON_SOCKET_FILE_NAME)
        } else {
            user_home
                .join(crate::CODEWHALE_APP_DIR)
                .join("run")
                .join(DAEMON_SOCKET_FILE_NAME)
        }
    };
    let len = path.as_os_str().len();
    if len > MAX_SOCKET_PATH_BYTES {
        return Err(DaemonSocketError::PathTooLong {
            path,
            len,
            max: MAX_SOCKET_PATH_BYTES,
        });
    }
    Ok(path)
}

/// The socket path this host would use with no explicit override.
#[cfg(unix)]
pub fn default_socket_path() -> Result<PathBuf, DaemonSocketError> {
    resolve_socket_path(&SocketPathInputs::from_environment(None)?)
}

/// Digest bytes that name a derived socket. Six bytes (twelve hex characters,
/// the same digest length the runtime-boundary id uses) make the basename a
/// fixed 23 bytes with the `store-` prefix and the `.sock` suffix, which keeps
/// the hard kernel path limit reachable only for an already-close default
/// endpoint instead of consuming the remaining headroom.
#[cfg(unix)]
const DERIVED_STORE_HASH_BYTES: usize = 6;

/// The control endpoint for a selected runtime store: the well-known default
/// socket when the store is the default selection, and a private sibling
/// derived from the store root otherwise.
///
/// One control endpoint per runtime store, as a pure function of the store
/// selection. A client that selects a non-default store (`CODEWHALE_RUNTIME_DIR`,
/// e.g. the VS Code extension's per-workspace store) elects its own owner
/// instead of colliding with the default store's owner on the shared socket;
/// the default store keeps the historical path, so discovery, `doctor` and
/// single-engine setups are unchanged.
///
/// `store_root` is the canonicalized runtime store root the caller selected;
/// `None` means "the default selection" (no override in effect).
///
/// Unix only: a Windows owner endpoint is a named pipe.
#[cfg(unix)]
pub fn store_selected_socket_path(
    store_root: Option<&Path>,
    inputs: &SocketPathInputs,
) -> Result<PathBuf, DaemonSocketError> {
    use anyhow::Context as _;
    use sha2::Digest as _;

    let base = resolve_socket_path(inputs)?;
    let Some(store_root) = store_root else {
        return Ok(base);
    };
    let mut digest = sha2::Sha256::new();
    // The tag keeps this name from colliding with any other use of the store
    // root's digest in the same directory.
    digest.update(b"Codewhale/store-socket/v1\0");
    digest.update(store_root.as_os_str().as_encoded_bytes());
    let suffix: String = digest.finalize()[..DERIVED_STORE_HASH_BYTES]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let parent = base
        .parent()
        .context("default daemon socket has no parent directory")
        .map_err(DaemonSocketError::State)?;
    let derived = parent.join(format!("store-{suffix}.sock"));
    let len = derived.as_os_str().len();
    if len > MAX_SOCKET_PATH_BYTES {
        return Err(DaemonSocketError::PathTooLong {
            path: derived,
            len,
            max: MAX_SOCKET_PATH_BYTES,
        });
    }
    Ok(derived)
}

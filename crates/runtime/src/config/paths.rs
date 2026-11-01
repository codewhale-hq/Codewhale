//! Filesystem path resolution helpers for config/cache/workspace locations.
//!
//! Pure path-building helpers extracted verbatim from `config.rs`. They depend
//! only on `std`, `codewhale-paths`, and `shellexpand` plus one another, so they
//! form a clean leaf. `config.rs` pulls them back in (`use paths::{...}`) for the
//! workspace-trust and config-loading logic that stays there, and re-exports
//! the `pub(crate)` entry point (`expand_path`) so
//! external `crate::config::` callers resolve unchanged (#3311).
//!
//! Visibility note: helpers that were file-private `fn` in `config.rs` are
//! `pub(crate)` here purely so the parent module can name them; none are
//! re-exported publicly, so the crate's external surface is unchanged.

use std::path::{Path, PathBuf};

pub fn default_config_path() -> anyhow::Result<PathBuf> {
    try_default_config_path()
}

pub(crate) fn try_default_config_path() -> anyhow::Result<PathBuf> {
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    {
        with_test_state_path(try_default_config_path_from_environment, || {
            Ok(crate::test_support::unsealed_test_state_root()
                .join(codewhale_config::CONFIG_FILE_NAME))
        })
    }

    #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
    try_default_config_path_from_environment()
}

fn try_default_config_path_from_environment() -> anyhow::Result<PathBuf> {
    codewhale_config::resolve_config_path(None)
}

/// Holding [`lock_test_env`] is not enough to read the process environment:
/// many tests take that lock only to serialize unrelated variables, and
/// trusting it routed them at a populated `~/.codewhale/config.toml` (#5355,
/// #5359). Settings already requires a sealed `EnvVarGuard`; config paths
/// must use the same gate.
#[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
fn with_test_state_path<T>(
    from_environment: impl FnOnce() -> T,
    isolated: impl FnOnce() -> T,
) -> T {
    let honor_guarded_environment = crate::test_support::guarded_environment_provides_state_paths();
    crate::test_support::with_test_env_lock(|| {
        if honor_guarded_environment {
            from_environment()
        } else {
            isolated()
        }
    })
}

pub(crate) fn codewhale_home_dir() -> Result<Option<PathBuf>, codewhale_paths::PathOverrideError> {
    codewhale_paths::codewhale_home_override()
}

/// The user-global config document: `$CODEWHALE_HOME/config.toml` when an
/// explicit home is set, otherwise `~/.codewhale/config.toml` (falling back to
/// the legacy `~/.deepseek/config.toml` only when that file already exists).
///
/// Credential writes are rerouted here when the ambient config path resolves
/// to a workspace-scoped document (#5045, #5193); non-credential settings keep
/// the ambient scoping.
pub fn home_config_path() -> Option<PathBuf> {
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    {
        with_test_state_path(home_config_path_from_environment, || {
            Some(
                crate::test_support::unsealed_test_state_root()
                    .join(codewhale_config::CONFIG_FILE_NAME),
            )
        })
    }

    #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
    home_config_path_from_environment()
}

/// Compare the physical user-global document using the same parent path
/// normalization and file-symlink rejection as ConfigStore.
pub fn is_home_config_path(path: &Path) -> bool {
    let Some(home) = home_config_path() else {
        return false;
    };
    match (
        codewhale_config::resolve_config_path(Some(home)),
        codewhale_config::resolve_config_path(Some(path.to_path_buf())),
    ) {
        (Ok(home), Ok(path)) => home == path,
        _ => false,
    }
}

fn home_config_path_from_environment() -> Option<PathBuf> {
    match codewhale_home_dir() {
        Ok(Some(home)) => return Some(home.join(codewhale_config::CONFIG_FILE_NAME)),
        Ok(None) => {}
        Err(error) => {
            tracing::error!(
                error = %error,
                "invalid Codewhale home override; refusing to substitute a different config path"
            );
            return None;
        }
    }

    codewhale_paths::user_home().map(|home| {
        let primary = home.join(".codewhale").join("config.toml");
        if primary.exists() {
            return primary;
        }
        let legacy = home.join(".deepseek").join("config.toml");
        if legacy.exists() {
            return legacy;
        }
        primary
    })
}

pub(crate) fn workspace_config_key(workspace: &Path) -> String {
    canonicalize_or_keep(workspace)
        .to_string_lossy()
        .into_owned()
}

pub fn canonicalize_or_keep(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub(crate) fn env_config_path() -> Result<Option<PathBuf>, codewhale_paths::PathOverrideError> {
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    {
        with_test_state_path(env_config_path_unlocked, || Ok(None))
    }
    #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
    {
        env_config_path_unlocked()
    }
}

fn env_config_path_unlocked() -> Result<Option<PathBuf>, codewhale_paths::PathOverrideError> {
    codewhale_paths::config_path_override()
}

pub(crate) fn expand_pathbuf(path: PathBuf) -> PathBuf {
    if let Some(raw) = path.to_str() {
        return expand_path(raw);
    }
    path
}

pub(crate) fn default_managed_config_path() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        Some(PathBuf::from("/etc/deepseek/managed_config.toml"))
    }
    #[cfg(not(unix))]
    {
        codewhale_paths::user_home().map(|home| {
            let primary = home.join(".codewhale").join("managed_config.toml");
            if primary.exists() {
                return primary;
            }
            home.join(".deepseek").join("managed_config.toml")
        })
    }
}

pub(crate) fn default_requirements_path() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        Some(PathBuf::from("/etc/deepseek/requirements.toml"))
    }
    #[cfg(not(unix))]
    {
        codewhale_paths::user_home().map(|home| {
            let primary = home.join(".codewhale").join("requirements.toml");
            if primary.exists() {
                return primary;
            }
            home.join(".deepseek").join("requirements.toml")
        })
    }
}

pub fn expand_path(path: &str) -> PathBuf {
    if let Some(stripped) = path.strip_prefix('~')
        && (stripped.is_empty() || stripped.starts_with('/') || stripped.starts_with('\\'))
        && let Some(mut home) = codewhale_paths::user_home()
    {
        let suffix = stripped.trim_start_matches(['/', '\\']);
        if !suffix.is_empty() {
            home.push(suffix);
        }
        return home;
    }

    let expanded = shellexpand::tilde(path);
    PathBuf::from(expanded.as_ref())
}

pub fn default_skills_dir() -> Option<PathBuf> {
    default_user_state_path("skills")
}

pub fn owned_skills_dir() -> Option<PathBuf> {
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    if !crate::test_support::guarded_environment_provides_state_paths() {
        return default_skills_dir();
    }
    codewhale_paths::codewhale_home()
        .ok()
        .flatten()
        .map(|home| home.join("skills"))
}

pub(crate) fn default_mcp_config_path() -> Option<PathBuf> {
    default_user_state_path("mcp.json")
}

pub(crate) fn default_notes_path() -> Option<PathBuf> {
    default_user_state_path("notes.txt")
}

pub(crate) fn default_memory_path() -> Option<PathBuf> {
    default_user_state_path("memory.md")
}

fn default_user_state_path(name: &str) -> Option<PathBuf> {
    #[cfg(any(test, all(feature = "test-thread-scoped-state", debug_assertions)))]
    {
        with_test_state_path(
            || default_user_state_path_from_environment(name),
            || Some(crate::test_support::unsealed_test_state_root().join(name)),
        )
    }

    #[cfg(not(any(test, all(feature = "test-thread-scoped-state", debug_assertions))))]
    default_user_state_path_from_environment(name)
}

fn default_user_state_path_from_environment(name: &str) -> Option<PathBuf> {
    match codewhale_home_dir() {
        Ok(Some(home)) => return Some(home.join(name)),
        Ok(None) => {}
        Err(error) => {
            tracing::error!(
                error = %error,
                "invalid Codewhale home override; refusing to substitute a different state root"
            );
            return None;
        }
    }
    codewhale_paths::user_home().map(|home| {
        let primary = home.join(".codewhale").join(name);
        if primary.exists() {
            return primary;
        }
        let legacy = home.join(".deepseek").join(name);
        if legacy.exists() {
            return legacy;
        }
        primary
    })
}

/// Resolve the directory pasted images should land in. Prefers
/// `~/.codewhale/clipboard-images/` so the path is stable across worktrees and
/// matches the location described in user-facing docs; falls back to
/// `<workspace>/clipboard-images/` if the home dir is unavailable.
pub fn clipboard_images_dir(workspace: &Path) -> PathBuf {
    let home = codewhale_paths::user_home();
    clipboard_images_dir_for_home(workspace, home.as_deref())
}

pub fn clipboard_images_dir_for_home(workspace: &Path, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        return home.join(".codewhale").join("clipboard-images");
    }
    workspace.join("clipboard-images")
}

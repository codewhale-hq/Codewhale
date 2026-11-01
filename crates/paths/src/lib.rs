//! Canonical user-scoped runtime path resolution for Codewhale.
//!
//! This leaf crate owns only the environment and platform-home decision. File
//! migration and per-subsystem fallback remain with the crate that owns those
//! files.
#![deny(missing_docs)]

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

/// Canonical Codewhale app directory name under the user home.
pub const CODEWHALE_APP_DIR: &str = ".codewhale";

/// Legacy DeepSeek-branded directory retained for compatibility reads.
pub const LEGACY_APP_DIR: &str = ".deepseek";

/// An environment-provided runtime path was not safe to use as a global path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathOverrideError {
    variable: &'static str,
    path: PathBuf,
    kind: PathOverrideErrorKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PathOverrideErrorKind {
    Relative,
    HomeUnavailable,
}

impl fmt::Display for PathOverrideError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            PathOverrideErrorKind::Relative => write!(
                formatter,
                "{} must be an absolute path, got {}",
                self.variable,
                self.path.display()
            ),
            PathOverrideErrorKind::HomeUnavailable => write!(
                formatter,
                "{} uses '~', but the user home directory could not be resolved: {}",
                self.variable,
                self.path.display()
            ),
        }
    }
}

impl std::error::Error for PathOverrideError {}

/// Return the explicit Codewhale home override, if one is configured.
///
/// Unicode values are trimmed so whitespace-only values are treated as unset,
/// matching the existing config and secret-store contract. Non-Unicode path
/// values are preserved on platforms that support them instead of silently
/// dropping an otherwise valid filesystem path. A leading `~` is expanded;
/// every other relative value is rejected.
pub fn codewhale_home_override() -> Result<Option<PathBuf>, PathOverrideError> {
    absolute_path_env("CODEWHALE_HOME")
}

/// Whether `CODEWHALE_HOME` establishes an explicit isolation boundary.
#[must_use]
pub fn codewhale_home_is_explicit() -> bool {
    path_env("CODEWHALE_HOME").is_some()
}

/// Return the legacy `DEEPSEEK_HOME` compatibility override, if configured.
///
/// New state must use [`codewhale_home`]. This resolver exists only for readers
/// whose persisted format still explicitly supports the legacy environment
/// alias.
#[must_use]
pub fn legacy_deepseek_home_override() -> Option<PathBuf> {
    path_env("DEEPSEEK_HOME")
}

/// Resolve the user's platform home, preferring `HOME` before `USERPROFILE`.
///
/// The explicit environment order makes CLI, state, config, and secret paths
/// deterministic in hermetic shells. On Windows, `HOMEDRIVE` plus `HOMEPATH`
/// remains a compatibility fallback before the platform resolver. The platform
/// resolver remains last for ordinary desktop launches without those variables.
///
/// Only an absolute candidate is a home. A relative `HOME` (`HOME=.`,
/// `HOME=build`) would otherwise resolve against the process's working
/// directory and move config, state, and secrets into whatever repository the
/// process runs in. [`codewhale_home`] names that case as an error.
#[must_use]
pub fn user_home() -> Option<PathBuf> {
    // An explicit invalid home is an error, not permission to fall back to
    // the ambient Windows profile and escape a caller's isolation boundary.
    if let Some(home) = path_env("HOME") {
        return home.is_absolute().then_some(home);
    }
    path_env("USERPROFILE")
        .filter(|path| path.is_absolute())
        .or_else(windows_home_from_environment)
        .or_else(dirs::home_dir)
        .filter(|path| path.is_absolute())
}

#[cfg(windows)]
fn windows_home_from_environment() -> Option<PathBuf> {
    let mut path = path_env("HOMEDRIVE")?;
    path.push(path_env("HOMEPATH")?);
    (!path.as_os_str().is_empty()).then_some(path)
}

#[cfg(not(windows))]
fn windows_home_from_environment() -> Option<PathBuf> {
    None
}

/// Resolve the canonical Codewhale runtime home.
///
/// A valid explicit `CODEWHALE_HOME` is returned after `~` expansion. Otherwise
/// this is `<user home>/.codewhale`.
pub fn codewhale_home() -> Result<Option<PathBuf>, PathOverrideError> {
    if let Some(home) = codewhale_home_override()? {
        return Ok(Some(home));
    }
    if let Some(home) = user_home() {
        return Ok(Some(home.join(CODEWHALE_APP_DIR)));
    }
    // No absolute home anywhere: when that is because `HOME` is relative, say
    // so rather than reporting a missing home.
    match path_env("HOME").filter(|path| !path.is_absolute()) {
        Some(path) => Err(PathOverrideError {
            variable: "HOME",
            path,
            kind: PathOverrideErrorKind::Relative,
        }),
        None => Ok(None),
    }
}

/// Return the explicit config-file override, preferring the Codewhale name.
///
/// `~` is expanded through the canonical user-home resolver before the path is
/// validated. All other relative paths are rejected so a process working in a
/// repository can never turn a global config override into a repo-local file.
pub fn config_path_override() -> Result<Option<PathBuf>, PathOverrideError> {
    if let Some(path) = absolute_path_env("CODEWHALE_CONFIG_PATH")? {
        return Ok(Some(path));
    }
    absolute_path_env("DEEPSEEK_CONFIG_PATH")
}

/// Read an optional path environment variable and require a global path.
///
/// Empty and whitespace-only values are treated as unset. A leading `~` path
/// is expanded first; any path still relative after expansion is rejected.
pub fn absolute_path_env(variable: &'static str) -> Result<Option<PathBuf>, PathOverrideError> {
    path_env(variable)
        .map(|path| validate_absolute_path(variable, path))
        .transpose()
}

/// Expand a leading `~` and reject a path that is not absolute.
pub fn validate_absolute_path(
    variable: &'static str,
    path: PathBuf,
) -> Result<PathBuf, PathOverrideError> {
    let original = path.clone();
    let path = match path.to_str() {
        Some("~") => user_home().ok_or_else(|| PathOverrideError {
            variable,
            path: original.clone(),
            kind: PathOverrideErrorKind::HomeUnavailable,
        })?,
        Some(value)
            if value
                .strip_prefix('~')
                .is_some_and(|suffix| suffix.starts_with('/') || suffix.starts_with('\\')) =>
        {
            let mut home = user_home().ok_or_else(|| PathOverrideError {
                variable,
                path: original.clone(),
                kind: PathOverrideErrorKind::HomeUnavailable,
            })?;
            let suffix = value[1..].trim_start_matches(['/', '\\']);
            if !suffix.is_empty() {
                home.push(suffix);
            }
            home
        }
        _ => path,
    };

    if path.is_absolute() {
        Ok(path)
    } else {
        Err(PathOverrideError {
            variable,
            path: original,
            kind: PathOverrideErrorKind::Relative,
        })
    }
}

/// Resolve the ambient legacy DeepSeek home used for compatibility reads.
///
/// This never follows `CODEWHALE_HOME`: callers must suppress legacy fallback
/// whenever [`codewhale_home_is_explicit`] is true.
#[must_use]
pub fn legacy_deepseek_home() -> Option<PathBuf> {
    user_home().map(|home| home.join(LEGACY_APP_DIR))
}

fn path_env(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).and_then(normalize_path_value)
}

fn normalize_path_value(value: OsString) -> Option<PathBuf> {
    if value.is_empty() {
        return None;
    }
    match value.to_str() {
        Some(value) => {
            let value = value.trim();
            (!value.is_empty()).then(|| PathBuf::from(value))
        }
        None => Some(PathBuf::from(value)),
    }
}

/// Strip a trailing `.worktrees/<name>` segment so all worktrees of the same
/// checkout share a `project_hash`. If the path doesn't look like a worktree
/// it's returned unchanged.
#[must_use]
pub fn strip_worktree_suffix(path: &Path) -> PathBuf {
    let mut components: Vec<_> = path.components().collect();
    if components.len() >= 2
        && let Some(parent) = components.get(components.len() - 2)
        && parent.as_os_str() == ".worktrees"
    {
        components.truncate(components.len() - 2);
        let mut p = PathBuf::new();
        for c in components {
            p.push(c.as_os_str());
        }
        return p;
    }
    path.to_path_buf()
}

/// Hex-encoded deterministic FNV-1a digest. This is only a directory tag, not
/// a security boundary, but it must remain stable across process launches.
///
/// Lives here, below both `codewhale-snapshot` and `codewhale-runtime`, because
/// each derives its own on-disk layout from the same project/worktree split.
/// Two copies would be two things to keep in step.
#[must_use]
pub fn stable_hex(path: &Path) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_path_values_are_trimmed_and_whitespace_is_unset() {
        assert_eq!(
            normalize_path_value(OsString::from("  /tmp/codewhale  ")),
            Some(PathBuf::from("/tmp/codewhale"))
        );
        assert_eq!(normalize_path_value(OsString::from(" \t\n ")), None);
        assert_eq!(normalize_path_value(OsString::new()), None);
    }

    #[test]
    fn relative_global_overrides_are_rejected_with_the_variable_name() {
        let error = validate_absolute_path(
            "CODEWHALE_CONFIG_PATH",
            PathBuf::from(".codewhale/config.toml"),
        )
        .expect_err("relative global config path must fail closed");
        let message = error.to_string();
        assert!(message.contains("CODEWHALE_CONFIG_PATH"), "{message}");
        assert!(message.contains(".codewhale/config.toml"), "{message}");
        assert!(message.contains("absolute"), "{message}");
    }

    #[test]
    fn absolute_global_overrides_are_preserved() {
        let path = if cfg!(windows) {
            PathBuf::from(r"C:\codewhale\config.toml")
        } else {
            PathBuf::from("/tmp/codewhale/config.toml")
        };
        assert_eq!(
            validate_absolute_path("CODEWHALE_CONFIG_PATH", path.clone()),
            Ok(path)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_non_unicode_path_values_are_preserved() {
        use std::os::unix::ffi::OsStringExt;

        let value = OsString::from_vec(b"codewhale-\xff-home".to_vec());
        assert_eq!(
            normalize_path_value(value.clone()),
            Some(PathBuf::from(value))
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_non_unicode_path_values_are_preserved() {
        use std::os::windows::ffi::OsStringExt;

        let value = OsString::from_wide(&[b'C' as u16, b':' as u16, b'\\' as u16, 0xd800]);
        assert_eq!(
            normalize_path_value(value.clone()),
            Some(PathBuf::from(value))
        );
    }
}

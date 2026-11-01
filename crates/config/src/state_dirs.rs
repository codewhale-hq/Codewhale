use crate::config_migration::default_config_path;
use crate::external_credentials::quote_os_path;
use crate::path_io::{
    checked_path_exists, normalize_config_file_path, normalize_project_workspace,
};
use crate::provider_toml::CONFIG_FILE_NAME;
use anyhow::Context as _;
use anyhow::{Result, bail};
use codewhale_paths::{CODEWHALE_APP_DIR, LEGACY_APP_DIR};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Resolve the primary CodeWhale home directory.
///
/// `$CODEWHALE_HOME` takes precedence when set. Otherwise defaults to
/// `$HOME/.codewhale`. This is the write target for new product state.
pub fn codewhale_home() -> Result<PathBuf> {
    codewhale_paths::codewhale_home()
        .map_err(anyhow::Error::new)?
        .context("failed to resolve home directory")
}

/// Whether `$CODEWHALE_HOME` is set to a non-empty value.
///
/// An explicit CodeWhale home is an isolation boundary: state/config resolvers
/// must not fall back to ambient legacy `~/.deepseek` data outside that root.
pub fn codewhale_home_is_explicit() -> bool {
    codewhale_paths::codewhale_home_is_explicit()
}

/// Resolve the legacy DeepSeek home directory (`$HOME/.deepseek`).
///
/// Always returns the legacy path regardless of whether it exists.
pub fn legacy_deepseek_home() -> Result<PathBuf> {
    codewhale_paths::legacy_deepseek_home().context("failed to resolve home directory")
}

/// Reject state subdirs that could escape the state root via path injection.
///
/// `ensure_state_dir` / `resolve_state_dir` are public APIs taking an arbitrary
/// subdir string; every in-tree caller passes a hardcoded single component
/// (e.g. `"sessions"`, `"."`). This validates defensively so a future caller
/// can never traverse out of the state root via `..` components or an absolute
/// path. Nested relative paths such as `"a/b"` are permitted.
pub(crate) fn ensure_safe_state_subdir(subdir: &str) -> Result<()> {
    if subdir.is_empty() {
        bail!("state subdir must not be empty");
    }
    let path = std::path::Path::new(subdir);
    if path.is_absolute() {
        bail!("state subdir must not be an absolute path: {subdir}");
    }
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::RootDir | std::path::Component::Prefix(_)
        )
    }) {
        bail!("state subdir must not contain a root or prefix: {subdir}");
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("state subdir must not contain parent-dir (..) components: {subdir}");
    }
    Ok(())
}

/// Resolve a state subdirectory, preferring the CodeWhale root if
/// it already exists, otherwise falling back to the legacy root.
///
/// This is the read-path resolver: it returns the primary path when
/// migration has occurred or on a fresh install, but keeps reading
/// from the legacy path for users who haven't migrated yet.
pub fn resolve_state_dir(subdir: &str) -> Result<PathBuf> {
    ensure_safe_state_subdir(subdir)?;
    let explicit_codewhale_home = codewhale_home_is_explicit();
    let primary = codewhale_home()?.join(subdir);
    if explicit_codewhale_home || primary.exists() {
        return Ok(primary);
    }
    let legacy = legacy_deepseek_home()?.join(subdir);
    if legacy.exists() {
        return Ok(legacy);
    }
    // Neither exists — return primary for first-write creation.
    Ok(primary)
}

/// Ensure a state subdirectory exists under the primary CodeWhale root,
/// creating it if necessary. This is the write-path resolver.
///
/// On the first creation of a real subdirectory (not the root sentinel `"."`),
/// if a legacy `~/.deepseek/<subdir>` exists but the primary
/// `~/.codewhale/<subdir>` does not, the legacy directory is relocated into
/// the primary location so the user keeps their data and the legacy tree
/// stops growing (#3240). After migration, [`resolve_state_dir`] finds the
/// data in the primary location; the read resolver itself is unchanged.
pub fn ensure_state_dir(subdir: &str) -> Result<PathBuf> {
    let (dir, migration) = ensure_state_dir_with_migration(subdir)?;
    if let Some(migration) = migration {
        eprintln!("{}", migration.user_notice());
    }
    Ok(dir)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateMigrationKind {
    Relocated,
    Copied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateMigration {
    pub subdir: String,
    pub legacy_path: PathBuf,
    pub primary_path: PathBuf,
    pub kind: StateMigrationKind,
}

impl StateMigration {
    pub fn user_notice(&self) -> String {
        let action = match self.kind {
            StateMigrationKind::Relocated => "relocated",
            StateMigrationKind::Copied => "copied",
        };
        let legacy_detail = match self.kind {
            StateMigrationKind::Relocated => {
                "The legacy .deepseek copy for this state path was removed by the move."
            }
            StateMigrationKind::Copied => {
                "The legacy .deepseek copy was left in place because a direct move failed."
            }
        };

        format!(
            "Codewhale migrated legacy state ({action}):\n  {} -> {}\nYour data was preserved. Use .codewhale as the canonical state location from now on.\n{legacy_detail}\nIf no other apps use it, you can remove the legacy .deepseek tree after confirming everything looks right.",
            self.legacy_path.display(),
            self.primary_path.display(),
        )
    }
}

/// Variant of [`ensure_state_dir`] that exposes whether a legacy state path was
/// migrated. Most callers should use [`ensure_state_dir`]; this is kept for
/// tests and future UI surfaces that want to render the notice themselves.
pub fn ensure_state_dir_with_migration(subdir: &str) -> Result<(PathBuf, Option<StateMigration>)> {
    ensure_safe_state_subdir(subdir)?;
    let explicit_codewhale_home = codewhale_home_is_explicit();
    let dir = codewhale_home()?.join(subdir);
    let migration = if explicit_codewhale_home {
        None
    } else {
        match migrate_legacy_state_dir(&dir, subdir)? {
            LegacyStateMigration::NotNeeded => None,
            LegacyStateMigration::Migrated(migration) => Some(migration),
            // Creating an empty primary here would make it authoritative and
            // hide the legacy data for good. Keep using the legacy directory
            // (the read resolver does the same) and retry on the next call.
            LegacyStateMigration::Failed { legacy } => return Ok((legacy, None)),
        }
    };
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}/", dir.display()))?;
    Ok((dir, migration))
}

pub(crate) enum LegacyStateMigration {
    NotNeeded,
    Migrated(StateMigration),
    /// Neither the move nor the copy completed. The primary was left absent,
    /// so the legacy directory stays authoritative and migration retries.
    Failed {
        legacy: PathBuf,
    },
}

/// One-time relocation of a legacy `~/.deepseek/<subdir>` state directory into
/// the primary `~/.codewhale/<subdir>` location (#3240). No-op once the primary
/// exists, for the root sentinel `"."` (a whole-tree move is owned by the
/// config-file migration), or when no legacy directory is present.
pub(crate) fn migrate_legacy_state_dir(
    primary: &Path,
    subdir: &str,
) -> Result<LegacyStateMigration> {
    if primary.exists() || subdir == "." || subdir.is_empty() {
        return Ok(LegacyStateMigration::NotNeeded);
    }
    let legacy = match legacy_deepseek_home() {
        Ok(home) => home.join(subdir),
        Err(_) => return Ok(LegacyStateMigration::NotNeeded),
    };
    if !legacy.exists() {
        return Ok(LegacyStateMigration::NotNeeded);
    }
    // The primary's parent (the ~/.codewhale root) must exist for the rename.
    if let Some(parent) = primary.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            target: "config::migration",
            "Could not create {} for state migration ({}); writing to primary anyway",
            parent.display(),
            err
        );
    }
    match std::fs::rename(&legacy, primary) {
        Ok(()) => {
            tracing::info!(
                target: "config::migration",
                "Migrated legacy state directory {} -> {} (relocated). The .deepseek copy was removed.",
                legacy.display(),
                primary.display()
            );
            return Ok(LegacyStateMigration::Migrated(StateMigration {
                subdir: subdir.to_string(),
                legacy_path: legacy,
                primary_path: primary.to_path_buf(),
                kind: StateMigrationKind::Relocated,
            }));
        }
        Err(err) => {
            // Cross-device rename or permission issue: fall back to a
            // recursive copy so the user keeps their data. The legacy tree is
            // left in place; it stops growing because writes now target the
            // primary path.
            match copy_dir_into_place(&legacy, primary) {
                Ok(()) => {
                    tracing::info!(
                        target: "config::migration",
                        "Migrated legacy state directory {} -> {} (copied; rename failed: {err}). \
                         The legacy .deepseek copy was left in place.",
                        legacy.display(),
                        primary.display()
                    );
                    return Ok(LegacyStateMigration::Migrated(StateMigration {
                        subdir: subdir.to_string(),
                        legacy_path: legacy,
                        primary_path: primary.to_path_buf(),
                        kind: StateMigrationKind::Copied,
                    }));
                }
                Err(copy_err) => {
                    // A concurrent process may have completed the move/copy
                    // after our initial check. Use that completed primary,
                    // rather than resuming writes to the legacy tree.
                    if primary.is_dir() {
                        return Ok(LegacyStateMigration::NotNeeded);
                    }
                    tracing::warn!(
                        target: "config::migration",
                        "Could not migrate legacy state {} -> {} (rename: {err}; copy: {copy_err:#}). \
                         The legacy path stays in use and migration retries next time.",
                        legacy.display(),
                        primary.display()
                    );
                }
            }
        }
    }
    Ok(LegacyStateMigration::Failed { legacy })
}

/// Copy `src` to a staging sibling of `dst` and rename it into place, so a
/// copy that fails midway never leaves a partial `dst` that later runs would
/// treat as a completed migration.
pub(crate) fn copy_dir_into_place(src: &Path, dst: &Path) -> Result<()> {
    let mut staging_name = OsString::from(".");
    staging_name.push(dst.file_name().unwrap_or_default());
    staging_name.push(".migrating-");
    // Each attempt owns only its unique private directory. A fixed sibling
    // could be another process's active copy and must never be cleared.
    let staging = tempfile::Builder::new().prefix(&staging_name).tempdir_in(
        dst.parent()
            .context("migration destination has no parent")?,
    )?;
    copy_dir_recursive(src, staging.path())?;
    std::fs::rename(staging.path(), dst).with_context(|| {
        format!(
            "failed to move {} into place at {}",
            staging.path().display(),
            dst.display()
        )
    })
}

/// Recursively copy a directory tree from `src` to `dst`, creating `dst`.
/// Symlinks and other non-file/non-dir entries are skipped (rare in state dirs).
pub(crate) fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in
        std::fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in {}", src.display()))?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read file type for {}", path.display()))?;
        if file_type.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else if file_type.is_file() {
            std::fs::copy(&path, &target).with_context(|| {
                format!("failed to copy {} -> {}", path.display(), target.display())
            })?;
        }
    }
    Ok(())
}

/// Resolve a project-local state subdirectory, preferring `.codewhale/`
/// when it exists, falling back to `.deepseek/` for legacy projects.
///
/// Returns `(true, path)` when the primary `.codewhale/` path is used,
/// `(false, path)` for the legacy fallback. The boolean helps callers
/// emit a deprecation notice on legacy paths.
pub fn resolve_project_state_dir(workspace: &Path, subdir: &str) -> Result<(bool, PathBuf)> {
    ensure_safe_state_subdir(subdir)?;
    let workspace = normalize_project_workspace(workspace)?;
    let primary = workspace.join(CODEWHALE_APP_DIR).join(subdir);
    if primary.exists() {
        return Ok((true, primary));
    }
    let legacy = workspace.join(LEGACY_APP_DIR).join(subdir);
    Ok((false, legacy))
}

/// Ensure a project-local state subdirectory exists under `.codewhale/`,
/// creating it if necessary. Returns the directory path.
pub fn ensure_project_state_dir(workspace: &Path, subdir: &str) -> Result<PathBuf> {
    ensure_safe_state_subdir(subdir)?;
    let workspace = normalize_project_workspace(workspace)?;
    let dir = workspace.join(CODEWHALE_APP_DIR).join(subdir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}/", dir.display()))?;
    Ok(dir)
}

pub fn resolve_config_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return normalize_config_file_path(path);
    }
    if let Some(path) = codewhale_paths::config_path_override().map_err(anyhow::Error::new)? {
        return normalize_config_file_path(path);
    }
    default_config_path()
}

pub fn require_explicit_config_exists(explicit: Option<PathBuf>) -> Result<()> {
    if let Some(path) = explicit {
        let path = normalize_config_file_path(path)?;
        if !checked_path_exists(&path)? {
            bail!("config file does not exist: {}", quote_os_path(&path));
        }
    }
    Ok(())
}

/// Whether `path` names a workspace-scoped config document —
/// `<repo>/.codewhale/config.toml` (or the legacy `.deepseek` layout) inside a
/// checkout — rather than a user-global config file.
///
/// Credential writes (api_key values, `auth_mode` markers, oauth/external
/// credential pointers) must never target such a document: a key saved while
/// working in one repo would be invisible from every other repo, and the repo
/// file stores it in plaintext where it is easy to commit by accident (#5045,
/// #5193).
///
/// A path is classified workspace-scoped only when its parent directory is a
/// `.codewhale`/`.deepseek` app dir outside the user's home AND the document
/// belongs to a workspace: it is relative (resolves against the process cwd),
/// its base directory contains the process cwd, or its base directory is a
/// checkout (has a `.git` entry). An explicit `$CODEWHALE_HOME` config is
/// user-global wherever that home points, even when the directory itself
/// happens to be named `.codewhale`; other custom locations (for example
/// `CODEWHALE_CONFIG_PATH=~/team.toml` or an isolated test directory) stay
/// honored as deliberate user-scoped choices.
#[must_use]
pub fn config_path_is_workspace_scoped(path: &Path) -> bool {
    config_path_is_workspace_scoped_with_context(
        path,
        codewhale_paths::codewhale_home_override()
            .ok()
            .flatten()
            .as_deref(),
        codewhale_paths::user_home().as_deref(),
        std::env::current_dir().ok().as_deref(),
    )
}

/// Environment-free core of [`config_path_is_workspace_scoped`], split out so
/// scope classification is testable without mutating process-global state.
pub(crate) fn config_path_is_workspace_scoped_with_context(
    path: &Path,
    explicit_codewhale_home: Option<&Path>,
    user_home: Option<&Path>,
    current_dir: Option<&Path>,
) -> bool {
    if let Some(home) = explicit_codewhale_home
        && same_lexical_or_canonical_path(path, &home.join(CONFIG_FILE_NAME))
    {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    let parent_is_app_dir = parent
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name == CODEWHALE_APP_DIR || name == LEGACY_APP_DIR);
    if !parent_is_app_dir {
        return false;
    }
    let Some(base) = parent.parent() else {
        return true;
    };
    if let Some(home) = user_home
        && same_lexical_or_canonical_path(base, home)
    {
        return false;
    }
    if path.is_relative() {
        // Resolves against the process cwd: repo-scoped by construction.
        return true;
    }
    // The document belongs to the workspace the process is sitting in…
    if let Some(cwd) = current_dir
        && canonicalize_or_keep(cwd).starts_with(canonicalize_or_keep(base))
    {
        return true;
    }
    // …or to some other checkout (a `.git` entry beside the app dir).
    base.join(".git").exists()
}

/// Lexical equality first, canonical equality as a fallback so an existing
/// path still matches through symlinked parents (e.g. `/tmp` on macOS).
pub(crate) fn same_lexical_or_canonical_path(a: &Path, b: &Path) -> bool {
    a == b || canonicalize_or_keep(a) == canonicalize_or_keep(b)
}

pub(crate) fn canonicalize_or_keep(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod credential_scope_tests {
    use super::config_path_is_workspace_scoped_with_context;
    use std::path::Path;

    #[test]
    fn config_inside_current_workspace_is_workspace_scoped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let cwd = repo.join("nested/dir");
        for app_dir in [".codewhale", ".deepseek"] {
            let config = repo.join(app_dir).join("config.toml");
            assert!(
                config_path_is_workspace_scoped_with_context(
                    &config,
                    None,
                    Some(Path::new("/home/user")),
                    Some(&cwd),
                ),
                "{} should be workspace-scoped when cwd sits inside the repo",
                config.display()
            );
        }
    }

    #[test]
    fn relative_app_dir_config_is_workspace_scoped() {
        assert!(config_path_is_workspace_scoped_with_context(
            Path::new(".codewhale/config.toml"),
            None,
            Some(Path::new("/home/user")),
            Some(Path::new("/somewhere/else")),
        ));
    }

    #[test]
    fn checkout_config_outside_cwd_is_workspace_scoped_via_git_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git marker");
        std::fs::create_dir_all(repo.join(".codewhale")).expect("app dir");
        assert!(config_path_is_workspace_scoped_with_context(
            &repo.join(".codewhale/config.toml"),
            None,
            Some(Path::new("/home/user")),
            Some(Path::new("/somewhere/else")),
        ));
    }

    #[test]
    fn user_global_and_custom_locations_are_not_workspace_scoped() {
        let home = Path::new("/home/user");
        let elsewhere = Some(Path::new("/somewhere/else"));
        for global_config in [
            "/home/user/.codewhale/config.toml",
            "/home/user/.deepseek/config.toml",
            "/home/user/team-config.toml",
            "/etc/codewhale/config.toml",
        ] {
            assert!(
                !config_path_is_workspace_scoped_with_context(
                    Path::new(global_config),
                    None,
                    Some(home),
                    elsewhere,
                ),
                "{global_config} should stay user-global"
            );
        }
        // An isolated app-dir-shaped location with no workspace relationship
        // (no cwd ancestry, no checkout marker) stays honored: test harnesses
        // and deliberate overrides point there.
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(!config_path_is_workspace_scoped_with_context(
            &temp.path().join(".codewhale/config.toml"),
            None,
            Some(home),
            elsewhere,
        ));
    }

    #[test]
    fn explicit_codewhale_home_config_is_user_global_even_when_dir_is_app_named() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let explicit = repo.join(".codewhale");
        // Even with cwd inside the repo, the explicit CODEWHALE_HOME config is
        // the user-global scope by definition.
        assert!(!config_path_is_workspace_scoped_with_context(
            &explicit.join("config.toml"),
            Some(&explicit),
            Some(Path::new("/home/user")),
            Some(&repo),
        ));
        // A different repo-scoped document is still workspace-scoped.
        assert!(config_path_is_workspace_scoped_with_context(
            &repo.join("other/.codewhale/config.toml"),
            Some(&explicit),
            Some(Path::new("/home/user")),
            Some(&repo.join("other")),
        ));
    }
}

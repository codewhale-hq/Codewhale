//! TUI-owned host services for the session-export slice (FEAT-025 D5/D7).
//!
//! This module deliberately lives outside `commands/groups/session`, which
//! FEAT-043 moves into `codewhale-commands`. It owns the two filesystem
//! services the `/export` slice needs from the host:
//!
//! * the shared `last-copy.md` recovery writer reused by `/export` and `/copy`
//!   (D5), and
//! * the protected export-destination resolver/writer used only by `/export`
//!   (D7).
//!
//! The algorithm, check order, error text, and platform behavior are the exact
//! baseline implementations relocated unchanged. The portable `/export`
//! handler reaches these services only through
//! `CommandSessionExportContext` — never by importing this module — so the
//! future portable group keeps no host dependency.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

/// Write the export to a predictable last-copy file under the Codewhale home
/// (#5555): a clipboard-only export on SSH/headless must never dead-end the
/// user, so the same content lands at `<home>/exports/last-copy.md` and every
/// failure message names it. Returns the path when the write succeeded.
pub(crate) fn write_last_copy(markdown: &str) -> Option<PathBuf> {
    let home = codewhale_paths::codewhale_home().ok().flatten()?;
    let exports_dir = home.join("exports");
    std::fs::create_dir_all(&exports_dir).ok()?;
    let physical_home = std::fs::canonicalize(&home).ok()?;
    let physical_exports = std::fs::canonicalize(&exports_dir).ok()?;
    if !physical_exports.starts_with(&physical_home) {
        return None;
    }
    write_last_copy_to(&exports_dir, markdown).ok()
}

fn write_last_copy_to(exports_dir: &Path, markdown: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(exports_dir)?;
    let path = exports_dir.join("last-copy.md");
    // Reuse the private atomic writer: random same-directory temp names,
    // restrictive creation mode, symlink-safe replacement, and Windows
    // replace retries are all part of the existing persistence contract.
    codewhale_util::utils::write_atomic(&path, markdown.as_bytes())?;
    Ok(path)
}

/// Resolve a requested export destination against the trusted workspace root.
///
/// Verbatim baseline algorithm (D7): trim; empty → `export path is empty`;
/// reject `..` components; canonicalize the workspace with its current
/// fallback; rebase workspace-absolute paths (raw or canonicalized); otherwise
/// join workspace-relative; require a file name. Errors are returned unwrapped
/// so the portable handler keeps the exact baseline text.
pub(crate) fn resolve_export_path(workspace: &Path, raw: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("export path is empty".to_string());
    }
    let requested = PathBuf::from(raw);
    if requested
        .components()
        .any(|component| component == Component::ParentDir)
    {
        return Err(
            "export paths may not contain `..`; use an explicit normalized absolute path instead"
                .to_string(),
        );
    }
    // Resolve the trusted workspace root once so platform aliases such as
    // macOS `/var -> /private/var` do not make every workspace-relative
    // export look like it traverses a user-controlled symlink. Requested
    // components beneath that root remain lexical and are checked below.
    let resolved_workspace =
        fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let path = if requested.is_absolute() {
        if let Ok(relative) = requested.strip_prefix(workspace) {
            resolved_workspace.join(relative)
        } else if let Ok(relative) = requested.strip_prefix(&resolved_workspace) {
            resolved_workspace.join(relative)
        } else {
            requested
        }
    } else {
        resolved_workspace.join(requested)
    };
    if path.file_name().is_none() {
        return Err(format!("export path must name a file: {}", path.display()));
    }
    Ok(path)
}

/// Write rendered export bytes to a resolved destination with the exact
/// baseline protections (D7): parent presence/directory checks, symlink
/// rejection (leaf and ancestors), no overwrite by default, non-regular-file
/// rejection, exclusive creation with `0o600` on Unix, `fsync`, cleanup after a
/// failed write, forced atomic replacement, and owner-only permissions.
pub(crate) fn write_export_file(path: &Path, contents: &[u8], force: bool) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| format!("path has no parent directory: {}", path.display()))?;
    let parent_metadata = fs::metadata(parent).map_err(|err| {
        format!(
            "parent directory {} is unavailable: {err}",
            parent.display()
        )
    })?;
    if !parent_metadata.is_dir() {
        return Err(format!("parent is not a directory: {}", parent.display()));
    }
    reject_symlink_components(path)?;

    match fs::symlink_metadata(path) {
        Ok(_) if !force => {
            return Err(format!(
                "destination already exists: {}. Re-run with `/export file --force <path>` to replace it",
                path.display()
            ));
        }
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(format!(
                "refusing to replace a non-regular file: {}",
                path.display()
            ));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("could not inspect {}: {err}", path.display())),
    }

    if force {
        codewhale_util::utils::write_atomic(path, contents).map_err(|err| err.to_string())?;
        set_owner_only(path).map_err(|err| format!("could not secure file permissions: {err}"))?;
        return Ok(());
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::AlreadyExists {
            format!(
                "destination already exists: {}. Re-run with `/export file --force <path>` to replace it",
                path.display()
            )
        } else {
            err.to_string()
        }
    })?;
    if let Err(err) = file.write_all(contents).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(err.to_string());
    }
    set_owner_only(path).map_err(|err| format!("could not secure file permissions: {err}"))
}

fn reject_symlink_components(path: &Path) -> Result<(), String> {
    for component_path in path.ancestors() {
        match fs::symlink_metadata(component_path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "refusing export through symlink component: {}",
                    component_path.display()
                ));
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(format!(
                    "could not inspect path component {}: {err}",
                    component_path.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn last_copy_writes_the_export_without_leaving_a_temp_artifact() {
        let tmp = TempDir::new().expect("tempdir");
        let dir = tmp.path().join("exports");
        let path = write_last_copy_to(&dir, "# export\n\nhello\n").expect("write");
        assert_eq!(path, dir.join("last-copy.md"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "# export\n\nhello\n"
        );
        // The next export overwrites the same predictable path.
        write_last_copy_to(&dir, "# second\n").expect("rewrite");
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "# second\n");
        assert_eq!(
            std::fs::read_dir(&dir).expect("read exports").count(),
            1,
            "the atomic writer must not leave a temp artifact"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "recovery copy must remain private");
        }
    }

    #[test]
    fn last_copy_stays_inside_an_explicit_codewhale_home() {
        let ambient = TempDir::new().expect("ambient home");
        let isolated = TempDir::new().expect("isolated Codewhale home");
        let _env_lock = crate::test_support::lock_test_env();
        let _home = crate::test_support::EnvVarGuard::set("HOME", ambient.path());
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", isolated.path());

        let path = write_last_copy("isolated response").expect("recovery copy");

        assert_eq!(path, isolated.path().join("exports/last-copy.md"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("read recovery copy"),
            "isolated response"
        );
        assert!(
            !ambient.path().join("exports/last-copy.md").exists(),
            "explicit CODEWHALE_HOME must prevent ambient-home writes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn last_copy_refuses_an_exports_symlink_outside_codewhale_home() {
        use std::os::unix::fs::symlink;

        let ambient = TempDir::new().expect("ambient home");
        let isolated = TempDir::new().expect("isolated Codewhale home");
        let external = TempDir::new().expect("external dir");
        symlink(external.path(), isolated.path().join("exports")).expect("exports symlink");
        let _env_lock = crate::test_support::lock_test_env();
        let _home = crate::test_support::EnvVarGuard::set("HOME", ambient.path());
        let _codewhale_home =
            crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", isolated.path());

        assert_eq!(write_last_copy("must stay isolated"), None);
        assert!(
            !external.path().join("last-copy.md").exists(),
            "recovery content must not escape through a nested symlink"
        );
    }
}

//! Place a staged bundle at its final path, and the containment guards.
//!
//! [`finalize_install`] reserves a private unique backup before moving an
//! incumbent and writes `.installed-from` last. Failed publication restores
//! only into an unoccupied destination. After publication, an error retains
//! both the incumbent backup and the current path: without serialization we
//! cannot prove that path still names this operation's publication, so it is
//! never recursively deleted or overwritten as rollback.
//! [`ensure_target_within_plugins_dir`] mirrors discovery's fail-closed rule
//! that a bundle must resolve to a direct child of the plugins root.
//!
//! This is not a serialized or crash-atomic bundle swap. A retained recovery
//! path is not proof that the old bundle was restored or remains active.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use codewhale_skill_install::install::{
    self as skill_install, INSTALLED_FROM_MARKER, validate_skill_name_segment,
};

use super::stage::StagedPlugin;
use super::{InstalledPlugin, PluginInstallError, PluginInstallOutcome};

pub(super) fn finalize_install(
    staged: StagedPlugin,
    spec: &str,
    url: Option<&str>,
    source_checksum: &str,
    user_plugins_dir: &Path,
    update: bool,
) -> Result<PluginInstallOutcome> {
    let final_path = user_plugins_dir.join(&staged.name);
    let mut backup_container: Option<PathBuf> = None;
    if final_path.exists() {
        if !update {
            let has_marker = final_path.join(INSTALLED_FROM_MARKER).exists();
            let _ = fs::remove_dir_all(&staged.staged_path);
            if has_marker {
                return Err(PluginInstallError::AlreadyInstalled(staged.name).into());
            }
            return Err(PluginInstallError::NotInstalledHere(staged.name).into());
        }
        if !final_path.join(INSTALLED_FROM_MARKER).exists() {
            let _ = fs::remove_dir_all(&staged.staged_path);
            return Err(PluginInstallError::NotInstalledHere(staged.name).into());
        }
        let mut builder = tempfile::Builder::new();
        builder.prefix(".plugin-backup-").rand_bytes(16);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            builder.permissions(fs::Permissions::from_mode(0o700));
        }
        let reserved = builder
            .tempdir_in(user_plugins_dir)
            .context("failed to reserve a private plugin backup container")?;
        crate::registry::ensure_private_plugin_state_directory(reserved.path())
            .map_err(anyhow::Error::msg)?;
        // Persist before moving any user data: TempDir cleanup must never
        // delete the incumbent during an error return or unwind.
        let container = reserved.keep();
        let backup = container.join("bundle");
        if let Err(error) = crate::builtin::publish_snapshot(&final_path, &backup) {
            if let Err(cleanup) = fs::remove_dir(&container) {
                tracing::warn!(path = %container.display(), %cleanup, "empty plugin backup container could not be removed");
            }
            return Err(error).with_context(|| {
                format!(
                    "failed to backup existing plugin at {}",
                    final_path.display()
                )
            });
        }
        backup_container = Some(container);
    }
    if let Err(error) = crate::builtin::publish_snapshot(&staged.staged_path, &final_path) {
        if let Some(container) = backup_container.as_deref() {
            return Err(restore_backup(container, &final_path, error.into()));
        }
        return Err(error)
            .context("failed to install staged plugin; existing destination preserved");
    }

    // Discovery fail-closed rule: the installed bundle must canonicalize to a
    // direct child of the user plugins root.
    if let Err(error) = ensure_target_within_plugins_dir(&final_path, user_plugins_dir) {
        return Err(retained_publication_error(
            error,
            &final_path,
            backup_container.as_deref(),
        ));
    }

    // Write the marker last so a partial install never leaves a stale
    // `.installed-from` on disk.
    if let Err(error) = skill_install::write_installed_from_v2(
        &final_path,
        spec,
        url,
        source_checksum,
        &staged.content_hash,
        &staged.name,
    ) {
        return Err(retained_publication_error(
            error,
            &final_path,
            backup_container.as_deref(),
        ));
    }

    let installed_content_hash = match crate::agent_plugin::resolve_manifest_path(&final_path)
        .ok_or_else(|| anyhow::anyhow!("installed plugin has no supported manifest"))
        .and_then(|manifest_path| {
            crate::manifest::PluginManifest::validate_from_path(&manifest_path)
                .map(|validated| validated.content_hash)
                .map_err(anyhow::Error::msg)
        }) {
        Ok(hash) => hash,
        Err(error) => {
            return Err(retained_publication_error(
                error.context("installed plugin failed post-copy validation"),
                &final_path,
                backup_container.as_deref(),
            ));
        }
    };
    if let Some(container) = backup_container
        && let Err(error) = fs::remove_dir_all(&container)
    {
        // The new bundle is already installed and validated. Cleanup failure
        // does not undo it; retain the exact old-copy path in the diagnostic.
        tracing::warn!(path = %container.display(), %error, "plugin installed but prior backup cleanup failed; retained backup requires recovery cleanup");
    }

    Ok(PluginInstallOutcome::Installed(InstalledPlugin {
        name: staged.name,
        path: final_path,
        content_hash: staged.content_hash,
        installed_content_hash,
        source_checksum: source_checksum.to_string(),
    }))
}

/// A failed no-replace publication did not install our staged tree. Restore
/// with the same exclusive primitive, so an intervening creator is preserved.
pub(super) fn restore_backup(
    container: &Path,
    final_path: &Path,
    error: anyhow::Error,
) -> anyhow::Error {
    let backup = container.join("bundle");
    match crate::builtin::publish_snapshot(&backup, final_path) {
        Ok(()) => {
            if let Err(cleanup) = fs::remove_dir(container) {
                tracing::warn!(path = %container.display(), %cleanup, "restored plugin but empty recovery container could not be removed");
            }
            error.context(format!(
                "failed to install staged plugin; previous plugin restored at {}",
                final_path.display()
            ))
        }
        Err(restore) => error.context(format!(
            "failed to install staged plugin and automatic restoration failed: {restore}; \
             previous plugin retained at {}; current destination {} was not replaced; recovery required",
            backup.display(),
            final_path.display()
        )),
    }
}

fn retained_publication_error(
    error: anyhow::Error,
    final_path: &Path,
    backup_container: Option<&Path>,
) -> anyhow::Error {
    let previous = backup_container.map_or_else(
        || "no previous plugin backup exists".to_string(),
        |container| {
            format!(
                "previous plugin retained at {}",
                container.join("bundle").display()
            )
        },
    );
    error.context(format!(
        "plugin publication failed validation; {previous}; current destination {} retained; \
         automatic restoration refused because its publication identity is not pinned; recovery required",
        final_path.display()
    ))
}

// ─────────────────────────────────────────────────────────────────────────────
// Path guards
// ─────────────────────────────────────────────────────────────────────────────

pub(super) fn plugin_target_path(name: &str, user_plugins_dir: &Path) -> Result<PathBuf> {
    let name = validate_skill_name_segment(name)
        .map_err(|error| anyhow::anyhow!("plugin name is not a safe directory name: {error:#}"))?;
    Ok(user_plugins_dir.join(name))
}

/// The resolved bundle must be a direct child of the resolved plugins root,
/// matching discovery's fail-closed containment rule.
pub(super) fn ensure_target_within_plugins_dir(
    target: &Path,
    user_plugins_dir: &Path,
) -> Result<()> {
    let root = fs::canonicalize(user_plugins_dir).with_context(|| {
        format!(
            "failed to resolve plugins directory {}",
            user_plugins_dir.display()
        )
    })?;
    let target = fs::canonicalize(target)
        .with_context(|| format!("failed to resolve {}", target.display()))?;
    if target.parent() != Some(root.as_path()) {
        bail!(
            "plugin path {} escapes plugins directory {}",
            target.display(),
            root.display()
        );
    }
    Ok(())
}

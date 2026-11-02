//! Stage a bundle into a private `.staging-*` sibling of the plugins root.
//!
//! Nothing here ever touches the destination directory: staging either
//! produces a validated [`StagedPlugin`] or removes its own residue. The
//! local-copy path additionally rejects symlinks anywhere in the source and
//! never copies a stale `.installed-from` marker, so provenance always
//! reflects *this* install.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::manifest::PluginManifest;
use codewhale_skill_install::install::{INSTALLED_FROM_MARKER, validate_skill_name_segment};

use super::PluginInstallError;

/// File count cap for local copies, mirroring the registry staging budget.
const MAX_BUNDLE_FILES: usize = 4_096;

#[derive(Debug)]
pub(super) struct StagedPlugin {
    pub(super) name: String,
    pub(super) staged_path: PathBuf,
    pub(super) content_hash: String,
}

pub(super) fn fresh_staging_dir(user_plugins_dir: &Path) -> Result<PathBuf> {
    ensure_plugins_dir(user_plugins_dir)?;
    // Discovery excludes this reserved staging namespace. A crashed stage
    // can remain for recovery; later attempts reserve a fresh UUID path and
    // never treat the stale copy as an installed bundle or reuse its path.
    let staged_path = user_plugins_dir.join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir(&staged_path)
        .with_context(|| format!("failed to create staging dir {}", staged_path.display()))?;
    Ok(staged_path)
}

/// Create the user plugins root when missing. The persisted plugin state
/// (`state.json`) lives in this same directory, so it must satisfy the
/// registry's owner-only contract the first time `/plugin install` brings it
/// into existence — a pre-existing directory is left untouched (trust reports
/// unsafe permissions fail-closed rather than silently repairing them).
#[cfg(unix)]
fn ensure_plugins_dir(user_plugins_dir: &Path) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(user_plugins_dir).with_context(|| {
        format!(
            "failed to create user plugins directory {}",
            user_plugins_dir.display()
        )
    })
}

#[cfg(not(unix))]
fn ensure_plugins_dir(user_plugins_dir: &Path) -> Result<()> {
    fs::create_dir_all(user_plugins_dir).with_context(|| {
        format!(
            "failed to create user plugins directory {}",
            user_plugins_dir.display()
        )
    })
}

/// Validate the staged tree and return the manifest name + content hash.
pub(super) fn validate_staged(staged_path: &Path) -> Result<(String, String)> {
    let manifest_path = crate::agent_plugin::resolve_manifest_path(staged_path)
        .ok_or_else(|| {
            anyhow::anyhow!("staged bundle has no plugin.json, .claude-plugin/plugin.json, kimi.plugin.json, or plugin.toml")
        })?;
    let validated = PluginManifest::validate_from_path(&manifest_path)
        .map_err(|error| anyhow::anyhow!("staged plugin manifest failed validation: {error}"))?;
    let name = validated.manifest.plugin.name.clone();
    validate_skill_name_segment(&name).map_err(|error| {
        anyhow::anyhow!("[plugin].name is not a safe directory name: {error:#}")
    })?;
    Ok((name, validated.content_hash))
}

/// Copy a local bundle directory into staging. Symlinks anywhere in the
/// source are rejected; a stale `.installed-from` marker is never copied so
/// provenance always reflects *this* install.
pub(super) fn stage_local_copy(
    source: &Path,
    user_plugins_dir: &Path,
    max_size: u64,
) -> Result<StagedPlugin> {
    // Validate the source first; this also rejects symlinked roots/manifests.
    let manifest_path =
        crate::agent_plugin::resolve_manifest_path(source).ok_or_else(|| {
            anyhow::anyhow!(
                "source is not a valid plugin bundle: no plugin.json, .claude-plugin/plugin.json, kimi.plugin.json, or plugin.toml"
            )
        })?;
    PluginManifest::validate_from_path(&manifest_path)
        .map_err(|error| anyhow::anyhow!("source is not a valid plugin bundle: {error}"))?;
    let canonical_source = source
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", source.display()))?;
    if let Ok(canonical_plugins) = user_plugins_dir.canonicalize()
        && (canonical_source == canonical_plugins
            || canonical_source.starts_with(&canonical_plugins))
    {
        bail!(
            "cannot install a bundle from inside the user plugins directory {}; \
             it is already in place",
            canonical_plugins.display()
        );
    }

    let staged_path = fresh_staging_dir(user_plugins_dir)?;
    let result = (|| -> Result<StagedPlugin> {
        let mut budget = CopyBudget::default();
        copy_bundle_regular_files(&canonical_source, &staged_path, max_size, &mut budget)?;
        let (name, content_hash) = validate_staged(&staged_path)?;
        Ok(StagedPlugin {
            name,
            staged_path: staged_path.clone(),
            content_hash,
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staged_path);
    }
    result
}

#[derive(Default)]
struct CopyBudget {
    files: usize,
    bytes: u64,
}

fn copy_bundle_regular_files(
    source: &Path,
    dest: &Path,
    max_size: u64,
    budget: &mut CopyBudget,
) -> Result<()> {
    for entry in fs::read_dir(source)
        .with_context(|| format!("failed to read bundle dir {}", source.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(PluginInstallError::SymlinkRejected.into());
        }
        let name = entry.file_name();
        if name == std::ffi::OsStr::new(INSTALLED_FROM_MARKER) {
            continue;
        }
        let target = dest.join(&name);
        if metadata.is_dir() {
            fs::create_dir(&target)
                .with_context(|| format!("failed to create {}", target.display()))?;
            copy_bundle_regular_files(&path, &target, max_size, budget)?;
        } else if metadata.is_file() {
            budget.files = budget.files.saturating_add(1);
            if budget.files > MAX_BUNDLE_FILES {
                bail!("bundle exceeds the {MAX_BUNDLE_FILES} file limit");
            }
            budget.bytes = budget.bytes.saturating_add(metadata.len());
            if budget.bytes > max_size {
                return Err(PluginInstallError::OversizedBundle { limit: max_size }.into());
            }
            fs::copy(&path, &target).with_context(|| {
                format!("failed to copy {} to {}", path.display(), target.display())
            })?;
        }
    }
    Ok(())
}

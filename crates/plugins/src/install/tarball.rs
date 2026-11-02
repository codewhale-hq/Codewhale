//! Two-pass tarball reader for remote bundles.
//!
//! Pass one ([`scan_tarball_for_bundle`]) writes nothing: it rejects traversal and
//! absolute paths, enforces the uncompressed size cap from the headers, and
//! locates the single bundle root — the directory holding the bundle's
//! manifest (`plugin.json`, `kimi.plugin.json`, or the legacy `plugin.toml`). Pass two
//! ([`extract_into`]) extracts only entries under that root, so a mono-repo's
//! symlinks elsewhere in the archive are never materialized.

use std::collections::BTreeSet;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use flate2::read::GzDecoder;

use codewhale_skill_install::install::is_safe_path;

use super::PluginInstallError;
use super::stage::{StagedPlugin, fresh_staging_dir, validate_staged};

/// Validate a tarball and extract the manifest-rooted subtree into a
/// `.staging-*` sibling of the destination.
pub(super) fn stage_tarball(
    bytes: &[u8],
    user_plugins_dir: &Path,
    max_size: u64,
    bundle_path: Option<&str>,
) -> Result<StagedPlugin> {
    let scan = scan_tarball_for_bundle(bytes, max_size, bundle_path)?;
    let staged_path = fresh_staging_dir(user_plugins_dir)?;
    let result = extract_into(&scan, bytes, &staged_path, max_size)
        .and_then(|()| validate_staged(&staged_path));
    match result {
        Ok((name, content_hash)) => Ok(StagedPlugin {
            name,
            staged_path,
            content_hash,
        }),
        Err(error) => {
            let _ = fs::remove_dir_all(&staged_path);
            Err(error)
        }
    }
}

#[derive(Debug)]
pub(super) struct TarballScan {
    /// Archive-relative directory containing the bundle's manifest (`""` when
    /// the manifest sits at the archive root).
    plugin_root: String,
}

/// First pass: validate entry paths, enforce the uncompressed size cap, and
/// locate the single bundle root. Nothing is written in this pass.
#[cfg(test)]
pub(super) fn scan_tarball(bytes: &[u8], max_size: u64) -> Result<TarballScan> {
    scan_tarball_for_bundle(bytes, max_size, None)
}

fn scan_tarball_for_bundle(
    bytes: &[u8],
    max_size: u64,
    bundle_path: Option<&str>,
) -> Result<TarballScan> {
    let cursor = std::io::Cursor::new(bytes);
    let gz = GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);

    let mut total_size: u64 = 0;
    let mut manifest_paths: Vec<String> = Vec::new();

    for entry in archive
        .entries()
        .context("failed to read tar entries (corrupt archive?)")?
    {
        let entry = entry.context("failed to read tar entry")?;
        let header = entry.header().clone();
        let path = entry
            .path()
            .context("tar entry has invalid path")?
            .to_path_buf();
        let path_str = path.to_string_lossy().into_owned();
        if !is_safe_path(&path) {
            return Err(PluginInstallError::PathTraversal(path_str).into());
        }
        if let Ok(size) = header.size() {
            total_size = total_size.saturating_add(size);
            if total_size > max_size {
                return Err(PluginInstallError::OversizedBundle { limit: max_size }.into());
            }
        }
        if header.entry_type().is_file()
            && path.file_name().is_some_and(|name| {
                name == std::ffi::OsStr::new(crate::agent_plugin::PLUGIN_JSON_NAME)
                    || name == std::ffi::OsStr::new(crate::agent_plugin::KIMI_PLUGIN_JSON_NAME)
                    || name == std::ffi::OsStr::new(crate::agent_plugin::PLUGIN_TOML_NAME)
            })
        {
            manifest_paths.push(path_str);
        }
    }

    // A dual-published bundle carries `plugin.json` and `plugin.toml` in the
    // same directory; that is one root, not two. Manifests in different
    // directories stay an ambiguous mono-repo and are rejected.
    let roots: BTreeSet<String> = manifest_paths
        .iter()
        .map(|manifest| {
            crate::agent_plugin::plugin_root_for_manifest(Path::new(manifest))
                .map(|root| root.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .filter(|root| {
            bundle_path.is_none_or(|wanted| {
                root == wanted
                    || root
                        .split_once('/')
                        .is_some_and(|(_, relative)| relative == wanted)
            })
        })
        .collect();
    if roots.len() != 1 {
        return Err(PluginInstallError::PluginTomlRoots(roots.len()).into());
    }
    let plugin_root = roots.into_iter().next().unwrap_or_default();
    Ok(TarballScan { plugin_root })
}

/// Second pass: extract only entries under the scanned bundle root.
fn extract_into(scan: &TarballScan, bytes: &[u8], dest: &Path, max_size: u64) -> Result<()> {
    let cursor = std::io::Cursor::new(bytes);
    let gz = GzDecoder::new(cursor);
    let mut archive = tar::Archive::new(gz);
    let mut total_size: u64 = 0;

    for entry in archive
        .entries()
        .context("failed to read tar entries (corrupt archive?)")?
    {
        let mut entry = entry.context("failed to read tar entry")?;
        let header = entry.header().clone();
        let entry_type = header.entry_type();
        let path = entry
            .path()
            .context("tar entry has invalid path")?
            .to_path_buf();
        let path_str = path.to_string_lossy().into_owned();
        if !is_safe_path(&path) {
            return Err(PluginInstallError::PathTraversal(path_str).into());
        }

        // Keep only the bundle subtree. Entries outside it (including any
        // symlinks a mono-repo ships elsewhere) are ignored, never extracted.
        let stripped = if scan.plugin_root.is_empty() {
            path_str.clone()
        } else if path_str == scan.plugin_root {
            String::new()
        } else if let Some(rest) = path_str.strip_prefix(&format!("{}/", scan.plugin_root)) {
            rest.to_string()
        } else {
            continue;
        };
        if stripped.is_empty() {
            // The bundle root directory itself — the staging dir already exists.
            continue;
        }
        // Defense-in-depth: re-validate the stripped path.
        let stripped_path = Path::new(&stripped);
        if !is_safe_path(stripped_path) {
            return Err(PluginInstallError::PathTraversal(stripped).into());
        }
        if entry_type.is_symlink() || entry_type.is_hard_link() {
            return Err(PluginInstallError::SymlinkRejected.into());
        }

        let target = dest.join(stripped_path);
        // Final paranoia check: the composed target must stay under dest.
        let target_components: Vec<_> = target.components().collect();
        let dest_components: Vec<_> = dest.components().collect();
        if !target_components.starts_with(dest_components.as_slice()) {
            return Err(PluginInstallError::PathTraversal(stripped).into());
        }

        if entry_type.is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("failed to create dir {}", target.display()))?;
            continue;
        }
        if entry_type.is_file() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create dir {}", parent.display()))?;
            }
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .with_context(|| format!("failed to read {}", path.display()))?;
            total_size = total_size.saturating_add(buf.len() as u64);
            if total_size > max_size {
                return Err(PluginInstallError::OversizedBundle { limit: max_size }.into());
            }
            let mut options = fs::OpenOptions::new();
            options.create_new(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt as _;
                options.mode(0o600);
            }
            let mut out = options
                .open(&target)
                .with_context(|| format!("failed to create {}", target.display()))?;
            out.write_all(&buf)
                .with_context(|| format!("failed to write {}", target.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let executable = header.mode().context("invalid archive file mode")? & 0o111 != 0;
                let mode = if executable { 0o700 } else { 0o600 };
                out.set_permissions(fs::Permissions::from_mode(mode))
                    .with_context(|| format!("failed to set mode for {}", target.display()))?;
            }
        }
    }
    Ok(())
}

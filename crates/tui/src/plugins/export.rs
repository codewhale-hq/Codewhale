//! `/plugin export` — publish a loaded plugin as a spec-valid Agent Plugins
//! v1.0.0 bundle.
//!
//! The export writes a fresh directory containing `plugin.json`, `mcp.json`
//! when the plugin declares MCP servers, and the bundle's content tree —
//! including the standard `skills/` layout other clients read. The source
//! bundle is never modified: export is a publish step, not the (separately
//! specified, not yet implemented) on-disk migration.
//!
//! Guarantees:
//!
//! * The target directory is created fresh or must be empty; a failed export
//!   removes a directory it created and never touches a pre-existing one.
//! * Every emitted document is re-validated against the standard's shape
//!   ([`super::agent_plugin::validate_plugin_json`] /
//!   [`super::agent_plugin::validate_mcp_json`]) before it is written.
//! * Symlinks anywhere in the source tree are rejected, matching install
//!   staging; the `.installed-from` marker and any manifest files are not
//!   copied (fresh manifests are generated).
//! * Skills land at the standard `skills/` root even when the source bundle
//!   declared a custom location; colliding skill directory names are an
//!   error, never a silent merge.
//! * A name that is invalid under the standard is slugified, the original is
//!   preserved as the display name, and a slug that collides with another
//!   loaded plugin is an error.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::agent_plugin;
use super::manifest::PluginPathSpec;
use super::types::LoadedPlugin;
use codewhale_util::path_identity::metadata_is_link_or_reparse;

/// Mirrors the install staging budget: a validated bundle already fits the
/// review limits, and export refuses to grow unbounded copies regardless.
const MAX_EXPORT_FILES: usize = 4_096;

/// Files generated fresh (or provenance-local) at the destination; never
/// copied out of the source tree.
const EXPORT_EXCLUDED_ROOT_FILES: [&str; 4] = [
    agent_plugin::PLUGIN_JSON_NAME,
    agent_plugin::PLUGIN_TOML_NAME,
    agent_plugin::MCP_JSON_NAME,
    codewhale_skill_install::install::INSTALLED_FROM_MARKER,
];

/// What an export wrote, for the caller to render.
#[derive(Debug, Clone)]
pub struct PluginExportReceipt {
    pub target: PathBuf,
    /// Name the bundle was published under (post-slugification).
    pub exported_name: String,
    /// Display name preserved in `extensions["net.codewhale"]` when the
    /// published name differs from the source name.
    pub display_name: Option<String>,
    pub wrote_mcp_json: bool,
    pub skills_normalized: bool,
    pub files_copied: usize,
}

/// Publish `plugin` as an Agent Plugins bundle under `target`.
///
/// `existing_names` must contain every other loaded plugin's name so a
/// slugified name that would collide is an error.
pub fn export_plugin_bundle(
    plugin: &LoadedPlugin,
    target: &Path,
    existing_names: &BTreeSet<String>,
) -> Result<PluginExportReceipt, String> {
    let source = &plugin.canonical_root;
    if !source.is_dir() {
        return Err(format!(
            "plugin `{}` bundle root is not a directory: {}",
            plugin.name(),
            source.display()
        ));
    }

    let target_metadata = fs::symlink_metadata(target).ok();
    let created = match &target_metadata {
        Some(metadata) => {
            if metadata_is_link_or_reparse(metadata) || !metadata.is_dir() {
                return Err(format!(
                    "export target must be a real directory, not a link or file: {}",
                    target.display()
                ));
            }
            let mut entries = fs::read_dir(target).map_err(|e| {
                format!("failed to inspect export target {}: {e}", target.display())
            })?;
            if entries.next().is_some() {
                return Err(format!(
                    "export target is not empty: {} (choose a fresh directory)",
                    target.display()
                ));
            }
            false
        }
        None => true,
    };

    // Canonicalize the nearest existing ancestor so the containment guards
    // hold even when the target itself does not exist yet.
    let canonical_target = canonicalize_with_missing_tail(target)?;
    if canonical_target == *source || canonical_target.starts_with(source) {
        return Err(format!(
            "export target {} must not be the plugin bundle or inside it",
            target.display()
        ));
    }
    if source.starts_with(&canonical_target) {
        return Err(format!(
            "export target {} must not contain the plugin bundle",
            target.display()
        ));
    }

    // Normalization decision first: the emitted manifest must describe the
    // tree the export actually writes. A custom skills layout is moved to the
    // standard `skills/` root below, so the manifest copy used for emission
    // always carries the default spec when skills exist at all.
    let skills_normalized = plugin
        .manifest
        .skills
        .as_ref()
        .is_some_and(|spec| !agent_plugin::is_default_skills_spec(spec));
    let mut manifest = plugin.manifest.clone();
    if skills_normalized {
        manifest.skills = Some(PluginPathSpec {
            path: Some("skills".to_string()),
            paths: Vec::new(),
        });
    }

    let emission = agent_plugin::manifest_to_standard(&manifest, existing_names)?;
    let plugin_value = serde_json::to_value(&emission.plugin_json)
        .map_err(|e| format!("failed to encode plugin.json: {e}"))?;
    agent_plugin::validate_plugin_json(&plugin_value)?;
    let mcp_value = emission
        .mcp_json
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .map_err(|e| format!("failed to encode mcp.json: {e}"))?;
    if let Some(value) = &mcp_value {
        agent_plugin::validate_mcp_json(value)?;
    }

    if created {
        fs::create_dir_all(target)
            .map_err(|e| format!("failed to create export target {}: {e}", target.display()))?;
    }
    let files_copied = match write_export(
        source,
        target,
        &plugin_value,
        mcp_value.as_ref(),
        &plugin.manifest,
    ) {
        Ok(copied) => copied,
        Err(error) => {
            if created {
                let _ = fs::remove_dir_all(target);
            }
            return Err(error);
        }
    };
    Ok(PluginExportReceipt {
        target: target.to_path_buf(),
        exported_name: emission.exported_name,
        display_name: emission.display_name,
        wrote_mcp_json: mcp_value.is_some(),
        skills_normalized,
        files_copied,
    })
}

fn write_export(
    source: &Path,
    target: &Path,
    plugin_value: &serde_json::Value,
    mcp_value: Option<&serde_json::Value>,
    source_manifest: &super::manifest::PluginManifest,
) -> Result<usize, String> {
    let mut budget = 0_usize;
    copy_bundle_tree(source, target, true, &mut budget)?;
    normalize_skills_layout(source_manifest, target)?;
    write_json_document(&target.join(agent_plugin::PLUGIN_JSON_NAME), plugin_value)?;
    if let Some(value) = mcp_value {
        write_json_document(&target.join(agent_plugin::MCP_JSON_NAME), value)?;
    }
    Ok(budget)
}

fn write_json_document(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("failed to serialize {}: {e}", path.display()))?;
    text.push('\n');
    fs::write(path, text).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

/// Recursively copy the source tree, rejecting links and skipping root-level
/// files that export regenerates (manifests) or that encode local provenance
/// (the install marker).
fn copy_bundle_tree(
    source: &Path,
    target: &Path,
    root_level: bool,
    copied: &mut usize,
) -> Result<(), String> {
    let mut entries = fs::read_dir(source)
        .map_err(|e| format!("failed to read bundle directory {}: {e}", source.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to read bundle directory {}: {e}", source.display()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        if root_level && EXPORT_EXCLUDED_ROOT_FILES.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|e| format!("failed to inspect bundle entry {}: {e}", path.display()))?;
        if metadata_is_link_or_reparse(&metadata) {
            return Err(format!(
                "plugin bundle contains a symbolic link or reparse point: {}",
                path.display()
            ));
        }
        let destination = target.join(&name);
        if metadata.is_dir() {
            fs::create_dir(&destination)
                .map_err(|e| format!("failed to create {}: {e}", destination.display()))?;
            copy_bundle_tree(&path, &destination, false, copied)?;
        } else if metadata.is_file() {
            *copied += 1;
            if *copied > MAX_EXPORT_FILES {
                return Err(format!(
                    "plugin bundle exceeds the {MAX_EXPORT_FILES}-file export limit"
                ));
            }
            fs::copy(&path, &destination).map_err(|e| {
                format!(
                    "failed to copy {} to {}: {e}",
                    path.display(),
                    destination.display()
                )
            })?;
        } else {
            return Err(format!(
                "plugin bundle entry is neither a regular file nor directory: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Move skills declared at a non-standard location into the standard
/// `skills/` root of the exported tree. Collisions between skill directory
/// names are an error.
fn normalize_skills_layout(
    source_manifest: &super::manifest::PluginManifest,
    target: &Path,
) -> Result<(), String> {
    let Some(spec) = &source_manifest.skills else {
        return Ok(());
    };
    if agent_plugin::is_default_skills_spec(spec) {
        return Ok(());
    }
    let declared: Vec<&String> = spec.path.iter().chain(spec.paths.iter()).collect();
    let skills_root = target.join("skills");
    for relative in declared {
        if relative.as_str() == "skills" {
            continue;
        }
        let source_dir = target.join(relative);
        if !source_dir.is_dir() {
            return Err(format!(
                "declared skills path `{relative}` is missing from the copied bundle"
            ));
        }
        if !skills_root.exists() {
            fs::create_dir(&skills_root)
                .map_err(|e| format!("failed to create {}: {e}", skills_root.display()))?;
        }
        let mut entries = fs::read_dir(&source_dir)
            .map_err(|e| {
                format!(
                    "failed to read skills directory {}: {e}",
                    source_dir.display()
                )
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                format!(
                    "failed to read skills directory {}: {e}",
                    source_dir.display()
                )
            })?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let destination = skills_root.join(entry.file_name());
            if destination.exists() {
                return Err(format!(
                    "skills layout normalization collision: `{}` exists in more than one skills path",
                    entry.file_name().to_string_lossy()
                ));
            }
            fs::rename(entry.path(), &destination).map_err(|e| {
                format!(
                    "failed to move skill {} into the standard skills/ tree: {e}",
                    entry.path().display()
                )
            })?;
        }
        // Remove the declared directory once emptied; leave any deeper
        // ancestors (they may hold non-skill content).
        let _ = fs::remove_dir(&source_dir);
    }
    Ok(())
}

/// Canonicalize a path whose final component may not exist yet, by resolving
/// the nearest existing ancestor.
fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf, String> {
    let mut missing = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        match cursor.canonicalize() {
            Ok(canonical) => {
                let mut resolved = canonical;
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(_) => {
                let Some(file_name) = cursor.file_name().map(|name| name.to_os_string()) else {
                    return Err(format!(
                        "export target {} has no existing ancestor",
                        path.display()
                    ));
                };
                missing.push(file_name);
                cursor = cursor
                    .parent()
                    .ok_or_else(|| {
                        format!("export target {} has no existing ancestor", path.display())
                    })?
                    .to_path_buf();
            }
        }
    }
}

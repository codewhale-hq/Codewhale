//! Runtime adapters for reviewed, content-addressed plugin components.
//!
//! This module is the only place that translates mutable discovery paths into
//! immutable staged component paths. Consumers still revalidate the attached
//! [`PluginAuthority`] at their execution boundary so disable, revoke, and
//! uninstall transitions in another process fail closed immediately.

use std::path::{Path, PathBuf};

use super::PluginRegistry;
use super::activation::PluginActivationCapability;
use super::registry::verify_plugin_component_authority;
use super::types::{LoadedPlugin, PluginScope};
use codewhale_config::PluginAuthority;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginComponentSource {
    pub plugin_name: String,
    pub path: PathBuf,
    pub authority: PluginAuthority,
}

fn component_paths(plugin: &LoadedPlugin, capability: PluginActivationCapability) -> &[PathBuf] {
    match capability {
        PluginActivationCapability::Commands => &plugin.components.commands,
        PluginActivationCapability::Agents => &plugin.components.agents,
        PluginActivationCapability::Hooks => &plugin.components.hooks,
        // Extension-host entry modules; only active under the extension-host policy.
        PluginActivationCapability::Native => &plugin.components.native,
        PluginActivationCapability::Skills
        | PluginActivationCapability::McpStdio
        | PluginActivationCapability::McpRemote
        | PluginActivationCapability::Providers
        | PluginActivationCapability::Lsp
        | PluginActivationCapability::FilesystemRoots
        | PluginActivationCapability::LifecycleMutation => &[],
    }
}

/// Why `path` cannot be an extension-host entry, if it cannot.
///
/// A `native` entry is one `.mjs`, `.js` or `.mts` ES module file: the host imports
/// exactly that file and re-hashes it first. This is the one statement of the
/// rule. Discovery reports it as an error diagnostic (so `/plugin validate`
/// and the review screen show it) and activation refuses the entry, both only
/// while the activation policy supports `Native` (`[features]
/// extension_host`). With the flag off, `native` stays inventory-only and any
/// path is accepted as before.
///
/// `is_regular_file` comes from the caller's own view of the bundle (the
/// validated manifest's hashed files, or the activation read), so this
/// function touches no filesystem.
#[must_use]
pub fn native_entry_problem(path: &Path, is_regular_file: bool) -> Option<&'static str> {
    const RULE: &str = "a native entry must be one .mjs, .js or .mts ES module file";
    let is_module = path
        .extension()
        .is_some_and(|extension| extension == "mjs" || extension == "js" || extension == "mts");
    (!is_module || !is_regular_file).then_some(RULE)
}

fn scope_precedence(scope: PluginScope) -> u8 {
    match scope {
        PluginScope::Workspace => 0,
        PluginScope::User => 1,
        PluginScope::Builtin => 2,
    }
}

/// Resolve one active component kind into immutable staged paths.
///
/// Workspace bundles win same-name collisions over user and built-in bundles;
/// consumers retain their existing non-plugin precedence above this list.
pub fn active_component_sources(
    registry: &PluginRegistry,
    capability: PluginActivationCapability,
) -> (Vec<PluginComponentSource>, Vec<String>) {
    let mut plugins = registry
        .active_plugins()
        .into_iter()
        .filter(|plugin| plugin.component_active(capability))
        .collect::<Vec<_>>();
    plugins.sort_by(|left, right| {
        scope_precedence(left.scope)
            .cmp(&scope_precedence(right.scope))
            .then_with(|| left.name().cmp(right.name()))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut sources = Vec::new();
    let mut errors = Vec::new();
    for plugin in plugins {
        let Some(authority) = registry.authority_for(plugin.id.as_str()) else {
            errors.push(format!(
                "Plugin `{}` has no persisted runtime authority",
                plugin.name()
            ));
            continue;
        };
        if let Err(reason) = verify_plugin_component_authority(&authority, capability) {
            errors.push(format!(
                "Plugin `{}` {} adapter was denied: {reason}",
                plugin.name(),
                capability.as_str()
            ));
            continue;
        }
        let Some(staged_root) = plugin.staged_root.as_deref() else {
            errors.push(format!(
                "Plugin `{}` has no immutable runtime snapshot",
                plugin.name()
            ));
            continue;
        };
        for source_path in component_paths(plugin, capability) {
            match staged_component_path(&plugin.canonical_root, staged_root, source_path) {
                Ok(path) => sources.push(PluginComponentSource {
                    plugin_name: plugin.name().to_string(),
                    path,
                    authority: authority.clone(),
                }),
                Err(reason) => errors.push(format!(
                    "Plugin `{}` {} component was denied: {reason}",
                    plugin.name(),
                    capability.as_str()
                )),
            }
        }
    }
    (sources, errors)
}

pub(super) fn staged_component_path(
    canonical_root: &Path,
    staged_root: &Path,
    source_path: &Path,
) -> Result<PathBuf, String> {
    let relative = source_path
        .strip_prefix(canonical_root)
        .map_err(|_| "reviewed component escaped the plugin root".to_string())?;
    let staged_root = staged_root
        .canonicalize()
        .map_err(|_| "runtime snapshot is unavailable".to_string())?;
    let candidate = staged_root.join(relative);
    let candidate = candidate
        .canonicalize()
        .map_err(|_| "runtime component is unavailable".to_string())?;
    if !candidate.starts_with(&staged_root) {
        return Err("runtime component escaped the immutable snapshot".to_string());
    }
    Ok(candidate)
}

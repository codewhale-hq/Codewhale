pub mod activation;
pub mod agent_plugin;
pub mod builtin;
pub mod composition_review;
pub mod composition_scope;
pub mod context;
pub mod discovery;
pub mod export;
pub mod host_services;
pub mod install;
pub mod managed_policy;
pub mod manifest;
pub mod marketplace;
pub mod matcher;
pub mod mutation;
pub mod native_presets;
pub mod providers;
pub mod recommend;
pub mod registry;
pub mod runtime;
pub mod types;

#[cfg(test)]
mod host_service_tests;
#[cfg(any(test, feature = "test-support"))]
pub mod test_fixture;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

pub use context::{HostEnvironment, PluginDiscoveryContext};
pub use discovery::PluginCatalogStamp;
pub use registry::PluginRegistry;

pub const PLUGIN_RELOAD_NUDGE: &str =
    "Plugins changed on disk. They apply when the session is idle, or run /plugin reload.";

pub const PLUGIN_PROVIDER_NEW_SESSION_NOTICE: &str =
    "Plugin provider changes apply in a new session.";

fn active_provider_declarations(
    registry: &PluginRegistry,
) -> BTreeMap<String, (String, Vec<String>)> {
    registry
        .active_plugins()
        .into_iter()
        .filter(|plugin| !plugin.manifest.providers.is_empty())
        .map(|plugin| {
            (
                plugin.id.as_str().to_string(),
                (
                    plugin.content_hash.clone(),
                    plugin.manifest.providers.keys().cloned().collect(),
                ),
            )
        })
        .collect()
}

#[must_use]
pub fn provider_declarations_changed(before: &PluginRegistry, after: &PluginRegistry) -> bool {
    active_provider_declarations(before) != active_provider_declarations(after)
}

/// Returns the reload nudge once per distinct on-disk catalog stamp.
#[must_use]
pub fn plugin_reload_nudge(
    registry: &PluginRegistry,
    last_nudged: &mut Option<PluginCatalogStamp>,
) -> Option<&'static str> {
    if !registry.on_disk_catalog_changed() {
        return None;
    }
    let live = registry.live_catalog_stamp();
    if last_nudged.as_ref() == Some(&live) {
        return None;
    }
    *last_nudged = Some(live);
    Some(PLUGIN_RELOAD_NUDGE)
}

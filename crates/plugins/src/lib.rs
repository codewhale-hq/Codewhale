pub mod activation;
pub mod agent_plugin;
pub mod builtin;
pub mod composition_review;
pub mod composition_scope;
pub mod context;
pub mod discovery;
pub mod export;
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

#[cfg(any(test, feature = "test-support"))]
pub mod test_fixture;
#[cfg(test)]
mod tests;

pub use context::{HostEnvironment, PluginDiscoveryContext};
pub use discovery::PluginCatalogStamp;
pub use registry::PluginRegistry;

pub const PLUGIN_RELOAD_NUDGE: &str = "Plugins changed on disk. Run /plugin reload to apply.";

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

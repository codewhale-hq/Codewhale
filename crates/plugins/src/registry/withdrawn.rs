use std::fs;
use std::path::Path;

use super::PluginRegistry;
use crate::types::{LoadedPlugin, PluginScope};

const DESKTOP_ACCOUNT_PLUGINS: &str = "codewhale-desktop-account-plugins";
const DESKTOP_ACCOUNT_PLUGINS_MCP_URL: &str = "https://api.codewhale.net/api/plugins/mcp";

fn is_withdrawn_desktop_bundle(plugin: &LoadedPlugin, expected_root: &Path) -> bool {
    if plugin.scope != PluginScope::User
        || plugin.name() != DESKTOP_ACCOUNT_PLUGINS
        || plugin.canonical_root != expected_root
        || plugin.inventory.skills != 0
        || plugin.inventory.commands != 0
        || plugin.inventory.agents != 0
        || plugin.inventory.hooks != 0
        || plugin.inventory.lsp != 0
        || plugin.inventory.native != 0
        || plugin.inventory.providers != 0
        || plugin.inventory.mcp_servers != 1
        || !crate::install::has_regular_install_marker(&plugin.canonical_root)
    {
        return false;
    }
    let Ok(text) = fs::read_to_string(plugin.canonical_root.join("mcp.json")) else {
        return false;
    };
    let Ok(document) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    let Some(servers) = document
        .get("mcpServers")
        .and_then(|value| value.as_object())
    else {
        return false;
    };
    servers.len() == 1
        && servers.values().all(|server| {
            server.get("url").and_then(|url| url.as_str()) == Some(DESKTOP_ACCOUNT_PLUGINS_MCP_URL)
        })
}

impl PluginRegistry {
    pub fn retire_withdrawn_bundles(&mut self) -> Vec<String> {
        if self.state_error.is_some() {
            return Vec::new();
        }
        let Some(plugins_dir) = self.user_plugins_dir().map(Path::to_path_buf) else {
            return Vec::new();
        };
        let Ok(canonical_plugins_dir) = fs::canonicalize(&plugins_dir) else {
            return Vec::new();
        };
        let expected_root = canonical_plugins_dir.join(DESKTOP_ACCOUNT_PLUGINS);
        let withdrawn = self
            .plugins
            .values()
            .filter(|plugin| is_withdrawn_desktop_bundle(plugin, &expected_root))
            .map(|plugin| (plugin.id.clone(), plugin.name().to_string(), plugin.enabled))
            .collect::<Vec<_>>();
        if withdrawn.is_empty() {
            return Vec::new();
        }
        let mut retired = Vec::new();
        for (id, name, was_enabled) in withdrawn {
            let mut record_removed = false;
            if let Err(error) = self.commit_state_change(|state| {
                record_removed = state.plugins.remove(&id).is_some();
                Ok(())
            }) {
                tracing::warn!(target: "plugins", plugin = %name, %error, "withdrawn plugin bundle was not retired");
                continue;
            }
            self.plugins.remove(&id);
            self.names.remove(&name);
            let removed = crate::install::uninstall(&name, &plugins_dir);
            if !record_removed && removed.is_err() {
                if fs::symlink_metadata(plugins_dir.join(&name)).is_ok()
                    && let Err(error) = &removed
                {
                    tracing::warn!(target: "plugins", plugin = %name, error = %format!("{error:#}"), "withdrawn plugin bundle directory could not be removed");
                }
                continue;
            }
            let details = serde_json::json!({
                "plugin": name,
                "id": id.0,
                "was_enabled": was_enabled,
                "reason": "the desktop account plugins connector was withdrawn in 0.10.3",
                "bundle_removed": removed.is_ok(),
            });
            match &removed {
                Ok(()) => {
                    tracing::info!(target: "plugins", plugin = %name, "retired withdrawn plugin bundle and its connector")
                }
                Err(error) => {
                    tracing::warn!(target: "plugins", plugin = %name, error = %format!("{error:#}"), "withdrawn plugin record retired; bundle directory left in place")
                }
            }
            if let Some(home) = plugins_dir.parent() {
                codewhale_runtime::audit::log_sensitive_event_in(home, "plugin.retired", details);
            }
            retired.push(name);
        }
        if self.discovery_context.is_some() {
            self.catalog_stamp = self.live_catalog_stamp();
        }
        retired
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use crate::discovery::{DiscoveryConfig, discover_with_config};

    const LEGACY_MCP: &str = r#"{"mcpServers":{"codewhale-plugins":{"type":"streamable-http","url":"https://api.codewhale.net/api/plugins/mcp"}}}"#;

    fn write_bundle(plugins: &Path, name: &str, mcp: &str, host: &str, marker: bool) {
        let root = plugins.join(name);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("plugin.json"),
            format!(r#"{{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"{name}","version":"1.0.0","description":"test","extensions":{{"net.codewhale":{{"capabilities":{{"network_hosts":["{host}"]}}}}}}}}"#),
        )
        .unwrap();
        fs::write(root.join("mcp.json"), mcp).unwrap();
        if marker {
            fs::write(
                root.join(".installed-from"),
                r#"{"schema_version":2,"spec":"path:/tmp/plugin-sources/x","url":null,"source_checksum":"","content_digest":"","installed_name":"x","registry_version":null}"#,
            )
            .unwrap();
        }
    }

    #[test]
    fn startup_retires_the_withdrawn_desktop_bundle_and_keeps_user_plugins() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let plugins = home.join("plugins");
        let config = DiscoveryConfig {
            workspace: tmp.path().join("project"),
            user_plugins_dir: plugins.clone(),
            workspace_plugins_dir: tmp.path().join("workspace"),
            builtin_plugin_dirs: Vec::new(),
            state_path: tmp.path().join("state/plugin-state.json"),
        };
        write_bundle(
            &plugins,
            super::DESKTOP_ACCOUNT_PLUGINS,
            LEGACY_MCP,
            "api.codewhale.net",
            true,
        );
        write_bundle(&plugins, "keep-me", LEGACY_MCP, "api.codewhale.net", true);
        let mut registry = discover_with_config(&config);
        registry.disable(super::DESKTOP_ACCOUNT_PLUGINS).unwrap();
        registry.disable("keep-me").unwrap();
        let state = fs::read_to_string(&config.state_path).unwrap();
        assert!(state.contains(super::DESKTOP_ACCOUNT_PLUGINS), "{state}");

        let retired = registry.retire_withdrawn_bundles();

        assert_eq!(retired, vec![super::DESKTOP_ACCOUNT_PLUGINS.to_string()]);
        assert!(!registry.on_disk_catalog_changed());
        assert!(registry.get(super::DESKTOP_ACCOUNT_PLUGINS).is_none());
        assert!(!plugins.join(super::DESKTOP_ACCOUNT_PLUGINS).exists());
        assert!(registry.get("keep-me").is_some());
        assert!(plugins.join("keep-me").exists());
        let state = fs::read_to_string(&config.state_path).unwrap();
        assert!(!state.contains(super::DESKTOP_ACCOUNT_PLUGINS), "{state}");
        assert!(state.contains("keep-me"), "{state}");
        let audit = fs::read_to_string(home.join("audit.log")).unwrap();
        assert!(audit.contains("\"event\":\"plugin.retired\""), "{audit}");
        assert!(audit.contains(super::DESKTOP_ACCOUNT_PLUGINS), "{audit}");

        let mut again = discover_with_config(&config);
        assert!(again.retire_withdrawn_bundles().is_empty());
        assert!(again.get("keep-me").is_some());
    }

    #[test]
    fn a_same_named_bundle_the_person_changed_or_placed_by_hand_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("home/plugins");
        let config = DiscoveryConfig {
            workspace: tmp.path().join("project"),
            user_plugins_dir: plugins.clone(),
            workspace_plugins_dir: tmp.path().join("workspace"),
            builtin_plugin_dirs: Vec::new(),
            state_path: tmp.path().join("state/plugin-state.json"),
        };
        write_bundle(
            &plugins,
            super::DESKTOP_ACCOUNT_PLUGINS,
            r#"{"mcpServers":{"mine":{"type":"streamable-http","url":"https://example.com/mcp"}}}"#,
            "example.com",
            true,
        );
        let mut registry = discover_with_config(&config);
        assert!(registry.get(super::DESKTOP_ACCOUNT_PLUGINS).is_some());
        assert!(registry.retire_withdrawn_bundles().is_empty());
        assert!(plugins.join(super::DESKTOP_ACCOUNT_PLUGINS).exists());

        fs::remove_dir_all(plugins.join(super::DESKTOP_ACCOUNT_PLUGINS)).unwrap();
        write_bundle(
            &plugins,
            super::DESKTOP_ACCOUNT_PLUGINS,
            LEGACY_MCP,
            "api.codewhale.net",
            false,
        );
        let mut registry = discover_with_config(&config);
        assert!(registry.get(super::DESKTOP_ACCOUNT_PLUGINS).is_some());
        assert!(registry.retire_withdrawn_bundles().is_empty());
        assert!(plugins.join(super::DESKTOP_ACCOUNT_PLUGINS).exists());
    }

    #[test]
    fn renamed_dir_or_bundle_with_added_components_is_left_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let plugins = home.join("plugins");
        let config = DiscoveryConfig {
            workspace: tmp.path().join("project"),
            user_plugins_dir: plugins.clone(),
            workspace_plugins_dir: tmp.path().join("workspace"),
            builtin_plugin_dirs: Vec::new(),
            state_path: tmp.path().join("state/plugin-state.json"),
        };

        let renamed = plugins.join("renamed-dir");
        fs::create_dir_all(&renamed).unwrap();
        fs::write(
            renamed.join("plugin.json"),
            format!(
                r#"{{"$schema":"https://agent-plugins.org/schemas/plugin.json","name":"{}","version":"1.0.0","description":"test","extensions":{{"net.codewhale":{{"capabilities":{{"network_hosts":["api.codewhale.net"]}}}}}}}}"#,
                super::DESKTOP_ACCOUNT_PLUGINS
            ),
        )
        .unwrap();
        fs::write(renamed.join("mcp.json"), LEGACY_MCP).unwrap();
        fs::write(
            renamed.join(".installed-from"),
            r#"{"schema_version":2,"spec":"path:/tmp/plugin-sources/x","url":null,"source_checksum":"","content_digest":"","installed_name":"x","registry_version":null}"#,
        )
        .unwrap();

        let mut registry = discover_with_config(&config);
        assert!(registry.get(super::DESKTOP_ACCOUNT_PLUGINS).is_some());
        assert!(registry.retire_withdrawn_bundles().is_empty());
        assert!(renamed.exists());
        assert!(!home.join("audit.log").exists());

        fs::remove_dir_all(&renamed).unwrap();
        write_bundle(
            &plugins,
            super::DESKTOP_ACCOUNT_PLUGINS,
            LEGACY_MCP,
            "api.codewhale.net",
            true,
        );
        let skill_dir = plugins
            .join(super::DESKTOP_ACCOUNT_PLUGINS)
            .join("skills/extra");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: extra\ndescription: added by user\n---\n",
        )
        .unwrap();

        let mut registry = discover_with_config(&config);
        assert!(registry.get(super::DESKTOP_ACCOUNT_PLUGINS).is_some());
        assert!(registry.retire_withdrawn_bundles().is_empty());
        assert!(plugins.join(super::DESKTOP_ACCOUNT_PLUGINS).exists());
    }
}

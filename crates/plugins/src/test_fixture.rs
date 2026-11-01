use std::fs;
use std::path::PathBuf;

use tempfile::TempDir;

use super::PluginRegistry;
use super::discovery::{DiscoveryConfig, discover_with_config};

pub struct DeclarativePluginFixture {
    _temp: TempDir,
    pub workspace: PathBuf,
    pub marker: PathBuf,
    pub config: DiscoveryConfig,
    pub registry: PluginRegistry,
}

impl Default for DeclarativePluginFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl DeclarativePluginFixture {
    pub fn new() -> Self {
        let temp = TempDir::new().expect("plugin fixture tempdir");
        let workspace = temp.path().join("project");
        let user_plugins_dir = temp.path().join("user");
        let plugin = user_plugins_dir.join("runtime-demo");
        let marker = workspace.join("plugin-hook-ran");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(plugin.join("commands")).expect("commands");
        fs::create_dir_all(plugin.join("agents")).expect("agents");
        fs::create_dir_all(plugin.join("hooks")).expect("hooks");
        fs::write(
            plugin.join("plugin.toml"),
            "schema_version = 1\n[plugin]\nname = \"runtime-demo\"\nversion = \"1.0.0\"\n[commands]\npath = \"commands\"\n[agents]\npath = \"agents\"\n[hooks]\npath = \"hooks\"\n",
        )
        .expect("manifest");
        fs::write(
            plugin.join("commands/plugin-hello.md"),
            "---\ndescription: Send a reviewed plugin greeting\n---\nhello from plugin $ARGUMENTS",
        )
        .expect("command");
        fs::write(
            plugin.join("agents/plugin-scout.toml"),
            "id = \"plugin-scout\"\ndisplay_name = \"Plugin Scout\"\ndescription = \"Reviewed staged scout\"\nrole_hint = \"scout\"\n",
        )
        .expect("agent");
        let command = format!("printf plugin-hook-ran > {}", marker.display());
        fs::write(
            plugin.join("hooks/hooks.toml"),
            format!(
                "enabled = true\n\n[[hooks]]\nname = \"plugin-start\"\nevent = \"session_start\"\ncommand = {}\ncontinue_on_error = false\n",
                toml::Value::String(command)
            ),
        )
        .expect("hooks");

        let config = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir,
            workspace_plugins_dir: workspace.join(".codewhale/plugins"),
            builtin_plugin_dirs: Vec::new(),
            state_path: temp.path().join("state/plugin-state.json"),
        };
        let mut registry = discover_with_config(&config);
        registry.trust("runtime-demo").expect("trust plugin");
        registry.enable("runtime-demo").expect("enable plugin");
        let registry = discover_with_config(&config);
        assert!(
            registry.is_active("runtime-demo"),
            "restart restores plugin"
        );

        Self {
            _temp: temp,
            workspace,
            marker,
            config,
            registry,
        }
    }

    pub fn disable_from_fresh_registry(&self) -> PluginRegistry {
        let mut registry = discover_with_config(&self.config);
        registry.disable("runtime-demo").expect("disable plugin");
        discover_with_config(&self.config)
    }

    pub fn revoke_from_fresh_registry(&self) -> PluginRegistry {
        let mut registry = discover_with_config(&self.config);
        registry
            .revoke_trust("runtime-demo")
            .expect("revoke plugin");
        discover_with_config(&self.config)
    }
}

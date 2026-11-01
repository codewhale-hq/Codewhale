use super::{ExtensionHostManager, ExtensionHostOptions, HostAttachment};
use crate::command_catalog::{BuiltinCommand, CatalogGuard, CommandCatalog, UserCommand};
use crate::tools::spec::{ToolContext, ToolSpec};
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::discovery::{DiscoveryConfig, discover_with_config};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What the registry is told is a built-in command in the tests that are not
/// about the command table. Those that are (`commands::extension_host_tests`)
/// install the real one: this module may not depend on `crate::commands`.
#[derive(Debug)]
struct StubBuiltinCommands;

impl CommandCatalog for StubBuiltinCommands {
    fn answers_to(&self, name: &str) -> bool {
        matches!(
            name,
            "help" | "trust" | "model" | "jihua" | "zidong" | "stub-alias"
        )
    }
    fn builtins(&self) -> Vec<BuiltinCommand> {
        Vec::new()
    }
    fn user_commands(&self, _: &Path) -> Vec<UserCommand> {
        Vec::new()
    }
}

/// The stub catalog, for this thread until the guard drops.
pub(super) fn stub_builtin_commands() -> CatalogGuard {
    CatalogGuard::install(Arc::new(StubBuiltinCommands))
}

pub fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/extension_host")
}

pub fn fake_authority(plugin_id: &str) -> codewhale_config::PluginAuthority {
    codewhale_config::PluginAuthority {
        plugin_id: codewhale_config::PluginId(plugin_id.to_string()),
        plugin_name: plugin_id.to_string(),
        workspace: PathBuf::from("/w"),
        state_path: PathBuf::from("/s"),
        managed_policy_path: Some(PathBuf::from("/managed-policy.json")),
        managed_policy_source: Some(PathBuf::from("/managed-policy.json")),
        source_manifest: PathBuf::from("/m"),
        staged_manifest: PathBuf::from("/sm"),
        content_hash: format!("hash-{plugin_id}"),
        capability_hash: "cap".to_string(),
        state_generation: 1,
    }
}

/// A Node for the integration tests, or `None` (skip) when there is none and
/// the tests were not explicitly required.
pub fn node_for_tests(test: &str) -> Option<PathBuf> {
    let resolution = codewhale_dependencies::resolve_extension_host_runtime(
        codewhale_config::extension_host::ExtensionHostRuntime::Node,
        None,
        None,
        crate::extension_host::bundle_sha256(),
    );
    match resolution.selected.as_ref() {
        Some(runtime) => Some(runtime.path.clone()),
        None if std::env::var_os("CODEWHALE_EXT_HOST_TESTS").is_some() => panic!(
            "{test}: CODEWHALE_EXT_HOST_TESTS is set but no Node ^22.19 || >=24 was found: {}",
            resolution.failure()
        ),
        None => {
            eprintln!(
                "skipping {test}: no Node ^22.19 || >=24 ({})",
                resolution.failure()
            );
            None
        }
    }
}

/// Fixture plugins installed into a private user plugin dir through the
/// reviewed installer (`plugins::install`, local path), then reviewed
/// (trusted) and enabled through the real registry. Callers must hold a
/// `TestPolicyGuard::extension_host(true)` on this thread.
pub struct FixturePlugins {
    _temp: tempfile::TempDir,
    pub config: DiscoveryConfig,
    pub root: PathBuf,
    /// The stub built-in command catalog, so a fixture plugin's commands can
    /// register; a test about the real table installs its own after this.
    _commands: CatalogGuard,
}

impl FixturePlugins {
    pub async fn new(names: &[&str]) -> Self {
        use codewhale_plugins::install::{
            DEFAULT_MAX_SIZE_BYTES, PluginInstallOutcome, PluginInstallSource, install,
        };
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("project");
        let user = temp.path().join("user");
        std::fs::create_dir_all(&workspace).unwrap();
        for name in names {
            let outcome = install(
                PluginInstallSource::LocalPath(fixtures_dir().join(name)),
                &user,
                DEFAULT_MAX_SIZE_BYTES,
                &codewhale_net::network_policy::NetworkPolicy::default(),
                false,
                &|_| None,
            )
            .await
            .unwrap_or_else(|error| panic!("install {name}: {error:#}"));
            assert!(
                matches!(outcome, PluginInstallOutcome::Installed(ref installed) if installed.name == *name),
                "install {name}: {outcome:?}"
            );
        }
        let config = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: user,
            workspace_plugins_dir: workspace.join(".codewhale/plugins"),
            builtin_plugin_dirs: Vec::new(),
            state_path: temp.path().join("state/plugin-state.json"),
        };
        let mut registry = discover_with_config(&config);
        for name in names {
            registry
                .trust(name)
                .unwrap_or_else(|e| panic!("trust {name}: {e}"));
            registry
                .enable(name)
                .unwrap_or_else(|e| panic!("enable {name}: {e}"));
        }
        let root = temp.path().join("home");
        let fixture = Self {
            _temp: temp,
            config,
            root,
            _commands: stub_builtin_commands(),
        };
        let registry = fixture.registry();
        for name in names {
            assert!(
                registry.is_active(name),
                "{name} must be active under policy v4"
            );
        }
        fixture
    }

    pub fn registry(&self) -> Arc<PluginRegistry> {
        Arc::new(discover_with_config(&self.config))
    }

    pub fn disable(&self, name: &str) -> Arc<PluginRegistry> {
        let mut registry = discover_with_config(&self.config);
        registry.disable(name).unwrap();
        self.registry()
    }

    pub fn workspace(&self) -> &Path {
        &self.config.workspace
    }

    pub fn manager(&self, node: PathBuf) -> Arc<ExtensionHostManager> {
        Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime: codewhale_config::extension_host::ExtensionHostRuntime::Node,
            node_override: Some(node),
            root: Some(self.root.clone()),
            ..Default::default()
        }))
    }
}

pub fn host_tool(engine: &HostAttachment, workspace: &Path, name: &str) -> Arc<dyn ToolSpec> {
    let mut registry = crate::tools::registry::ToolRegistryBuilder::new()
        .build(ToolContext::new(workspace).with_plugin_registry(engine.plugin_view()));
    let installed = engine.install_tools(&mut registry);
    assert!(
        installed.contains(&name.to_string()),
        "{name} not installed: {installed:?}"
    );
    registry.get(name).unwrap()
}

pub fn installed(engine: &HostAttachment, workspace: &Path) -> Vec<String> {
    let mut registry = crate::tools::registry::ToolRegistryBuilder::new()
        .build(ToolContext::new(workspace).with_plugin_registry(engine.plugin_view()));
    engine.install_tools(&mut registry)
}

pub fn plugin_id(fixture: &FixturePlugins, name: &str) -> String {
    fixture
        .registry()
        .get(name)
        .unwrap()
        .id
        .as_str()
        .to_string()
}

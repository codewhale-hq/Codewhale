use std::env::VarError;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::discovery::DiscoveryConfig;
use super::registry::PluginRegistry;

/// Immutable environment inherited from the host before workspace dotenv is
/// loaded. Reviewed plugins may resolve only values from this snapshot.
#[derive(Debug, Clone, Default)]
pub struct HostEnvironment {
    entries: Arc<[(OsString, OsString)]>,
}

impl HostEnvironment {
    #[must_use]
    pub fn capture() -> Self {
        Self {
            entries: std::env::vars_os().collect::<Vec<_>>().into(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn from_entries(entries: impl IntoIterator<Item = (OsString, OsString)>) -> Self {
        Self {
            entries: entries.into_iter().collect::<Vec<_>>().into(),
        }
    }

    #[must_use]
    pub fn entries(&self) -> &[(OsString, OsString)] {
        &self.entries
    }

    #[must_use]
    pub fn get_os(&self, name: &OsStr) -> Option<&OsStr> {
        self.entries
            .iter()
            .rev()
            .find(|(key, _)| environment_names_equal(key, name))
            .map(|(_, value)| value.as_os_str())
    }

    pub fn var(&self, name: &str) -> Result<String, VarError> {
        let Some(value) = self.get_os(OsStr::new(name)) else {
            return Err(VarError::NotPresent);
        };
        value
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| VarError::NotUnicode(value.to_os_string()))
    }
}

fn environment_names_equal(left: &OsStr, right: &OsStr) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

/// Process-lifetime plugin discovery inputs captured before repository-local
/// dotenv files can affect the process environment.
#[derive(Debug, Clone)]
pub struct PluginDiscoveryContext {
    user_plugins_dir: PathBuf,
    state_path: PathBuf,
    builtin_plugin_dirs: Arc<[PathBuf]>,
    host_environment: Arc<HostEnvironment>,
    managed_policy_path: Result<PathBuf, String>,
}

impl PluginDiscoveryContext {
    #[must_use]
    pub fn capture_pre_dotenv() -> Arc<Self> {
        let user_plugins_dir = super::discovery::default_user_plugins_dir();
        let state_path = user_plugins_dir.join("state.json");
        let host_environment = Arc::new(HostEnvironment::capture());
        let managed_policy_path = super::managed_policy::resolve_managed_policy_path(
            &state_path,
            Some(host_environment.as_ref()),
        );
        Arc::new(Self {
            state_path,
            user_plugins_dir,
            builtin_plugin_dirs: Vec::new().into(),
            host_environment,
            managed_policy_path,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn from_config_and_environment(
        config: &DiscoveryConfig,
        host_environment: HostEnvironment,
    ) -> Arc<Self> {
        let managed_policy_path = super::managed_policy::resolve_managed_policy_path(
            &config.state_path,
            Some(&host_environment),
        );
        Arc::new(Self {
            user_plugins_dir: config.user_plugins_dir.clone(),
            state_path: config.state_path.clone(),
            builtin_plugin_dirs: config.builtin_plugin_dirs.clone().into(),
            host_environment: Arc::new(host_environment),
            managed_policy_path,
        })
    }

    pub(super) fn managed_policy_source(&self) -> Result<PathBuf, String> {
        self.managed_policy_path.clone()
    }

    #[must_use]
    pub fn registry_for_workspace(self: &Arc<Self>, workspace: &Path) -> Arc<PluginRegistry> {
        let config = DiscoveryConfig {
            workspace: workspace.to_path_buf(),
            user_plugins_dir: self.user_plugins_dir.clone(),
            workspace_plugins_dir: super::discovery::default_workspace_plugins_dir(workspace),
            builtin_plugin_dirs: self.builtin_plugin_dirs.to_vec(),
            state_path: self.state_path.clone(),
        };
        let mut registry = super::discovery::discover_with_context(&config, Arc::clone(self));
        // An upgrade re-roots the built-ins; keep their review (K4).
        registry.carry_forward_builtin_trust();
        // Only after carry-forward: retiring superseded built-in records is
        // safe once this build holds its own. Unit tests call the collector
        // directly so that no test ever mutates a home implicitly.
        #[cfg(not(any(test, feature = "test-support")))]
        registry.collect_garbage_at_startup();
        Arc::new(registry)
    }

    #[must_use]
    pub fn host_environment(&self) -> Arc<HostEnvironment> {
        Arc::clone(&self.host_environment)
    }

    /// The pre-dotenv user plugins root (`~/.codewhale/plugins`). Exposed for
    /// the install on-ramp (#5182), which must fetch into exactly the root
    /// this context discovers from.
    #[must_use]
    pub fn user_plugins_dir(&self) -> &Path {
        &self.user_plugins_dir
    }

    #[must_use]
    pub fn catalog_stamp_for_workspace(
        &self,
        workspace: &Path,
    ) -> super::discovery::PluginCatalogStamp {
        let mut roots = vec![
            self.user_plugins_dir.clone(),
            super::discovery::default_workspace_plugins_dir(workspace),
        ];
        roots.extend(self.builtin_plugin_dirs.iter().cloned());
        super::discovery::PluginCatalogStamp::capture(roots)
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;

    use super::{HostEnvironment, PluginDiscoveryContext};

    #[test]
    fn host_environment_is_an_immutable_value_snapshot() {
        let snapshot = HostEnvironment::from_entries([(
            OsString::from("PLUGIN_TOKEN"),
            OsString::from("before"),
        )]);

        assert_eq!(snapshot.var("PLUGIN_TOKEN").unwrap(), "before");
        assert!(snapshot.var("MISSING").is_err());
    }

    #[test]
    fn discovery_roots_are_frozen_before_later_environment_changes() {
        let _lock = codewhale_runtime::test_support::lock_test_env();
        let temp = tempfile::tempdir().unwrap();
        let first_home = temp.path().join("first-home");
        let second_home = temp.path().join("second-home");
        let workspace = temp.path().join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let _first =
            codewhale_runtime::test_support::EnvVarGuard::set("CODEWHALE_HOME", &first_home);
        let context = PluginDiscoveryContext::capture_pre_dotenv();
        let _second =
            codewhale_runtime::test_support::EnvVarGuard::set("CODEWHALE_HOME", &second_home);

        for (home, name) in [(&first_home, "before"), (&second_home, "after")] {
            let bundle = home.join("plugins").join(name);
            fs::create_dir_all(&bundle).unwrap();
            fs::write(
                bundle.join("plugin.toml"),
                format!("schema_version = 1\n[plugin]\nname = \"{name}\"\nversion = \"1.0.0\"\n"),
            )
            .unwrap();
        }

        let registry = context.registry_for_workspace(&workspace);
        assert!(registry.get("before").is_some());
        assert!(registry.get("after").is_none());
        assert_eq!(
            registry.state_path(),
            Some(first_home.join("plugins/state.json").as_path())
        );
    }

    #[test]
    fn contextless_rediscovery_remains_empty_and_tracks_the_requested_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let next_workspace = temp.path().join("next-workspace");
        let bundle = next_workspace.join(".codewhale/plugins/ambient");
        fs::create_dir_all(&bundle).unwrap();
        fs::write(
            bundle.join("plugin.toml"),
            "schema_version = 1\n[plugin]\nname = \"ambient\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();

        let registry =
            crate::PluginRegistry::empty(&workspace).rediscover_for_workspace(&next_workspace);
        assert!(registry.is_empty());
        assert_eq!(registry.workspace(), next_workspace);
    }
}

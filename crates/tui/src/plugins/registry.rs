use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::activation::{PluginActivationCapability, PluginActivationPolicy};
use super::managed_policy::{
    ManagedPluginPolicy, ManagedPolicyOutcome, load_managed_policy, resolve_managed_policy_path,
};
use super::manifest::PluginInventory;
use super::path_identity::metadata_is_link_or_reparse;
#[cfg(windows)]
use super::path_identity::windows_file_identity;
use super::types::{
    LoadedPlugin, PluginAuthority, PluginDiagnostic, PluginDiagnosticLevel, PluginId,
    PluginTrustStatus,
};

pub(crate) mod gc;

const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_REVIEW_HISTORY: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginStateFile {
    schema_version: u32,
    #[serde(default)]
    plugins: BTreeMap<PluginId, PersistedPluginState>,
}

impl Default for PluginStateFile {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            plugins: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPluginState {
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    trust: Option<TrustReceipt>,
    #[serde(default)]
    review_history: Vec<TrustReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustReceipt {
    content_hash: String,
    capability_hash: String,
    reviewed_capabilities: PluginInventory,
    reviewed_at: String,
}

#[derive(Debug, Clone, Default)]
pub struct PluginRegistry {
    plugins: BTreeMap<PluginId, LoadedPlugin>,
    names: BTreeMap<String, PluginId>,
    diagnostics: Vec<PluginDiagnostic>,
    state: PluginStateFile,
    state_path: Option<PathBuf>,
    state_error: Option<String>,
    managed_policy: Option<ManagedPluginPolicy>,
    managed_policy_path: Option<PathBuf>,
    managed_policy_error: Option<String>,
    workspace: PathBuf,
    discovery_context: Option<std::sync::Arc<super::context::PluginDiscoveryContext>>,
    catalog_stamp: super::discovery::PluginCatalogStamp,
    /// Ephemeral caller binding, never persisted in plugin state.
    caller_selection: Option<crate::extension_host::composition_scope::SelectionRevision>,
    selected_native_entries: Vec<crate::extension_host::composition_scope::NativePresetRef>,
    /// Catalog-only Native owners with no upstream default. Ephemeral caller
    /// selection data; an empty entry list must not broaden these owners.
    unselected_native_catalogs: BTreeSet<String>,
}

impl PluginRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub(crate) fn caller_selection(
        &self,
    ) -> Option<crate::extension_host::composition_scope::SelectionRevision> {
        self.caller_selection
    }
    pub(crate) fn bind_caller(
        &self,
        revision: crate::extension_host::composition_scope::SelectionRevision,
    ) -> Self {
        let mut view = self.clone();
        view.caller_selection = Some(revision);
        view
    }
    pub(crate) fn selected_native_entries(
        &self,
    ) -> &[crate::extension_host::composition_scope::NativePresetRef] {
        &self.selected_native_entries
    }
    /// Keep a narrowed selector across rediscovery even when its receipt is now
    /// invalid. Desired-owner admission then withdraws it; failure must never
    /// widen this caller back to every Native entry.
    pub(crate) fn retain_native_selection_from(&self, previous: &Self) -> Self {
        let mut view = self.clone();
        if !previous.selected_native_entries.is_empty()
            || !previous.unselected_native_catalogs.is_empty()
        {
            view.selected_native_entries = previous.selected_native_entries.clone();
            view.unselected_native_catalogs = previous.unselected_native_catalogs.clone();
        }
        view
    }

    pub(crate) fn native_catalog_requires_selection(&self, plugin_id: &str) -> bool {
        self.unselected_native_catalogs.contains(plugin_id)
    }

    pub(crate) fn native_entry_selected(&self, plugin_id: &str, path: &str, sha256: &str) -> bool {
        if self.unselected_native_catalogs.contains(plugin_id) {
            return false;
        }
        if self.selected_native_entries.is_empty() {
            return true;
        }
        self.selected_native_entries.iter().any(|selected| {
            selected.plugin_id == plugin_id
                && selected.entry.path == path
                && selected.entry.sha256 == sha256
                && self
                    .get(plugin_id)
                    .is_some_and(|plugin| plugin.content_hash == selected.content_hash)
        })
    }

    /// Select one reviewed Native inventory entry. No additional Agent capability.
    pub(crate) fn with_native_preset(
        &self,
        selected: crate::extension_host::composition_scope::NativePresetRef,
    ) -> Result<Self, String> {
        let (sources, _) =
            super::runtime::active_component_sources(self, PluginActivationCapability::Native);
        let source = sources
            .into_iter()
            .find(|source| {
                source.authority.plugin_id.as_str() == selected.plugin_id
                    && source.authority.content_hash == selected.content_hash
                    && source.path.to_string_lossy() == selected.entry.path
            })
            .ok_or("Native preset is not in the current reviewed inventory")?;
        if let Some(problem) = super::runtime::native_entry_problem(&source.path, true) {
            return Err(problem.into());
        }
        let file = super::manifest::open_bundle_file(&source.path)
            .map_err(|_| "Native preset cannot be opened")?;
        let mut bytes = Vec::new();
        file.take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Native preset cannot be read")?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("Native preset entry exceeds the bundle limit".into());
        }
        if crate::hashing::sha256_hex(&bytes) != selected.entry.sha256 {
            return Err("Native preset bytes changed".into());
        }
        let mut view = self.clone();
        view.caller_selection = None;
        view.unselected_native_catalogs.remove(&selected.plugin_id);
        view.selected_native_entries
            .retain(|entry| entry.plugin_id != selected.plugin_id);
        view.selected_native_entries.push(selected);
        Ok(view)
    }

    /// Construct a fail-closed registry for a workspace without consulting
    /// process environment or filesystem discovery roots.
    #[must_use]
    pub fn empty(workspace: &Path) -> Self {
        Self {
            workspace: workspace.to_path_buf(),
            ..Self::default()
        }
    }

    pub(crate) fn from_discovery(
        plugins: Vec<LoadedPlugin>,
        mut diagnostics: Vec<PluginDiagnostic>,
        state_path: PathBuf,
        workspace: PathBuf,
        discovery_context: Option<std::sync::Arc<super::context::PluginDiscoveryContext>>,
    ) -> Self {
        let (state, state_error) = match load_state(&state_path) {
            Ok(state) => (state, None),
            Err(error) => {
                diagnostics.push(PluginDiagnostic::error(
                    "state-invalid",
                    format!("Plugin state is fail-closed and will not be overwritten: {error}"),
                    Some(state_path.clone()),
                ));
                (PluginStateFile::default(), Some(error))
            }
        };
        let catalog_stamp = discovery_context
            .as_ref()
            .map(|context| context.catalog_stamp_for_workspace(&workspace))
            .unwrap_or_default();
        let host_environment = discovery_context
            .as_ref()
            .map(|context| context.host_environment());
        let policy_path = resolve_managed_policy_path(&state_path, host_environment.as_deref());
        let (managed_policy, managed_policy_error) = match load_managed_policy(&policy_path) {
            ManagedPolicyOutcome::Absent => (None, None),
            ManagedPolicyOutcome::Loaded(policy) => (Some(policy), None),
            ManagedPolicyOutcome::Invalid(error) => {
                diagnostics.push(PluginDiagnostic::error(
                    "policy-invalid",
                    format!(
                        "Managed plugin policy is fail-closed; no plugin may stay enabled until it is repaired or removed: {error}"
                    ),
                    Some(policy_path.clone()),
                ));
                (None, Some(error))
            }
        };
        let mut registry = Self {
            plugins: BTreeMap::new(),
            names: BTreeMap::new(),
            diagnostics,
            state,
            state_path: Some(state_path),
            state_error,
            managed_policy,
            managed_policy_path: Some(policy_path),
            managed_policy_error,
            workspace,
            discovery_context,
            catalog_stamp,
            caller_selection: None,
            selected_native_entries: Vec::new(),
            unselected_native_catalogs: BTreeSet::new(),
        };
        for plugin in plugins {
            registry.register_loaded(plugin);
        }
        registry.apply_state();
        registry
    }

    fn register_loaded(&mut self, plugin: LoadedPlugin) {
        self.names
            .insert(plugin.name().to_string(), plugin.id.clone());
        self.plugins.insert(plugin.id.clone(), plugin);
    }

    fn apply_state(&mut self) {
        let state_path = self.state_path.clone();
        // Hoisted so the loop below can borrow `self.plugins` mutably: a
        // malformed policy fails closed (nothing stays enabled), a valid one
        // forbids whatever it does not allow, and an absent one forbids nothing.
        let managed_policy_invalid = self.managed_policy_error.is_some();
        let managed_policy = self.managed_policy.clone();
        for (id, plugin) in &mut self.plugins {
            let persisted = self.state.plugins.get(id);
            plugin.state_generation = persisted.map_or(0, |state| state.generation);
            plugin.enabled = persisted.is_some_and(|state| state.enabled);
            plugin.trust_status = match persisted.and_then(|state| state.trust.as_ref()) {
                Some(receipt) if receipt.capability_hash != plugin.capability_hash => {
                    PluginTrustStatus::CapabilitiesChanged
                }
                Some(receipt) if receipt.content_hash != plugin.content_hash => {
                    PluginTrustStatus::ContentChanged
                }
                Some(_) => PluginTrustStatus::Trusted,
                None => PluginTrustStatus::NeverReviewed,
            };
            if self.state_error.is_some() {
                plugin.enabled = false;
                plugin.trust_status = PluginTrustStatus::NeverReviewed;
            }
            // The managed policy is enforced here — inside the single choke
            // point that turns persisted state into live enablement — so a
            // plugin enabled before the policy arrived, or hand-edited to
            // `enabled: true`, never comes back enabled. There is no window
            // in which a forbidden plugin is observably active.
            let policy_forbids = managed_policy_invalid
                || managed_policy
                    .as_ref()
                    .is_some_and(|policy| !policy.allows(id));
            if policy_forbids {
                plugin.enabled = false;
            }
            plugin.staged_root = state_path.as_deref().and_then(|state_path| {
                let staged_root = runtime_stage_path(state_path, id, &plugin.content_hash);
                staged_bundle_matches(&staged_root, &plugin.content_hash, &plugin.capability_hash)
                    .then_some(staged_root)
            });
            if let Some(staged_root) = plugin.staged_root.clone() {
                match super::discovery::load_staged_skill_snapshots(
                    &staged_root,
                    &plugin.content_hash,
                    &plugin.capability_hash,
                ) {
                    Ok(snapshots) => plugin.skill_snapshots = snapshots,
                    Err(error) => {
                        plugin.staged_root = None;
                        plugin.enabled = false;
                        plugin.diagnostics.push(PluginDiagnostic::error(
                            "staged-skill-invalid",
                            format!("Plugin runtime Skill snapshot is fail-closed: {error}"),
                            Some(staged_root),
                        ));
                    }
                }
            }
        }
        if self.selected_native_entries.is_empty() && self.unselected_native_catalogs.is_empty() {
            (
                self.selected_native_entries,
                self.unselected_native_catalogs,
            ) = super::native_presets::default_selection(self);
        }
    }

    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Re-discover for a new workspace using the immutable pre-dotenv roots
    /// and environment. Registries without a context are test/ad-hoc values
    /// and remain fail-closed instead of consulting ambient process state.
    #[must_use]
    pub fn catalog_stamp(&self) -> &super::discovery::PluginCatalogStamp {
        &self.catalog_stamp
    }

    #[must_use]
    pub fn live_catalog_stamp(&self) -> super::discovery::PluginCatalogStamp {
        self.discovery_context
            .as_ref()
            .map_or_else(super::discovery::PluginCatalogStamp::default, |context| {
                context.catalog_stamp_for_workspace(&self.workspace)
            })
    }

    /// True when a plugin bundle directory has appeared, vanished, or been
    /// rewritten since this registry was last discovered. Does not auto-reload.
    #[must_use]
    pub fn on_disk_catalog_changed(&self) -> bool {
        self.discovery_context.is_some() && self.live_catalog_stamp() != self.catalog_stamp
    }

    #[must_use]
    pub fn rediscover_for_workspace(&self, workspace: &Path) -> std::sync::Arc<Self> {
        self.discovery_context.as_ref().map_or_else(
            || std::sync::Arc::new(Self::empty(workspace)),
            |context| context.registry_for_workspace(workspace),
        )
    }

    #[must_use]
    pub fn host_environment(&self) -> Option<std::sync::Arc<super::context::HostEnvironment>> {
        self.discovery_context
            .as_ref()
            .map(|context| context.host_environment())
    }

    #[cfg(test)]
    pub(crate) fn replace_skill_snapshots_for_test(
        &mut self,
        selector: &str,
        snapshots: Vec<super::types::PluginSkillSnapshot>,
    ) {
        let id = self
            .resolve_id(selector)
            .cloned()
            .expect("test plugin exists");
        self.plugins
            .get_mut(&id)
            .expect("test plugin exists")
            .skill_snapshots = snapshots;
    }

    #[must_use]
    pub fn authority_for(&self, selector: &str) -> Option<PluginAuthority> {
        self.get(selector)
            .and_then(|plugin| plugin.authority(self.state_path.clone()?, self.workspace.clone()))
    }

    #[must_use]
    pub fn list(&self) -> Vec<&LoadedPlugin> {
        let mut plugins = self.plugins.values().collect::<Vec<_>>();
        plugins.sort_by(|left, right| {
            left.scope
                .cmp(&right.scope)
                .then_with(|| left.name().cmp(right.name()))
                .then_with(|| left.id.cmp(&right.id))
        });
        plugins
    }

    #[must_use]
    pub fn get(&self, selector: &str) -> Option<&LoadedPlugin> {
        let id = self.resolve_id(selector)?;
        self.plugins.get(id)
    }

    #[must_use]
    pub fn active_plugins(&self) -> Vec<&LoadedPlugin> {
        self.list()
            .into_iter()
            .filter(|plugin| plugin.active())
            .collect()
    }

    /// Compatibility name retained for the MCP adapter. Unlike the old
    /// registry this returns only trusted, active bundles.
    #[must_use]
    pub fn list_enabled(&self) -> Vec<&LoadedPlugin> {
        self.active_plugins()
    }

    #[must_use]
    pub fn enabled_plugins(&self) -> Vec<&LoadedPlugin> {
        self.list()
            .into_iter()
            .filter(|plugin| plugin.enabled)
            .collect()
    }

    #[must_use]
    pub fn is_enabled(&self, selector: &str) -> bool {
        self.get(selector).is_some_and(|plugin| plugin.enabled)
    }

    #[must_use]
    pub fn is_active(&self, selector: &str) -> bool {
        self.get(selector).is_some_and(LoadedPlugin::active)
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[PluginDiagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn validation_is_clean(&self) -> bool {
        !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.level == PluginDiagnosticLevel::Error)
            && self.plugins.values().all(|plugin| {
                !plugin
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.level == PluginDiagnosticLevel::Error)
            })
    }

    #[must_use]
    pub fn state_error(&self) -> Option<&str> {
        self.state_error.as_deref()
    }

    #[must_use]
    pub fn state_path(&self) -> Option<&Path> {
        self.state_path.as_deref()
    }

    /// The fail-closed load error for the managed plugin policy, if the
    /// document exists but could not be used. `None` means no policy file was
    /// found or the loaded policy is valid.
    #[must_use]
    pub fn managed_policy_error(&self) -> Option<&str> {
        self.managed_policy_error.as_deref()
    }

    /// The resolved managed policy source: the env override when set,
    /// otherwise `managed-policy.json` next to the plugin state file.
    #[must_use]
    pub fn managed_policy_path(&self) -> Option<&Path> {
        self.managed_policy_path.as_deref()
    }

    /// The pre-dotenv user plugins root, when this registry was built from a
    /// discovery context. Registries without one (tests, fail-closed ad-hoc
    /// values) return `None`, and the mutation controller refuses to write.
    #[must_use]
    pub fn user_plugins_dir(&self) -> Option<&Path> {
        self.discovery_context
            .as_ref()
            .map(|context| context.user_plugins_dir())
    }

    /// Remove the persisted state entry for a bundle. This is the uninstall
    /// hook (#5182): the caller deletes the bundle bits first, then prunes
    /// the entry through the same locked, fail-closed state transaction used
    /// by trust/enable/disable/revoke.
    pub fn prune_state_entry(&mut self, selector: &str) -> Result<(), String> {
        let id = self
            .resolve_id(selector)
            .cloned()
            .ok_or_else(|| format!("Plugin bundle `{selector}` was not found"))?;
        self.commit_state_change(|state| {
            state.plugins.remove(&id);
            Ok(())
        })
    }

    pub fn trust(&mut self, selector: &str) -> Result<(), String> {
        let plugin = self
            .get(selector)
            .ok_or_else(|| format!("Plugin bundle `{selector}` was not found"))?;
        let plugin = plugin.clone();
        let id = plugin.id.clone();
        let state_path = self
            .state_path
            .as_deref()
            .ok_or_else(|| "Plugin registry has no persistence store".to_string())?;
        stage_bundle(state_path, &plugin)?;
        let receipt = TrustReceipt {
            content_hash: plugin.content_hash.clone(),
            capability_hash: plugin.capability_hash.clone(),
            reviewed_capabilities: plugin.inventory.clone(),
            reviewed_at: chrono::Utc::now().to_rfc3339(),
        };
        self.commit_state_change(|state| {
            let entry = state.plugins.entry(id).or_default();
            entry.generation = entry
                .generation
                .checked_add(1)
                .ok_or_else(|| "Plugin authority generation is exhausted".to_string())?;
            // Trust records review and staging only. Even if an older state
            // kept the enablement bit across revocation or content drift,
            // re-review must never reactivate the bundle implicitly.
            entry.enabled = false;
            entry.trust = Some(receipt.clone());
            entry.review_history.push(receipt);
            if entry.review_history.len() > MAX_REVIEW_HISTORY {
                let remove = entry.review_history.len() - MAX_REVIEW_HISTORY;
                entry.review_history.drain(..remove);
            }
            Ok(())
        })
    }

    pub fn revoke_trust(&mut self, selector: &str) -> Result<(), String> {
        let id = self
            .resolve_id(selector)
            .cloned()
            .ok_or_else(|| format!("Plugin bundle `{selector}` was not found"))?;
        self.commit_state_change(|state| {
            let entry = state.plugins.entry(id).or_default();
            entry.generation = entry
                .generation
                .checked_add(1)
                .ok_or_else(|| "Plugin authority generation is exhausted".to_string())?;
            entry.trust = None;
            Ok(())
        })
    }

    pub fn enable(&mut self, selector: &str) -> Result<(), String> {
        let plugin = self
            .get(selector)
            .ok_or_else(|| format!("Plugin bundle `{selector}` was not found"))?
            .clone();
        // The organization policy is the outermost gate: re-read it so a
        // document that landed after discovery still refuses, and refuse
        // before diagnosing trust or staging the user can never act on.
        self.refresh_managed_policy();
        if let Some(error) = self.managed_policy_error.as_deref() {
            let path = self.managed_policy_path.as_deref().map_or_else(
                || "(unknown path)".to_string(),
                |path| path.display().to_string(),
            );
            return Err(format!(
                "Managed plugin policy at {path} is invalid, so no plugin may be enabled: {error}"
            ));
        }
        if self
            .managed_policy
            .as_ref()
            .is_some_and(|policy| !policy.allows(&plugin.id))
        {
            return Err(format!(
                "Plugin bundle `{}` is forbidden by the managed plugin policy: `{}` is not on the plugin allowlist",
                plugin.name(),
                plugin.id.as_str()
            ));
        }
        if !plugin.trusted() {
            return Err(format!(
                "Plugin bundle `{}` requires capability review before enablement (trust: {})",
                plugin.name(),
                plugin.trust_status.as_str()
            ));
        }
        if plugin.staged_root.is_none() {
            return Err(format!(
                "Plugin bundle `{}` has no verified Codewhale runtime snapshot; review and trust it again before enablement",
                plugin.name()
            ));
        }
        if !plugin.applicable {
            return Err(format!(
                "Plugin bundle `{}` does not apply to this host",
                plugin.name()
            ));
        }
        if !plugin.inventory.can_activate_supported_components() {
            let unsupported = plugin.inventory.unsupported_labels();
            return Err(format!(
                "Plugin bundle `{}` has no supported declarative components to activate; inactive: {}",
                plugin.name(),
                unsupported.join(", ")
            ));
        }
        let id = plugin.id.clone();
        self.commit_state_change(|state| {
            let entry = state.plugins.entry(id).or_default();
            entry.generation = entry
                .generation
                .checked_add(1)
                .ok_or_else(|| "Plugin authority generation is exhausted".to_string())?;
            entry.enabled = true;
            Ok(())
        })
    }

    pub fn disable(&mut self, selector: &str) -> Result<(), String> {
        let id = self
            .resolve_id(selector)
            .cloned()
            .ok_or_else(|| format!("Plugin bundle `{selector}` was not found"))?;
        self.commit_state_change(|state| {
            let entry = state.plugins.entry(id).or_default();
            entry.generation = entry
                .generation
                .checked_add(1)
                .ok_or_else(|| "Plugin authority generation is exhausted".to_string())?;
            entry.enabled = false;
            Ok(())
        })
    }

    /// Carry a built-in bundle's review across Codewhale upgrades (K4).
    ///
    /// Each build materializes its built-ins under a digest-named snapshot
    /// root and a plugin id is bound to its root, so an upgrade presents the
    /// same built-in under a new id with no persisted state. Left alone it is
    /// `NeverReviewed` and disabled, which turns Computer Use off for every
    /// user who had enabled it. Once per new id, the newest review of a
    /// same-named built-in is carried forward:
    ///
    /// * capability hash unchanged: the review stands for the new bytes. The
    ///   bundle is staged and re-receipted under its new id and keeps its
    ///   prior enablement.
    /// * capability hash changed: the prior receipt is recorded as is, so the
    ///   bundle reports `capabilities-changed` and stays disabled until the
    ///   user reviews the changes.
    ///
    /// Fail-closed: nothing is carried when the new id already has state,
    /// when the most recently reviewed same-named predecessor has since been
    /// revoked, or when the state file is invalid. Older ids are left
    /// untouched, so a still-running older binary keeps its own authority.
    /// Only the built-in scope is ever carried; user and workspace bundles
    /// still require review of the exact bytes on disk.
    pub(crate) fn carry_forward_builtin_trust(&mut self) {
        if self.state_error.is_some() || self.state_path.is_none() {
            return;
        }
        let candidates: Vec<LoadedPlugin> = self
            .plugins
            .values()
            .filter(|plugin| plugin.scope == super::types::PluginScope::Builtin)
            .filter(|plugin| builtin_predecessor(&self.state, &plugin.id, plugin.name()).is_some())
            .cloned()
            .collect();
        for plugin in candidates {
            if let Err(error) = self.carry_forward_one_builtin(&plugin) {
                tracing::warn!(
                    target: "plugins",
                    plugin = plugin.name(),
                    %error,
                    "built-in plugin review could not be carried across the upgrade; it needs review again"
                );
            }
        }
    }

    fn carry_forward_one_builtin(&mut self, plugin: &LoadedPlugin) -> Result<(), String> {
        let state_path = self
            .state_path
            .clone()
            .ok_or_else(|| "Plugin registry has no persistence store".to_string())?;
        let same_capabilities = builtin_predecessor(&self.state, &plugin.id, plugin.name())
            .and_then(|entry| entry.trust.as_ref())
            .is_some_and(|receipt| receipt.capability_hash == plugin.capability_hash);
        // Staging is content-addressed and idempotent, so it runs before the
        // state lock; the decision is re-derived from the locked state below.
        if same_capabilities {
            stage_bundle(&state_path, plugin)?;
        }
        let id = plugin.id.clone();
        let name = plugin.name().to_string();
        let applicable = plugin.applicable;
        let carried = TrustReceipt {
            content_hash: plugin.content_hash.clone(),
            capability_hash: plugin.capability_hash.clone(),
            reviewed_capabilities: plugin.inventory.clone(),
            reviewed_at: chrono::Utc::now().to_rfc3339(),
        };
        self.commit_state_change(|state| {
            let Some(predecessor) = builtin_predecessor(state, &id, &name).cloned() else {
                return Ok(());
            };
            let Some(prior) = predecessor.trust else {
                return Ok(());
            };
            let mut entry = PersistedPluginState {
                generation: 1,
                enabled: false,
                trust: None,
                review_history: predecessor.review_history,
            };
            if prior.capability_hash == carried.capability_hash {
                if !same_capabilities {
                    // Changed under us to a state that needs a staged copy we
                    // did not make; the next discovery carries it.
                    return Ok(());
                }
                entry.enabled = predecessor.enabled && applicable;
                entry.trust = Some(carried.clone());
                entry.review_history.push(carried);
                if entry.review_history.len() > MAX_REVIEW_HISTORY {
                    let remove = entry.review_history.len() - MAX_REVIEW_HISTORY;
                    entry.review_history.drain(..remove);
                }
            } else {
                entry.trust = Some(prior);
            }
            state.plugins.insert(id, entry);
            Ok(())
        })
    }

    fn commit_state_change(
        &mut self,
        mutate: impl FnOnce(&mut PluginStateFile) -> Result<(), String>,
    ) -> Result<(), String> {
        if let Some(error) = &self.state_error {
            return Err(format!(
                "Plugin state is fail-closed; repair or move the malformed state file before mutating it: {error}"
            ));
        }
        let Some(path) = self.state_path.as_deref() else {
            return Err("Plugin registry has no persistence store".to_string());
        };
        let lock_path = state_lock_path(path);
        if let Some(parent) = lock_path.parent() {
            ensure_private_plugin_state_directory(parent)?;
        }
        let lock_file = open_state_lock(&lock_path, true)?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .write()
            .map_err(|e| format!("failed to lock plugin state for update: {e}"))?;
        let mut next = load_state_unlocked(path)?;
        mutate(&mut next)?;
        save_state(path, &next)?;
        self.state = next;
        // Runtime selection rechecks the persisted review through its own
        // read lock. Publish the completed state transaction before deriving
        // that view; retaining the writer here would deadlock on enable.
        drop(_guard);
        self.apply_state();
        Ok(())
    }

    /// Re-read the policy document from its resolved path so `enable`
    /// enforces the on-disk document even when it landed or changed after
    /// discovery. Refreshing never touches diagnostics: the discovery-time
    /// `policy-invalid` diagnostic and the `enable` refusal carry the error.
    fn refresh_managed_policy(&mut self) {
        let Some(path) = self.managed_policy_path.clone() else {
            return;
        };
        match load_managed_policy(&path) {
            ManagedPolicyOutcome::Absent => {
                self.managed_policy = None;
                self.managed_policy_error = None;
            }
            ManagedPolicyOutcome::Loaded(policy) => {
                self.managed_policy = Some(policy);
                self.managed_policy_error = None;
            }
            ManagedPolicyOutcome::Invalid(error) => {
                self.managed_policy = None;
                self.managed_policy_error = Some(error);
            }
        }
    }

    fn resolve_id(&self, selector: &str) -> Option<&PluginId> {
        self.plugins
            .keys()
            .find(|id| id.as_str() == selector)
            .or_else(|| self.names.get(selector))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.plugins.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.plugins.is_empty()
    }
}

fn load_state(path: &Path) -> Result<PluginStateFile, String> {
    validate_existing_plugin_state_parent(path)?;
    let lock_path = state_lock_path(path);
    let lock_exists = path_entry_exists(&lock_path)?;
    if lock_exists {
        let lock_file = open_state_lock(&lock_path, false)?;
        let lock = fd_lock::RwLock::new(lock_file);
        let _guard = lock
            .read()
            .map_err(|e| format!("failed to read-lock plugin state: {e}"))?;
        return load_state_unlocked(path);
    }
    load_state_unlocked(path)
}

/// Maximum bytes read from the plugin state file.
const MAX_PLUGIN_STATE_BYTES: u64 = 16 * 1024 * 1024;

fn load_state_unlocked(path: &Path) -> Result<PluginStateFile, String> {
    let Some(file) = open_existing_regular_file(path, false)? else {
        return Ok(PluginStateFile::default());
    };
    let mut raw = String::new();
    file.take(MAX_PLUGIN_STATE_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
    if raw.len() as u64 > MAX_PLUGIN_STATE_BYTES {
        return Err(format!(
            "plugin state {} exceeds the {} MiB limit",
            path.display(),
            MAX_PLUGIN_STATE_BYTES / (1024 * 1024)
        ));
    }
    let state: PluginStateFile = serde_json::from_str(&raw)
        .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
    if state.schema_version != STATE_SCHEMA_VERSION {
        return Err(format!(
            "unsupported plugin state schema {}; expected {STATE_SCHEMA_VERSION}",
            state.schema_version
        ));
    }
    Ok(state)
}

fn save_state(path: &Path, state: &PluginStateFile) -> Result<(), String> {
    save_state_with_hardener(path, state, harden_plugin_state_file)
}

/// Publish a hardened JSON document atomically. Generic over the schema so
/// sibling Codewhale-owned stores (e.g. the marketplace catalog store) share
/// the exact durability and no-follow path this registry was audited for.
pub(crate) fn save_state_with_hardener<T: serde::Serialize>(
    path: &Path,
    state: &T,
    harden_temporary: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "Plugin state path must have a private parent directory".to_string())?;
    ensure_private_plugin_state_directory(parent)?;

    let mut body = serde_json::to_string_pretty(state)
        .map_err(|error| format!("failed to serialize {}: {error}", path.display()))?;
    body.push('\n');
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("failed to create private plugin state temp file: {error}"))?;
    temporary
        .write_all(body.as_bytes())
        .map_err(|error| format!("failed to write private plugin state temp file: {error}"))?;
    temporary
        .flush()
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|error| format!("failed to flush private plugin state temp file: {error}"))?;

    // Restrict the exact temporary object before its atomic rename publishes
    // it under the stable state path. Post-publish hardening leaves a Windows
    // race in which another local principal can open the inherited DACL.
    #[cfg(windows)]
    {
        // `NamedTempFile` keeps a writer handle open. The ACL hardener
        // intentionally opens its target with FILE_SHARE_READ only, so close
        // that writer before safely reopening the name for ACL mutation. Its
        // parent was hardened above, which prevents another principal from
        // replacing the temporary entry between those operations.
        let temporary = temporary.into_temp_path();
        harden_temporary(temporary.as_ref())?;
        persist_plugin_state(temporary, path)
    }
    #[cfg(not(windows))]
    {
        harden_temporary(temporary.path())?;
        persist_plugin_state(temporary, path)
    }
}

#[cfg(unix)]
fn persist_plugin_state(temporary: tempfile::NamedTempFile, path: &Path) -> Result<(), String> {
    persist_plugin_state_with_directory_sync(temporary, path, fs::File::sync_all)
}

#[cfg(unix)]
fn persist_plugin_state_with_directory_sync(
    temporary: tempfile::NamedTempFile,
    path: &Path,
    sync_directory: impl FnOnce(&fs::File) -> std::io::Result<()>,
) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt as _;

    temporary
        .persist(path)
        .map_err(|error| error.error)
        .map_err(|error| format!("failed to atomically persist {}: {error}", path.display()))?;
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "Plugin state path must have a private parent directory".to_string())?;
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(parent)
        .map_err(|error| {
            format!("failed to open plugin state directory for durability sync: {error}")
        })?;
    sync_directory(&directory).map_err(|error| {
        format!(
            "plugin state was published but its directory durability could not be confirmed: {error}"
        )
    })
}

#[cfg(windows)]
fn persist_plugin_state(mut temporary: tempfile::TempPath, path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows::Win32::Foundation::WIN32_ERROR;
    use windows::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_TEMPORARY, MOVEFILE_REPLACE_EXISTING,
        MOVEFILE_WRITE_THROUGH, MoveFileExW, SetFileAttributesW,
    };
    use windows::core::PCWSTR;

    fn wide_path(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let temporary_path = temporary.to_path_buf();
    let temporary_wide = wide_path(&temporary_path);
    let destination_wide = wide_path(path);
    // NamedTempFile marks the source as temporary. Clear only that temporary
    // caching hint before publication, matching tempfile's own persistence
    // contract while retaining the owner-only DACL applied above.
    // SAFETY: `temporary_wide` is NUL-terminated and live.
    unsafe {
        SetFileAttributesW(
            PCWSTR::from_raw(temporary_wide.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
        )
    }
    .map_err(|error| {
        format!("failed to prepare private plugin state temp file for publication: {error}")
    })?;

    // The state lock serializes writers, but not everything that opens the
    // stable file: another registry re-applies the state directory's
    // inheritable ACL before it takes that lock, and Defender or the indexer
    // can scan the file. Each holds the destination only briefly, and
    // MoveFileExW then reports a refusal that clears on its own. Re-attempt
    // only the rename, on the schedule the other atomic writers share; the
    // hardened temporary and the stable file are both left untouched.
    let mut attempt = 0;
    // SAFETY: both paths are NUL-terminated and live.
    while let Err(error) = unsafe {
        MoveFileExW(
            PCWSTR::from_raw(temporary_wide.as_ptr()),
            PCWSTR::from_raw(destination_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } {
        // MoveFileExW reports an HRESULT; the shared classifier reads the
        // Win32 code it wraps.
        let backoff = WIN32_ERROR::from_error(&error).and_then(|code| {
            crate::utils::windows_publish_retry_delay(
                &std::io::Error::from_raw_os_error(code.0 as i32),
                attempt,
            )
        });
        if let Some(backoff) = backoff {
            std::thread::sleep(backoff);
            attempt += 1;
            continue;
        }
        // Restore tempfile's cleanup hint on the still-private source. The
        // stable state path remains untouched when MoveFileExW fails.
        // SAFETY: `temporary_wide` is NUL-terminated and live.
        let _ = unsafe {
            SetFileAttributesW(
                PCWSTR::from_raw(temporary_wide.as_ptr()),
                FILE_ATTRIBUTE_TEMPORARY,
            )
        };
        return Err(format!(
            "failed to atomically and durably persist {}: {error}",
            path.display()
        ));
    }

    // The old temporary pathname no longer exists. Disarm TempPath cleanup.
    temporary.disable_cleanup(true);
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
fn persist_plugin_state(temporary: tempfile::NamedTempFile, path: &Path) -> Result<(), String> {
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .map(|_| ())
        .map_err(|error| format!("failed to atomically persist {}: {error}", path.display()))
}

pub(crate) fn state_lock_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_else(|| "state.json".into());
    name.push(".lock");
    path.with_file_name(name)
}

#[cfg(not(windows))]
pub(crate) fn open_state_lock(path: &Path, create: bool) -> Result<fs::File, String> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|e| format!("failed to open plugin state lock: {e}"))?;
    validate_opened_regular_file(path, &file)?;
    // Discovery/doctor opens existing locks with `create=false` and must be
    // byte-for-byte and descriptor-for-descriptor non-mutating. ACL/mode
    // hardening belongs only to trust/enable/disable/revoke updates.
    if create {
        harden_plugin_state_file(path)?;
    }
    Ok(file)
}

#[cfg(windows)]
pub(crate) fn open_state_lock(path: &Path, create: bool) -> Result<fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt as _;

    const LOCK_ACCESS_WITH_OWNER: u32 = 0x001e_019f;
    const LOCK_ACCESS_WITHOUT_OWNER: u32 = 0x0016_019f;

    let (file, owner_mode) = if create {
        match open_windows_state_lock(path, true, LOCK_ACCESS_WITH_OWNER) {
            Ok(file) => (file, WindowsAclOwnerMode::NormalizeCurrentUser),
            Err(error) if is_windows_access_denied(&error) => {
                // The first attempt can only be retried when Windows denied
                // WRITE_OWNER. Do not recreate the entry here: a disappeared
                // lock is a concurrent mutation that must fail closed instead
                // of turning into a fresh object with an unchecked owner.
                let file = open_windows_state_lock(path, false, LOCK_ACCESS_WITHOUT_OWNER)
                    .map_err(|error| format!("failed to open plugin state lock: {error}"))?;
                (file, WindowsAclOwnerMode::VerifyCurrentUser)
            }
            Err(error) => {
                return Err(format!("failed to open plugin state lock: {error}"));
            }
        }
    } else {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .truncate(false)
            // Open the reparse point itself. `validate_opened_regular_file`
            // then rejects it instead of following it to an unrelated target.
            .custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
        let file = options
            .open(path)
            .map_err(|error| format!("failed to open plugin state lock: {error}"))?;
        (file, WindowsAclOwnerMode::VerifyCurrentUser)
    };

    validate_opened_regular_file(path, &file)?;
    // Discovery/doctor opens existing locks with `create=false` and must be
    // byte-for-byte and descriptor-for-descriptor non-mutating. ACL/mode
    // hardening belongs only to trust/enable/disable/revoke updates.
    if create {
        harden_opened_plugin_state_file(path, &file, owner_mode)?;
    }
    Ok(file)
}

#[cfg(windows)]
fn open_windows_state_lock(
    path: &Path,
    create: bool,
    access_mode: u32,
) -> std::io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt as _;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(create)
        .truncate(false)
        // Open the reparse point itself. `validate_opened_regular_file` then
        // rejects it instead of following it to an unrelated ACL target.
        .custom_flags(0x0020_0000) // FILE_FLAG_OPEN_REPARSE_POINT
        .access_mode(access_mode)
        .open(path)
}

pub(crate) fn path_entry_exists(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("failed to inspect {}: {error}", path.display())),
    }
}

/// Open an existing state file without following its final link/reparse point.
/// `None` is returned only for a genuinely absent entry; an existing unsafe
/// object always fails closed.
pub(crate) fn open_existing_regular_file(
    path: &Path,
    write: bool,
) -> Result<Option<fs::File>, String> {
    if !path_entry_exists(path)? {
        return Ok(None);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(write);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // A swapped FIFO must not block before the handle can be validated.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options
        .open(path)
        .map_err(|e| format!("failed to open {} safely: {e}", path.display()))?;
    validate_opened_regular_file(path, &file)?;
    Ok(Some(file))
}

#[cfg(unix)]
pub(crate) fn validate_opened_regular_file(path: &Path, file: &fs::File) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = file
        .metadata()
        .map_err(|e| format!("failed to inspect opened {}: {e}", path.display()))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(format!(
            "{} must be one regular, non-hard-linked file",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub(crate) fn validate_opened_regular_file(path: &Path, file: &fs::File) -> Result<(), String> {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    let metadata = file
        .metadata()
        .map_err(|e| format!("failed to inspect opened {}: {e}", path.display()))?;
    let identity = windows_file_identity(file)
        .map_err(|e| format!("failed to identify opened {}: {e}", path.display()))?;
    if !metadata.is_file()
        || identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || identity.links != 1
    {
        return Err(format!(
            "{} must be one regular, non-reparse, non-hard-linked file",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
pub(crate) fn validate_opened_regular_file(path: &Path, file: &fs::File) -> Result<(), String> {
    let metadata = file
        .metadata()
        .map_err(|e| format!("failed to inspect opened {}: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} must be a regular file", path.display()));
    }
    Ok(())
}

pub(crate) fn validate_existing_plugin_state_parent(path: &Path) -> Result<(), String> {
    let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Ok(());
    };
    match fs::symlink_metadata(parent) {
        Ok(_) => validate_plugin_state_directory_for_read(parent),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "failed to inspect plugin state directory {}: {error}",
            parent.display()
        )),
    }
}

#[cfg(unix)]
fn validate_plugin_state_directory_for_read(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};

    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            format!("failed to open plugin state directory without following links: {error}")
        })?;
    let metadata = directory
        .metadata()
        .map_err(|error| format!("failed to inspect opened plugin state directory: {error}"))?;
    // SAFETY: geteuid has no pointer or lifetime preconditions.
    let effective_uid = unsafe { libc::geteuid() };
    validate_unix_plugin_state_directory_fields(
        metadata.is_dir(),
        metadata.uid(),
        metadata.permissions().mode(),
        effective_uid,
    )
}

#[cfg(unix)]
fn validate_unix_plugin_state_directory_fields(
    is_directory: bool,
    owner_uid: u32,
    mode: u32,
    effective_uid: u32,
) -> Result<(), String> {
    if !is_directory || owner_uid != effective_uid || mode & 0o077 != 0 {
        return Err(
            "Plugin state directory must be current-user-owned and inaccessible to group or other users"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_plugin_state_directory_for_read(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn ensure_private_plugin_state_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt as _;

    if !path_entry_exists(path)? {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(path)
            .map_err(|error| format!("failed to create plugin state directory: {error}"))?;
    }
    validate_plugin_state_directory_for_read(path)
}

#[cfg(windows)]
pub(crate) fn ensure_private_plugin_state_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("failed to create plugin state directory: {error}"))?;
    set_windows_owner_only_acl(path)
}

#[cfg(all(not(unix), not(windows)))]
pub(crate) fn ensure_private_plugin_state_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("failed to create plugin state directory: {error}"))
}

#[cfg(windows)]
pub(crate) fn harden_plugin_state_file(path: &Path) -> Result<(), String> {
    set_windows_owner_only_acl(path)
}

#[cfg(windows)]
fn harden_opened_plugin_state_file(
    path: &Path,
    file: &fs::File,
    owner_mode: WindowsAclOwnerMode,
) -> Result<(), String> {
    validate_opened_regular_file(path, file)?;
    ensure_windows_registry_path_still_opened(path, file)?;
    apply_windows_owner_only_acl(file, 0x001f_01ff, owner_mode)
}

#[cfg(unix)]
pub(crate) fn harden_plugin_state_file(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
        format!(
            "failed to restrict plugin state file permissions for {}: {error}",
            path.display()
        )
    })
}

#[cfg(all(not(unix), not(windows)))]
pub(crate) fn harden_plugin_state_file(_path: &Path) -> Result<(), String> {
    Ok(())
}

/// The newest persisted review of another built-in with this name, when it
/// may be carried to `id`: `id` has no state yet, and the same-named built-in
/// entry with the most recent review still holds its receipt. A revoked entry
/// is dated by its last review, so revoking blocks carrying until the user
/// reviews a build again; ties go to the revocation. Entries that were never
/// reviewed (for example, disabled before any review) granted nothing and are
/// ignored.
fn builtin_predecessor<'a>(
    state: &'a PluginStateFile,
    id: &PluginId,
    name: &str,
) -> Option<&'a PersistedPluginState> {
    if state.plugins.contains_key(id) {
        return None;
    }
    let builtin = super::types::PluginScope::Builtin.as_str();
    let mut newest: Option<(&PersistedPluginState, i64)> = None;
    for (other, entry) in &state.plugins {
        let mut parts = other.as_str().splitn(3, '/');
        if parts.next() != Some(builtin) || parts.nth(1) != Some(name) {
            continue;
        }
        let Some(last_review) = entry.trust.as_ref().or(entry.review_history.last()) else {
            continue;
        };
        let reviewed = chrono::DateTime::parse_from_rfc3339(&last_review.reviewed_at)
            .map_or(i64::MIN, |at| at.timestamp_micros());
        let revoked = entry.trust.is_none();
        if newest.is_none_or(|(_, at)| reviewed > at || (reviewed == at && revoked)) {
            newest = Some((entry, reviewed));
        }
    }
    newest
        .map(|(entry, _)| entry)
        .filter(|entry| entry.trust.is_some())
}

/// Directory name of a plugin id's runtime snapshots under `.runtime/v2`.
fn runtime_stage_key(id: &PluginId) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale-plugin-stage-v2\0");
    hasher.update(id.as_str().as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn runtime_stage_path(state_path: &Path, id: &PluginId, content_hash: &str) -> PathBuf {
    let key = runtime_stage_key(id);
    let state_parent = state_path.parent().unwrap_or_else(|| Path::new("."));
    let state_parent = state_parent
        .canonicalize()
        .unwrap_or_else(|_| state_parent.to_path_buf());
    state_parent
        .join(".runtime")
        .join("v2")
        .join(key)
        .join(content_hash)
}

fn staged_bundle_matches(root: &Path, content_hash: &str, capability_hash: &str) -> bool {
    let Some(manifest_path) = super::agent_plugin::resolve_manifest_path(root) else {
        return false;
    };
    super::manifest::PluginManifest::validate_from_path(&manifest_path).is_ok_and(|validated| {
        validated.content_hash == content_hash
            && validated.capability_hash == capability_hash
            && root
                .canonicalize()
                .is_ok_and(|root| validated.canonical_root == root)
    })
}

fn stage_bundle(state_path: &Path, plugin: &LoadedPlugin) -> Result<PathBuf, String> {
    // Resolve the state directory before deriving the content-addressed path.
    // On macOS an existing ancestor such as `/var` canonicalizes to
    // `/private/var`; when the final `state/` directory does not exist yet,
    // deriving the destination first would preserve the non-canonical prefix
    // and the subsequent containment proof would correctly reject it as an
    // escape. Trust is already the mutating boundary, so creating this private
    // parent here is both safe and necessary for a stable path identity.
    let state_parent = state_path
        .parent()
        .ok_or_else(|| "plugin state path has no parent directory".to_string())?;
    ensure_private_plugin_state_directory(state_parent)?;
    let destination = runtime_stage_path(state_path, &plugin.id, &plugin.content_hash);
    if destination.exists() {
        if !staged_bundle_matches(&destination, &plugin.content_hash, &plugin.capability_hash) {
            return Err(
                "Existing Codewhale plugin runtime snapshot failed content validation; remove the exact .runtime entry and review again"
                    .to_string(),
            );
        }
        // Trust is a mutating boundary, so it may upgrade an older verified
        // snapshot to the finalized non-writable permission contract.
        harden_staged_tree(&destination)?;
        return Ok(destination.canonicalize().unwrap_or(destination));
    }

    let parent = destination
        .parent()
        .ok_or_else(|| "plugin runtime snapshot has no parent".to_string())?;
    ensure_private_runtime_parent(state_path, parent)?;
    let temporary = parent.join(format!(".staging-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir(&temporary)
        .map_err(|e| format!("failed to create temporary plugin runtime snapshot: {e}"))?;
    set_owner_only_directory(&temporary)?;

    let staged = (|| {
        copy_bundle_tree(&plugin.canonical_root, &temporary)?;
        if !staged_bundle_matches(&temporary, &plugin.content_hash, &plugin.capability_hash) {
            return Err(
                "Plugin bundle changed while Codewhale was staging it; no runtime authority was granted"
                    .to_string(),
            );
        }
        // Finalize descendants before activation, but keep the temporary root
        // owner-writable through the atomic rename. macOS rejects renaming a
        // directory whose own mode is already 0500 even when both parents are
        // writable. The destination root is hardened immediately after the
        // rename, before its path is returned or persisted as authority.
        harden_staged_tree_contents(&temporary)?;
        if let Err(error) = fs::rename(&temporary, &destination) {
            // Another process may have won the same content-addressed race.
            // Reuse only after exact validation and hardening at this explicit
            // mutation boundary; every other rename failure remains fatal.
            if staged_bundle_matches(&destination, &plugin.content_hash, &plugin.capability_hash) {
                harden_staged_tree(&destination)?;
                return destination.canonicalize().map_err(|e| {
                    format!("failed to finalize raced plugin runtime snapshot path: {e}")
                });
            }
            return Err(format!(
                "failed to activate content-addressed plugin runtime snapshot: {error}"
            ));
        }
        set_staged_read_only_directory(&destination)?;
        destination
            .canonicalize()
            .map_err(|e| format!("failed to finalize plugin runtime snapshot path: {e}"))
    })();
    if staged.is_err() && temporary.exists() {
        let _ = fs::remove_dir_all(&temporary);
    }
    staged
}

fn ensure_private_runtime_parent(state_path: &Path, parent: &Path) -> Result<(), String> {
    let configured_base = state_path
        .parent()
        .ok_or_else(|| "plugin state path has no parent directory".to_string())?;
    ensure_private_plugin_state_directory(configured_base)?;
    let base_metadata = fs::symlink_metadata(configured_base)
        .map_err(|e| format!("failed to inspect plugin state directory: {e}"))?;
    if metadata_is_link_or_reparse(&base_metadata) || !base_metadata.is_dir() {
        return Err(
            "plugin state directory must not be a symbolic link or reparse point".to_string(),
        );
    }
    // `runtime_stage_path` canonicalizes the same parent. Match that identity
    // here as well (notably `/var` -> `/private/var` on macOS) before proving
    // that every runtime component stays beneath the state directory.
    let base = configured_base
        .canonicalize()
        .map_err(|e| format!("failed to canonicalize plugin state directory: {e}"))?;
    let relative = parent
        .strip_prefix(&base)
        .or_else(|_| parent.strip_prefix(configured_base))
        .map_err(|_| "plugin runtime snapshot escaped the state directory".to_string())?;
    let mut cursor = base;
    for component in relative.components() {
        use std::path::Component;
        let Component::Normal(component) = component else {
            return Err("plugin runtime snapshot contains an invalid path component".to_string());
        };
        cursor.push(component);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata_is_link_or_reparse(&metadata) => {
                return Err(
                    "plugin runtime snapshot directory may not traverse symbolic links or reparse points"
                        .to_string(),
                );
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err("plugin runtime snapshot parent is not a directory".to_string());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&cursor) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = fs::symlink_metadata(&cursor).map_err(|e| {
                            format!(
                                "failed to inspect concurrently created plugin runtime snapshot directory: {e}"
                            )
                        })?;
                        if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
                            return Err(
                                "concurrently created plugin runtime snapshot parent is not a safe directory"
                                    .to_string(),
                            );
                        }
                    }
                    Err(error) => {
                        return Err(format!(
                            "failed to create plugin runtime snapshot directory: {error}"
                        ));
                    }
                }
            }
            Err(error) => {
                return Err(format!(
                    "failed to inspect plugin runtime snapshot directory: {error}"
                ));
            }
        }
        set_owner_only_directory(&cursor)?;
    }
    Ok(())
}

#[derive(Default)]
struct StageBudget {
    files: usize,
    bytes: u64,
}

fn copy_bundle_tree(source: &Path, destination: &Path) -> Result<(), String> {
    let mut budget = StageBudget::default();
    copy_bundle_tree_bounded(source, destination, &mut budget)
}

#[cfg(not(unix))]
fn copy_bundle_tree_bounded(
    source: &Path,
    destination: &Path,
    budget: &mut StageBudget,
) -> Result<(), String> {
    use std::io::Read as _;
    let metadata = fs::symlink_metadata(source)
        .map_err(|e| format!("failed to inspect plugin content during staging: {e}"))?;
    if metadata_is_link_or_reparse(&metadata) {
        return Err("Plugin content changed into a symbolic link during staging".to_string());
    }
    if !metadata.is_dir() {
        return Err("Plugin runtime source is not a directory".to_string());
    }
    #[cfg(windows)]
    let source_guard = open_windows_bundle_directory(source)?;
    #[cfg(windows)]
    ensure_windows_registry_path_still_opened(source, &source_guard)?;
    let mut entries = fs::read_dir(source)
        .map_err(|e| format!("failed to read plugin content during staging: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to enumerate plugin content during staging: {e}"))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|e| format!("failed to inspect plugin entry during staging: {e}"))?;
        if metadata_is_link_or_reparse(&metadata) {
            return Err("Plugin content may not contain symbolic links".to_string());
        }
        if metadata.is_dir() {
            fs::create_dir(&destination_path)
                .map_err(|e| format!("failed to create staged plugin directory: {e}"))?;
            set_owner_only_directory(&destination_path)?;
            copy_bundle_tree_bounded(&source_path, &destination_path, budget)?;
        } else if metadata.is_file() {
            budget.files = budget.files.saturating_add(1);
            if budget.files > 4_096 {
                return Err("Plugin content exceeded the staging file limit".to_string());
            }
            let mut source_file = super::manifest::open_bundle_file(&source_path)
                .map_err(|e| format!("failed to open plugin file without following links: {e}"))?;
            #[cfg(windows)]
            ensure_windows_registry_path_still_opened(&source_path, &source_file)?;
            let mut destination_file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&destination_path)
                .map_err(|e| format!("failed to create staged plugin file: {e}"))?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = source_file
                    .read(&mut buffer)
                    .map_err(|e| format!("failed to read plugin file during staging: {e}"))?;
                if read == 0 {
                    break;
                }
                budget.bytes = budget.bytes.saturating_add(read as u64);
                if budget.bytes > 64 * 1024 * 1024 {
                    return Err("Plugin content exceeded the staging byte limit".to_string());
                }
                destination_file
                    .write_all(&buffer[..read])
                    .map_err(|e| format!("failed to write staged plugin file: {e}"))?;
            }
            destination_file
                .sync_all()
                .map_err(|e| format!("failed to sync staged plugin file: {e}"))?;
            #[cfg(windows)]
            // The containing staging directory is already owner-only. Close
            // the writer before reopening this path with the ACL hardener's
            // deliberately restrictive share mode.
            drop(destination_file);
            preserve_owner_only_file_mode(&destination_path, &metadata)?;
            #[cfg(windows)]
            ensure_windows_registry_path_still_opened(&source_path, &source_file)?;
        } else {
            return Err(
                "Plugin content must contain only regular files and directories".to_string(),
            );
        }
    }
    #[cfg(windows)]
    ensure_windows_registry_path_still_opened(source, &source_guard)?;
    Ok(())
}

#[cfg(windows)]
fn open_windows_bundle_directory(path: &Path) -> Result<fs::File, String> {
    use std::os::windows::fs::OpenOptionsExt as _;

    let file = OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001)
        .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        .open(path)
        .map_err(|e| format!("failed to open plugin directory safely: {e}"))?;
    let metadata = file
        .metadata()
        .map_err(|e| format!("failed to inspect opened plugin directory: {e}"))?;
    let identity = windows_file_identity(&file)
        .map_err(|e| format!("failed to identify opened plugin directory: {e}"))?;
    if !metadata.is_dir() || identity.attributes & 0x0000_0400 != 0 {
        return Err("Plugin directory changed into a reparse point during staging".to_string());
    }
    Ok(file)
}

#[cfg(windows)]
fn ensure_windows_registry_path_still_opened(path: &Path, opened: &fs::File) -> Result<(), String> {
    let after = fs::symlink_metadata(path)
        .map_err(|e| format!("failed to re-inspect staged source path: {e}"))?;
    if metadata_is_link_or_reparse(&after) {
        return Err("Plugin path changed into a reparse point during staging".to_string());
    }
    let expect_directory = if after.is_dir() {
        true
    } else if after.is_file() {
        false
    } else {
        return Err("Plugin path changed into an unsupported object during staging".to_string());
    };
    let current = super::manifest::open_bundle_identity_probe(path, expect_directory)
        .map_err(|e| format!("failed to reopen staged source path safely: {e}"))?;
    let opened = windows_file_identity(opened)
        .map_err(|e| format!("failed to identify retained plugin handle: {e}"))?;
    let current = windows_file_identity(&current)
        .map_err(|e| format!("failed to identify current plugin path: {e}"))?;
    if opened.volume != current.volume || opened.index != current.index {
        return Err("Plugin path identity changed while staging".to_string());
    }
    if opened.links != 1 && after.is_file() {
        return Err("Plugin content may not contain hard-linked files".to_string());
    }
    Ok(())
}

#[cfg(unix)]
fn copy_bundle_tree_bounded(
    source: &Path,
    destination: &Path,
    budget: &mut StageBudget,
) -> Result<(), String> {
    use std::ffi::CString;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;

    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| "plugin runtime source path contains an invalid byte".to_string())?;
    // SAFETY: `source` is a NUL-terminated path and successful descriptors
    // are immediately owned by `OwnedFd`.
    let fd = unsafe {
        libc::open(
            source.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!(
            "failed to open plugin root without following links: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `fd` is a unique successful result from `open` above.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    copy_bundle_directory_fd(fd.as_raw_fd(), destination, budget)
}

#[cfg(unix)]
fn copy_bundle_directory_fd(
    source_fd: std::os::fd::RawFd,
    destination: &Path,
    budget: &mut StageBudget,
) -> Result<(), String> {
    use std::ffi::{CStr, CString, OsString};
    use std::io::Read as _;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStringExt;

    // `fdopendir` owns its descriptor, so duplicate the directory fd retained
    // by this stack frame for subsequent `openat` calls.
    // SAFETY: `source_fd` is an open directory descriptor.
    let iter_fd = unsafe { libc::dup(source_fd) };
    if iter_fd < 0 {
        return Err(format!(
            "failed to duplicate plugin directory descriptor: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: `iter_fd` is a fresh descriptor and ownership transfers to DIR.
    let directory = unsafe { libc::fdopendir(iter_fd) };
    if directory.is_null() {
        // SAFETY: fdopendir failed, so ownership did not transfer.
        unsafe { libc::close(iter_fd) };
        return Err(format!(
            "failed to enumerate plugin directory safely: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: `directory` remains valid until closed below.
        let entry = unsafe { libc::readdir(directory) };
        if entry.is_null() {
            break;
        }
        // SAFETY: POSIX dirent d_name is NUL-terminated for returned entries.
        let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        names.push(OsString::from_vec(name.to_vec()));
    }
    // SAFETY: closes DIR and its duplicated descriptor exactly once.
    unsafe { libc::closedir(directory) };
    names.sort();

    for name in names {
        let name_c = CString::new(name.clone().into_vec())
            .map_err(|_| "plugin entry name contains an invalid byte".to_string())?;
        let mut stat = MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: source_fd and name are valid; stat points to writable memory.
        if unsafe {
            libc::fstatat(
                source_fd,
                name_c.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(format!(
                "failed to inspect plugin entry safely: {}",
                std::io::Error::last_os_error()
            ));
        }
        // SAFETY: fstatat initialized stat after returning success.
        let stat = unsafe { stat.assume_init() };
        let kind = stat.st_mode & libc::S_IFMT;
        let destination_path = destination.join(&name);
        if kind == libc::S_IFDIR {
            // SAFETY: openat is anchored to the already-open parent and
            // O_NOFOLLOW prevents a concurrent directory-to-symlink swap.
            let child_fd = unsafe {
                libc::openat(
                    source_fd,
                    name_c.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if child_fd < 0 {
                return Err(format!(
                    "failed to open plugin directory safely: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: unique descriptor returned by openat.
            let child_fd = unsafe { OwnedFd::from_raw_fd(child_fd) };
            fs::create_dir(&destination_path)
                .map_err(|e| format!("failed to create staged plugin directory: {e}"))?;
            set_owner_only_directory(&destination_path)?;
            copy_bundle_directory_fd(child_fd.as_raw_fd(), &destination_path, budget)?;
        } else if kind == libc::S_IFREG {
            if stat.st_nlink != 1 {
                return Err("Plugin content may not contain hard-linked files".to_string());
            }
            budget.files = budget.files.saturating_add(1);
            if budget.files > 4_096 {
                return Err("Plugin content exceeded the staging file limit".to_string());
            }
            // SAFETY: openat is anchored and O_NOFOLLOW prevents a file swap
            // to a symbolic link between metadata inspection and open.
            let file_fd = unsafe {
                libc::openat(
                    source_fd,
                    name_c.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if file_fd < 0 {
                return Err(format!(
                    "failed to open plugin file safely: {}",
                    std::io::Error::last_os_error()
                ));
            }
            // SAFETY: unique descriptor returned by openat.
            let mut source_file = unsafe { fs::File::from_raw_fd(file_fd) };
            let opened = source_file
                .metadata()
                .map_err(|e| format!("failed to inspect opened plugin file: {e}"))?;
            if !opened.is_file() {
                return Err("Plugin entry changed type during staging".to_string());
            }
            let mut destination_file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&destination_path)
                .map_err(|e| format!("failed to create staged plugin file: {e}"))?;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = source_file
                    .read(&mut buffer)
                    .map_err(|e| format!("failed to read plugin file during staging: {e}"))?;
                if read == 0 {
                    break;
                }
                budget.bytes = budget.bytes.saturating_add(read as u64);
                if budget.bytes > 64 * 1024 * 1024 {
                    return Err("Plugin content exceeded the staging byte limit".to_string());
                }
                destination_file
                    .write_all(&buffer[..read])
                    .map_err(|e| format!("failed to write staged plugin file: {e}"))?;
            }
            destination_file
                .sync_all()
                .map_err(|e| format!("failed to sync staged plugin file: {e}"))?;
            preserve_owner_only_file_mode(&destination_path, &opened)?;
        } else if kind == libc::S_IFLNK {
            return Err("Plugin content may not contain symbolic links".to_string());
        } else {
            return Err(
                "Plugin content must contain only regular files and directories".to_string(),
            );
        }
    }
    Ok(())
}

fn harden_staged_tree(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("failed to harden staged plugin content: {e}"))?;
    if metadata_is_link_or_reparse(&metadata) {
        return Err(
            "Staged plugin content changed into a symbolic link or reparse point before hardening"
                .to_string(),
        );
    }
    if metadata.is_dir() {
        let entries = fs::read_dir(path)
            .map_err(|e| format!("failed to read staged plugin content: {e}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("failed to enumerate staged plugin content: {e}"))?;
        for entry in entries {
            harden_staged_tree(&entry.path())?;
        }
        set_staged_read_only_directory(path)?;
    } else if metadata.is_file() {
        set_staged_read_only_file(path, &metadata)?;
    } else {
        return Err("Staged plugin content changed type before activation".to_string());
    }
    Ok(())
}

fn harden_staged_tree_contents(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|e| format!("failed to harden staged plugin root: {e}"))?;
    if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
        return Err("Staged plugin root changed type before activation".to_string());
    }
    let entries = fs::read_dir(path)
        .map_err(|e| format!("failed to read staged plugin root: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("failed to enumerate staged plugin root: {e}"))?;
    for entry in entries {
        harden_staged_tree(&entry.path())?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_staged_read_only_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o500))
        .map_err(|e| format!("failed to make staged plugin directory non-writable: {e}"))
}

#[cfg(windows)]
fn set_staged_read_only_directory(path: &Path) -> Result<(), String> {
    // GENERIC_READ | GENERIC_EXECUTE. The owner can inspect/traverse the
    // finalized stage but ordinary child processes cannot rewrite it through
    // inherited full-control directory ACEs.
    set_windows_owner_only_acl_with_mask(path, 0xa000_0000)
}

#[cfg(all(not(unix), not(windows)))]
fn set_staged_read_only_directory(_path: &Path) -> Result<(), String> {
    Err("Plugin runtime staging cannot make directories non-writable on this platform".to_string())
}

#[cfg(unix)]
fn set_staged_read_only_file(path: &Path, source: &fs::Metadata) -> Result<(), String> {
    preserve_owner_only_file_mode(path, source)
}

#[cfg(windows)]
fn set_staged_read_only_file(path: &Path, source: &fs::Metadata) -> Result<(), String> {
    preserve_owner_only_file_mode(path, source)
}

#[cfg(all(not(unix), not(windows)))]
fn set_staged_read_only_file(path: &Path, source: &fs::Metadata) -> Result<(), String> {
    preserve_owner_only_file_mode(path, source)
}

#[cfg(unix)]
fn set_owner_only_directory(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|e| {
        format!(
            "failed to restrict plugin runtime directory permissions for {}: {e}",
            path.display()
        )
    })
}

#[cfg(windows)]
fn set_owner_only_directory(path: &Path) -> Result<(), String> {
    set_windows_owner_only_acl(path)
}

#[cfg(windows)]
fn set_windows_owner_only_acl(path: &Path) -> Result<(), String> {
    set_windows_owner_only_acl_with_mask(path, 0x001f_01ff)
}

#[cfg(windows)]
#[derive(Clone, Copy)]
enum WindowsAclOwnerMode {
    // The handle has WRITE_OWNER, so restoring the current-user ownership and
    // DACL together keeps the authority boundary atomic.
    NormalizeCurrentUser,
    // A previously hardened current-user-owned object may deliberately deny
    // WRITE_OWNER. Re-hardening may replace its DACL only after proving the
    // existing owner is still the current user.
    VerifyCurrentUser,
}

#[cfg(windows)]
enum WindowsAclTargetOpenError {
    Io(std::io::Error),
    Validation(String),
}

#[cfg(windows)]
impl WindowsAclTargetOpenError {
    fn should_retry_without_write_owner(&self) -> bool {
        matches!(self, Self::Io(error) if is_windows_access_denied(error))
    }

    fn into_message(self) -> String {
        match self {
            Self::Io(error) => format!("failed to open Windows plugin ACL target safely: {error}"),
            Self::Validation(message) => message,
        }
    }
}

#[cfg(windows)]
fn is_windows_access_denied(error: &std::io::Error) -> bool {
    // Only retry the expected access denial from a missing WRITE_OWNER grant.
    // A sharing violation, missing path, reparse validation failure, or any
    // other open failure must remain fail-closed without opening a new handle.
    error.raw_os_error() == Some(5) // ERROR_ACCESS_DENIED
}

#[cfg(windows)]
fn open_windows_acl_target(
    path: &Path,
    access_mode: u32,
) -> Result<fs::File, WindowsAclTargetOpenError> {
    use std::os::windows::fs::OpenOptionsExt as _;

    let target = OpenOptions::new()
        .access_mode(access_mode)
        .share_mode(0x0000_0001) // FILE_SHARE_READ
        .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
        .open(path)
        .map_err(WindowsAclTargetOpenError::Io)?;
    let opened = target.metadata().map_err(|error| {
        WindowsAclTargetOpenError::Validation(format!(
            "failed to inspect opened Windows plugin ACL target: {error}"
        ))
    })?;
    let opened_identity = windows_file_identity(&target).map_err(|error| {
        WindowsAclTargetOpenError::Validation(format!(
            "failed to identify opened Windows plugin ACL target: {error}"
        ))
    })?;
    if opened_identity.attributes & 0x0000_0400 != 0 || !(opened.is_file() || opened.is_dir()) {
        return Err(WindowsAclTargetOpenError::Validation(
            "Windows plugin ACL target changed into a reparse point or unsupported object"
                .to_string(),
        ));
    }
    ensure_windows_registry_path_still_opened(path, &target)
        .map_err(WindowsAclTargetOpenError::Validation)?;
    Ok(target)
}

#[cfg(windows)]
fn set_windows_owner_only_acl_with_mask(path: &Path, access_mask: u32) -> Result<(), String> {
    const ACL_ACCESS_WITH_OWNER: u32 = 0x0002_0000 | 0x0004_0000 | 0x0008_0000;
    const ACL_ACCESS_WITHOUT_OWNER: u32 = 0x0002_0000 | 0x0004_0000;

    // Bind ACL mutation to the exact object opened without following a
    // reparse point. A pathname-only SetNamedSecurityInfoW call could inspect
    // a safe entry and then follow a junction substituted before the update.
    let before = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect Windows plugin ACL target: {error}"))?;
    if metadata_is_link_or_reparse(&before) || !(before.is_file() || before.is_dir()) {
        return Err(
            "Windows plugin ACL target must be a regular non-reparse file or directory".to_string(),
        );
    }
    let (target, owner_mode) = match open_windows_acl_target(path, ACL_ACCESS_WITH_OWNER) {
        Ok(target) => (target, WindowsAclOwnerMode::NormalizeCurrentUser),
        Err(error) if error.should_retry_without_write_owner() => {
            let target = open_windows_acl_target(path, ACL_ACCESS_WITHOUT_OWNER)
                .map_err(WindowsAclTargetOpenError::into_message)?;
            (target, WindowsAclOwnerMode::VerifyCurrentUser)
        }
        Err(error) => return Err(error.into_message()),
    };

    apply_windows_owner_only_acl(&target, access_mask, owner_mode)
}

#[cfg(windows)]
fn apply_windows_owner_only_acl(
    target: &fs::File,
    access_mask: u32,
    owner_mode: WindowsAclOwnerMode,
) -> Result<(), String> {
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, WIN32_ERROR};
    use windows::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_REVISION, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION,
        GetLengthSid, GetTokenInformation, InitializeAcl, OBJECT_INHERIT_ACE,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token = HANDLE::default();
    // SAFETY: output handle points to valid storage and the pseudo process
    // handle is valid for the current process.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|error| format!("failed to open the current Windows security token: {error}"))?;
    let result = (|| {
        let mut required = 0_u32;
        // The first call intentionally obtains the required byte count.
        // SAFETY: null buffer queries size; `required` is live.
        let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut required) };
        if required < size_of::<TOKEN_USER>() as u32 {
            return Err("Windows token did not expose a current-user SID".to_string());
        }
        let words = (required as usize).div_ceil(size_of::<usize>());
        let mut token_buffer = vec![MaybeUninit::<usize>::zeroed(); words];
        // SAFETY: aligned buffer is at least `required` bytes and remains alive
        // for every SID/ACL operation below.
        unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                Some(token_buffer.as_mut_ptr().cast()),
                required,
                &mut required,
            )
        }
        .map_err(|error| format!("failed to read the current Windows user SID: {error}"))?;
        // SAFETY: successful TokenUser query initialized a TOKEN_USER at the
        // beginning of the aligned buffer.
        let token_user = unsafe { &*token_buffer.as_ptr().cast::<TOKEN_USER>() };
        let sid = token_user.User.Sid;
        // SAFETY: SID comes from the successful token query above.
        let sid_len = unsafe { GetLengthSid(sid) } as usize;
        if sid_len == 0 {
            return Err("Windows current-user SID is invalid".to_string());
        }
        let acl_bytes =
            size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_len;
        let acl_words = acl_bytes.div_ceil(size_of::<usize>());
        let mut acl_buffer = vec![MaybeUninit::<usize>::zeroed(); acl_words];
        let acl = acl_buffer.as_mut_ptr().cast::<ACL>();
        // SAFETY: aligned ACL buffer is large enough for one full-access ACE
        // containing the current user SID.
        unsafe { InitializeAcl(acl, acl_bytes as u32, ACL_REVISION) }
            .map_err(|error| format!("failed to initialize a private Windows ACL: {error}"))?;
        unsafe {
            windows::Win32::Security::AddAccessAllowedAceEx(
                acl,
                ACL_REVISION,
                CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE,
                access_mask,
                sid,
            )
        }
        .map_err(|error| format!("failed to grant the current Windows user access: {error}"))?;

        let (security_information, owner) = match owner_mode {
            WindowsAclOwnerMode::NormalizeCurrentUser => (
                OWNER_SECURITY_INFORMATION
                    | DACL_SECURITY_INFORMATION
                    | PROTECTED_DACL_SECURITY_INFORMATION,
                Some(sid),
            ),
            WindowsAclOwnerMode::VerifyCurrentUser => {
                // The caller could not obtain WRITE_OWNER. Mutate only an
                // exact handle whose current owner is already the token user.
                ensure_windows_plugin_target_owner(target, sid)?;
                (
                    DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                    None,
                )
            }
        };

        // SAFETY: `target` retains the exact validated non-reparse object and
        // the ACL/SID buffers remain alive through the call. The normalization
        // path writes owner and DACL together; the fallback path has already
        // verified the owner through this retained handle.
        let status = unsafe {
            SetSecurityInfo(
                HANDLE(target.as_raw_handle()),
                SE_FILE_OBJECT,
                security_information,
                owner,
                None,
                Some(acl),
                None,
            )
        };
        if status != WIN32_ERROR(0) {
            return Err(format!(
                "failed to restrict Windows plugin runtime ACL: error {}",
                status.0
            ));
        }
        if let WindowsAclOwnerMode::VerifyCurrentUser = owner_mode {
            // A handle that predated our restrictive share barrier may still
            // mutate the descriptor. Do not hand out authority if it changed
            // ownership around the DACL-only fallback.
            ensure_windows_plugin_target_owner(target, sid)?;
        }
        Ok(())
    })();
    // SAFETY: token is the unique real handle returned by OpenProcessToken.
    let _ = unsafe { CloseHandle(token) };
    result
}

/// Require a current-user-owned target before changing its DACL. The caller
/// has already opened the exact non-reparse object with a restrictive sharing
/// barrier, so this does not reintroduce a path-following race.
#[cfg(windows)]
fn ensure_windows_plugin_target_owner(
    target: &fs::File,
    expected_owner: windows::Win32::Security::PSID,
) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle as _;
    use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree, WIN32_ERROR};
    use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
    use windows::Win32::Security::{
        EqualSid, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    };

    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR(std::ptr::null_mut());
    // SAFETY: `target` remains open for the complete call, all requested
    // output locations are valid, and Windows allocates `descriptor` for the
    // caller to free with LocalFree below.
    let status = unsafe {
        GetSecurityInfo(
            HANDLE(target.as_raw_handle()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            Some(&mut descriptor),
        )
    };
    if status != WIN32_ERROR(0) {
        if !descriptor.0.is_null() {
            // SAFETY: a non-null descriptor came from GetSecurityInfo and is
            // documented to be released by LocalFree exactly once.
            let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        }
        return Err(format!(
            "failed to inspect Windows plugin ACL target owner: error {}",
            status.0
        ));
    }
    // SAFETY: `owner` is non-null from GetSecurityInfo; `expected_owner` is the caller's SID.
    let owner_matches = !owner.0.is_null() && unsafe { EqualSid(owner, expected_owner) }.is_ok();
    if !descriptor.0.is_null() {
        // SAFETY: the successful GetSecurityInfo allocation is released only
        // after the owner SID comparison above completes.
        let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    }
    if !owner_matches {
        return Err(
            "Windows plugin ACL target owner is not the current user; refusing to harden a foreign-owned object"
                .to_string(),
        );
    }
    Ok(())
}

#[cfg(all(not(unix), not(windows)))]
fn set_owner_only_directory(_path: &Path) -> Result<(), String> {
    Err("Plugin runtime staging is unavailable on this platform because owner-only filesystem permissions cannot be enforced".to_string())
}

#[cfg(unix)]
fn preserve_owner_only_file_mode(path: &Path, source: &fs::Metadata) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let executable = source.permissions().mode() & 0o111 != 0;
    let mode = if executable { 0o500 } else { 0o400 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|e| format!("failed to restrict staged plugin file permissions: {e}"))
}

#[cfg(windows)]
fn preserve_owner_only_file_mode(path: &Path, _source: &fs::Metadata) -> Result<(), String> {
    // The protected handle-relative DACL is the Windows non-writable
    // authority. Avoid `set_permissions(path)`, which can follow a reparse
    // point substituted after metadata inspection.
    set_windows_owner_only_acl_with_mask(path, 0xa000_0000)
}

#[cfg(all(not(unix), not(windows)))]
fn preserve_owner_only_file_mode(path: &Path, _source: &fs::Metadata) -> Result<(), String> {
    let mut permissions = fs::metadata(path)
        .map_err(|e| format!("failed to inspect staged plugin file permissions: {e}"))?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
        .map_err(|e| format!("failed to restrict staged plugin file permissions: {e}"))
}

/// Recheck a persisted plugin receipt, the mutable reviewed source, and the
/// Codewhale-owned immutable runtime copy, then require the named adapter to
/// be in this build's activation policy. Every adapter must call this with its
/// specific capability rather than treating bundle-wide activity as authority
/// to run every inventoried surface.
pub fn verify_plugin_component_authority(
    authority: &PluginAuthority,
    capability: PluginActivationCapability,
) -> Result<(), String> {
    verify_plugin_authority(authority)?;
    if !PluginActivationPolicy::current().is_supported(capability) {
        return Err(format!(
            "Plugin bundle `{}` capability `{}` is inactive in this Codewhale build",
            authority.plugin_name,
            capability.as_str()
        ));
    }
    Ok(())
}

/// Recheck a persisted plugin receipt, the mutable reviewed source, and the
/// Codewhale-owned immutable runtime copy. This function performs no writes.
///
/// Known limitation, deliberate: this is **not** memoized on `(path, mtime,
/// len)`, even though re-walking both trees is the dominant cost of an MCP
/// dispatch (#6209). The reviewed source tree is user-writable, and
/// `utimensat(2)` lets any same-uid process restore an mtime after an
/// equal-length in-place rewrite. A stat-keyed cache would then hand back the
/// pre-tamper digest, `content_hash` would still match, and a modified bundle
/// would dispatch as reviewed — turning the one check that stands between a
/// reviewed bundle and an altered one into a check of whether someone
/// remembered to reset a timestamp. The cost is paid per dispatch on purpose.
/// Reduce the *number* of calls instead; see `McpConnection::is_transport_ready`.
pub fn verify_plugin_authority(authority: &PluginAuthority) -> Result<(), String> {
    verify_plugin_state_authority(authority)?;
    for (label, manifest_path) in [
        ("reviewed source", &authority.source_manifest),
        ("Codewhale runtime snapshot", &authority.staged_manifest),
    ] {
        let current =
            super::manifest::PluginManifest::validate_from_path(manifest_path).map_err(|_| {
                format!(
                    "Plugin bundle `{}` {label} could not be revalidated",
                    authority.plugin_name
                )
            })?;
        if current.content_hash != authority.content_hash
            || current.capability_hash != authority.capability_hash
        {
            return Err(format!(
                "Plugin bundle `{}` {label} changed after review",
                authority.plugin_name
            ));
        }
    }
    Ok(())
}

/// Cheap cross-process revocation probe used while an established MCP request
/// is in flight. Full source/stage hashing is intentionally done before each
/// dispatch; the watcher only needs to notice the locked state transition.
pub fn verify_plugin_state_authority(authority: &PluginAuthority) -> Result<(), String> {
    let state_parent = authority
        .state_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| "Plugin authority state has no private parent directory".to_string())?;
    validate_plugin_state_directory_for_read(state_parent).map_err(|_| {
        "Plugin authority state directory is not private; the bundle is disabled fail-closed"
            .to_string()
    })?;
    let lock_path = state_lock_path(&authority.state_path);
    let lock_file = open_state_lock(&lock_path, false).map_err(|_| {
        "Plugin authority state lock is missing; review and enable the bundle again".to_string()
    })?;
    let lock = fd_lock::RwLock::new(lock_file);
    let _guard = lock
        .read()
        .map_err(|_| "Plugin authority state could not be read safely".to_string())?;
    let state = load_state_unlocked(&authority.state_path).map_err(|_| {
        "Plugin authority state is invalid; the bundle is disabled fail-closed".to_string()
    })?;
    let active = state
        .plugins
        .get(&authority.plugin_id)
        .is_some_and(|entry| {
            entry.generation == authority.state_generation
                && entry.enabled
                && entry.trust.as_ref().is_some_and(|receipt| {
                    receipt.content_hash == authority.content_hash
                        && receipt.capability_hash == authority.capability_hash
                })
        });
    if !active {
        return Err(format!(
            "Plugin bundle `{}` is disabled, revoked, or no longer matches its review receipt",
            authority.plugin_name
        ));
    }
    Ok(())
}

#[cfg(test)]
mod state_publication_tests {
    use super::{PluginStateFile, harden_plugin_state_file, save_state_with_hardener};

    fn prepare_private_directory(_path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
    }

    #[test]
    fn successful_state_publication_replaces_the_stable_file_without_temp_debris() {
        let directory = tempfile::tempdir().unwrap();
        prepare_private_directory(directory.path());
        let state_path = directory.path().join("state-鲸.json");
        std::fs::write(&state_path, b"old-authoritative-state").unwrap();

        save_state_with_hardener(
            &state_path,
            &PluginStateFile::default(),
            harden_plugin_state_file,
        )
        .unwrap();

        let published = std::fs::read_to_string(&state_path).unwrap();
        assert!(published.contains("\"schema_version\": 1"));
        let entries = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [std::ffi::OsString::from("state-鲸.json")]);
    }

    #[test]
    fn failed_temp_hardening_never_publishes_new_plugin_state() {
        let directory = tempfile::tempdir().unwrap();
        prepare_private_directory(directory.path());
        let state_path = directory.path().join("state.json");
        std::fs::write(&state_path, b"old-authoritative-state").unwrap();

        let error =
            save_state_with_hardener(&state_path, &PluginStateFile::default(), |temporary_path| {
                assert!(temporary_path.is_file());
                assert!(
                    std::fs::read_to_string(temporary_path)
                        .unwrap()
                        .contains("\"schema_version\": 1")
                );
                assert_eq!(
                    std::fs::read(&state_path).unwrap(),
                    b"old-authoritative-state",
                    "the stable path must still hold the old state while hardening runs"
                );
                Err("injected pre-publication ACL failure".to_string())
            })
            .unwrap_err();

        assert!(error.contains("injected pre-publication ACL failure"));
        assert_eq!(
            std::fs::read(&state_path).unwrap(),
            b"old-authoritative-state"
        );
        let entries = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [std::ffi::OsString::from("state.json")]);
    }

    #[cfg(unix)]
    #[test]
    fn directory_sync_failure_reports_that_the_new_state_was_published() {
        use super::persist_plugin_state_with_directory_sync;
        use std::io::Write as _;

        let directory = tempfile::tempdir().unwrap();
        prepare_private_directory(directory.path());
        let state_path = directory.path().join("state.json");
        std::fs::write(&state_path, b"old-authoritative-state").unwrap();
        let mut temporary = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        temporary.write_all(b"new-authoritative-state").unwrap();
        temporary.flush().unwrap();
        temporary.as_file().sync_all().unwrap();

        let error = persist_plugin_state_with_directory_sync(temporary, &state_path, |_| {
            Err(std::io::Error::other(
                "injected post-publication directory sync failure",
            ))
        })
        .unwrap_err();

        assert!(error.contains("published but its directory durability could not be confirmed"));
        assert_eq!(
            std::fs::read(&state_path).unwrap(),
            b"new-authoritative-state"
        );
        let entries = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [std::ffi::OsString::from("state.json")]);
    }
}

#[cfg(all(test, unix))]
mod unix_state_directory_tests {
    use super::validate_unix_plugin_state_directory_fields;

    #[test]
    fn state_directory_validation_rejects_an_owner_mismatch() {
        let error = validate_unix_plugin_state_directory_fields(true, 41, 0o700, 42).unwrap_err();
        assert!(error.contains("current-user-owned"));
    }
}

#[cfg(all(test, windows))]
mod windows_acl_tests {
    use super::{
        PluginStateFile, WindowsAclOwnerMode, apply_windows_owner_only_acl,
        ensure_private_runtime_parent, ensure_windows_plugin_target_owner,
        harden_plugin_state_file, harden_staged_tree_contents, open_state_lock,
        save_state_with_hardener, set_windows_owner_only_acl, state_lock_path,
    };
    use std::ffi::c_void;
    use std::mem::{MaybeUninit, size_of};
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, AdjustTokenPrivileges,
        CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, DuplicateTokenEx, EqualSid, GetAce,
        GetAclInformation, GetFileSecurityW, GetSecurityDescriptorControl,
        GetSecurityDescriptorDacl, GetSecurityDescriptorOwner, OBJECT_INHERIT_ACE,
        OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, RevertToSelf, SE_DACL_PROTECTED,
        SecurityImpersonation, TOKEN_ADJUST_PRIVILEGES, TOKEN_DUPLICATE, TOKEN_IMPERSONATE,
        TOKEN_QUERY, TokenImpersonation,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken, SetThreadToken};
    use windows::core::{BOOL, PCWSTR};

    /// Pin the fixture to a thread token with privileges disabled. Hosted
    /// runners can hold backup/restore/take-ownership privileges that bypass
    /// the DACL, making a WRITE_OWNER denial depend on the host identity.
    /// The process token is unchanged, and dropping the guard reverts the thread.
    struct UnprivilegedThreadToken {
        process_token: HANDLE,
        restricted: HANDLE,
    }

    impl UnprivilegedThreadToken {
        fn adopt() -> windows::core::Result<Self> {
            let mut process_token = HANDLE::default();
            // SAFETY: the pseudo process handle is valid for the current
            // process and the output location is live across the call.
            unsafe {
                OpenProcessToken(
                    GetCurrentProcess(),
                    TOKEN_DUPLICATE | TOKEN_QUERY,
                    &mut process_token,
                )
            }?;
            let mut restricted = HANDLE::default();
            // SAFETY: `process_token` stays open for the call and `restricted`
            // receives a new handle ownership of which passes to the guard.
            let duplicated = unsafe {
                DuplicateTokenEx(
                    process_token,
                    TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY | TOKEN_IMPERSONATE,
                    None,
                    SecurityImpersonation,
                    TokenImpersonation,
                    &mut restricted,
                )
            };
            if let Err(error) = duplicated {
                // SAFETY: closing the only handle opened above.
                unsafe {
                    let _ = CloseHandle(process_token);
                }
                return Err(error);
            }
            // Own both handles before the fallible calls below, so a failure
            // still reverts and closes through `Drop`.
            let guard = Self {
                process_token,
                restricted,
            };
            // DisableAllPrivileges leaves the duplicate holding none, so the
            // DACL becomes the only thing that can grant WRITE_OWNER.
            // SAFETY: `restricted` is owned by `guard` for the whole call.
            unsafe { AdjustTokenPrivileges(guard.restricted, true, None, 0, None, None) }?;
            // SAFETY: impersonation is scoped to this thread and undone in Drop.
            unsafe { SetThreadToken(None, Some(guard.restricted)) }?;
            Ok(guard)
        }
    }

    impl Drop for UnprivilegedThreadToken {
        fn drop(&mut self) {
            // SAFETY: reverts this thread's identity and closes the two handles
            // this guard opened, each exactly once.
            unsafe {
                let _ = RevertToSelf();
                let _ = CloseHandle(self.restricted);
                let _ = CloseHandle(self.process_token);
            }
        }
    }

    fn create_junction(link: &std::path::Path, target: &std::path::Path) {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("invoke Windows junction creation");
        assert!(
            output.status.success(),
            "failed to create junction: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn acl_hardening_rejects_junction_targets() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let junction = directory.path().join("junction");
        std::fs::create_dir(&target).unwrap();
        create_junction(&junction, &target);

        let error = set_windows_owner_only_acl(&junction).unwrap_err();
        assert!(error.contains("non-reparse"), "unexpected error: {error}");
    }

    #[test]
    fn runtime_parent_creation_rejects_junction_components() {
        let directory = tempfile::tempdir().unwrap();
        let state_root = directory.path().join("state");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&state_root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        create_junction(&state_root.join(".runtime"), &outside);
        let state_path = state_root.join("state.json");
        let expected_parent = state_root.join(".runtime/v2/plugin");

        let error = ensure_private_runtime_parent(&state_path, &expected_parent).unwrap_err();
        assert!(
            error.contains("reparse points"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn staged_tree_hardening_rejects_junction_entries() {
        let directory = tempfile::tempdir().unwrap();
        let stage = directory.path().join("stage");
        let outside = directory.path().join("outside");
        std::fs::create_dir(&stage).unwrap();
        std::fs::create_dir(&outside).unwrap();
        create_junction(&stage.join("linked"), &outside);

        let error = harden_staged_tree_contents(&stage).unwrap_err();
        assert!(error.contains("reparse point"), "unexpected error: {error}");
    }

    #[test]
    fn state_lock_hardening_keeps_its_writer_handle() {
        let directory = tempfile::tempdir().unwrap();
        let state_directory = directory.path().join("state");
        std::fs::create_dir(&state_directory).unwrap();
        set_windows_owner_only_acl(&state_directory).unwrap();
        let lock_path = state_lock_path(&state_directory.join("state.json"));

        let lock = open_state_lock(&lock_path, true)
            .expect("state lock ACL hardening must not conflict with its writer handle");
        assert!(lock.metadata().unwrap().is_file());
    }

    #[test]
    fn owner_only_acl_rehardens_without_write_owner_access() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("state");
        std::fs::create_dir(&target).unwrap();
        set_windows_owner_only_acl(&target).unwrap();

        // Simulate an already-private Codewhale object whose owner is still
        // the current user, but whose DACL intentionally does not grant
        // WRITE_OWNER. Rehardening must restore the full owner-only ACL
        // rather than assuming it may take ownership again.
        let reduced = std::fs::OpenOptions::new()
            .access_mode(0x001f_01ff) // FILE_ALL_ACCESS for this setup only
            .share_mode(0x0000_0001)
            .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
            .open(&target)
            .unwrap();
        apply_windows_owner_only_acl(
            &reduced,
            0x0017_01ff,
            WindowsAclOwnerMode::VerifyCurrentUser,
        )
        .expect("current owner may restrict its DACL without changing ownership");
        drop(reduced);

        // From here the DACL must be the only authority. Held to the end of the
        // test so the fallback and the restored ACL are both observed through
        // an identity that cannot bypass the descriptor.
        let _unprivileged = UnprivilegedThreadToken::adopt()
            .expect("restricted thread identity for the WRITE_OWNER denial");

        let denied = std::fs::OpenOptions::new()
            .access_mode(0x0002_0000 | 0x0004_0000 | 0x0008_0000)
            .share_mode(0x0000_0001)
            .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
            .open(&target)
            .unwrap_err();
        assert_eq!(
            denied.raw_os_error(),
            Some(5),
            "the full-owner path must be unavailable before exercising the fallback"
        );

        set_windows_owner_only_acl(&target)
            .expect("rehardening must verify the current owner without requesting WRITE_OWNER");
        let restored = std::fs::OpenOptions::new()
            .access_mode(0x001f_01ff)
            .share_mode(0x0000_0001)
            .custom_flags(0x0220_0000)
            .open(&target);
        assert!(
            restored.is_ok(),
            "rehardening must restore the current user's full owner-only ACL"
        );
    }

    #[test]
    fn state_lock_rehardens_without_write_owner_access() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let state_directory = directory.path().join("state");
        std::fs::create_dir(&state_directory).unwrap();
        set_windows_owner_only_acl(&state_directory).unwrap();
        let lock_path = state_lock_path(&state_directory.join("state.json"));
        drop(open_state_lock(&lock_path, true).unwrap());

        let reduced = std::fs::OpenOptions::new()
            .access_mode(0x001f_01ff) // FILE_ALL_ACCESS for this setup only
            .share_mode(0x0000_0001)
            .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
            .open(&lock_path)
            .unwrap();
        apply_windows_owner_only_acl(
            &reduced,
            0x0016_019f, // FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC
            WindowsAclOwnerMode::VerifyCurrentUser,
        )
        .expect("current owner may remove WRITE_OWNER from an existing state lock");
        drop(reduced);

        // This sibling passed on the hosted runner only because its denial open
        // omits FILE_FLAG_BACKUP_SEMANTICS, which is what engages the
        // descriptor-bypassing privileges. That is an accident of the flags, not
        // a guarantee, so pin the identity here too.
        let _unprivileged = UnprivilegedThreadToken::adopt()
            .expect("restricted thread identity for the WRITE_OWNER denial");

        let denied = std::fs::OpenOptions::new()
            .access_mode(0x001e_019f) // FILE_GENERIC_READ | FILE_GENERIC_WRITE | WRITE_DAC | WRITE_OWNER
            .share_mode(0x0000_0001)
            .custom_flags(0x0020_0000) // FILE_FLAG_OPEN_REPARSE_POINT
            .open(&lock_path)
            .unwrap_err();
        assert_eq!(
            denied.raw_os_error(),
            Some(5),
            "the state-lock fallback must run only after WRITE_OWNER is denied"
        );

        let lock = open_state_lock(&lock_path, true)
            .expect("state-lock hardening must fall back to DACL-only rehardening");
        assert!(lock.metadata().unwrap().is_file());
    }

    #[test]
    fn owner_only_acl_rejects_an_owner_identity_mismatch() {
        use std::os::windows::fs::OpenOptionsExt as _;
        use windows::Win32::Security::{CreateWellKnownSid, WinWorldSid};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state");
        std::fs::create_dir(&path).unwrap();
        set_windows_owner_only_acl(&path).unwrap();
        let target = std::fs::OpenOptions::new()
            .access_mode(0x0002_0000) // READ_CONTROL
            .share_mode(0x0000_0001)
            .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
            .open(&path)
            .unwrap();

        // World is a valid SID that cannot match this user's object owner.
        // Supply it directly to the exact-handle verifier without changing
        // the fixture's real owner or relying on privileged owner mutation.
        let mut required = 0_u32;
        let _ = unsafe { CreateWellKnownSid(WinWorldSid, None, None, &mut required) };
        assert!(required > 0, "Windows did not report the world SID size");
        let words = (required as usize).div_ceil(size_of::<usize>());
        let mut sid_buffer = vec![MaybeUninit::<usize>::zeroed(); words];
        let world = PSID(sid_buffer.as_mut_ptr().cast());
        unsafe { CreateWellKnownSid(WinWorldSid, None, Some(world), &mut required) }.unwrap();

        let error = ensure_windows_plugin_target_owner(&target, world).unwrap_err();
        assert!(
            error.contains("not the current user"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn blocked_state_replacement_preserves_the_stable_authority_file() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        std::fs::write(&state_path, b"old-authoritative-state").unwrap();
        let retained = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001)
            .open(&state_path)
            .unwrap();

        let error = save_state_with_hardener(
            &state_path,
            &PluginStateFile::default(),
            harden_plugin_state_file,
        )
        .unwrap_err();

        assert!(
            error.contains("durably persist"),
            "unexpected error: {error}"
        );
        assert_eq!(
            std::fs::read(&state_path).unwrap(),
            b"old-authoritative-state"
        );
        drop(retained);
    }

    #[test]
    fn transient_state_replacement_contention_is_retried() {
        use std::os::windows::fs::OpenOptionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let state_path = directory.path().join("state.json");
        std::fs::write(&state_path, b"old-authoritative-state").unwrap();
        // FILE_SHARE_READ alone omits delete sharing: the short-lived handle a
        // concurrent ACL pass or scanner holds on the stable file.
        let retained = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001)
            .open(&state_path)
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            drop(retained);
        });

        save_state_with_hardener(
            &state_path,
            &PluginStateFile::default(),
            harden_plugin_state_file,
        )
        .expect("retry contended plugin state replacement");

        release.join().unwrap();
        let published = std::fs::read_to_string(&state_path).unwrap();
        assert!(published.contains("\"schema_version\": 1"));
        let entries = std::fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, [std::ffi::OsString::from("state.json")]);
    }

    #[test]
    fn owner_only_runtime_acl_is_protected_and_has_one_full_access_ace() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path().join("runtime");
        std::fs::create_dir(&runtime).unwrap();
        set_windows_owner_only_acl(&runtime).unwrap();

        let mut wide = runtime.as_os_str().encode_wide().collect::<Vec<_>>();
        wide.push(0);
        let mut required = 0_u32;
        // SAFETY: this size-probe intentionally supplies no destination buffer.
        let _ = unsafe {
            GetFileSecurityW(
                PCWSTR(wide.as_ptr()),
                (DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION).0,
                None,
                0,
                &mut required,
            )
        };
        assert!(
            required > 0,
            "Windows did not report a security descriptor size"
        );
        let words = (required as usize).div_ceil(size_of::<usize>());
        let mut descriptor = vec![MaybeUninit::<usize>::zeroed(); words];
        let descriptor = PSECURITY_DESCRIPTOR(descriptor.as_mut_ptr().cast::<c_void>());
        // SAFETY: the aligned destination is at least `required` bytes and the
        // UTF-16 path remains NUL terminated for the call.
        assert!(
            unsafe {
                GetFileSecurityW(
                    PCWSTR(wide.as_ptr()),
                    (DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION).0,
                    Some(descriptor),
                    required,
                    &mut required,
                )
            }
            .as_bool()
        );

        let mut present = BOOL::default();
        let mut defaulted = BOOL::default();
        let mut acl = std::ptr::null_mut::<ACL>();
        // SAFETY: `descriptor` contains the successful GetFileSecurityW result.
        unsafe { GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) }
            .unwrap();
        assert!(present.as_bool());
        assert!(!acl.is_null());

        let mut info = ACL_SIZE_INFORMATION::default();
        // SAFETY: `acl` is owned by the live descriptor buffer above.
        unsafe {
            GetAclInformation(
                acl,
                (&mut info as *mut ACL_SIZE_INFORMATION).cast(),
                size_of::<ACL_SIZE_INFORMATION>() as u32,
                AclSizeInformation,
            )
        }
        .unwrap();
        assert_eq!(info.AceCount, 1, "runtime DACL must name only the owner");

        let mut ace = std::ptr::null_mut::<c_void>();
        // SAFETY: the ACL contains exactly one ACE.
        unsafe { GetAce(acl, 0, &mut ace) }.unwrap();
        let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
        assert_eq!(ace.Header.AceType, 0, "owner entry must be an allow ACE");
        assert_eq!(ace.Mask, 0x001f_01ff, "owner entry must grant full access");
        let ace_sid = PSID(std::ptr::addr_of!(ace.SidStart).cast_mut().cast());
        let mut owner = PSID::default();
        let mut owner_defaulted = BOOL::default();
        // SAFETY: `descriptor` contains the live security descriptor and both
        // output pointers reference initialized storage.
        unsafe { GetSecurityDescriptorOwner(descriptor, &mut owner, &mut owner_defaulted) }
            .unwrap();
        assert!(
            !owner.0.is_null(),
            "runtime object must have an explicit owner"
        );
        assert!(
            !owner_defaulted.as_bool(),
            "runtime object owner must be explicitly assigned"
        );
        // SAFETY: both SIDs are owned by the live descriptor/ACL buffers.
        unsafe { EqualSid(owner, ace_sid) }
            .expect("runtime object owner must equal its sole current-user ACE");
        let inheritance = (CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE).0 as u8;
        assert_eq!(ace.Header.AceFlags & inheritance, inheritance);

        let mut control = 0_u16;
        let mut revision = 0_u32;
        // SAFETY: the descriptor buffer remains alive for this inspection.
        unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) }.unwrap();
        assert_ne!(
            control & SE_DACL_PROTECTED.0,
            0,
            "runtime DACL must not inherit broader parent permissions"
        );
    }
}

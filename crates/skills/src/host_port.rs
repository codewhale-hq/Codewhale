use std::sync::OnceLock;

use codewhale_config::{EntryRef, PluginAuthority};
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::composition_scope::SelectionRevision;
use codewhale_plugins::types::PluginSkillSnapshot;
use serde::{Deserialize, Serialize};

/// Public receipt reuses the existing process and host/owner lifetimes. The
/// host-only owner token never enters prompts or persisted queued messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeSkillRef {
    pub boot_id: String,
    pub host_generation: u64,
    pub owner_generation: u64,
    pub handle: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<SelectionRevision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<EntryRef>,
}

pub type NativeSkillRoots = Vec<(Vec<PluginSkillSnapshot>, PluginAuthority, NativeSkillRef)>;

pub trait SkillHostPort: Send + Sync {
    fn check_selection(
        &self,
        selection: Option<SelectionRevision>,
        plugins: Option<&PluginRegistry>,
        plugin_id: &str,
        hash: &str,
        scope: Option<&EntryRef>,
    ) -> Result<(), String>;
    fn verify_native_skill(
        &self,
        authority: &PluginAuthority,
        reference: &NativeSkillRef,
    ) -> Result<(), String>;
    fn native_roots(&self, plugins: &PluginRegistry) -> NativeSkillRoots;
}

static PORT: OnceLock<&'static dyn SkillHostPort> = OnceLock::new();

pub fn install_skill_host(port: &'static dyn SkillHostPort) {
    let _ = PORT.set(port);
}

const UNAVAILABLE: &str = "Native skill authority is unavailable; select the skill again";

pub fn check_selection(
    selection: Option<SelectionRevision>,
    plugins: Option<&PluginRegistry>,
    plugin_id: &str,
    hash: &str,
    scope: Option<&EntryRef>,
) -> Result<(), String> {
    match PORT.get() {
        Some(port) => port.check_selection(selection, plugins, plugin_id, hash, scope),
        None => Err(UNAVAILABLE.into()),
    }
}

pub fn verify_native_skill(
    authority: &PluginAuthority,
    reference: &NativeSkillRef,
) -> Result<(), String> {
    match PORT.get() {
        Some(port) => port.verify_native_skill(authority, reference),
        None => Err(UNAVAILABLE.into()),
    }
}

pub fn native_roots(plugins: &PluginRegistry) -> NativeSkillRoots {
    PORT.get()
        .map_or_else(Vec::new, |port| port.native_roots(plugins))
}

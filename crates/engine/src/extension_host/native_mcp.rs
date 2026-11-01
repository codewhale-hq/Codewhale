//! Native definition admission joins Rust's existing MCP pool; no transport here.
use super::ManagerShared;
use super::protocol::{RegisterKind, RegisterParams, RegisterResult};
use super::supervisor::HostRequestContext;
use super::tier::HostTier;
use codewhale_config::PluginAuthority;
use codewhale_config::{EntryRef, OwnerRef};
use codewhale_mcp::config::McpServerConfig;
use codewhale_plugins::composition_scope::SelectionRevision;
use codewhale_plugins::{PluginRegistry, activation::PluginActivationCapability};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};

pub(super) const MAX_PER_OWNER: usize = 64;
pub(super) const MAX_PER_HOST: usize = 256;
pub(super) const MAX_DEFINITION_BYTES: usize = 64 * 1024;
static EPOCH: AtomicU64 = AtomicU64::new(1);
pub fn epoch() -> u64 {
    EPOCH.load(Ordering::SeqCst)
}
pub(super) fn changed() {
    EPOCH.fetch_add(1, Ordering::SeqCst);
}

#[derive(Debug, Clone)]
pub(super) struct McpRegistration {
    pub handle: u64,
    pub owner: OwnerRef,
    pub scope: EntryRef,
    pub content_hash: String,
    pub host_generation: u64,
    pub name: String,
    pub config: McpServerConfig,
    pub cancel: tokio_util::sync::CancellationToken,
}
/// In-memory only: exact generation/handle/caller receipt. No host token is persisted.
#[derive(Debug, Clone)]
pub struct NativeMcpRef {
    shared: Weak<dyn NativeMcpAuthority>,
    registration: Arc<McpRegistration>,
    selection: SelectionRevision,
    caller_cancel: tokio_util::sync::CancellationToken,
}
// Keep MCP configs independent of the concrete manager's Engine/task graph.
// The weak port still targets the original manager and owns no runtime state.
trait NativeMcpAuthority: Send + Sync {
    fn validate_mcp(
        &self,
        selection: SelectionRevision,
        registration: &McpRegistration,
    ) -> Result<(), String>;
}
impl NativeMcpAuthority for ManagerShared {
    fn validate_mcp(
        &self,
        selection: SelectionRevision,
        registration: &McpRegistration,
    ) -> Result<(), String> {
        if !codewhale_plugins::activation::extension_host_policy_enabled() {
            return Err("Native MCP is disabled by current policy".into());
        }
        self.ready_host(HostTier::Plugin)
            .map_err(|status| status.to_string())?;
        if self
            .tier_runtime(HostTier::Plugin)
            .host_generation
            .load(Ordering::SeqCst)
            != registration.host_generation
        {
            return Err("Native MCP host generation changed".into());
        }
        if !self.selection_current(
            selection,
            &registration.owner.plugin_id,
            &registration.content_hash,
            Some(&registration.scope),
        ) {
            return Err("Native MCP entry is no longer selected for this caller".into());
        }
        let registry = self.registry.lock().expect("registry lock");
        if !registry.is_live_mcp(registration) {
            return Err("Native MCP definition was withdrawn".into());
        }
        Ok(())
    }
}
impl NativeMcpRef {
    pub fn validate(&self) -> Result<(), String> {
        self.shared
            .upgrade()
            .ok_or("Native MCP manager exited")?
            .validate_mcp(self.selection, &self.registration)
    }
    pub fn selection(&self) -> SelectionRevision {
        self.selection
    }
    pub fn catalog_identity(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.registration.host_generation,
            self.registration.owner.generation,
            self.registration.handle,
            self.selection.attachment_id,
            self.selection.revision,
            self.registration.scope.sha256
        )
    }
    pub async fn withdrawn(&self) {
        tokio::select! {
            biased;
            _ = self.caller_cancel.cancelled() => {},
            _ = self.registration.cancel.cancelled() => {},
        }
    }
}

pub(super) async fn admit(
    shared: &Arc<ManagerShared>,
    tier: HostTier,
    generation: u64,
    params: RegisterParams,
    cx: &HostRequestContext,
) -> RegisterResult {
    let outcome = async {
        params.check_spec()?;
        if params.kind != RegisterKind::McpServer
            || tier != HostTier::Plugin
            || params.scope.is_none()
        {
            return Err("MCP definitions require a reviewed Native entry scope".into());
        }
        if params.spec.description.len() > MAX_DEFINITION_BYTES {
            return Err("MCP definition exceeds 64 KiB".into());
        }
        let authority = shared
            .live_owner_authority(tier, |registry| {
                registry
                    .authority_for(&params.owner)
                    .ok_or_else(|| "stale MCP owner".to_string())?;
                Ok(params.owner.clone())
            })?
            .ok_or("MCP definition has no Native authority")?;
        let proposal = params.clone();
        let config = super::skills::bounded_review_check(
            Arc::clone(&shared.skill_admission),
            &cx.cancel,
            move || validate_definition(&authority, &proposal),
        )
        .await?;
        shared
            .ready_host(tier)
            .map_err(|status| status.to_string())?;
        let runtime = shared.tier_runtime(tier);
        let _host = runtime.host.lock().expect("host lock");
        if cx.cancel.is_cancelled() || runtime.host_generation.load(Ordering::SeqCst) != generation
        {
            return Err("MCP admission cancelled or host changed".into());
        }
        shared
            .registry
            .lock()
            .expect("registry lock")
            .register_mcp(&params, config, generation)
    }
    .await;
    match outcome {
        Ok(handle) => RegisterResult::Admitted { handle },
        Err(refused) => {
            shared.plugin_diagnostic(
                &params.owner.plugin_id,
                format!("MCP definition refused: {refused}"),
            );
            RegisterResult::Refused { refused }
        }
    }
}
fn validate_definition(
    authority: &PluginAuthority,
    params: &RegisterParams,
) -> Result<McpServerConfig, String> {
    codewhale_plugins::registry::verify_plugin_component_authority(
        authority,
        PluginActivationCapability::Native,
    )?;
    let declaration: serde_json::Value = serde_json::from_str(&params.spec.description)
        .map_err(|_| "MCP proposal is not a literal server definition")?;
    let text = serde_json::to_string(
        &serde_json::json!({"mcpServers":{(params.spec.name.clone()):declaration}}),
    )
    .map_err(|_| "MCP definition encoding failed")?;
    let mut servers = codewhale_plugins::agent_plugin::parse_mcp_json(&text)
        .map_err(|_| "MCP proposal does not match the existing transport schema")?;
    let config = servers
        .remove(&params.spec.name)
        .ok_or("MCP proposal omitted its server")?;
    // Native rows cannot transport credential values to Core. Authentication
    // remains the existing OAuth/environment-key authority at connection time.
    if !config.env.is_empty() || !config.headers.is_empty() {
        return Err("MCP credential values must remain under Core authentication authority".into());
    }
    let mut validated = codewhale_plugins::manifest::PluginManifest::validate_from_path(
        &authority.staged_manifest,
    )?;
    let root = &validated.canonical_root;
    validated.manifest.mcp_servers = Some(servers);
    validated
        .manifest
        .mcp_servers
        .as_mut()
        .expect("set servers")
        .insert(params.spec.name.clone(), config.clone());
    validated.manifest.validate_mcp_servers(root)?;
    codewhale_plugins::registry::verify_plugin_component_authority(
        authority,
        PluginActivationCapability::Native,
    )?;
    Ok(config)
}

pub fn for_plugins(
    plugins: &PluginRegistry,
) -> Result<Vec<(String, McpServerConfig, PluginAuthority, NativeMcpRef)>, String> {
    if !codewhale_plugins::activation::extension_host_policy_enabled() {
        return Ok(Vec::new());
    }
    let Some(selection) = plugins.caller_selection() else {
        return Ok(Vec::new());
    };
    let manager = super::manager();
    let shared = &manager.shared;
    if shared.ready_host(HostTier::Plugin).is_err() {
        return Ok(Vec::new());
    }
    let Some(caller_cancel) = shared.selection_cancellation(selection) else {
        return Ok(Vec::new());
    };
    let live_authority: Arc<dyn NativeMcpAuthority> = shared.clone();
    let definitions = shared.registry.lock().expect("registry lock").live_mcp();
    let mut result = Vec::new();
    let mut names = std::collections::HashSet::new();
    for definition in definitions {
        let Some(plugin) = plugins.get(&definition.owner.plugin_id) else {
            continue;
        };
        if !plugin.active()
            || plugin.content_hash != definition.content_hash
            || !shared.selection_current(
                selection,
                &definition.owner.plugin_id,
                &definition.content_hash,
                Some(&definition.scope),
            )
        {
            continue;
        }
        let Some(authority) = plugins.authority_for(&definition.owner.plugin_id) else {
            continue;
        };
        let receipt = NativeMcpRef {
            shared: Arc::downgrade(&live_authority),
            registration: Arc::new(definition.clone()),
            selection,
            caller_cancel: caller_cancel.clone(),
        };
        if receipt.validate().is_err()
            || codewhale_plugins::registry::verify_plugin_component_authority(
                &authority,
                PluginActivationCapability::Native,
            )
            .is_err()
        {
            continue;
        }
        let qualified =
            crate::mcp::qualified_plugin_server_name(&authority.plugin_name, &definition.name);
        if !names.insert(qualified) {
            return Err("Selected Native entries duplicate an MCP server namespace".into());
        }
        result.push((definition.name, definition.config, authority, receipt));
    }
    Ok(result)
}

#[cfg(test)]
mod tests;

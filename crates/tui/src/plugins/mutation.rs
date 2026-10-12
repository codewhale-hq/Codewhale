//! Plugin mutation controller (#5182).
//!
//! All plugin install / update / uninstall writes go through this module.
//! Discovery stays read-only: this controller is the only writer of the user
//! plugins root, and every request is gated by the per-domain
//! [`crate::network_policy::NetworkPolicy`] before any network or disk
//! mutation happens. Trust and enablement are *not* mutations of this module —
//! they remain the registry's hash-bound receipt flow (`PluginRegistry::trust`
//! / `enable`), which installed bits must pass through like any other bundle.

use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::network_policy::NetworkPolicy;

use super::install::{self, PluginInstallOutcome, PluginInstallSource, PluginUpdateResult};
use super::registry::PluginRegistry;
use super::types::PluginScope;

/// A single plugin write operation.
#[derive(Debug, Clone)]
pub enum PluginMutationRequest {
    /// Fetch (or copy) a bundle into the user plugins root. The bundle lands
    /// disabled and untrusted; the caller should route to the trust review.
    Install { source: PluginInstallSource },
    /// Install only when the staged copy matches a prior content review.
    /// A mismatch is rejected before the destination is created.
    InstallExact {
        source: PluginInstallSource,
        expected_content_hash: String,
    },
    /// Refresh a previously installed bundle by name or id. A changed
    /// bundle automatically invalidates its trust receipt at next discovery.
    Update { selector: String },
    /// Delete an installed bundle and prune its persisted state entry.
    /// Requires the bundle to be disabled first.
    Uninstall { selector: String },
}

/// Outcome of a [`PluginMutationRequest`]. The `NeedsApproval` /
/// `NetworkDenied` variants carry the blocked host and are returned without
/// side effects so the caller can route through its own approval flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginMutationOutcome {
    Installed,
    Updated,
    NoChange,
    Uninstalled,
    NeedsApproval(String),
    NetworkDenied(String),
}

/// What a mutation did, for the caller to render.
#[derive(Debug, Clone)]
pub struct PluginMutationReceipt {
    /// Installed/updated/removed plugin name (empty when blocked by policy).
    pub name: String,
    /// Final bundle path (present for install/update).
    pub path: Option<PathBuf>,
    /// Source payload hash before installer provenance is added.
    pub content_hash: Option<String>,
    /// Exact complete-tree hash after installer provenance is written.
    pub installed_content_hash: Option<String>,
    pub outcome: PluginMutationOutcome,
}

/// Inputs shared by mutation operations.
pub struct PluginMutationContext<'a> {
    pub network: &'a NetworkPolicy,
    pub max_size: u64,
}

/// Execute a mutation against the user plugins root described by `registry`.
///
/// The registry is the source of truth for the (pre-dotenv) user plugins
/// root, for name-collision checks across scopes, and — on uninstall — for
/// the disabled precondition and the state-entry prune. Callers rediscover
/// after a successful mutation; the in-memory registry is not updated here.
pub async fn execute(
    request: PluginMutationRequest,
    ctx: &PluginMutationContext<'_>,
    registry: &mut PluginRegistry,
) -> Result<PluginMutationReceipt> {
    match request {
        PluginMutationRequest::Install { source } => {
            install_plugin(source, None, ctx, registry).await
        }
        PluginMutationRequest::InstallExact {
            source,
            expected_content_hash,
        } => install_plugin(source, Some(expected_content_hash.as_str()), ctx, registry).await,
        PluginMutationRequest::Update { selector } => update_plugin(&selector, ctx, registry).await,
        PluginMutationRequest::Uninstall { selector } => uninstall_plugin(&selector, registry),
    }
}

pub async fn install_from_cli(
    source: String,
    config: crate::config::Config,
    mut registry: PluginRegistry,
) -> Result<()> {
    let runtime = tokio::runtime::Handle::current();
    let policy = crate::plugins::activation::extension_host_policy_enabled();
    let receipt = tokio::task::spawn_blocking(move || {
        let _scope = crate::plugins::activation::PolicyScope::propagate(policy);
        let network = config
            .network
            .map(|network| network.into_runtime())
            .unwrap_or_default();
        let source = PluginInstallSource::parse(&source)?;
        runtime.block_on(execute(
            PluginMutationRequest::Install { source },
            &PluginMutationContext {
                network: &network,
                max_size: install::DEFAULT_MAX_SIZE_BYTES,
            },
            &mut registry,
        ))
    })
    .await??;
    match receipt.outcome {
        PluginMutationOutcome::Installed => {
            println!("Installed {} (disabled, untrusted).", receipt.name);
            println!(
                "Open Codewhale, review `/plugin trust {}`, then enable it with `/plugin enable {}`. Start a new session after enabling provider declarations.",
                receipt.name, receipt.name
            );
            Ok(())
        }
        PluginMutationOutcome::NeedsApproval(host) => {
            bail!("Network approval required for {host}; use /network allow {host}, then retry")
        }
        PluginMutationOutcome::NetworkDenied(host) => bail!("Network access denied for {host}"),
        _ => bail!("Unexpected plugin install outcome"),
    }
}

fn user_plugins_dir(registry: &PluginRegistry) -> Result<PathBuf> {
    registry
        .user_plugins_dir()
        .map(PathBuf::from)
        .context("plugin registry has no user plugins root; install is fail-closed")
}

async fn install_plugin(
    source: PluginInstallSource,
    expected_content_hash: Option<&str>,
    ctx: &PluginMutationContext<'_>,
    registry: &mut PluginRegistry,
) -> Result<PluginMutationReceipt> {
    let plugins_dir = user_plugins_dir(registry)?;
    // Pre-check name collisions across scopes: a builtin or workspace bundle
    // with the same name would shadow (or be shadowed by) the install.
    let name_conflict = |name: &str| -> Option<String> {
        registry.get(name).map(|existing| {
            format!(
                "plugin name '{name}' is already used by the {} bundle at {}; \
                 choose a different name or remove that bundle first",
                existing.scope.as_str(),
                existing.canonical_root.display()
            )
        })
    };
    let outcome = match expected_content_hash {
        Some(expected) => {
            install::install_with_expected_content_hash(
                source,
                &plugins_dir,
                ctx.max_size,
                ctx.network,
                &name_conflict,
                expected,
            )
            .await?
        }
        None => {
            install::install(
                source,
                &plugins_dir,
                ctx.max_size,
                ctx.network,
                false,
                &name_conflict,
            )
            .await?
        }
    };
    Ok(match outcome {
        PluginInstallOutcome::Installed(installed) => PluginMutationReceipt {
            name: installed.name,
            path: Some(installed.path),
            content_hash: Some(installed.content_hash),
            installed_content_hash: Some(installed.installed_content_hash),
            outcome: PluginMutationOutcome::Installed,
        },
        PluginInstallOutcome::NeedsApproval(host) => blocked(host, true),
        PluginInstallOutcome::NetworkDenied(host) => blocked(host, false),
    })
}

async fn update_plugin(
    selector: &str,
    ctx: &PluginMutationContext<'_>,
    registry: &mut PluginRegistry,
) -> Result<PluginMutationReceipt> {
    let plugin = registry
        .get(selector)
        .with_context(|| format!("Plugin bundle `{selector}` was not found"))?
        .clone();
    if plugin.scope != PluginScope::User {
        bail!(
            "only user-scope bundles installed via /plugin install can be updated; \
             `{selector}` is a {} bundle",
            plugin.scope.as_str()
        );
    }
    let plugins_dir = user_plugins_dir(registry)?;
    let outcome = install::update(plugin.name(), &plugins_dir, ctx.max_size, ctx.network).await?;
    Ok(match outcome {
        PluginUpdateResult::NoChange => PluginMutationReceipt {
            name: plugin.name().to_string(),
            path: None,
            content_hash: None,
            installed_content_hash: None,
            outcome: PluginMutationOutcome::NoChange,
        },
        PluginUpdateResult::Updated(installed) => PluginMutationReceipt {
            name: installed.name,
            path: Some(installed.path),
            content_hash: Some(installed.content_hash),
            installed_content_hash: Some(installed.installed_content_hash),
            outcome: PluginMutationOutcome::Updated,
        },
        PluginUpdateResult::NeedsApproval(host) => blocked(host, true),
        PluginUpdateResult::NetworkDenied(host) => blocked(host, false),
    })
}

fn uninstall_plugin(
    selector: &str,
    registry: &mut PluginRegistry,
) -> Result<PluginMutationReceipt> {
    let plugin = registry
        .get(selector)
        .with_context(|| format!("Plugin bundle `{selector}` was not found"))?
        .clone();
    if plugin.scope != PluginScope::User {
        bail!(
            "refusing to uninstall the {} bundle `{selector}`; remove it from its own root",
            plugin.scope.as_str()
        );
    }
    if plugin.enabled {
        bail!("plugin `{selector}` is enabled; disable it first with /plugin disable {selector}");
    }
    let plugins_dir = user_plugins_dir(registry)?;
    install::uninstall(plugin.name(), &plugins_dir)?;
    registry
        .prune_state_entry(selector)
        .map_err(anyhow::Error::msg)?;
    Ok(PluginMutationReceipt {
        name: plugin.name().to_string(),
        path: None,
        content_hash: None,
        installed_content_hash: None,
        outcome: PluginMutationOutcome::Uninstalled,
    })
}

fn blocked(host: String, needs_approval: bool) -> PluginMutationReceipt {
    PluginMutationReceipt {
        name: String::new(),
        path: None,
        content_hash: None,
        installed_content_hash: None,
        outcome: if needs_approval {
            PluginMutationOutcome::NeedsApproval(host)
        } else {
            PluginMutationOutcome::NetworkDenied(host)
        },
    }
}

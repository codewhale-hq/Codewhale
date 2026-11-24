use std::sync::Arc;

use codewhale_config::PluginAuthority;
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::activation::{
    PluginActivationCapability, PolicyScope, extension_host_policy_enabled,
};
use codewhale_plugins::install::dsh::{plan_npm_install, refusal_text};
use codewhale_plugins::providers::{PluginProviderDeclaration, validate_declarations};
use codewhale_plugins::recommend::{
    PluginMatchSource, PluginNextStep, PluginTaskRecommendation, lookup_reviewable_plugin,
};
use codewhale_plugins::registry::verify_plugin_component_authority;
use serde::Deserialize;
use serde_json::{Value, json};

use super::dsh_broker::{
    BrokerCall, BrokerEffect, BrokerFuture, BrokerMethods, BrokerRefusal, decode,
};
use super::protocol::DshService;
use crate::pricing::BalanceInfo;

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum CatalogMethod {
    #[serde(rename = "install.plan")]
    InstallPlan(InstallRequest),
    #[serde(rename = "routes.list")]
    RoutesList(NoInput),
    #[serde(rename = "routes.resolve")]
    RoutesResolve(RouteRequest),
    #[serde(rename = "balance")]
    Balance(NoInput),
}

impl BrokerMethods for CatalogMethod {
    const NAMES: &'static [&'static str] =
        &["install.plan", "routes.list", "routes.resolve", "balance"];
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallRequest {
    package: String,
    version: String,
    #[serde(default)]
    registry: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RouteRequest {
    provider: String,
    #[serde(default)]
    model: Option<String>,
}

pub(super) struct AccountBalance {
    pub available: bool,
    pub reading: Option<BalanceInfo>,
}

type ApprovedRoutes = Vec<(String, PluginProviderDeclaration)>;

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    serve_with(call, None)
}

pub(super) fn serve_with(call: BrokerCall, account: Option<AccountBalance>) -> BrokerFuture {
    Box::pin(async move {
        let caller = call.owner.plugin_id.clone();
        match decode::<CatalogMethod>(&call)? {
            CatalogMethod::InstallPlan(request) => {
                let plugins = plugins(&call)?;
                call.context
                    .act("install.plan", BrokerEffect::Read, Some(&request.package))
                    .await?;
                install_plan(&plugins, &caller, request)
            }
            CatalogMethod::RoutesList(NoInput {}) => {
                let plugins = plugins(&call)?;
                call.context
                    .act("routes.list", BrokerEffect::Read, None)
                    .await?;
                let routes = approved_routes(plugins, &caller).await?;
                let routes: Vec<Value> = routes
                    .iter()
                    .map(|(name, declaration)| route(name, declaration))
                    .collect();
                Ok(json!({ "routes": routes }))
            }
            CatalogMethod::RoutesResolve(request) => {
                let plugins = plugins(&call)?;
                call.context
                    .act(
                        "routes.resolve",
                        BrokerEffect::Read,
                        Some(&request.provider),
                    )
                    .await?;
                let routes = approved_routes(plugins, &caller).await?;
                resolve_route(&caller, routes, request)
            }
            CatalogMethod::Balance(NoInput {}) => {
                call.context
                    .act("balance", BrokerEffect::Read, None)
                    .await?;
                Ok(balance(account))
            }
        }
    })
}

fn plugins(call: &BrokerCall) -> Result<Arc<PluginRegistry>, BrokerRefusal> {
    call.context
        .tool()
        .plugin_registry
        .clone()
        .ok_or(BrokerRefusal::NotAvailable(DshService::Catalog))
}

fn refused(message: impl Into<String>) -> BrokerRefusal {
    BrokerRefusal::Refused {
        service: DshService::Catalog,
        message: message.into(),
    }
}

fn review_command(name: &str, next_step: PluginNextStep) -> String {
    PluginTaskRecommendation {
        name: name.to_string(),
        source: PluginMatchSource::Installed {
            id: name.to_string(),
        },
        matched_terms: Vec::new(),
        score: 0,
        next_step,
    }
    .command()
}

fn install_plan(
    plugins: &PluginRegistry,
    caller: &str,
    request: InstallRequest,
) -> Result<Value, BrokerRefusal> {
    let plan = plan_npm_install(
        &request.package,
        &request.version,
        request.registry.as_deref(),
    )
    .map_err(|reason| {
        tracing::info!(target: "codewhale::dsh_catalog", plugin = %caller, "install plan refused");
        refused(reason)
    })?;
    let existing = plugins.get(&plan.plugin_name);
    let (status, command, message) = match existing {
        Some(existing) => {
            let command = lookup_reviewable_plugin(existing.name(), plugins, &[])
                .map(|reviewable| reviewable.command())
                .unwrap_or_else(|| review_command(existing.name(), PluginNextStep::Inspect));
            let message = format!(
                "A plugin named `{}` is already present; nothing was installed. Review it with `{command}`. A market listing does not establish that it is the same bundle.",
                existing.name()
            );
            ("already_present", command, message)
        }
        None => {
            let command = review_command(
                &plan.plugin_name,
                PluginNextStep::SourceInstall {
                    spec: plan.spec.clone(),
                },
            );
            let message = format!(
                "Codewhale installs a plugin only through its reviewed install flow; nothing was installed. Review and install {}@{} with `{command}`.",
                request.package, request.version
            );
            ("review_required", command, message)
        }
    };
    tracing::info!(
        target: "codewhale::dsh_catalog",
        plugin = %caller,
        package = %request.package,
        version = %request.version,
        status,
        "install planned for review; nothing installed"
    );
    let mut answer = json!({
        "status": status,
        "package": request.package,
        "version": request.version,
        "plugin": plan.plugin_name,
        "command": command,
        "message": message,
    });
    if existing.is_none() {
        answer["spec"] = Value::String(plan.spec);
    }
    Ok(answer)
}

fn declared_routes(
    plugins: &PluginRegistry,
    caller: &str,
) -> Result<Option<(ApprovedRoutes, PluginAuthority)>, String> {
    let Some(plugin) = plugins.get(caller).filter(|plugin| plugin.active()) else {
        return Ok(None);
    };
    if plugin.manifest.providers.is_empty() {
        return Ok(None);
    }
    validate_declarations(&plugin.manifest.providers)?;
    let authority = plugins
        .authority_for(caller)
        .ok_or("the plugin has no review receipt")?;
    let routes = plugin
        .manifest
        .providers
        .iter()
        .map(|(name, declaration)| (name.clone(), declaration.clone()))
        .collect();
    Ok(Some((routes, authority)))
}

async fn approved_routes(
    plugins: Arc<PluginRegistry>,
    caller: &str,
) -> Result<ApprovedRoutes, BrokerRefusal> {
    let receipt = |reason: String| {
        tracing::info!(
            target: "codewhale::dsh_catalog",
            plugin = %caller,
            reason = %reason,
            "provider routes refused: the review receipt does not hold"
        );
        refused(
            "the review that approved this plugin's provider routes no longer holds; review the plugin again in Codewhale",
        )
    };
    let Some((routes, authority)) = declared_routes(&plugins, caller).map_err(receipt)? else {
        return Ok(Vec::new());
    };
    let policy = extension_host_policy_enabled();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = crate::test_support::env_scope_ticket();
    let verified = tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let _policy = PolicyScope::propagate(policy);
        verify_plugin_component_authority(&authority, PluginActivationCapability::Providers)
    })
    .await;
    match verified {
        Ok(Ok(())) => Ok(routes),
        Ok(Err(reason)) => Err(receipt(reason)),
        Err(error) => Err(receipt(error.to_string())),
    }
}

fn resolve_route(
    caller: &str,
    routes: ApprovedRoutes,
    request: RouteRequest,
) -> Result<Value, BrokerRefusal> {
    let Some((name, declaration)) = routes
        .into_iter()
        .find(|(name, _)| *name == request.provider)
    else {
        tracing::info!(
            target: "codewhale::dsh_catalog",
            plugin = %caller,
            "provider route refused: not approved for this plugin"
        );
        return Err(refused(format!(
            "`{}` is not a provider route approved for this plugin. A plugin cannot register a model adapter in Codewhale: a model route is a provider route you approve in the plugin's review",
            refusal_text(&request.provider)
        )));
    };
    let model = match request.model {
        None => declaration.model.clone(),
        Some(model) => {
            let offered = if declaration.models.is_empty() {
                declaration
                    .model
                    .as_deref()
                    .is_none_or(|default| default == model)
            } else {
                declaration.models.contains(&model)
            };
            if !offered {
                return Err(refused(format!(
                    "model `{}` is not on the approved provider route `{name}`",
                    refusal_text(&model)
                )));
            }
            Some(model)
        }
    };
    Ok(json!({ "route": route(&name, &declaration), "model": model }))
}

fn route(name: &str, declaration: &PluginProviderDeclaration) -> Value {
    json!({
        "provider": name,
        "base_url": declaration.base_url,
        "default_model": declaration.model,
        "models": declaration.models,
    })
}

fn balance(account: Option<AccountBalance>) -> Value {
    let Some(account) = account else {
        return json!({ "status": "unknown" });
    };
    if !account.available {
        return json!({ "status": "absent" });
    }
    match account
        .reading
        .filter(|reading| !reading.total_balance.trim().is_empty())
    {
        Some(reading) => json!({
            "status": "known",
            "currency": reading.currency,
            "total": reading.total_balance,
            "topped_up": reading.topped_up_balance,
            "granted": reading.granted_balance,
        }),
        None => json!({ "status": "unknown" }),
    }
}

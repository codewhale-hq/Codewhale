use std::io::{self, BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use codewhale_command_contract::facets::CommandPresentationContext;
use codewhale_localization::{Locale, MessageId, tr};
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::host_services::{PersonVia, PluginApprover, ReviewApproval, approver_label};
use codewhale_plugins::install::PluginInstallSource;
use codewhale_plugins::types::LoadedPlugin;

use super::render::{ReviewTextContext, escape_review_text, render_bundle_detail};
use crate::commands::contract::{apply_named_replacements, portable_detail, resolve_message_id};

const AGENT_MARK: &str = "CODEWHALE_AGENT_PROJECT";
const INSTALLED_FROM: &str = ".installed-from";

pub(crate) fn agent_project() -> Option<PathBuf> {
    std::env::var_os(AGENT_MARK)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub(crate) trait Console {
    fn interactive(&self) -> bool;
    fn agent_marked(&self) -> bool;
    fn print(&mut self, text: &str);
    fn confirm(&mut self, prompt: &str) -> bool;
}

pub(crate) struct ProcessConsole;

impl Console for ProcessConsole {
    fn interactive(&self) -> bool {
        io::stdin().is_terminal() && io::stdout().is_terminal() && io::stderr().is_terminal()
    }

    fn agent_marked(&self) -> bool {
        agent_project().is_some()
    }

    fn print(&mut self, text: &str) {
        println!("{text}");
    }

    fn confirm(&mut self, prompt: &str) -> bool {
        let mut stderr = io::stderr();
        let _ = write!(stderr, "{prompt} [y/N] ");
        let _ = stderr.flush();
        let mut answer = String::new();
        io::stdin().lock().read_line(&mut answer).is_ok()
            && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }
}

struct Presentation(Locale);

impl CommandPresentationContext for Presentation {
    fn translate(&self, key: &str, replacements: &[(&str, &str)]) -> Result<String, String> {
        let id = resolve_message_id(key).ok_or("unknown translation key")?;
        apply_named_replacements(&tr(self.0, id), replacements)
            .ok_or_else(|| "invalid translation replacement contract".to_string())
    }

    fn language_name(&self) -> &'static str {
        self.0.translation_target_name()
    }
}

fn text(value: &str) -> String {
    escape_review_text(value, ReviewTextContext::Plain)
}

fn shell_word(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '/' | ':'))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn find<'a>(registry: &'a PluginRegistry, locale: Locale, name: &str) -> Result<&'a LoadedPlugin> {
    registry.get(name).ok_or_else(|| {
        anyhow!(tr(locale, MessageId::CmdPluginBundleNotFound).replace("{name}", &text(name)))
    })
}

fn command_line_approver(registry: &PluginRegistry, marked: bool) -> PluginApprover {
    PluginApprover::command_line(marked.then(|| registry.workspace()))
}

fn host_service_lines(
    registry: &PluginRegistry,
    plugin: &LoadedPlugin,
    marked: bool,
) -> Vec<String> {
    let approver = command_line_approver(registry, marked);
    let mut lines = registry
        .host_service_review_for(plugin.name(), &approver)
        .map(|review| review.text())
        .unwrap_or_default();
    if !lines.is_empty() && !marked {
        lines.push("A trust from the command line grants none of these host services.".to_string());
    }
    lines
}

fn review_text(
    registry: &PluginRegistry,
    plugin: &LoadedPlugin,
    locale: Locale,
    marked: bool,
) -> String {
    let mut output = render_bundle_detail(
        &mut Presentation(locale),
        &portable_detail(plugin),
        true,
        ReviewTextContext::Plain,
    );
    if !output.ends_with('\n') {
        output.push('\n');
    }
    for line in host_service_lines(registry, plugin, marked) {
        output.push_str(&text(&line));
        output.push('\n');
    }
    output.push_str(&format!("Bundle review token: {}", plugin.review_token()));
    output
}

fn trust_command(plugin: &LoadedPlugin) -> String {
    format!(
        "codewhale plugins trust {} --review-token {} --yes",
        shell_word(&text(plugin.name())),
        plugin.review_token()
    )
}

fn review_json(
    registry: &PluginRegistry,
    plugin: &LoadedPlugin,
    locale: Locale,
    marked: bool,
) -> serde_json::Value {
    let mut record = crate::plugin_cli_record(plugin);
    if let Some(object) = record.as_object_mut() {
        object.insert("action".to_string(), "review".into());
        object.insert("trusted_by_this_command".to_string(), false.into());
        object.insert(
            "bundle_review_token".to_string(),
            plugin.review_token().into(),
        );
        object.insert(
            "review".to_string(),
            review_text(registry, plugin, locale, marked).into(),
        );
    }
    record
}

fn trust_as(
    registry: &mut Arc<PluginRegistry>,
    locale: Locale,
    name: &str,
    approver: PluginApprover,
) -> Result<()> {
    let host_services = match approver {
        PluginApprover::Agent(_) => {
            installed_by_the_agent(find(registry, locale, name)?, registry.workspace())?;
            registry
                .host_service_review_for(name, &approver)
                .map(|review| review.token)
        }
        PluginApprover::Person { .. } | PluginApprover::CommandLine => None,
    };
    Arc::make_mut(registry)
        .trust_reviewed(
            name,
            ReviewApproval {
                approved_by: approver,
                host_services,
            },
        )
        .map_err(|error| failed(locale, &error))
}

fn installed_by_the_agent(plugin: &LoadedPlugin, project: &Path) -> Result<()> {
    let source = std::fs::read_to_string(plugin.canonical_root.join(INSTALLED_FROM))
        .ok()
        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok())
        .and_then(|marker| marker.get("spec")?.as_str().map(str::to_string))
        .and_then(|spec| PluginInstallSource::parse(&spec).ok());
    let folder = match source {
        Some(PluginInstallSource::LocalPath(folder) | PluginInstallSource::Dsh(folder))
            if folder.is_absolute() =>
        {
            Some(folder)
        }
        _ => None,
    };
    let inside = folder.is_some_and(|folder| {
        match (
            std::fs::canonicalize(folder),
            std::fs::canonicalize(project),
        ) {
            (Ok(folder), Ok(project)) => folder.starts_with(project),
            _ => false,
        }
    });
    if !inside {
        bail!(
            "The agent may approve only a plugin it installed with `codewhale install` from a folder inside its project, and `{}` was not installed that way. Nothing was changed; ask the person to review it.",
            text(plugin.name())
        );
    }
    Ok(())
}

fn failed(locale: Locale, error: &str) -> anyhow::Error {
    anyhow!(tr(locale, MessageId::CmdPluginActionFailed).replace("{error}", &text(error)))
}

fn report(
    registry: &PluginRegistry,
    locale: Locale,
    name: &str,
    action: &str,
    json: bool,
    console: &mut dyn Console,
) -> Result<()> {
    let plugin = find(registry, locale, name)?;
    let inactive = plugin
        .inventory
        .unsupported_labels()
        .into_iter()
        .map(str::to_string)
        .collect::<Vec<_>>();
    if json {
        console.print(&serde_json::to_string_pretty(&serde_json::json!({
            "name": text(plugin.name()),
            "action": action,
            "state": plugin.state_label(),
            "trust": plugin.trust_status.as_str(),
            "enabled": plugin.enabled,
            "active": plugin.active(),
            "approved_by": plugin.approved_by,
            "enabled_by": plugin.enabled_by,
            "host_services": plugin.host_services,
            "content_hash": plugin.content_hash,
            "capability_hash": plugin.capability_hash,
            "inactive_components": inactive,
        }))?);
    } else {
        console.print(
            &tr(locale, MessageId::CmdPluginBundleMutationSuccess)
                .replace("{name}", &text(plugin.name()))
                .replace("{action}", action),
        );
        if action == "trusted" {
            console.print(&format!(
                "Recorded as {}.",
                approver_label(plugin.approved_by.as_ref())
            ));
        }
        if action == "enabled" && !inactive.is_empty() {
            console.print(&format!(
                "Compatibility: {}. Supported declarative components are active; inactive: {}.",
                plugin.compatibility().as_str(),
                inactive.join(", ")
            ));
        }
    }
    Ok(())
}

fn block_on_mutation<F: std::future::Future>(future: F) -> Result<F::Output> {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => Ok(handle.block_on(future)),
        Err(_) => Ok(tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(future)),
    }
}

fn manifest_version_at(path: &Path) -> Option<String> {
    let manifest_path = codewhale_plugins::agent_plugin::resolve_manifest_path(path)?;
    codewhale_plugins::manifest::PluginManifest::validate_from_path(&manifest_path)
        .ok()
        .map(|validated| validated.manifest.plugin.version)
}

pub(crate) fn show(
    registry: &PluginRegistry,
    locale: Locale,
    name: &str,
    json: bool,
    console: &mut dyn Console,
) -> Result<()> {
    let marked = console.agent_marked();
    let plugin = find(registry, locale, name)?;
    if json {
        console.print(&serde_json::to_string_pretty(&review_json(
            registry, plugin, locale, marked,
        ))?);
    } else {
        console.print(&review_text(registry, plugin, locale, marked));
    }
    Ok(())
}

pub(crate) struct TrustRequest {
    pub name: String,
    pub review_token: Option<String>,
    pub yes: bool,
    pub json: bool,
}

pub(crate) fn trust(
    registry: &mut Arc<PluginRegistry>,
    locale: Locale,
    request: TrustRequest,
    console: &mut dyn Console,
) -> Result<()> {
    let TrustRequest {
        name,
        review_token,
        yes,
        json,
    } = request;
    let marked = console.agent_marked();
    let plugin = find(registry, locale, &name)?;
    let review_again = format!(
        "run `codewhale plugins trust {}` and read the new review",
        shell_word(&text(&name))
    );
    let prompted = if marked {
        command_line_approver(registry, true)
    } else {
        PluginApprover::person(PersonVia::Terminal)
    };

    let Some(presented) = review_token else {
        if yes {
            bail!(
                "`--yes` only skips the confirmation prompt. Trust also needs --review-token, the bundle review token that names the exact content to trust; to see it, {review_again}"
            );
        }
        show(registry, locale, &name, json, console)?;
        if json {
            return Ok(());
        }
        if !console.interactive() {
            console.print(&format!(
                "\nNot trusted. The bundle review token ties trust to exactly the content shown above; it does not record that anyone read this review. A trust made with it is recorded as {}. To trust, run:\n  {}",
                approver_label(Some(&command_line_approver(registry, marked))),
                trust_command(plugin)
            ));
            return Ok(());
        }
        if !console.confirm("\nTrust this bundle with exactly the capabilities above?") {
            console.print("Not trusted.");
            return Ok(());
        }
        trust_as(registry, locale, &name, prompted)?;
        return report(registry, locale, &name, "trusted", json, console);
    };

    if presented.split('.').next() != Some(plugin.content_hash.as_str()) {
        bail!(
            "Plugin bundle `{}` content changed since this bundle review token was issued (current content hash {}); {review_again}",
            text(&name),
            plugin.content_hash,
        );
    }
    if presented != plugin.review_token() {
        bail!(
            "Bundle review token does not match this bundle's current content and capability set; {review_again}"
        );
    }
    let approver = if yes {
        command_line_approver(registry, marked)
    } else {
        if json || !console.interactive() {
            bail!(
                "Trust needs confirmation: pass --yes with the bundle review token, or run in a terminal to be shown the review and asked"
            );
        }
        show(registry, locale, &name, false, console)?;
        if !console.confirm("\nTrust this bundle with exactly the capabilities above?") {
            console.print("Not trusted.");
            return Ok(());
        }
        prompted
    };

    trust_as(registry, locale, &name, approver)?;
    report(registry, locale, &name, "trusted", json, console)
}

pub(crate) fn update(
    registry: &mut Arc<PluginRegistry>,
    locale: Locale,
    name: &str,
    network: &codewhale_net::network_policy::NetworkPolicy,
    json: bool,
    console: &mut dyn Console,
) -> Result<()> {
    use codewhale_plugins::mutation::{
        PluginMutationContext, PluginMutationOutcome, PluginMutationRequest, check_network_outcome,
        execute,
    };
    let before = find(registry, locale, name)?.clone();
    let selector = before.name().to_string();
    let ctx = PluginMutationContext {
        network,
        max_size: codewhale_plugins::install::DEFAULT_MAX_SIZE_BYTES,
    };
    let receipt = block_on_mutation(execute(
        PluginMutationRequest::Update { selector },
        &ctx,
        Arc::make_mut(registry),
    ))?
    .map_err(|error| failed(locale, &format!("{error:#}")))?;
    check_network_outcome(&receipt.outcome)?;
    match receipt.outcome {
        PluginMutationOutcome::Updated => {
            *registry = registry.rediscover_for_workspace(registry.workspace());
            let version = registry
                .get(&receipt.name)
                .map(|plugin| plugin.manifest.plugin.version.clone())
                .or_else(|| receipt.path.as_deref().and_then(manifest_version_at))
                .unwrap_or_default();
            if json {
                let mut record = registry
                    .get(&receipt.name)
                    .map(crate::plugin_cli_record)
                    .unwrap_or_else(|| {
                        serde_json::json!({
                            "name": text(&receipt.name),
                            "version": text(&version),
                            "path": receipt.path.as_ref().map(|p| text(&p.to_string_lossy())),
                            "content_hash": receipt.installed_content_hash.or(receipt.content_hash),
                        })
                    });
                if let Some(object) = record.as_object_mut() {
                    object.insert("action".to_string(), "updated".into());
                }
                console.print(&serde_json::to_string_pretty(&record)?);
            } else {
                let action = if version.is_empty() {
                    "updated".to_string()
                } else {
                    format!("updated to {}", text(&version))
                };
                console.print(
                    &tr(locale, MessageId::CmdPluginBundleMutationSuccess)
                        .replace("{name}", &text(&receipt.name))
                        .replace("{action}", &action),
                );
            }
            Ok(())
        }
        PluginMutationOutcome::NoChange => {
            if json {
                let mut record = crate::plugin_cli_record(&before);
                if let Some(object) = record.as_object_mut() {
                    object.insert("action".to_string(), "already_current".into());
                }
                console.print(&serde_json::to_string_pretty(&record)?);
            } else {
                console.print(
                    &tr(locale, MessageId::CmdPluginBundleMutationSuccess)
                        .replace("{name}", &text(&receipt.name))
                        .replace("{action}", "already current"),
                );
            }
            Ok(())
        }
        _ => bail!("Unexpected plugin update outcome"),
    }
}

pub(crate) fn uninstall(
    registry: &mut Arc<PluginRegistry>,
    locale: Locale,
    name: &str,
    yes: bool,
    json: bool,
    console: &mut dyn Console,
) -> Result<()> {
    use codewhale_plugins::mutation::{PluginMutationContext, PluginMutationRequest, execute};
    use codewhale_plugins::types::PluginScope;
    let plugin = find(registry, locale, name)?.clone();
    let plugin_name = plugin.name().to_string();
    let version = plugin.manifest.plugin.version.clone();
    if !yes {
        if json || !console.interactive() {
            bail!("Uninstall needs confirmation: pass --yes, or run in a terminal to be asked");
        }
        let prompt = if version.is_empty() {
            format!("Remove {}?", text(&plugin_name))
        } else {
            format!("Remove {} {}?", text(&plugin_name), text(&version))
        };
        if !console.confirm(&prompt) {
            console.print("Not removed.");
            return Ok(());
        }
    }
    if plugin.scope == PluginScope::User && plugin.enabled {
        Arc::make_mut(registry)
            .disable(&plugin_name)
            .map_err(|error| failed(locale, &error))?;
    }
    let network = codewhale_net::network_policy::NetworkPolicy::default();
    let ctx = PluginMutationContext {
        network: &network,
        max_size: codewhale_plugins::install::DEFAULT_MAX_SIZE_BYTES,
    };
    let receipt = block_on_mutation(execute(
        PluginMutationRequest::Uninstall {
            selector: plugin_name,
        },
        &ctx,
        Arc::make_mut(registry),
    ))?
    .map_err(|error| failed(locale, &format!("{error:#}")))?;
    if json {
        console.print(&serde_json::to_string_pretty(&serde_json::json!({
            "name": text(&receipt.name),
            "version": text(&version),
            "action": "uninstalled",
        }))?);
    } else {
        console.print(
            &tr(locale, MessageId::CmdPluginBundleMutationSuccess)
                .replace("{name}", &text(&receipt.name))
                .replace("{action}", "uninstalled"),
        );
    }
    Ok(())
}

pub(crate) fn enable(
    registry: &mut Arc<PluginRegistry>,
    locale: Locale,
    name: &str,
    json: bool,
    console: &mut dyn Console,
) -> Result<()> {
    let plugin = find(registry, locale, name)?;
    if !plugin.trusted() {
        if let Some(error) = registry.state_error() {
            bail!(
                "Plugin state could not be read, so the trust of `{}` is unknown and nothing was changed: {}",
                text(name),
                text(error)
            );
        }
        bail!(
            "Plugin bundle `{}` is not trusted (trust: {}), so it cannot be enabled. Review it first with `codewhale plugins trust {}`.",
            text(name),
            plugin.trust_status.as_str(),
            shell_word(&text(name))
        );
    }
    let approver = command_line_approver(registry, console.agent_marked());
    Arc::make_mut(registry)
        .enable_by(name, approver)
        .map_err(|error| failed(locale, &error))?;
    report(registry, locale, name, "enabled", json, console)
}

pub(crate) fn disable(
    registry: &mut Arc<PluginRegistry>,
    locale: Locale,
    name: &str,
    json: bool,
    console: &mut dyn Console,
) -> Result<()> {
    find(registry, locale, name)?;
    Arc::make_mut(registry)
        .disable(name)
        .map_err(|error| failed(locale, &error))?;
    report(registry, locale, name, "disabled", json, console)
}

pub(crate) fn revoke(
    registry: &mut Arc<PluginRegistry>,
    locale: Locale,
    name: &str,
    json: bool,
    console: &mut dyn Console,
) -> Result<()> {
    find(registry, locale, name)?;
    Arc::make_mut(registry)
        .revoke_trust(name)
        .map_err(|error| failed(locale, &error))?;
    report(registry, locale, name, "trust-revoked", json, console)
}

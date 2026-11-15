use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use codewhale_config::{AppMode, PluginAuthority};
use codewhale_execpolicy::ApprovalMode;
use codewhale_mcp::config::McpServerConfig;
use codewhale_net::network_policy::NetworkPolicy;
use codewhale_plugins::activation::{PolicyScope, extension_host_policy_enabled};
use codewhale_plugins::agent_plugin::resolve_manifest_path;
use codewhale_plugins::host_services::{
    AgentApproval, AgentPosture, PersonVia, PluginApprover, ReviewApproval, approver_label, offered,
};
use codewhale_plugins::install::dsh::{has_unresolved_npm_dependencies, try_is_dsh_package};
use codewhale_plugins::install::{
    DEFAULT_MAX_SIZE_BYTES, NpmDepsNetworkError, PluginArchiveMismatch, PluginContentMismatch,
    PluginInstallError, PluginInstallSource, PluginNameConflict, RemoteDownloadOutcome,
    RemotePreviewOutcome, clean_dir_is_strictly_inside, download_remote_install_archive,
    has_regular_install_marker, installed_by_the_agent, path_is_within, preview_dsh,
    preview_dsh_with_network, stage_downloaded_remote_install,
};
use codewhale_plugins::manifest::PluginManifest;
use codewhale_plugins::mutation::{
    PluginMutationContext, PluginMutationOutcome, PluginMutationRequest, execute,
};
use codewhale_plugins::registry::PluginRegistry;
use codewhale_plugins::types::{LoadedPlugin, PluginDiagnosticLevel, PluginScope};
use codewhale_runtime::runtime_policy::approval_wire;
use codewhale_sanitize::redact::redact_model_bound_secrets;
use codewhale_sanitize::sanitize::{is_bidi_control, sanitize_text, strip_ansi_into};
use codewhale_util::bundle_file::open_bundle_file;
use codewhale_util::path_identity::metadata_is_link_or_reparse;
use codewhale_util::utils::truncate_with_ellipsis;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::dsh_broker::{
    BrokerCall, BrokerEffect, BrokerFuture, BrokerMethods, BrokerRefusal, builtin_services, decode,
};
use super::protocol::DshService;
use super::registry::OwnerState;
use crate::core::engine::LiveRuntimeAuthority;
use crate::mcp::{
    McpPool, McpServerConfigExt, format_mcp_error_for_display, qualified_plugin_server_name,
};
use crate::tools::spec::ToolContext;

const LOG: &str = "codewhale::dsh_plugins";
const MAX_INPUT_FIELDS: usize = 32;
const MAX_ENTRIES: usize = 100;
const MAX_ROWS_PER_BUNDLE: usize = 64;
const MAX_NAME_BYTES: usize = 128;
const MAX_ENTRY_ID_BYTES: usize = 256;
const MAX_VERSION_BYTES: usize = 64;
const MAX_TEXT_BYTES: usize = 512;
const MAX_DIAGNOSTIC_BYTES: usize = 1024;
const MAX_TARGET_BYTES: usize = 4096;
const MAX_REGISTRY_BYTES: usize = 1024;
const MAX_ROW_BYTES: usize = 512;
const MAX_APPROVED_BUILDS: usize = 64;
const MAX_BUNDLE_FILES: usize = 16_384;
const MAX_CONVERSION_JSON_BYTES: u64 = 1024 * 1024;
const HOST_LOAD_TIMEOUT_PER_ROW: Duration = Duration::from_secs(5);
const HOST_POLL_INTERVAL: Duration = Duration::from_millis(20);
const MAX_HOST_WAIT: Duration = Duration::from_secs(60);
const MAX_TOOL_WARNINGS_PER_CONNECTOR: usize = 20;
const HOST_OFF_DIAGNOSTIC: &str =
    "the extension host is switched off in this process, so native plugin code did not start";
const ASK_PERSON_HINT: &str = "ask the person: step to Ask, or use the Plugins page";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallInput {
    target: String,
    #[serde(default = "default_enabled")]
    enabled: bool,
    #[serde(default)]
    approved_builds: Option<Vec<String>>,
    #[serde(default)]
    registry: Option<String>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveInput {
    target: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToggleInput {
    target: String,
    #[serde(default)]
    row: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum PluginsMethod {
    #[serde(rename = "list")]
    List(NoInput),
    #[serde(rename = "status")]
    Status(NoInput),
    #[serde(rename = "posture")]
    Posture(NoInput),
    #[serde(rename = "tools")]
    Tools(NoInput),
    #[serde(rename = "install")]
    Install(InstallInput),
    #[serde(rename = "remove")]
    Remove(RemoveInput),
    #[serde(rename = "enable")]
    Enable(ToggleInput),
    #[serde(rename = "disable")]
    Disable(ToggleInput),
}

impl BrokerMethods for PluginsMethod {
    const NAMES: &'static [&'static str] = &[
        "list", "status", "posture", "tools", "install", "remove", "enable", "disable",
    ];
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move {
        if let Some(fields) = call.input.as_object()
            && fields.len() > MAX_INPUT_FIELDS
            && PluginsMethod::NAMES.contains(&call.method.as_str())
        {
            return Err(BrokerRefusal::InvalidInput {
                service: DshService::Plugins,
                message: format!(
                    "a plugins {} input has at most {MAX_INPUT_FIELDS} fields",
                    call.method
                ),
            });
        }
        let method = decode::<PluginsMethod>(&call)?;
        if !builtin_services(&call.owner.plugin_id).contains(&DshService::Plugins) {
            tracing::info!(target: LOG, plugin = %call.owner.plugin_id, "plugins refused: not the creator module");
            return Err(refused());
        }
        let (tool, authority) = live(&call);
        if tool.execution.owner_agent_id.is_some() || tool.execution.child_host.is_some() {
            tracing::info!(target: LOG, "plugins refused: a sub-agent caller");
            return Err(refused());
        }
        match method {
            PluginsMethod::List(_) => {
                call.context.act("list", BrokerEffect::Read, None).await?;
                list(&call, &tool).await
            }
            PluginsMethod::Status(_) => {
                call.context.act("status", BrokerEffect::Read, None).await?;
                status(&call, &tool)
            }
            PluginsMethod::Posture(_) => {
                call.context
                    .act("posture", BrokerEffect::Read, None)
                    .await?;
                posture(&tool, authority)
            }
            PluginsMethod::Tools(_) => {
                call.context.act("tools", BrokerEffect::Read, None).await?;
                tools(&call, &tool)
            }
            PluginsMethod::Install(input) => install(&call, tool, authority, input).await,
            PluginsMethod::Remove(input) => remove(&call, tool, authority, input).await,
            PluginsMethod::Enable(input) => toggle(&call, tool, authority, input, true).await,
            PluginsMethod::Disable(input) => toggle(&call, tool, authority, input, false).await,
        }
    })
}

#[derive(Serialize)]
struct ListResponse {
    entries: Vec<BundleInfo>,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleInfo {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    enabled: bool,
    installed: bool,
    removable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    read_only_reason: Option<&'static str>,
    rows: Vec<BundleRowInfo>,
    overrides: Vec<Value>,
    official: bool,
    optional: bool,
    updatable: bool,
    availability: &'static str,
    codewhale: BundleCodewhaleInfo,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleRowInfo {
    row_id: String,
    module_name: String,
    entry_id: String,
}

#[derive(Serialize)]
struct BundleCodewhaleInfo {
    plugin: String,
    scope: &'static str,
    trust: &'static str,
    state: &'static str,
    active: bool,
    approved_by: Option<ApproverView>,
    enabled_by: Option<ApproverView>,
    services: Vec<ServiceStateView>,
}

#[derive(Serialize)]
struct ApproverView {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    via: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    posture: Option<&'static str>,
    this_project: bool,
    label: &'static str,
}

#[derive(Serialize)]
struct ServiceStateView {
    service: &'static str,
    state: &'static str,
}

#[derive(Serialize)]
struct StatusResponse {
    entries: Vec<StatusRow>,
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StatusRow {
    entry_id: String,
    module_name: String,
    enabled: bool,
    fiber_phase: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostic: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    patch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    read_only_reason: Option<&'static str>,
}

#[derive(Serialize)]
struct PostureResponse {
    mode: &'static str,
    permission_mode: &'static str,
    admitted_by_person: bool,
}

struct ComponentRow {
    label: String,
    is_native: bool,
}

async fn list(call: &BrokerCall, tool: &ToolContext) -> Result<Value, BrokerRefusal> {
    let registry = live_registry(call, tool)?;
    let all_plugins = registry.list();
    let counts = name_counts(&all_plugins);
    let mut truncated = all_plugins.len() > MAX_ENTRIES;
    let has_state_error = registry.state_error().is_some();
    let diagnostic = registry
        .state_error()
        .map(|error| scrub_state_error(error, registry.state_path(), &mut truncated));

    let mut entries = Vec::with_capacity(all_plugins.len().min(MAX_ENTRIES));
    for plugin in all_plugins.into_iter().take(MAX_ENTRIES) {
        let prefix = id_prefix(plugin, &counts);
        let plugin_name = sanitize_inline(plugin.name(), MAX_NAME_BYTES, &mut truncated);
        let version = Some(sanitize_inline(
            &plugin.manifest.plugin.version,
            MAX_VERSION_BYTES,
            &mut truncated,
        ))
        .filter(|value| !value.is_empty());
        let description = plugin
            .manifest
            .plugin
            .description
            .as_deref()
            .map(|raw| sanitize_inline(raw, MAX_TEXT_BYTES, &mut truncated))
            .filter(|value| !value.is_empty());
        let all_rows = collect_component_rows(plugin, &mut truncated);
        if all_rows.len() > MAX_ROWS_PER_BUNDLE {
            truncated = true;
        }
        let rows = all_rows
            .into_iter()
            .take(MAX_ROWS_PER_BUNDLE)
            .map(|row| {
                let (row_id, module_name, entry_id) =
                    row_identities(prefix, &row.label, &mut truncated);
                BundleRowInfo {
                    row_id,
                    module_name,
                    entry_id,
                }
            })
            .collect();

        let removable =
            plugin.scope == PluginScope::User && has_install_record(&plugin.canonical_root).await;
        let read_only_reason = match plugin.scope {
            PluginScope::Builtin => Some("management-required"),
            PluginScope::User | PluginScope::Workspace => None,
        };
        let availability = match plugin.scope {
            PluginScope::Builtin => "installation",
            PluginScope::User | PluginScope::Workspace => "profile",
        };
        let enabled = !has_state_error && plugin.enabled;
        let active = !has_state_error && plugin.active();
        let state = if has_state_error {
            "disabled"
        } else {
            plugin.state_label()
        };
        let approved_by = if has_state_error {
            None
        } else {
            plugin
                .approved_by
                .as_ref()
                .map(|approver| approver_view(approver, plugin.outside_reach))
        };
        let enabled_by = if has_state_error {
            None
        } else {
            plugin
                .enabled_by
                .as_ref()
                .map(|approver| approver_view(approver, plugin.outside_reach))
        };
        let services = if has_state_error {
            Vec::new()
        } else {
            service_views(&registry, plugin)
        };

        entries.push(BundleInfo {
            name: plugin_name.clone(),
            version,
            description,
            enabled,
            installed: true,
            removable,
            read_only_reason,
            rows,
            overrides: Vec::new(),
            official: false,
            optional: false,
            updatable: false,
            availability,
            codewhale: BundleCodewhaleInfo {
                plugin: plugin_name,
                scope: plugin.scope.as_str(),
                trust: plugin.trust_status.as_str(),
                state,
                active,
                approved_by,
                enabled_by,
                services,
            },
        });
    }

    serde_json::to_value(ListResponse {
        entries,
        truncated,
        diagnostic,
    })
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))
}

fn status(call: &BrokerCall, tool: &ToolContext) -> Result<Value, BrokerRefusal> {
    let registry = live_registry(call, tool)?;
    let all_plugins = registry.list();
    let counts = name_counts(&all_plugins);
    let mut truncated = false;
    let mut hit_entry_limit = false;
    let has_state_error = registry.state_error().is_some();
    let top_diagnostic = registry
        .state_error()
        .map(|error| scrub_state_error(error, registry.state_path(), &mut truncated));
    let host_policy = extension_host_policy_enabled();

    let mut entries = Vec::new();
    for plugin in all_plugins {
        let prefix = id_prefix(plugin, &counts);
        let rows = collect_component_rows(plugin, &mut truncated);
        let row_count = rows.len();
        let enabled = !has_state_error && plugin.enabled;
        let report = if host_policy && enabled && rows.iter().any(|row| row.is_native) {
            super::owner_report(plugin.id.as_str())
        } else {
            None
        };
        let single_name = sanitize_inline(plugin.name(), MAX_NAME_BYTES, &mut truncated);
        let plugin_diagnostics = collect_plugin_diagnostics(plugin);

        for row in rows {
            if entries.len() >= MAX_ENTRIES {
                hit_entry_limit = true;
                break;
            }
            let (_, module_name, entry_id) = row_identities(prefix, &row.label, &mut truncated);
            let fiber_phase = row_fiber_phase(&row, enabled, host_policy, report.as_ref());
            let diagnostic = row_diagnostic(
                plugin,
                &row,
                enabled,
                host_policy,
                report.as_ref(),
                &plugin_diagnostics,
                &mut truncated,
            );
            let (patch_id, read_only_reason) = match plugin.scope {
                PluginScope::Builtin => (None, Some("management-required")),
                PluginScope::User | PluginScope::Workspace => {
                    if row_count == 1 {
                        (Some(single_name.clone()), None)
                    } else {
                        (None, Some("unaddressable"))
                    }
                }
            };

            entries.push(StatusRow {
                entry_id,
                module_name,
                enabled,
                fiber_phase,
                diagnostic,
                patch_id,
                read_only_reason,
            });
        }
        if hit_entry_limit {
            break;
        }
    }

    truncated |= hit_entry_limit;

    serde_json::to_value(StatusResponse {
        entries,
        truncated,
        diagnostic: top_diagnostic,
    })
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))
}

fn posture(
    tool: &ToolContext,
    authority: Option<LiveRuntimeAuthority>,
) -> Result<Value, BrokerRefusal> {
    let Some(authority) = authority else {
        return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
    };
    let app_mode = authority.mode();
    let approval_mode = authority.approval_mode();
    let mode = if app_mode == AppMode::Plan {
        "read-only"
    } else {
        approval_mode.agent_baseline_sandbox_mode()
    };
    let permission_mode = if app_mode == AppMode::Plan {
        "plan"
    } else if approval_mode == ApprovalMode::Never {
        "never"
    } else {
        approval_wire(approval_mode)
    };
    serde_json::to_value(PostureResponse {
        mode,
        permission_mode,
        admitted_by_person: tool.execution.human_decision.is_some(),
    })
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))
}

#[derive(Serialize)]
struct ToolsResponse {
    names: Vec<String>,
    tools: Vec<String>,
}

fn tools(call: &BrokerCall, tool: &ToolContext) -> Result<Value, BrokerRefusal> {
    let names: Vec<String> = call
        .context
        .caller_tools()
        .iter()
        .filter(|name| !super::dsh_broker::is_gate_tool(name))
        .filter(|name| {
            !tool
                .disallowed_tools
                .iter()
                .any(|denied| denied.eq_ignore_ascii_case(name))
        })
        .cloned()
        .collect();
    serde_json::to_value(ToolsResponse {
        names: names.clone(),
        tools: names,
    })
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))
}

fn live_registry(
    call: &BrokerCall,
    tool: &ToolContext,
) -> Result<Arc<PluginRegistry>, BrokerRefusal> {
    let Some(fallback) = tool.plugin_registry.as_ref() else {
        return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
    };
    Ok(super::caller_view(
        &tool.workspace,
        Some(&call.session_id),
        tool.execution.owner_agent_id.as_deref(),
    )
    .unwrap_or_else(|| Arc::clone(fallback)))
}

async fn has_install_record(canonical_root: &Path) -> bool {
    let root = canonical_root.to_path_buf();
    tokio::task::spawn_blocking(move || has_regular_install_marker(&root))
        .await
        .unwrap_or(false)
}

fn name_counts<'a>(plugins: &[&'a LoadedPlugin]) -> HashMap<&'a str, usize> {
    let mut counts = HashMap::with_capacity(plugins.len());
    for plugin in plugins {
        *counts.entry(plugin.name()).or_insert(0) += 1;
    }
    counts
}

fn id_prefix<'a>(plugin: &'a LoadedPlugin, counts: &HashMap<&str, usize>) -> &'a str {
    if counts.get(plugin.name()).copied().unwrap_or(0) > 1 {
        plugin.id.as_str()
    } else {
        plugin.name()
    }
}

fn row_identities(prefix: &str, label: &str, truncated: &mut bool) -> (String, String, String) {
    let row_id = sanitize_inline(label, MAX_ENTRY_ID_BYTES, truncated);
    let combined = sanitize_inline(&format!("{prefix}/{label}"), MAX_ENTRY_ID_BYTES, truncated);
    (row_id, combined.clone(), combined)
}

fn approver_view(approver: &PluginApprover, outside_reach: bool) -> ApproverView {
    let (via, posture, this_project) = match approver {
        PluginApprover::Person { via } => (Some(person_via_wire(*via)), None, true),
        PluginApprover::Agent(agent) => (
            None,
            Some(agent_posture_wire(agent.posture)),
            !outside_reach,
        ),
        PluginApprover::CommandLine => (None, None, true),
    };
    ApproverView {
        kind: approver.kind(),
        via,
        posture,
        this_project,
        label: approver_label(Some(approver)),
    }
}

fn person_via_wire(via: PersonVia) -> &'static str {
    match via {
        PersonVia::Terminal => "terminal",
        PersonVia::App => "app",
        PersonVia::ApprovalCard => "approval_card",
    }
}

fn agent_posture_wire(posture: AgentPosture) -> &'static str {
    match posture {
        AgentPosture::FullAccess => "full_access",
        AgentPosture::AutoReview => "auto_review",
        AgentPosture::CommandLine => "command_line",
    }
}

fn service_views(registry: &PluginRegistry, plugin: &LoadedPlugin) -> Vec<ServiceStateView> {
    let Some(review) = registry.host_service_review(plugin.id.as_str()) else {
        return Vec::new();
    };
    review
        .lines
        .into_iter()
        .map(|line| {
            let can_use = plugin.host_services.contains(&line.service) && offered(line.service);
            ServiceStateView {
                service: line.service.name(),
                state: if can_use {
                    "can use"
                } else {
                    "this version does not offer it"
                },
            }
        })
        .collect()
}

fn collect_component_rows(plugin: &LoadedPlugin, truncated: &mut bool) -> Vec<ComponentRow> {
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    let root = &plugin.canonical_root;

    let mut push = |label: String, is_native: bool| {
        let cleaned = sanitize_inline(&label, MAX_NAME_BYTES, truncated);
        if !cleaned.is_empty() && seen.insert(cleaned.clone()) {
            rows.push(ComponentRow {
                label: cleaned,
                is_native,
            });
        }
    };

    for (category, paths, is_native) in [
        ("native", &plugin.components.native, true),
        ("skills", &plugin.components.skills, false),
        ("commands", &plugin.components.commands, false),
        ("agents", &plugin.components.agents, false),
        ("hooks", &plugin.components.hooks, false),
        ("lsp", &plugin.components.lsp, false),
    ] {
        let mut labels: Vec<String> = paths
            .iter()
            .map(|path| relative_component_label(category, root, path))
            .collect();
        labels.sort();
        for label in labels {
            push(label, is_native);
        }
    }

    if let Some(mcp_servers) = plugin.manifest.mcp_servers.as_ref() {
        for name in mcp_servers.keys() {
            push(format!("mcp/{name}"), false);
        }
    }
    for name in plugin.manifest.providers.keys() {
        push(format!("providers/{name}"), false);
    }

    rows
}

fn relative_component_label(category: &str, root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).ok().and_then(|stripped| {
        let parts: Vec<String> = stripped
            .components()
            .filter_map(|component| match component {
                std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        (!parts.is_empty()).then(|| parts.join("/"))
    });
    match rel {
        Some(rel) if rel == category || rel.starts_with(&format!("{category}/")) => rel,
        Some(rel) => format!("{category}/{rel}"),
        None => {
            let leaf = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| "component".to_string());
            format!("{category}/{leaf}")
        }
    }
}

fn row_fiber_phase(
    row: &ComponentRow,
    enabled: bool,
    host_policy: bool,
    report: Option<&super::OwnerReport>,
) -> Option<&'static str> {
    if !row.is_native || !enabled || !host_policy {
        return None;
    }
    match report.and_then(|entry| entry.state.as_ref()) {
        Some(OwnerState::Activating) => Some("loading"),
        Some(OwnerState::Active) => Some("active"),
        Some(OwnerState::Failed(_)) | Some(OwnerState::Faulted(_)) => Some("failed"),
        Some(OwnerState::Revoked) | None => None,
    }
}

fn collect_plugin_diagnostics(plugin: &LoadedPlugin) -> Vec<String> {
    plugin
        .diagnostics
        .iter()
        .map(|diagnostic| {
            let level = match diagnostic.level {
                PluginDiagnosticLevel::Warning => "warning",
                PluginDiagnosticLevel::Error => "error",
            };
            let suffix = diagnostic
                .path
                .as_deref()
                .map(|path| format!(" ({})", diagnostic_path(&plugin.canonical_root, path)))
                .unwrap_or_default();
            format!(
                "{level} [{}]: {}{suffix}",
                diagnostic.code, diagnostic.message
            )
        })
        .collect()
}

fn strip_root_variants<'a>(path: &'a Path, root: &Path) -> Option<&'a Path> {
    if let Ok(rel) = path.strip_prefix(root) {
        return Some(rel);
    }
    let root_str = root.to_str()?;
    for (prefix, replacement) in [
        ("/private/var/", "/var/"),
        ("/private/tmp/", "/tmp/"),
        ("/private/etc/", "/etc/"),
    ] {
        if let Some(rest) = root_str.strip_prefix(prefix) {
            let alt = format!("{replacement}{rest}");
            if let Ok(rel) = path.strip_prefix(&alt) {
                return Some(rel);
            }
        }
    }
    if let Some(unprefixed) = root_str.strip_prefix(r"\\?\")
        && let Ok(rel) = path.strip_prefix(unprefixed)
    {
        return Some(rel);
    }
    None
}

fn diagnostic_path(root: &Path, path: &Path) -> String {
    if let Some(rel) = strip_root_variants(path, root) {
        let mut parts = Vec::new();
        for component in rel.components() {
            match component {
                std::path::Component::Normal(part) => {
                    parts.push(part.to_string_lossy().into_owned());
                }
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_) => return "<outside-path>".to_string(),
            }
        }
        if !parts.is_empty() {
            return parts.join("/");
        }
    }
    "<outside-path>".to_string()
}

fn row_diagnostic(
    plugin: &LoadedPlugin,
    row: &ComponentRow,
    enabled: bool,
    host_policy: bool,
    report: Option<&super::OwnerReport>,
    plugin_diagnostics: &[String],
    truncated: &mut bool,
) -> Option<String> {
    let mut parts = Vec::new();
    let mut push_unique = |raw: &str| {
        let stripped = strip_plugin_root(raw, &plugin.canonical_root);
        let cleaned = clean_diagnostic_fragment(&stripped);
        if !cleaned.is_empty() && !parts.contains(&cleaned) {
            parts.push(cleaned);
        }
    };

    if row.is_native && enabled && !host_policy {
        push_unique(HOST_OFF_DIAGNOSTIC);
    }
    if let (true, Some(report)) = (row.is_native, report) {
        match report.state.as_ref() {
            Some(OwnerState::Failed(reason)) | Some(OwnerState::Faulted(reason)) => {
                push_unique(reason);
                for line in &report.diagnostics {
                    push_unique(line);
                }
            }
            None => {
                for line in &report.diagnostics {
                    push_unique(line);
                }
            }
            Some(OwnerState::Activating | OwnerState::Active | OwnerState::Revoked) => {}
        }
    }
    for line in plugin_diagnostics {
        push_unique(line);
    }

    if parts.is_empty() {
        return None;
    }
    Some(cap_text(parts.join("; "), MAX_DIAGNOSTIC_BYTES, truncated))
}

fn scrub_state_error(error: &str, state_path: Option<&Path>, truncated: &mut bool) -> String {
    let mut text = error.to_string();
    if let Some(path) = state_path {
        let name = path
            .file_name()
            .map(|leaf| leaf.to_string_lossy().into_owned())
            .filter(|leaf| !leaf.is_empty())
            .unwrap_or_else(|| "plugin-state.json".to_string());
        text = strip_path_with_replacement(&text, path, &name);
        if let Some(parent) = path.parent() {
            text = strip_path_prefix_clean(&text, parent);
        }
    }
    let cleaned = clean_diagnostic_fragment(&text);
    cap_text(cleaned, MAX_DIAGNOSTIC_BYTES, truncated)
}

fn strip_plugin_root(text: &str, root: &Path) -> String {
    strip_path_prefix_clean(text, root)
}

fn strip_path_with_replacement(text: &str, target: &Path, replacement: &str) -> String {
    let raw = target.to_string_lossy();
    if raw.is_empty() {
        return text.to_string();
    }
    let mut out = text.replace(raw.as_ref(), replacement);
    if let Some(unprefixed) = raw.strip_prefix("/private/") {
        out = out.replace(&format!("/{unprefixed}"), replacement);
    }
    if let Some(unprefixed) = raw.strip_prefix(r"\\?\") {
        out = out.replace(unprefixed, replacement);
    }
    out
}

fn strip_path_prefix_clean(text: &str, root: &Path) -> String {
    let raw = root.to_string_lossy();
    if raw.is_empty() {
        return text.to_string();
    }
    let mut out = strip_single_root(text, raw.as_ref());
    if let Some(unprefixed) = raw.strip_prefix("/private/") {
        out = strip_single_root(&out, &format!("/{unprefixed}"));
    }
    if let Some(unprefixed) = raw.strip_prefix(r"\\?\") {
        out = strip_single_root(&out, unprefixed);
    }
    out
}

fn strip_single_root(text: &str, root: &str) -> String {
    let trimmed = root.trim_end_matches(['/', '\\']);
    if trimmed.is_empty() {
        return text.to_string();
    }
    text.replace(&format!("{trimmed}/"), "")
        .replace(&format!("{trimmed}\\"), "")
        .replace(trimmed, "<plugin-root>")
}

fn clean_diagnostic_fragment(raw: &str) -> String {
    let mut no_ansi = String::with_capacity(raw.len());
    strip_ansi_into(raw, &mut no_ansi);
    let paths_scrubbed = scrub_outside_paths(&no_ansi);
    let redacted = redact_model_bound_secrets(&sanitize_text(&paths_scrubbed));
    collapse_controls(&redacted)
}

fn sanitize_inline(raw: &str, max_bytes: usize, truncated: &mut bool) -> String {
    let mut no_ansi = String::with_capacity(raw.len());
    strip_ansi_into(raw, &mut no_ansi);
    let redacted = redact_model_bound_secrets(&sanitize_text(&no_ansi));
    cap_text(collapse_controls(&redacted), max_bytes, truncated)
}

fn collapse_controls(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_was_space = false;
    for ch in raw.chars() {
        if matches!(ch, '\r' | '\n' | '\t') {
            if !last_was_space {
                out.push(' ');
                last_was_space = true;
            }
            continue;
        }
        if ch.is_control() || is_bidi_control(ch) {
            continue;
        }
        last_was_space = ch == ' ';
        out.push(ch);
    }
    out.trim().to_string()
}

fn cap_text(cleaned: String, max_bytes: usize, truncated: &mut bool) -> String {
    if cleaned.len() > max_bytes {
        *truncated = true;
        truncate_with_ellipsis(&cleaned, max_bytes, "...")
    } else {
        cleaned
    }
}

fn scrub_outside_paths(text: &str) -> String {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(
            r#"(^|[\s(\['"=:<`{,|])((?:file://[^\s)\]'";,>`}|]+)|(?:/(?:etc|usr|var|tmp|home|Users|Volumes|private|opt|root|proc|sys|dev|mnt|media|srv|bin|sbin|lib|lib64|System|Library|Applications)(?:/[^\s)\]'";,>`}|]*)?)|(?:/[A-Za-z0-9._~@-]+/[^\s)\]'";,>`}|]*)|(?:[A-Za-z]:[/\\][^\s)\]'";,>`}|]*)|(?:\\\\[^\s)\]'";,>`}|]+))"#,
        )
        .expect("outside-path regex")
    });
    re.replace_all(text, "${1}<outside-path>").into_owned()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangeResult {
    changed: bool,
    application: &'static str,
    stage: &'static str,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ManagementErrorWire>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundle: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

#[derive(Serialize)]
struct ManagementErrorWire {
    code: &'static str,
    diagnostic: String,
    message: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PendingApprover {
    Agent(AgentPosture),
    Person,
    Nobody,
}

impl PendingApprover {
    fn as_approver(
        self,
        call: &BrokerCall,
        tool: &ToolContext,
        workspace: &Path,
    ) -> Option<PluginApprover> {
        match self {
            Self::Agent(posture) => Some(build_agent_approval(call, tool, posture, workspace)),
            Self::Person => Some(PluginApprover::person(PersonVia::ApprovalCard)),
            Self::Nobody => None,
        }
    }
}

struct ScrubContext<'a> {
    workspace: &'a Path,
    canon_workspace: Option<&'a Path>,
    plugin_root: Option<&'a Path>,
    user_plugins_dir: Option<&'a Path>,
    state_path: Option<&'a Path>,
}

enum PreGateLocal {
    Ready {
        plugin_name: String,
        source_package: Option<String>,
        version: Option<String>,
        pre_hash: String,
        reach_gated: bool,
    },
    Deferred {
        deferred_error: Option<(&'static str, String)>,
    },
}

enum PreparedSource {
    Local {
        source: PluginInstallSource,
        source_dir: PathBuf,
        pre_gate: PreGateLocal,
        canon_workspace: Option<PathBuf>,
    },
    Remote {
        source: PluginInstallSource,
        label: String,
        host: String,
        canon_workspace: Option<PathBuf>,
    },
}

enum PreparedExisting {
    Found {
        plugin: Box<LoadedPlugin>,
        is_own: bool,
        own_denial_reason: Option<&'static str>,
        has_marker: bool,
        canon_workspace: Option<PathBuf>,
    },
    Missing,
    Unremovable(String),
    NotSoleRow(String),
    Ambiguous(String),
}

#[cfg(test)]
type PostInstallTestHook = Box<dyn FnOnce(&Path, &tokio_util::sync::CancellationToken) + Send>;

#[cfg(test)]
type PostDisableTestHook = Box<dyn FnOnce(&LoadedPlugin) + Send>;

#[cfg(test)]
thread_local! {
    static POST_INSTALL_TEST_HOOK: std::cell::RefCell<Option<PostInstallTestHook>> =
        const { std::cell::RefCell::new(None) };
    static POST_DISABLE_TEST_HOOK: std::cell::RefCell<Option<PostDisableTestHook>> =
        const { std::cell::RefCell::new(None) };
    static MCP_CONNECT_DEADLINE_FOR_TEST: std::cell::Cell<Option<Duration>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(super) struct McpConnectDeadlineGuard {
    previous: Option<Duration>,
}

#[cfg(test)]
impl Drop for McpConnectDeadlineGuard {
    fn drop(&mut self) {
        MCP_CONNECT_DEADLINE_FOR_TEST.with(|cell| {
            cell.set(self.previous);
        });
    }
}

#[cfg(test)]
pub(super) fn set_mcp_connect_deadline_for_test(deadline: Duration) -> McpConnectDeadlineGuard {
    let previous = MCP_CONNECT_DEADLINE_FOR_TEST.with(|cell| cell.replace(Some(deadline)));
    McpConnectDeadlineGuard { previous }
}

fn mcp_connect_deadline() -> Duration {
    #[cfg(test)]
    if let Some(override_deadline) = MCP_CONNECT_DEADLINE_FOR_TEST.with(|cell| cell.get()) {
        return override_deadline;
    }
    super::tool::TOOL_CALL_DEADLINE
}

#[cfg(test)]
pub(super) fn set_post_install_hook_for_test(hook: PostInstallTestHook) {
    POST_INSTALL_TEST_HOOK.with(|cell| {
        *cell.borrow_mut() = Some(hook);
    });
}

#[cfg(test)]
pub(super) fn set_post_disable_hook_for_test(hook: PostDisableTestHook) {
    POST_DISABLE_TEST_HOOK.with(|cell| {
        *cell.borrow_mut() = Some(hook);
    });
}

#[cfg(test)]
fn fire_post_install_hook_for_test(
    installed_dir: &Path,
    cancel: &tokio_util::sync::CancellationToken,
) {
    if let Some(hook) = POST_INSTALL_TEST_HOOK.with(|cell| cell.borrow_mut().take()) {
        hook(installed_dir, cancel);
    }
}

#[cfg(test)]
fn fire_post_disable_hook_for_test(plugin: &LoadedPlugin) {
    if let Some(hook) = POST_DISABLE_TEST_HOOK.with(|cell| cell.borrow_mut().take()) {
        hook(plugin);
    }
}

fn validate_required_str(
    value: &str,
    field: &str,
    max_bytes: usize,
) -> Result<String, BrokerRefusal> {
    if value.contains('\0') {
        return Err(BrokerRefusal::InvalidInput {
            service: DshService::Plugins,
            message: format!("plugin {field} must not contain NUL bytes"),
        });
    }
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(BrokerRefusal::InvalidInput {
            service: DshService::Plugins,
            message: format!("plugin {field} must not be empty"),
        });
    }
    if trimmed.len() > max_bytes || value.len() > max_bytes {
        return Err(BrokerRefusal::InvalidInput {
            service: DshService::Plugins,
            message: format!("plugin {field} exceeds maximum length of {max_bytes} bytes"),
        });
    }
    Ok(trimmed.to_string())
}

fn validate_optional_str(
    value: Option<&str>,
    field: &str,
    max_bytes: usize,
) -> Result<Option<String>, BrokerRefusal> {
    value
        .map(|raw| validate_required_str(raw, field, max_bytes))
        .transpose()
}

fn validate_approved_builds(builds: Option<&[String]>) -> Result<bool, BrokerRefusal> {
    let Some(list) = builds else {
        return Ok(false);
    };
    if list.len() > MAX_APPROVED_BUILDS {
        return Err(BrokerRefusal::InvalidInput {
            service: DshService::Plugins,
            message: format!("approved_builds has at most {MAX_APPROVED_BUILDS} entries"),
        });
    }
    for entry in list {
        validate_required_str(entry, "approved_builds entry", MAX_TEXT_BYTES)?;
    }
    Ok(true)
}

fn approver_from_posture(authority: &LiveRuntimeAuthority) -> PendingApprover {
    if authority.mode() == AppMode::Plan {
        return PendingApprover::Nobody;
    }
    match authority.approval_mode() {
        ApprovalMode::Suggest | ApprovalMode::AcceptEdits => PendingApprover::Person,
        ApprovalMode::Auto => PendingApprover::Agent(AgentPosture::AutoReview),
        ApprovalMode::Bypass => PendingApprover::Agent(AgentPosture::FullAccess),
        ApprovalMode::Never => PendingApprover::Nobody,
    }
}

fn build_agent_approval(
    call: &BrokerCall,
    tool: &ToolContext,
    posture: AgentPosture,
    workspace: &Path,
) -> PluginApprover {
    let thread_id = tool
        .execution
        .runtime
        .active_thread_id
        .clone()
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| call.session_id.clone());
    PluginApprover::Agent(AgentApproval {
        posture,
        workspace: workspace.to_string_lossy().into_owned(),
        thread_id: Some(thread_id),
        turn_id: tool.origin_turn_id.clone(),
        tool_call_id: tool.origin_tool_call_id.clone(),
    })
}

fn has_agent_call_identity(call: &BrokerCall, tool: &ToolContext) -> bool {
    (!tool
        .execution
        .runtime
        .active_thread_id
        .as_deref()
        .is_none_or(str::is_empty)
        || !call.session_id.is_empty())
        && tool
            .origin_turn_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
        && tool
            .origin_tool_call_id
            .as_deref()
            .is_some_and(|id| !id.is_empty())
}

fn network_policy_for(tool: &ToolContext) -> NetworkPolicy {
    tool.network_policy
        .as_ref()
        .map(|decider| decider.policy().clone())
        .unwrap_or_default()
}

async fn ensure_no_links_in_tree(root: &Path) -> Result<(), String> {
    let root_meta = tokio::fs::symlink_metadata(root)
        .await
        .map_err(|error| format!("failed to inspect install source: {error}"))?;
    if metadata_is_link_or_reparse(&root_meta) || root_meta.file_type().is_symlink() {
        return Err(
            "symbolic links and reparse points are not allowed in plugin bundles".to_string(),
        );
    }
    if !root_meta.is_dir() {
        return Err("not-bundle: install source must be a directory".to_string());
    }
    let mut queue = vec![root.to_path_buf()];
    let mut seen_entries = 0usize;
    while let Some(dir) = queue.pop() {
        let mut reader = tokio::fs::read_dir(&dir)
            .await
            .map_err(|error| format!("failed to read install source: {error}"))?;
        while let Some(entry) = reader
            .next_entry()
            .await
            .map_err(|error| format!("failed to read install source entry: {error}"))?
        {
            seen_entries += 1;
            if seen_entries > MAX_BUNDLE_FILES {
                return Err(format!(
                    "plugin bundle exceeds maximum entry count ({MAX_BUNDLE_FILES})"
                ));
            }
            let path = entry.path();
            let meta = tokio::fs::symlink_metadata(&path)
                .await
                .map_err(|error| format!("failed to inspect install source entry: {error}"))?;
            if metadata_is_link_or_reparse(&meta) || meta.file_type().is_symlink() {
                return Err(
                    "symbolic links and reparse points are not allowed in plugin bundles"
                        .to_string(),
                );
            }
            if meta.is_dir() {
                queue.push(path);
            }
        }
    }
    Ok(())
}

fn read_dsh_source_package(root: &Path) -> Option<String> {
    let file = open_bundle_file(&root.join("CONVERSION.json")).ok()?;
    let mut body = String::new();
    file.take(MAX_CONVERSION_JSON_BYTES + 1)
        .read_to_string(&mut body)
        .ok()?;
    if body.len() as u64 > MAX_CONVERSION_JSON_BYTES {
        return None;
    }
    let parsed: Value = serde_json::from_str(&body).ok()?;
    parsed
        .get("source")
        .and_then(|source| source.get("package"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

enum TargetLookup<'a> {
    Found(&'a LoadedPlugin),
    Missing,
    Ambiguous {
        message: String,
        first: &'a LoadedPlugin,
    },
}

fn resolve_plugin_by_target<'a>(registry: &'a PluginRegistry, target: &str) -> TargetLookup<'a> {
    if let Some(plugin) = registry.get(target) {
        return TargetLookup::Found(plugin);
    }
    let matches: Vec<&LoadedPlugin> = registry
        .list()
        .into_iter()
        .filter(|plugin| {
            read_dsh_source_package(&plugin.canonical_root)
                .as_deref()
                .is_some_and(|pkg| pkg == target)
        })
        .collect();
    match matches.len() {
        0 => TargetLookup::Missing,
        1 => TargetLookup::Found(matches[0]),
        _ => {
            let names: Vec<_> = matches.iter().map(|plugin| plugin.name()).collect();
            TargetLookup::Ambiguous {
                message: format!(
                    "target `{target}` matches multiple installed plugins ({}); specify the plugin name or id",
                    names.join(", ")
                ),
                first: matches[0],
            }
        }
    }
}

fn non_empty_version(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn is_test_loopback_http_url(spec: &str) -> bool {
    cfg!(test)
        && spec.strip_prefix("http://127.0.0.1:").is_some_and(|rest| {
            rest.split('/')
                .next()
                .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        })
}

fn parse_install_target_syntax(spec: &str) -> Result<PluginInstallSource, (&'static str, String)> {
    if spec.starts_with("http://") && !is_test_loopback_http_url(spec) {
        return Err((
            "invalid-spec",
            "install target URL must use https://, not http://".to_string(),
        ));
    }
    let parsed = PluginInstallSource::parse_syntax(spec)
        .map_err(|error| ("invalid-spec", format!("invalid install spec: {error}")))?;
    match &parsed {
        PluginInstallSource::LocalPath(path) | PluginInstallSource::Dsh(path) => {
            if !path.is_absolute() {
                return Err((
                    "invalid-spec",
                    "local or DSH install target must be an absolute directory path, or a valid npm/git/github/https spec".to_string(),
                ));
            }
        }
        PluginInstallSource::Remote(remote) => {
            if let codewhale_skill_install::install::InstallSource::DirectUrl(url) = remote
                && !url.starts_with("https://")
                && !is_test_loopback_http_url(url)
            {
                return Err((
                    "invalid-spec",
                    "install target URL must use https://".to_string(),
                ));
            }
        }
    }
    Ok(parsed)
}

async fn dir_is_in_user_plugins(dir: &Path, user_plugins_dir: Option<&Path>) -> bool {
    let Some(up) = user_plugins_dir else {
        return false;
    };
    let Ok(cdir) = tokio::fs::canonicalize(dir).await else {
        return true;
    };
    match tokio::fs::canonicalize(up).await {
        Ok(cup) => path_is_within(&cup, &cdir, true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::symlink_metadata(up).await.is_ok()
        }
        Err(_) => true,
    }
}

fn is_macos_system_root_link(prefix: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        prefix == Path::new("/var") || prefix == Path::new("/tmp") || prefix == Path::new("/etc")
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = prefix;
        false
    }
}

async fn has_symlink_or_reparse_ancestor_or_target(path: &Path) -> Result<bool, std::io::Error> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => return Ok(true),
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                prefix.push(component.as_os_str());
            }
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => {
                prefix.push(part);
                let meta = tokio::fs::symlink_metadata(&prefix).await?;
                if metadata_is_link_or_reparse(&meta) && !is_macos_system_root_link(&prefix) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

async fn dir_has_clean_root_manifest(dir: &Path) -> bool {
    let Ok(meta) = tokio::fs::symlink_metadata(dir).await else {
        return false;
    };
    if metadata_is_link_or_reparse(&meta) || !meta.is_dir() {
        return false;
    }
    let mut clean_candidates = Vec::new();
    for rel in [
        "plugin.json",
        "kimi.plugin.json",
        "plugin.toml",
        ".claude-plugin/plugin.json",
        ".plugin/plugin.json",
        ".github/plugin/plugin.json",
    ] {
        if let Some(parent) = Path::new(rel)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            let parent_dir = dir.join(parent);
            let Ok(pmeta) = tokio::fs::symlink_metadata(&parent_dir).await else {
                continue;
            };
            if metadata_is_link_or_reparse(&pmeta) || !pmeta.is_dir() {
                continue;
            }
        }
        clean_candidates.push(dir.join(rel));
    }
    let dir_owned = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if clean_candidates
            .iter()
            .any(|candidate| open_bundle_file(candidate).is_ok())
        {
            return true;
        }
        let pkg = dir_owned.join("package.json");
        open_bundle_file(&pkg).is_ok() && try_is_dsh_package(&dir_owned).unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

fn remote_target_display(source: &PluginInstallSource, raw: &str) -> (String, String) {
    match source {
        PluginInstallSource::Remote(
            codewhale_skill_install::install::InstallSource::GitHubRepo(repo),
        ) => (repo.clone(), "github.com".to_string()),
        PluginInstallSource::Remote(
            codewhale_skill_install::install::InstallSource::DirectUrl(url),
        ) => {
            let host = reqwest::Url::parse(url)
                .ok()
                .and_then(|parsed| parsed.host_str().map(str::to_string))
                .unwrap_or_else(|| "unknown-host".to_string());
            (raw.to_string(), host)
        }
        _ => (raw.to_string(), "unknown-host".to_string()),
    }
}

fn classify_install_error(error: &anyhow::Error) -> &'static str {
    if error.downcast_ref::<PluginNameConflict>().is_some()
        || error
            .downcast_ref::<PluginInstallError>()
            .is_some_and(|kind| matches!(kind, PluginInstallError::AlreadyInstalled(_)))
    {
        return "already-installed";
    }
    if error
        .downcast_ref::<PluginInstallError>()
        .is_some_and(|kind| matches!(kind, PluginInstallError::NotInstalledHere(_)))
    {
        return "not-removable";
    }
    if error
        .downcast_ref::<PluginInstallError>()
        .is_some_and(|kind| matches!(kind, PluginInstallError::PluginTomlRoots(_)))
    {
        return "not-bundle";
    }
    let message = format!("{error:#}");
    classify_error_message(&message)
}

fn classify_error_message(message: &str) -> &'static str {
    if message.contains("incompatible-version") {
        "incompatible-version"
    } else if message.contains("unsupported-platform") {
        "unsupported-platform"
    } else if message.contains("not-bundle:")
        || message.contains("no plugin.json")
        || message.contains("not a valid plugin bundle")
        || message.contains("not a DSH bundle")
        || message.contains("does not declare `dsh.bundle`")
        || message.contains("does not declare dsh.bundle")
        || message.contains("package.json") && message.contains("no such file")
    {
        "not-bundle"
    } else {
        "operation-error"
    }
}

fn scrub_message(raw: &str, scrub: &ScrubContext<'_>) -> String {
    let mut text = raw.to_string();
    if let Some(plugin_root) = scrub.plugin_root {
        text = strip_path_prefix_clean(&text, plugin_root);
    }
    if let Some(canon_workspace) = scrub.canon_workspace {
        text = strip_path_with_replacement(&text, canon_workspace, "<project>");
    }
    text = strip_path_with_replacement(&text, scrub.workspace, "<project>");
    if let Some(user_plugins_dir) = scrub.user_plugins_dir {
        text = strip_path_with_replacement(&text, user_plugins_dir, "<plugins-dir>");
    }
    if let Some(state_path) = scrub.state_path {
        let name = state_path
            .file_name()
            .map(|leaf| leaf.to_string_lossy().into_owned())
            .filter(|leaf| !leaf.is_empty())
            .unwrap_or_else(|| "plugin-state.json".to_string());
        text = strip_path_with_replacement(&text, state_path, &name);
        if let Some(parent) = state_path.parent() {
            text = strip_path_prefix_clean(&text, parent);
        }
    }
    let mut truncated = false;
    cap_text(
        clean_diagnostic_fragment(&text),
        MAX_DIAGNOSTIC_BYTES,
        &mut truncated,
    )
}

fn scrub_warning(raw: &str, scrub: &ScrubContext<'_>) -> String {
    let mut text = raw.to_string();
    if let Some(plugin_root) = scrub.plugin_root {
        text = strip_path_prefix_clean(&text, plugin_root);
    }
    if let Some(canon_workspace) = scrub.canon_workspace {
        text = strip_path_with_replacement(&text, canon_workspace, "<project>");
    }
    text = strip_path_with_replacement(&text, scrub.workspace, "<project>");
    let mut truncated = false;
    cap_text(
        clean_diagnostic_fragment(&text),
        MAX_TEXT_BYTES,
        &mut truncated,
    )
}

fn change_ok(
    stage: &'static str,
    target: String,
    changed: bool,
    enabled: Option<bool>,
    bundle: Option<String>,
    version: Option<String>,
    warnings: Vec<String>,
) -> Result<Value, BrokerRefusal> {
    serde_json::to_value(ChangeResult {
        changed,
        application: "applied",
        stage,
        target,
        enabled,
        error: None,
        warnings,
        bundle,
        version,
    })
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))
}

struct ChangeOutcome {
    application: &'static str,
    stage: &'static str,
    target: String,
    changed: bool,
    enabled: Option<bool>,
    code: &'static str,
    diagnostic: String,
    bundle: Option<String>,
    version: Option<String>,
    warnings: Vec<String>,
}

fn change_with_outcome(outcome: ChangeOutcome) -> Result<Value, BrokerRefusal> {
    let error = Some(ManagementErrorWire {
        code: outcome.code,
        diagnostic: outcome.diagnostic.clone(),
        message: outcome.diagnostic,
    });
    serde_json::to_value(ChangeResult {
        changed: outcome.changed,
        application: outcome.application,
        stage: outcome.stage,
        target: outcome.target,
        enabled: outcome.enabled,
        error,
        warnings: outcome.warnings,
        bundle: outcome.bundle,
        version: outcome.version,
    })
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))
}

fn cancelled_result(
    stage: &'static str,
    target: String,
    bundle: Option<String>,
    version: Option<String>,
    warnings: Vec<String>,
) -> Result<Value, BrokerRefusal> {
    serde_json::to_value(ChangeResult {
        changed: false,
        application: "cancelled",
        stage,
        target,
        enabled: None,
        error: None,
        warnings,
        bundle,
        version,
    })
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))
}

fn record_outcome(
    call: &BrokerCall,
    method: &str,
    plugin: Option<&str>,
    hash: Option<&str>,
    approver: Option<&PluginApprover>,
    application: &str,
) {
    let (approver_kind, posture) = match approver {
        Some(PluginApprover::Person { via }) => ("person", Some(person_via_wire(*via))),
        Some(PluginApprover::Agent(agent)) => ("agent", Some(agent_posture_wire(agent.posture))),
        Some(PluginApprover::CommandLine) => ("command_line", None),
        None => ("none", None),
    };
    let mut entry = json!({
        "method": method,
        "plugin": plugin.unwrap_or(""),
        "hash": hash.unwrap_or(""),
        "approver": approver_kind,
        "application": application,
    });
    if let Some(posture) = posture {
        entry["posture"] = json!(posture);
    }
    let _ = call.context.record(entry);
}

fn is_managed_policy_error(msg: &str) -> bool {
    msg.contains("Managed plugin policy") || msg.contains("forbidden by the managed plugin policy")
}

fn agent_denial_reason(
    plugin: &LoadedPlugin,
    state_error: Option<&str>,
    installed_by_agent: bool,
) -> &'static str {
    if state_error.is_some() {
        return "this plugin has no valid review record";
    }
    match plugin.approved_by.as_ref() {
        None => "this plugin has no valid agent review record",
        Some(PluginApprover::Person { .. }) => "this plugin was approved by a person",
        Some(PluginApprover::CommandLine) => "this plugin was approved from the command line",
        Some(PluginApprover::Agent(agent)) if agent.posture == AgentPosture::CommandLine => {
            "this plugin was approved from the command line"
        }
        Some(PluginApprover::Agent(_)) if !plugin.trusted() => {
            "this plugin has no current valid agent review record"
        }
        Some(PluginApprover::Agent(_)) if plugin.outside_reach => {
            "this plugin was approved by the agent of another project"
        }
        Some(PluginApprover::Agent(_)) if !installed_by_agent => {
            "this plugin was not installed by the agent from a folder in this project"
        }
        Some(PluginApprover::Agent(_)) => "this plugin is not owned by this agent",
    }
}

fn is_existing_own(
    registry: &PluginRegistry,
    plugin: &LoadedPlugin,
) -> (bool, Option<&'static str>) {
    if registry.state_error().is_some() {
        return (false, Some("this plugin has no valid review record"));
    }
    let valid_agent = plugin.trusted()
        && !plugin.outside_reach
        && matches!(
            plugin.approved_by.as_ref(),
            Some(PluginApprover::Agent(agent)) if agent.posture != AgentPosture::CommandLine
        );
    let installed_own = installed_by_the_agent(&plugin.canonical_root, registry.workspace());
    if valid_agent && installed_own {
        (true, None)
    } else {
        (
            false,
            Some(agent_denial_reason(plugin, None, installed_own)),
        )
    }
}

fn collect_enable_warnings(
    registry: &PluginRegistry,
    plugin: &LoadedPlugin,
    approver: &PluginApprover,
    enabled_now: bool,
    registry_ignored: bool,
    scrub: &ScrubContext<'_>,
) -> Vec<String> {
    let mut warnings = Vec::new();
    if enabled_now {
        let reach = match approver {
            PluginApprover::Agent(_) => "enabled for this project only",
            PluginApprover::Person { .. } | PluginApprover::CommandLine => {
                "enabled for every project"
            }
        };
        warnings.push(reach.to_string());
    }
    warnings.push(approver_label(Some(approver)).to_string());
    if let Some(review) = registry.host_service_review(plugin.id.as_str()) {
        for line in review.lines {
            if !line.offered || !plugin.host_services.contains(&line.service) {
                warnings.push(scrub_warning(
                    &format!(
                        "service `{}`: this version does not offer it",
                        line.service.name()
                    ),
                    scrub,
                ));
            }
        }
    }
    if registry_ignored {
        warnings.push(
            "registry was ignored; Codewhale installs from the configured source".to_string(),
        );
    }
    warnings
}

async fn observe_native_activation(
    call: &BrokerCall,
    plugin: &LoadedPlugin,
    expected_authority: Option<&PluginAuthority>,
    scrub: &ScrubContext<'_>,
) -> Result<(), String> {
    if plugin.inventory.native == 0 || !plugin.enabled {
        return Ok(());
    }
    if !extension_host_policy_enabled() {
        return Err(HOST_OFF_DIAGNOSTIC.to_string());
    }
    let plugin_id = plugin.id.as_str();
    let check_report = |report: super::OwnerReport| -> Option<Result<(), String>> {
        match report.state.as_ref() {
            Some(OwnerState::Active) => Some(Ok(())),
            Some(OwnerState::Failed(reason)) | Some(OwnerState::Faulted(reason)) => {
                let mut combined = vec![scrub_message(reason, scrub)];
                for line in report.diagnostics {
                    let cleaned = scrub_message(&line, scrub);
                    if !cleaned.is_empty() && !combined.contains(&cleaned) {
                        combined.push(cleaned);
                    }
                }
                let mut truncated = false;
                Some(Err(cap_text(
                    combined.join("; "),
                    MAX_DIAGNOSTIC_BYTES,
                    &mut truncated,
                )))
            }
            Some(OwnerState::Revoked) => Some(Err(
                "the extension host revoked the plugin owner".to_string()
            )),
            Some(OwnerState::Activating) | None => None,
        }
    };
    if let Some(report) = super::owner_report_for_authority(plugin_id, expected_authority)
        && let Some(outcome) = check_report(report)
    {
        return outcome;
    }
    if super::manager().attached_engines() == 0 {
        return Err("the extension host is not running for this session".to_string());
    }
    let rows = (plugin.inventory.native as u32).max(1);
    let timeout = HOST_LOAD_TIMEOUT_PER_ROW
        .saturating_mul(rows)
        .min(MAX_HOST_WAIT);
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(report) = super::owner_report_for_authority(plugin_id, expected_authority)
            && let Some(outcome) = check_report(report)
        {
            return outcome;
        }
        if Instant::now() >= deadline {
            return Err(
                "the extension host did not finish starting native plugin code in time".to_string(),
            );
        }
        tokio::select! {
            () = tokio::time::sleep(HOST_POLL_INTERVAL) => {}
            () = call.context.cancel().cancelled() => {
                return Err("cancelled while waiting for the extension host to start plugin code".to_string());
            }
        }
    }
}

type LocalDshRefineResult = (PluginInstallSource, bool, Option<(&'static str, String)>);

async fn refine_local_dsh_source(
    source: PluginInstallSource,
    dir: PathBuf,
) -> Result<LocalDshRefineResult, BrokerRefusal> {
    tokio::task::spawn_blocking(move || match source {
        PluginInstallSource::LocalPath(path) => match try_is_dsh_package(&path) {
            Ok(true) => {
                let npm_deps = has_unresolved_npm_dependencies(&dir).unwrap_or(false);
                (PluginInstallSource::Dsh(path), npm_deps, None)
            }
            Ok(false) => (PluginInstallSource::LocalPath(path), false, None),
            Err(error) => (
                PluginInstallSource::LocalPath(path),
                false,
                Some(("not-bundle", format!("not-bundle: {error:#}"))),
            ),
        },
        other => {
            let npm_deps = matches!(other, PluginInstallSource::Dsh(_))
                && has_unresolved_npm_dependencies(&dir).unwrap_or(false);
            (other, npm_deps, None)
        }
    })
    .await
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))
}

enum McpActivationError {
    Cancelled,
    Failed(String),
}

struct EnableActivation<'a> {
    method: &'static str,
    stage: &'static str,
    target: String,
    changed: bool,
    registry: &'a Arc<PluginRegistry>,
    loaded: &'a LoadedPlugin,
    approver: &'a PluginApprover,
    version: Option<String>,
    warnings: Vec<String>,
}

fn record_mcp_attempt(
    call: &BrokerCall,
    plugin_name: &str,
    server_name: &str,
    qualified: &str,
    outcome: &str,
) {
    let _ = call.context.record(json!({
        "method": "mcp_connect",
        "plugin": plugin_name,
        "server": server_name,
        "qualified": qualified,
        "outcome": outcome,
    }));
}

fn record_remaining_timeouts(
    call: &BrokerCall,
    plugin_name: &str,
    servers: &[(&str, &McpServerConfig)],
    first_detail: &str,
    failures: &mut Vec<String>,
) {
    for (index, (server_name, _)) in servers.iter().enumerate() {
        let qualified = qualified_plugin_server_name(plugin_name, server_name);
        record_mcp_attempt(call, plugin_name, server_name, &qualified, "timed_out");
        let detail = if index == 0 {
            first_detail
        } else {
            "not attempted before call deadline elapsed"
        };
        failures.push(format!("`{server_name}`: {detail}"));
    }
}

fn format_connected_tools_warning(
    server_name: &str,
    tool_names: &[String],
    scrub: &ScrubContext<'_>,
) -> String {
    let mut clean_server_trunc = false;
    let clean_server = sanitize_inline(server_name, MAX_NAME_BYTES, &mut clean_server_trunc);
    if tool_names.is_empty() {
        return scrub_warning(
            &format!("connector `{clean_server}` connected with no tools"),
            scrub,
        );
    }
    let mut listed = Vec::new();
    let mut listed_bytes = 0usize;
    for raw_name in tool_names {
        if listed.len() >= MAX_TOOL_WARNINGS_PER_CONNECTOR {
            break;
        }
        let mut name_truncated = false;
        let clean_name = sanitize_inline(raw_name, MAX_ENTRY_ID_BYTES, &mut name_truncated);
        if clean_name.is_empty() {
            continue;
        }
        let next_bytes = listed_bytes + clean_name.len() + usize::from(!listed.is_empty()) * 2;
        if next_bytes > MAX_TEXT_BYTES.saturating_sub(96) && !listed.is_empty() {
            break;
        }
        listed_bytes = next_bytes;
        listed.push(clean_name);
    }
    let remaining = tool_names.len().saturating_sub(listed.len());
    let summary = if listed.is_empty() {
        format!(
            "connector `{clean_server}` connected ({} tools)",
            tool_names.len()
        )
    } else if remaining > 0 {
        format!(
            "connector `{clean_server}` tools: {} (+{remaining} more)",
            listed.join(", ")
        )
    } else {
        format!("connector `{clean_server}` tools: {}", listed.join(", "))
    };
    scrub_warning(&summary, scrub)
}

async fn connect_required_plugin_mcp_servers(
    call: &BrokerCall,
    forked: &mut McpPool,
    plugin_name: &str,
    required_servers: &[(&str, &McpServerConfig)],
    deadline: tokio::time::Instant,
    scrub: &ScrubContext<'_>,
) -> Result<Vec<String>, McpActivationError> {
    let mut connected_warnings = Vec::new();
    let mut failures = Vec::new();
    for (index, (server_name, server_config)) in required_servers.iter().enumerate() {
        let qualified = qualified_plugin_server_name(plugin_name, server_name);
        if call.context.cancel().is_cancelled() {
            record_mcp_attempt(call, plugin_name, server_name, &qualified, "cancelled");
            return Err(McpActivationError::Cancelled);
        }
        let is_reviewed_plugin_server =
            forked.static_server_config(&qualified).is_some_and(|cfg| {
                cfg.reviewed_plugin_source()
                    .is_some_and(|src| src.plugin_name() == plugin_name)
            });
        if !is_reviewed_plugin_server {
            record_mcp_attempt(call, plugin_name, server_name, &qualified, "failed");
            failures.push(format!(
                "`{server_name}`: missing reviewed plugin MCP entry `{qualified}`"
            ));
            continue;
        }
        if tokio::time::Instant::now() >= deadline {
            record_remaining_timeouts(
                call,
                plugin_name,
                &required_servers[index..],
                "call deadline elapsed before connecting",
                &mut failures,
            );
            break;
        }
        let attempt_cancel = tokio_util::sync::CancellationToken::new();
        let timed_out = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher_cancel = attempt_cancel.clone();
        let turn_cancel = call.context.cancel().clone();
        let watcher_timed_out = Arc::clone(&timed_out);
        let watcher = tokio::spawn(async move {
            tokio::select! {
                biased;
                () = turn_cancel.cancelled() => {
                    watcher_cancel.cancel();
                }
                () = tokio::time::sleep_until(deadline) => {
                    watcher_timed_out.store(true, std::sync::atomic::Ordering::SeqCst);
                    watcher_cancel.cancel();
                }
            }
        });
        let connect_res = forked
            .get_or_connect_with_cancel(&qualified, &attempt_cancel)
            .await;
        watcher.abort();
        let _ = watcher.await;
        if call.context.cancel().is_cancelled() {
            record_mcp_attempt(call, plugin_name, server_name, &qualified, "cancelled");
            return Err(McpActivationError::Cancelled);
        }
        if timed_out.load(std::sync::atomic::Ordering::SeqCst) && connect_res.is_err() {
            record_remaining_timeouts(
                call,
                plugin_name,
                &required_servers[index..],
                "timed out waiting for required MCP connector response",
                &mut failures,
            );
            break;
        }
        match connect_res {
            Ok(conn) => {
                let raw_tools: Vec<String> = conn
                    .tools()
                    .iter()
                    .filter(|tool| server_config.is_tool_enabled(&tool.name))
                    .map(|tool| McpPool::mcp_model_tool_name(&qualified, &tool.name))
                    .collect();
                let allowed_tools: Vec<String> = raw_tools
                    .into_iter()
                    .filter(|tool_name| forked.tool_allowed(tool_name))
                    .collect();
                record_mcp_attempt(call, plugin_name, server_name, &qualified, "connected");
                connected_warnings.push(format_connected_tools_warning(
                    server_name,
                    &allowed_tools,
                    scrub,
                ));
            }
            Err(error) => {
                record_mcp_attempt(call, plugin_name, server_name, &qualified, "failed");
                let reason = scrub_message(&format_mcp_error_for_display(&error), scrub);
                failures.push(format!("`{server_name}`: {reason}"));
            }
        }
    }
    if !failures.is_empty() {
        let joined = if failures.len() == 1 {
            format!("required MCP connector {}", failures[0])
        } else {
            format!("required MCP connectors failed: {}", failures.join("; "))
        };
        let mut truncated = false;
        return Err(McpActivationError::Failed(cap_text(
            joined,
            MAX_DIAGNOSTIC_BYTES,
            &mut truncated,
        )));
    }
    Ok(connected_warnings)
}

fn find_settled_attachment_registry(
    attachments: &std::sync::Mutex<BTreeMap<u64, super::AttachmentState>>,
    workspace: &Path,
    plugin_id: &str,
    content_hash: &str,
) -> (Option<Arc<PluginRegistry>>, bool) {
    let guard = attachments.lock().expect("attachments lock");
    let mut matching_workspace_pending = false;
    for state in guard.values() {
        if state.plugins.workspace() != workspace {
            continue;
        }
        let Some(loaded) = state.plugins.get(plugin_id) else {
            continue;
        };
        if !loaded.active() || loaded.content_hash != content_hash {
            continue;
        }
        if let Some(sel) = state.plugins.caller_selection()
            && state.selection.revision == Some(sel)
            && state.selection.desired.get(plugin_id) == Some(&loaded.content_hash)
        {
            return (Some(Arc::clone(&state.plugins)), false);
        }
        matching_workspace_pending = true;
    }
    (None, matching_workspace_pending)
}

async fn resolve_plugin_enabled_servers(
    call: &BrokerCall,
    registry: &Arc<PluginRegistry>,
    plugin: &LoadedPlugin,
    remaining: Duration,
    deadline: tokio::time::Instant,
    scrub: &ScrubContext<'_>,
) -> Result<(Arc<PluginRegistry>, Vec<(String, McpServerConfig)>), McpActivationError> {
    let mut servers: BTreeMap<String, McpServerConfig> = plugin
        .manifest
        .mcp_servers
        .iter()
        .flatten()
        .filter(|(_, config)| config.is_enabled())
        .map(|(name, config)| (name.clone(), config.clone()))
        .collect();
    if plugin.inventory.native == 0 {
        return Ok((Arc::clone(registry), servers.into_iter().collect()));
    }

    let shared = &super::manager().shared;
    let live_native: Vec<(String, McpServerConfig)> = {
        let owners = shared.registry.lock().expect("registry lock");
        owners
            .live_mcp()
            .into_iter()
            .filter(|reg| {
                reg.owner.plugin_id == plugin.id.as_str()
                    && reg.content_hash == plugin.content_hash
                    && reg.config.is_enabled()
            })
            .map(|reg| (reg.name, reg.config))
            .collect()
    };
    if live_native.is_empty() {
        return Ok((Arc::clone(registry), servers.into_iter().collect()));
    }

    let mut live_seen = HashSet::new();
    let live_required: Vec<(&str, &McpServerConfig)> = live_native
        .iter()
        .filter(|(_, cfg)| cfg.required)
        .map(|(name, cfg)| (name.as_str(), cfg))
        .collect();
    for (name, _) in &live_native {
        if !live_seen.insert(name.as_str()) || servers.contains_key(name) {
            for (req_name, _) in &live_required {
                let qualified = qualified_plugin_server_name(plugin.name(), req_name);
                record_mcp_attempt(call, plugin.name(), req_name, &qualified, "failed");
            }
            return Err(McpActivationError::Failed(scrub_message(
                &format!("duplicate plugin MCP server definition `{name}`"),
                scrub,
            )));
        }
    }

    let mut timed_out_waiting_settlement = false;
    let registry_for_fork = loop {
        if call.context.cancel().is_cancelled() {
            return Err(McpActivationError::Cancelled);
        }
        let (settled, pending) = find_settled_attachment_registry(
            &shared.attachments,
            registry.workspace(),
            plugin.id.as_str(),
            &plugin.content_hash,
        );
        if let Some(settled_registry) = settled {
            break settled_registry;
        }
        if !pending {
            break Arc::clone(registry);
        }
        if remaining.is_zero() || tokio::time::Instant::now() >= deadline {
            timed_out_waiting_settlement = true;
            break Arc::clone(registry);
        }
        tokio::select! {
            biased;
            () = call.context.cancel().cancelled() => return Err(McpActivationError::Cancelled),
            () = tokio::time::sleep_until(deadline) => {
                timed_out_waiting_settlement = true;
                break Arc::clone(registry);
            }
            () = tokio::time::sleep(HOST_POLL_INTERVAL) => {}
        }
    };

    if timed_out_waiting_settlement && !live_required.is_empty() {
        let mut failures = Vec::new();
        record_remaining_timeouts(
            call,
            plugin.name(),
            &live_required,
            "timed out waiting for extension attachment selection",
            &mut failures,
        );
        return Err(McpActivationError::Failed(scrub_message(
            &failures.join("; "),
            scrub,
        )));
    }

    let admitted = match super::native_mcp::for_plugins(&registry_for_fork) {
        Ok(entries) => entries,
        Err(error) => {
            for (req_name, _) in &live_required {
                let qualified = qualified_plugin_server_name(plugin.name(), req_name);
                record_mcp_attempt(call, plugin.name(), req_name, &qualified, "failed");
            }
            return Err(McpActivationError::Failed(scrub_message(&error, scrub)));
        }
    };

    let mut admitted_names = HashSet::new();
    for (name, cfg, authority, _) in admitted {
        if authority.plugin_id == plugin.id && cfg.is_enabled() {
            if servers.contains_key(&name) {
                for (req_name, _) in &live_required {
                    let qualified = qualified_plugin_server_name(plugin.name(), req_name);
                    record_mcp_attempt(call, plugin.name(), req_name, &qualified, "failed");
                }
                return Err(McpActivationError::Failed(scrub_message(
                    &format!("native MCP connector `{name}` collides with manifest entry"),
                    scrub,
                )));
            }
            admitted_names.insert(name.clone());
            servers.insert(name, cfg);
        }
    }

    let mut missing_required = Vec::new();
    for (name, _) in &live_required {
        if !admitted_names.contains(*name) {
            let qualified = qualified_plugin_server_name(plugin.name(), name);
            record_mcp_attempt(call, plugin.name(), name, &qualified, "failed");
            missing_required.push(format!("`{name}`"));
        }
    }
    if !missing_required.is_empty() {
        return Err(McpActivationError::Failed(scrub_message(
            &format!(
                "required native MCP connector(s) were not admitted by the extension host: {}",
                missing_required.join(", ")
            ),
            scrub,
        )));
    }

    Ok((registry_for_fork, servers.into_iter().collect()))
}

async fn activate_plugin_mcp_connectors(
    call: &BrokerCall,
    registry: &Arc<PluginRegistry>,
    plugin: &LoadedPlugin,
    scrub: &ScrubContext<'_>,
    warnings: &mut Vec<String>,
) -> Result<(), McpActivationError> {
    let remaining = mcp_connect_deadline();
    let deadline = tokio::time::Instant::now() + remaining;
    let (registry_for_fork, enabled_servers) =
        resolve_plugin_enabled_servers(call, registry, plugin, remaining, deadline, scrub).await?;
    if enabled_servers.is_empty() {
        return Ok(());
    }

    let required_servers: Vec<(&str, &McpServerConfig)> = enabled_servers
        .iter()
        .filter(|(_, config)| config.required)
        .map(|(name, config)| (name.as_str(), config))
        .collect();
    let optional_servers: Vec<(&str, &McpServerConfig)> = enabled_servers
        .iter()
        .filter(|(_, config)| !config.required)
        .map(|(name, config)| (name.as_str(), config))
        .collect();

    let mut connected_warnings = Vec::new();
    if !required_servers.is_empty() {
        if call.context.cancel().is_cancelled() {
            return Err(McpActivationError::Cancelled);
        }
        let Some(session_pool) = call.context.mcp_pool() else {
            let mut failures = Vec::new();
            for (server_name, _) in &required_servers {
                let qualified = qualified_plugin_server_name(plugin.name(), server_name);
                record_mcp_attempt(call, plugin.name(), server_name, &qualified, "failed");
                failures.push(format!("`{server_name}`"));
            }
            return Err(McpActivationError::Failed(scrub_message(
                &format!(
                    "the session MCP pool is unavailable to verify required MCP connector(s): {}",
                    failures.join(", ")
                ),
                scrub,
            )));
        };

        if remaining.is_zero() || tokio::time::Instant::now() >= deadline {
            let mut failures = Vec::new();
            record_remaining_timeouts(
                call,
                plugin.name(),
                &required_servers,
                "call deadline elapsed before connecting",
                &mut failures,
            );
            return Err(McpActivationError::Failed(scrub_message(
                &failures.join("; "),
                scrub,
            )));
        }

        let live_network_policy = call.context.tool().network_policy.clone();
        let mut forked = tokio::select! {
            biased;
            () = call.context.cancel().cancelled() => return Err(McpActivationError::Cancelled),
            () = tokio::time::sleep_until(deadline) => {
                let mut failures = Vec::new();
                record_remaining_timeouts(
                    call,
                    plugin.name(),
                    &required_servers,
                    "timed out waiting for MCP pool lock before deadline",
                    &mut failures,
                );
                return Err(McpActivationError::Failed(scrub_message(
                    &failures.join("; "),
                    scrub,
                )));
            }
            fork_res = async {
                let guard = session_pool.lock().await;
                guard.fork_isolated_for_plugins(
                    Arc::clone(&registry_for_fork),
                    live_network_policy,
                )
            } => match fork_res {
                Ok(pool) => pool,
                Err(error) => {
                    let diag = scrub_message(&format_mcp_error_for_display(&error), scrub);
                    for (server_name, _) in &required_servers {
                        let qualified = qualified_plugin_server_name(plugin.name(), server_name);
                        record_mcp_attempt(call, plugin.name(), server_name, &qualified, "failed");
                    }
                    return Err(McpActivationError::Failed(diag));
                }
            }
        };

        let connect_outcome = connect_required_plugin_mcp_servers(
            call,
            &mut forked,
            plugin.name(),
            &required_servers,
            deadline,
            scrub,
        )
        .await;
        forked.shutdown_all().await;
        connected_warnings = connect_outcome?;
    }

    warnings.extend(connected_warnings);
    for (server_name, _) in optional_servers {
        let mut clean_server_trunc = false;
        let clean_server = sanitize_inline(server_name, MAX_NAME_BYTES, &mut clean_server_trunc);
        let qualified = qualified_plugin_server_name(plugin.name(), server_name);
        let prefix = McpPool::mcp_model_tool_name(&qualified, "");
        warnings.push(scrub_warning(
            &format!(
                "connector `{clean_server}` was not started upfront; expected tools start with `{prefix}`"
            ),
            scrub,
        ));
    }
    warnings.push("connector tools become callable from the next message on".to_string());
    Ok(())
}

async fn finish_enabled_activation(
    call: &BrokerCall,
    mut activation: EnableActivation<'_>,
    scrub: &ScrubContext<'_>,
) -> std::ops::ControlFlow<Result<Value, BrokerRefusal>, Vec<String>> {
    let expected_authority = activation
        .registry
        .authority_for(activation.loaded.id.as_str());
    if let Err(host_err) =
        observe_native_activation(call, activation.loaded, expected_authority.as_ref(), scrub).await
    {
        record_outcome(
            call,
            activation.method,
            Some(activation.loaded.name()),
            Some(&activation.loaded.content_hash),
            Some(activation.approver),
            "failed",
        );
        return std::ops::ControlFlow::Break(change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: activation.stage,
            target: activation.target,
            changed: activation.changed,
            enabled: Some(activation.loaded.enabled),
            code: "operation-error",
            diagnostic: host_err,
            bundle: Some(activation.loaded.name().to_string()),
            version: non_empty_version(&activation.loaded.manifest.plugin.version)
                .or(activation.version),
            warnings: activation.warnings,
        }));
    }
    let (outcome_tag, diagnostic) = match activate_plugin_mcp_connectors(
        call,
        activation.registry,
        activation.loaded,
        scrub,
        &mut activation.warnings,
    )
    .await
    {
        Ok(()) => return std::ops::ControlFlow::Continue(activation.warnings),
        Err(McpActivationError::Cancelled) => (
            "cancelled",
            "operation cancelled before required MCP connector verification finished".to_string(),
        ),
        Err(McpActivationError::Failed(diag)) => ("failed", diag),
    };
    record_outcome(
        call,
        activation.method,
        Some(activation.loaded.name()),
        Some(&activation.loaded.content_hash),
        Some(activation.approver),
        outcome_tag,
    );
    std::ops::ControlFlow::Break(change_with_outcome(ChangeOutcome {
        application: "failed",
        stage: activation.stage,
        target: activation.target,
        changed: activation.changed,
        enabled: Some(activation.loaded.enabled),
        code: "operation-error",
        diagnostic,
        bundle: Some(activation.loaded.name().to_string()),
        version: non_empty_version(&activation.loaded.manifest.plugin.version)
            .or(activation.version),
        warnings: activation.warnings,
    }))
}

async fn prepare_local_source_and_hash_with_network(
    source: &PluginInstallSource,
    dir: &Path,
    network: Option<&NetworkPolicy>,
) -> Result<(String, Option<String>, Option<String>, String), (&'static str, String)> {
    match has_symlink_or_reparse_ancestor_or_target(dir).await {
        Ok(true) => {
            return Err((
                "operation-error",
                "symbolic links and reparse points are not allowed in plugin bundles".to_string(),
            ));
        }
        Ok(false) => {}
        Err(error) => {
            return Err((
                classify_error_message(&error.to_string()),
                format!("failed to inspect install source: {error}"),
            ));
        }
    }
    ensure_no_links_in_tree(dir)
        .await
        .map_err(|msg| (classify_error_message(&msg), msg))?;
    let source = source.clone();
    let dir = dir.to_path_buf();
    let network = network.cloned();
    let policy = extension_host_policy_enabled();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let _scope = PolicyScope::propagate(policy);
        match source {
            PluginInstallSource::LocalPath(_) => {
                let Some(manifest_path) = resolve_manifest_path(&dir) else {
                    return Err((
                        "not-bundle",
                        "not-bundle: source directory has no plugin.json, kimi.plugin.json, plugin.toml, or dsh.bundle"
                            .to_string(),
                    ));
                };
                let validated = PluginManifest::validate_from_path(&manifest_path)
                    .map_err(|error| ("not-bundle", format!("not-bundle: {error}")))?;
                Ok((
                    validated.manifest.plugin.name.clone(),
                    None,
                    non_empty_version(&validated.manifest.plugin.version),
                    validated.content_hash,
                ))
            }
            PluginInstallSource::Dsh(_) => {
                match try_is_dsh_package(&dir) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err((
                            "not-bundle",
                            "not-bundle: source directory does not declare dsh.bundle".to_string(),
                        ));
                    }
                    Err(error) => {
                        return Err(("not-bundle", format!("not-bundle: {error:#}")));
                    }
                }
                let result = match network.as_ref() {
                    Some(policy) => preview_dsh_with_network(&dir, policy),
                    None => preview_dsh(&dir),
                };
                let (conversion, hash) = result.map_err(|error| {
                    let code = if error.downcast_ref::<NpmDepsNetworkError>().is_some() {
                        "operation-error"
                    } else {
                        let msg = format!("{error:#}");
                        classify_error_message(&msg)
                    };
                    (code, format!("{error:#}"))
                })?;
                Ok((
                    conversion.plugin_name,
                    conversion.source_package,
                    conversion.source_version.and_then(|v| non_empty_version(&v)),
                    hash,
                ))
            }
            PluginInstallSource::Remote(_) => Err(("invalid-spec", "unexpected remote source".to_string())),
        }
    })
    .await
    .map_err(|_| ("operation-error", "source preview task failed".to_string()))?
}

async fn install(
    call: &BrokerCall,
    mut tool: ToolContext,
    authority: Option<LiveRuntimeAuthority>,
    input: InstallInput,
) -> Result<Value, BrokerRefusal> {
    let raw_target = validate_required_str(&input.target, "target", MAX_TARGET_BYTES)?;
    let registry_opt =
        validate_optional_str(input.registry.as_deref(), "registry", MAX_REGISTRY_BYTES)?;
    let has_approved_builds = validate_approved_builds(input.approved_builds.as_deref())?;
    let initial_registry = live_registry(call, &tool)?;
    let Some(mut current_authority) = authority else {
        return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
    };

    let mut base_warnings = Vec::new();
    let registry_ignored = registry_opt.is_some();
    if registry_ignored {
        base_warnings.push(
            "registry was ignored; Codewhale installs from the configured source".to_string(),
        );
    }
    if has_approved_builds {
        record_outcome(call, "install", None, None, None, "failed");
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: "install",
            target: raw_target,
            changed: false,
            enabled: None,
            code: "stale-approval",
            diagnostic:
                "pending build approvals are not used here; call install again without approved_builds"
                    .to_string(),
            bundle: None,
            version: None,
            warnings: base_warnings,
        });
    }

    let parsed = match parse_install_target_syntax(&raw_target) {
        Ok(parsed) => parsed,
        Err((code, msg)) => {
            let scrub = ScrubContext {
                workspace: initial_registry.workspace(),
                canon_workspace: None,
                plugin_root: None,
                user_plugins_dir: initial_registry.user_plugins_dir(),
                state_path: initial_registry.state_path(),
            };
            record_outcome(call, "install", None, None, None, "failed");
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "install",
                target: raw_target,
                changed: false,
                enabled: None,
                code,
                diagnostic: scrub_message(&msg, &scrub),
                bundle: None,
                version: None,
                warnings: base_warnings,
            });
        }
    };

    let pending = approver_from_posture(&current_authority);
    let policy = extension_host_policy_enabled();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = crate::test_support::env_scope_ticket();
    let prepared = match &parsed {
        PluginInstallSource::LocalPath(dir) | PluginInstallSource::Dsh(dir) => {
            if matches!(pending, PendingApprover::Nobody) {
                PreparedSource::Local {
                    source: parsed.clone(),
                    source_dir: dir.clone(),
                    pre_gate: PreGateLocal::Deferred {
                        deferred_error: None,
                    },
                    canon_workspace: None,
                }
            } else {
                if matches!(pending, PendingApprover::Agent(_))
                    && !has_agent_call_identity(call, &tool)
                {
                    record_outcome(call, "install", None, None, None, "failed");
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic:
                            "An approval by the agent must name the thread, turn and tool call that made it"
                                .to_string(),
                        bundle: None,
                        version: None,
                        warnings: base_warnings,
                    });
                }
                let registry_snapshot = Arc::clone(&initial_registry);
                let dir_path = dir.clone();
                let dir_probe = dir.clone();
                let dir_info = tokio::task::spawn_blocking(move || {
                    #[cfg(any(test, feature = "test-support"))]
                    let _env_scope = crate::test_support::join_env_scope(env_scope);
                    let _scope = PolicyScope::propagate(policy);
                    let fresh =
                        registry_snapshot.rediscover_for_workspace(registry_snapshot.workspace());
                    let canon_ws = std::fs::canonicalize(fresh.workspace()).ok();
                    let in_project = clean_dir_is_strictly_inside(&dir_probe, fresh.workspace());
                    let user_plugins_dir = fresh.user_plugins_dir().map(Path::to_path_buf);
                    (fresh, canon_ws, in_project, user_plugins_dir)
                })
                .await
                .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;
                let (fresh, canon_ws, in_project, user_plugins_dir) = dir_info;
                let ancestor_clean = !has_symlink_or_reparse_ancestor_or_target(&dir_path)
                    .await
                    .unwrap_or(true);
                let in_user_plugins =
                    dir_is_in_user_plugins(&dir_path, user_plugins_dir.as_deref()).await;
                let (is_own, reason) = if !in_project || !ancestor_clean {
                    (
                        false,
                        Some(
                            "source folder is outside this project, under .git, or reached through a link or reparse point",
                        ),
                    )
                } else if in_user_plugins {
                    (
                        false,
                        Some("source folder is inside the user plugin folder"),
                    )
                } else {
                    (true, None)
                };
                if !is_own && matches!(pending, PendingApprover::Agent(_)) {
                    let reason = reason.unwrap_or("plugin source is not owned by the agent");
                    record_outcome(call, "install", None, None, None, "failed");
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: format!("{reason}; {ASK_PERSON_HINT}"),
                        bundle: None,
                        version: None,
                        warnings: base_warnings,
                    });
                }
                let has_clean_manifest = if is_own {
                    true
                } else if ancestor_clean {
                    dir_has_clean_root_manifest(&dir_path).await
                } else {
                    false
                };
                let (parsed_refined, has_npm_deps, refine_error, tree_clean) =
                    if is_own || (ancestor_clean && has_clean_manifest) {
                        if ensure_no_links_in_tree(&dir_path).await.is_ok() {
                            let (refined, npm_deps, refine_err) =
                                refine_local_dsh_source(parsed.clone(), dir_path.clone()).await?;
                            (refined, npm_deps, refine_err, true)
                        } else {
                            (parsed.clone(), false, None, false)
                        }
                    } else {
                        (parsed.clone(), false, None, false)
                    };
                if is_own && matches!(pending, PendingApprover::Agent(_)) && has_npm_deps {
                    record_outcome(call, "install", None, None, None, "failed");
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: format!(
                            "DSH plugin requires downloading external npm dependencies from registry.npmjs.org; {ASK_PERSON_HINT}"
                        ),
                        bundle: None,
                        version: None,
                        warnings: base_warnings,
                    });
                }
                let pre_gate = if !is_own && (!ancestor_clean || !has_clean_manifest || !tree_clean)
                {
                    PreGateLocal::Deferred {
                        deferred_error: None,
                    }
                } else {
                    let mut reach_gated = false;
                    let mut preview_network = None;
                    if matches!(pending, PendingApprover::Person) && has_npm_deps {
                        let reach_target =
                            format!("{raw_target} npm dependencies from registry.npmjs.org");
                        call.context
                            .act("install", BrokerEffect::Reach, Some(&reach_target))
                            .await?;
                        if call.context.cancel().is_cancelled() {
                            record_outcome(call, "install", None, None, None, "cancelled");
                            return cancelled_result(
                                "install",
                                raw_target,
                                None,
                                None,
                                base_warnings,
                            );
                        }
                        let (refreshed_tool, refreshed_auth) = live(call);
                        tool = refreshed_tool;
                        let Some(refreshed_auth) = refreshed_auth else {
                            return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
                        };
                        current_authority = refreshed_auth;
                        match approver_from_posture(&current_authority) {
                            PendingApprover::Nobody => {
                                record_outcome(call, "install", None, None, None, "failed");
                                return change_with_outcome(ChangeOutcome {
                                    application: "failed",
                                    stage: "install",
                                    target: raw_target,
                                    changed: false,
                                    enabled: None,
                                    code: "operation-error",
                                    diagnostic:
                                        "permission mode changed during review and no longer allows plugin installation"
                                            .to_string(),
                                    bundle: None,
                                    version: None,
                                    warnings: base_warnings,
                                });
                            }
                            PendingApprover::Agent(_) => {
                                record_outcome(call, "install", None, None, None, "failed");
                                return change_with_outcome(ChangeOutcome {
                                    application: "failed",
                                    stage: "install",
                                    target: raw_target,
                                    changed: false,
                                    enabled: None,
                                    code: "operation-error",
                                    diagnostic: format!(
                                        "DSH plugin requires downloading external npm dependencies from registry.npmjs.org; {ASK_PERSON_HINT}"
                                    ),
                                    bundle: None,
                                    version: None,
                                    warnings: base_warnings,
                                });
                            }
                            PendingApprover::Person => {}
                        }
                        reach_gated = true;
                        preview_network = Some(network_policy_for(&tool));
                    }
                    let scrub = ScrubContext {
                        workspace: fresh.workspace(),
                        canon_workspace: canon_ws.as_deref(),
                        plugin_root: Some(&dir_path),
                        user_plugins_dir: fresh.user_plugins_dir(),
                        state_path: fresh.state_path(),
                    };
                    let preview = if let Some(err) = refine_error {
                        Err(err)
                    } else {
                        prepare_local_source_and_hash_with_network(
                            &parsed_refined,
                            &dir_path,
                            preview_network.as_ref(),
                        )
                        .await
                    };
                    match preview {
                        Ok((plugin_name, source_package, version, pre_hash)) => {
                            if is_own {
                                let fresh_ref = Arc::clone(&fresh);
                                let pname = plugin_name.clone();
                                let spkg = source_package.clone();
                                let collision = tokio::task::spawn_blocking(move || {
                                    #[cfg(any(test, feature = "test-support"))]
                                    let _env_scope = crate::test_support::join_env_scope(env_scope);
                                    let _scope = PolicyScope::propagate(policy);
                                    if let Some(existing) = fresh_ref.get(&pname) {
                                        return Some((
                                            existing.name().to_string(),
                                            existing.scope.as_str(),
                                        ));
                                    }
                                    if let Some(pkg) = spkg.as_deref() {
                                        match resolve_plugin_by_target(&fresh_ref, pkg) {
                                            TargetLookup::Found(existing) => {
                                                return Some((
                                                    existing.name().to_string(),
                                                    existing.scope.as_str(),
                                                ));
                                            }
                                            TargetLookup::Ambiguous { first, .. } => {
                                                return Some((
                                                    first.name().to_string(),
                                                    first.scope.as_str(),
                                                ));
                                            }
                                            TargetLookup::Missing => {}
                                        }
                                    }
                                    None
                                })
                                .await
                                .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;
                                if let Some((existing_name, scope)) = collision {
                                    record_outcome(
                                        call,
                                        "install",
                                        Some(&existing_name),
                                        Some(&pre_hash),
                                        None,
                                        "failed",
                                    );
                                    return change_with_outcome(ChangeOutcome {
                                        application: "failed",
                                        stage: "install",
                                        target: raw_target,
                                        changed: false,
                                        enabled: None,
                                        code: "already-installed",
                                        diagnostic: format!(
                                            "plugin `{existing_name}` is already installed ({scope} scope); remove it first, then install"
                                        ),
                                        bundle: Some(existing_name),
                                        version,
                                        warnings: base_warnings,
                                    });
                                }
                            }
                            PreGateLocal::Ready {
                                plugin_name,
                                source_package,
                                version,
                                pre_hash,
                                reach_gated,
                            }
                        }
                        Err((code, msg)) => {
                            if !is_own && !reach_gated {
                                PreGateLocal::Deferred {
                                    deferred_error: Some((code, msg)),
                                }
                            } else {
                                record_outcome(call, "install", None, None, None, "failed");
                                return change_with_outcome(ChangeOutcome {
                                    application: "failed",
                                    stage: "install",
                                    target: raw_target,
                                    changed: false,
                                    enabled: None,
                                    code,
                                    diagnostic: scrub_message(&msg, &scrub),
                                    bundle: None,
                                    version: None,
                                    warnings: base_warnings,
                                });
                            }
                        }
                    }
                };
                PreparedSource::Local {
                    source: parsed_refined,
                    source_dir: dir_path,
                    pre_gate,
                    canon_workspace: canon_ws,
                }
            }
        }
        PluginInstallSource::Remote(_) => {
            let (label, host) = remote_target_display(&parsed, &raw_target);
            let ws = initial_registry.workspace().to_path_buf();
            let canon_ws = tokio::fs::canonicalize(&ws).await.ok();
            PreparedSource::Remote {
                source: parsed.clone(),
                label,
                host,
                canon_workspace: canon_ws,
            }
        }
    };

    match prepared {
        PreparedSource::Remote {
            source,
            label,
            host,
            canon_workspace,
        } => {
            let scrub = ScrubContext {
                workspace: initial_registry.workspace(),
                canon_workspace: canon_workspace.as_deref(),
                plugin_root: None,
                user_plugins_dir: initial_registry.user_plugins_dir(),
                state_path: initial_registry.state_path(),
            };
            if matches!(pending, PendingApprover::Agent(_)) {
                record_outcome(call, "install", Some(&label), None, None, "failed");
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "install",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "operation-error",
                    diagnostic: format!(
                        "a remote plugin source is not written by this project's agent; {ASK_PERSON_HINT}"
                    ),
                    bundle: None,
                    version: None,
                    warnings: base_warnings,
                });
            }

            let reach_target = format!("{label} from {host}");
            call.context
                .act("install", BrokerEffect::Reach, Some(&reach_target))
                .await?;

            if call.context.cancel().is_cancelled() {
                record_outcome(call, "install", Some(&label), None, None, "cancelled");
                return cancelled_result("install", raw_target, None, None, base_warnings);
            }
            let (refreshed_tool, refreshed_auth) = live(call);
            tool = refreshed_tool;
            let Some(refreshed_auth) = refreshed_auth else {
                return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
            };
            current_authority = refreshed_auth;
            match approver_from_posture(&current_authority) {
                PendingApprover::Nobody => {
                    record_outcome(call, "install", Some(&label), None, None, "failed");
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic:
                            "permission mode changed during review and no longer allows plugin installation"
                                .to_string(),
                        bundle: None,
                        version: None,
                        warnings: base_warnings,
                    });
                }
                PendingApprover::Agent(_) => {
                    record_outcome(call, "install", Some(&label), None, None, "failed");
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: format!(
                            "a remote plugin source is not written by this project's agent; {ASK_PERSON_HINT}"
                        ),
                        bundle: None,
                        version: None,
                        warnings: base_warnings,
                    });
                }
                PendingApprover::Person => {}
            }

            let network = network_policy_for(&tool);
            let PluginInstallSource::Remote(remote_source) = source else {
                return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
            };
            let prepared_res = match download_remote_install_archive(
                remote_source,
                DEFAULT_MAX_SIZE_BYTES,
                &network,
            )
            .await
            {
                Ok(RemoteDownloadOutcome::Downloaded {
                    archive,
                    has_npm_deps,
                }) => {
                    let stage_network = if has_npm_deps {
                        let npm_reach = format!("{label} npm dependencies from registry.npmjs.org");
                        call.context
                            .act("install", BrokerEffect::Reach, Some(&npm_reach))
                            .await?;
                        if call.context.cancel().is_cancelled() {
                            record_outcome(call, "install", Some(&label), None, None, "cancelled");
                            return cancelled_result(
                                "install",
                                raw_target,
                                None,
                                None,
                                base_warnings,
                            );
                        }
                        let (refreshed_tool, refreshed_auth) = live(call);
                        tool = refreshed_tool;
                        let Some(refreshed_auth) = refreshed_auth else {
                            return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
                        };
                        current_authority = refreshed_auth;
                        match approver_from_posture(&current_authority) {
                            PendingApprover::Nobody => {
                                record_outcome(call, "install", Some(&label), None, None, "failed");
                                return change_with_outcome(ChangeOutcome {
                                    application: "failed",
                                    stage: "install",
                                    target: raw_target,
                                    changed: false,
                                    enabled: None,
                                    code: "operation-error",
                                    diagnostic:
                                        "permission mode changed during review and no longer allows plugin installation"
                                            .to_string(),
                                    bundle: None,
                                    version: None,
                                    warnings: base_warnings,
                                });
                            }
                            PendingApprover::Agent(_) => {
                                record_outcome(call, "install", Some(&label), None, None, "failed");
                                return change_with_outcome(ChangeOutcome {
                                    application: "failed",
                                    stage: "install",
                                    target: raw_target,
                                    changed: false,
                                    enabled: None,
                                    code: "operation-error",
                                    diagnostic: format!(
                                        "a remote plugin source is not written by this project's agent; {ASK_PERSON_HINT}"
                                    ),
                                    bundle: None,
                                    version: None,
                                    warnings: base_warnings,
                                });
                            }
                            PendingApprover::Person => {}
                        }
                        Some(network_policy_for(&tool))
                    } else {
                        None
                    };
                    match stage_downloaded_remote_install(
                        archive,
                        DEFAULT_MAX_SIZE_BYTES,
                        stage_network.as_ref(),
                    )
                    .await
                    {
                        Ok(RemotePreviewOutcome::Ready(prepared)) => Ok(prepared),
                        Ok(RemotePreviewOutcome::NeedsApproval(blocked_host)) => Err((
                            "operation-error",
                            format!("network approval required for {blocked_host}"),
                        )),
                        Ok(RemotePreviewOutcome::NetworkDenied(denied_host)) => Err((
                            "operation-error",
                            format!("network access denied for {denied_host}"),
                        )),
                        Err(error) => Err((
                            classify_install_error(&error),
                            scrub_message(&format!("{error:#}"), &scrub),
                        )),
                    }
                }
                Ok(RemoteDownloadOutcome::NeedsApproval(blocked_host)) => Err((
                    "operation-error",
                    format!("network approval required for {blocked_host}"),
                )),
                Ok(RemoteDownloadOutcome::NetworkDenied(denied_host)) => Err((
                    "operation-error",
                    format!("network access denied for {denied_host}"),
                )),
                Err(error) => Err((
                    classify_install_error(&error),
                    scrub_message(&format!("{error:#}"), &scrub),
                )),
            };
            let prepared = match prepared_res {
                Ok(prepared) => prepared,
                Err((code, msg)) => {
                    record_outcome(call, "install", Some(&label), None, None, "failed");
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code,
                        diagnostic: msg,
                        bundle: None,
                        version: None,
                        warnings: base_warnings,
                    });
                }
            };

            let fresh_snapshot = Arc::clone(&initial_registry);
            let prepared_name = prepared.name.clone();
            let pre_collision = tokio::task::spawn_blocking(move || {
                #[cfg(any(test, feature = "test-support"))]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                let _scope = PolicyScope::propagate(policy);
                let fresh = fresh_snapshot.rediscover_for_workspace(fresh_snapshot.workspace());
                fresh
                    .get(&prepared_name)
                    .map(|p| (p.name().to_string(), p.scope.as_str()))
            })
            .await
            .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;
            if let Some((existing_name, scope)) = pre_collision {
                record_outcome(
                    call,
                    "install",
                    Some(&existing_name),
                    Some(&prepared.content_hash),
                    None,
                    "failed",
                );
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "install",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "already-installed",
                    diagnostic: format!(
                        "plugin `{existing_name}` is already installed ({scope} scope); remove it first, then install"
                    ),
                    bundle: Some(existing_name),
                    version: None,
                    warnings: base_warnings,
                });
            }

            let write_target =
                format!("{} {} {}", prepared.name, prepared.content_hash, raw_target);
            call.context
                .act("install", BrokerEffect::Write, Some(&write_target))
                .await?;

            if call.context.cancel().is_cancelled() {
                record_outcome(
                    call,
                    "install",
                    Some(&prepared.name),
                    Some(&prepared.content_hash),
                    None,
                    "cancelled",
                );
                return cancelled_result(
                    "install",
                    raw_target,
                    Some(prepared.name),
                    None,
                    base_warnings,
                );
            }
            let (refreshed_tool, refreshed_auth) = live(call);
            tool = refreshed_tool;
            let Some(refreshed_auth) = refreshed_auth else {
                return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
            };
            match approver_from_posture(&refreshed_auth) {
                PendingApprover::Nobody => {
                    record_outcome(
                        call,
                        "install",
                        Some(&prepared.name),
                        Some(&prepared.content_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic:
                            "permission mode changed during review and no longer allows plugin installation"
                                .to_string(),
                        bundle: Some(prepared.name),
                        version: None,
                        warnings: base_warnings,
                    });
                }
                PendingApprover::Agent(_) => {
                    record_outcome(
                        call,
                        "install",
                        Some(&prepared.name),
                        Some(&prepared.content_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: format!(
                            "a remote plugin source is not written by this project's agent; {ASK_PERSON_HINT}"
                        ),
                        bundle: Some(prepared.name),
                        version: None,
                        warnings: base_warnings,
                    });
                }
                PendingApprover::Person => {}
            }

            let post_network = network_policy_for(&tool);
            let fresh_snapshot = Arc::clone(&initial_registry);
            let mut registry_for_exec = tokio::task::spawn_blocking(move || {
                #[cfg(any(test, feature = "test-support"))]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                let _scope = PolicyScope::propagate(policy);
                (*fresh_snapshot.rediscover_for_workspace(fresh_snapshot.workspace())).clone()
            })
            .await
            .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;

            let prepared_name_for_exec = prepared.name.clone();
            let expected_hash = prepared.content_hash.clone();
            let exec_result = execute(
                PluginMutationRequest::InstallRemoteExact {
                    prepared,
                    expected_content_hash: expected_hash.clone(),
                },
                &PluginMutationContext {
                    network: &post_network,
                    max_size: DEFAULT_MAX_SIZE_BYTES,
                },
                &mut registry_for_exec,
            )
            .await;

            let receipt = match exec_result {
                Ok(receipt) => receipt,
                Err(error) => {
                    let is_mismatch = error.downcast_ref::<PluginContentMismatch>().is_some()
                        || error.downcast_ref::<PluginArchiveMismatch>().is_some();
                    let code = classify_install_error(&error);
                    let msg = if is_mismatch {
                        "changed after review".to_string()
                    } else if code == "already-installed" {
                        format!(
                            "plugin `{prepared_name_for_exec}` is already installed; remove it first, then install"
                        )
                    } else {
                        scrub_message(&format!("{error:#}"), &scrub)
                    };
                    record_outcome(
                        call,
                        "install",
                        Some(&prepared_name_for_exec),
                        Some(&expected_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code,
                        diagnostic: msg,
                        bundle: Some(prepared_name_for_exec),
                        version: None,
                        warnings: base_warnings,
                    });
                }
            };

            match receipt.outcome {
                PluginMutationOutcome::NeedsApproval(blocked_host) => {
                    record_outcome(
                        call,
                        "install",
                        Some(&prepared_name_for_exec),
                        Some(&expected_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: format!("network approval required for {blocked_host}"),
                        bundle: Some(prepared_name_for_exec),
                        version: None,
                        warnings: base_warnings,
                    });
                }
                PluginMutationOutcome::NetworkDenied(denied_host) => {
                    record_outcome(
                        call,
                        "install",
                        Some(&prepared_name_for_exec),
                        Some(&expected_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: format!("network access denied for {denied_host}"),
                        bundle: Some(prepared_name_for_exec),
                        version: None,
                        warnings: base_warnings,
                    });
                }
                PluginMutationOutcome::Installed => {}
                _ => {
                    record_outcome(
                        call,
                        "install",
                        Some(&prepared_name_for_exec),
                        Some(&expected_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: "unexpected remote install outcome".to_string(),
                        bundle: Some(prepared_name_for_exec),
                        version: None,
                        warnings: base_warnings,
                    });
                }
            }

            if let Some(installed_dir) = receipt.path.as_deref() {
                #[cfg(test)]
                fire_post_install_hook_for_test(installed_dir, call.context.cancel());
                let _ = installed_dir;
            }
            let installed_hash = receipt.installed_content_hash.unwrap_or_default();
            finish_post_install(
                call,
                PostInstallInput {
                    reg_after_place: Arc::new(registry_for_exec),
                    installed_name: receipt.name,
                    expected_installed_hash: installed_hash,
                    version: None,
                    raw_target,
                    want_enabled: input.enabled,
                    warnings: base_warnings,
                    is_own: false,
                    source_dir: None,
                    canon_workspace,
                    registry_ignored,
                },
            )
            .await
        }
        PreparedSource::Local {
            mut source,
            source_dir,
            pre_gate,
            canon_workspace,
        } => {
            let (gate_target, audit_name, audit_hash, audit_version, mut reach_gated) =
                match &pre_gate {
                    PreGateLocal::Ready {
                        plugin_name,
                        pre_hash,
                        version,
                        reach_gated,
                        ..
                    } => (
                        format!("{plugin_name} {pre_hash} {raw_target}"),
                        Some(plugin_name.clone()),
                        Some(pre_hash.clone()),
                        version.clone(),
                        *reach_gated,
                    ),
                    PreGateLocal::Deferred { .. } => (raw_target.clone(), None, None, None, false),
                };
            call.context
                .act("install", BrokerEffect::Write, Some(&gate_target))
                .await?;

            if call.context.cancel().is_cancelled() {
                record_outcome(
                    call,
                    "install",
                    audit_name.as_deref(),
                    audit_hash.as_deref(),
                    None,
                    "cancelled",
                );
                return cancelled_result(
                    "install",
                    raw_target,
                    audit_name,
                    audit_version,
                    base_warnings,
                );
            }

            let (refreshed_tool, refreshed_auth) = live(call);
            tool = refreshed_tool;
            let Some(refreshed_auth) = refreshed_auth else {
                return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
            };
            let after_pending = approver_from_posture(&refreshed_auth);
            if matches!(after_pending, PendingApprover::Nobody) {
                record_outcome(
                    call,
                    "install",
                    audit_name.as_deref(),
                    audit_hash.as_deref(),
                    None,
                    "failed",
                );
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "install",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "operation-error",
                    diagnostic:
                        "permission mode changed during review and no longer allows plugin installation"
                            .to_string(),
                    bundle: audit_name,
                    version: audit_version,
                    warnings: base_warnings,
                });
            }

            let reg_snapshot2 = Arc::clone(&initial_registry);
            let dir_check2 = source_dir.clone();
            let verify2 = tokio::task::spawn_blocking(move || {
                #[cfg(any(test, feature = "test-support"))]
                let _env_scope = crate::test_support::join_env_scope(env_scope);
                let _scope = PolicyScope::propagate(policy);
                let fresh = reg_snapshot2.rediscover_for_workspace(reg_snapshot2.workspace());
                let canon_ws = std::fs::canonicalize(fresh.workspace()).ok();
                let in_project = clean_dir_is_strictly_inside(&dir_check2, fresh.workspace());
                let user_plugins_dir = fresh.user_plugins_dir().map(Path::to_path_buf);
                (fresh, canon_ws, in_project, user_plugins_dir)
            })
            .await
            .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;
            let (fresh2, canon_ws2, in_project2, user_plugins_dir2) = verify2;
            let ancestor_clean2 = !has_symlink_or_reparse_ancestor_or_target(&source_dir)
                .await
                .unwrap_or(true);
            let in_user_plugins2 =
                dir_is_in_user_plugins(&source_dir, user_plugins_dir2.as_deref()).await;
            let still_own = in_project2 && ancestor_clean2 && !in_user_plugins2;
            let effective_canon_ws = canon_workspace.or(canon_ws2);
            let scrub = ScrubContext {
                workspace: fresh2.workspace(),
                canon_workspace: effective_canon_ws.as_deref(),
                plugin_root: Some(&source_dir),
                user_plugins_dir: fresh2.user_plugins_dir(),
                state_path: fresh2.state_path(),
            };

            if matches!(after_pending, PendingApprover::Agent(_))
                && !has_agent_call_identity(call, &tool)
            {
                record_outcome(
                    call,
                    "install",
                    audit_name.as_deref(),
                    audit_hash.as_deref(),
                    None,
                    "failed",
                );
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "install",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "operation-error",
                    diagnostic:
                        "An approval by the agent must name the thread, turn and tool call that made it"
                            .to_string(),
                    bundle: audit_name,
                    version: audit_version,
                    warnings: base_warnings,
                });
            }
            if matches!(after_pending, PendingApprover::Agent(_)) && !still_own {
                record_outcome(
                    call,
                    "install",
                    audit_name.as_deref(),
                    audit_hash.as_deref(),
                    None,
                    "failed",
                );
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "install",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "operation-error",
                    diagnostic: format!(
                        "source folder is no longer cleanly inside this project; {ASK_PERSON_HINT}"
                    ),
                    bundle: audit_name,
                    version: audit_version,
                    warnings: base_warnings,
                });
            }

            let mut post_network = network_policy_for(&tool);
            let (plugin_name, source_package, version, pre_hash) = match pre_gate {
                PreGateLocal::Deferred { deferred_error } => {
                    if let Some((code, msg)) = deferred_error {
                        record_outcome(call, "install", None, None, None, "failed");
                        return change_with_outcome(ChangeOutcome {
                            application: "failed",
                            stage: "install",
                            target: raw_target,
                            changed: false,
                            enabled: None,
                            code,
                            diagnostic: scrub_message(&msg, &scrub),
                            bundle: None,
                            version: None,
                            warnings: base_warnings,
                        });
                    }
                    let clean_root = has_symlink_or_reparse_ancestor_or_target(&source_dir)
                        .await
                        .is_ok_and(|b| !b)
                        && ensure_no_links_in_tree(&source_dir).await.is_ok();
                    let (refined_source, deferred_npm_deps, post_refine_error) = if clean_root {
                        refine_local_dsh_source(source.clone(), source_dir.clone()).await?
                    } else {
                        (source, false, None)
                    };
                    source = refined_source;
                    if deferred_npm_deps {
                        if matches!(after_pending, PendingApprover::Agent(_)) {
                            record_outcome(call, "install", None, None, None, "failed");
                            return change_with_outcome(ChangeOutcome {
                                application: "failed",
                                stage: "install",
                                target: raw_target,
                                changed: false,
                                enabled: None,
                                code: "operation-error",
                                diagnostic: format!(
                                    "DSH plugin requires downloading external npm dependencies from registry.npmjs.org; {ASK_PERSON_HINT}"
                                ),
                                bundle: None,
                                version: None,
                                warnings: base_warnings,
                            });
                        }
                        let reach_target =
                            format!("{raw_target} npm dependencies from registry.npmjs.org");
                        call.context
                            .act("install", BrokerEffect::Reach, Some(&reach_target))
                            .await?;
                        if call.context.cancel().is_cancelled() {
                            record_outcome(call, "install", None, None, None, "cancelled");
                            return cancelled_result(
                                "install",
                                raw_target,
                                None,
                                None,
                                base_warnings,
                            );
                        }
                        let (refreshed_tool, refreshed_auth) = live(call);
                        tool = refreshed_tool;
                        let Some(refreshed_auth) = refreshed_auth else {
                            return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
                        };
                        match approver_from_posture(&refreshed_auth) {
                            PendingApprover::Nobody => {
                                record_outcome(call, "install", None, None, None, "failed");
                                return change_with_outcome(ChangeOutcome {
                                    application: "failed",
                                    stage: "install",
                                    target: raw_target,
                                    changed: false,
                                    enabled: None,
                                    code: "operation-error",
                                    diagnostic:
                                        "permission mode changed during review and no longer allows plugin installation"
                                            .to_string(),
                                    bundle: None,
                                    version: None,
                                    warnings: base_warnings,
                                });
                            }
                            PendingApprover::Agent(_) => {
                                record_outcome(call, "install", None, None, None, "failed");
                                return change_with_outcome(ChangeOutcome {
                                    application: "failed",
                                    stage: "install",
                                    target: raw_target,
                                    changed: false,
                                    enabled: None,
                                    code: "operation-error",
                                    diagnostic: format!(
                                        "DSH plugin requires downloading external npm dependencies from registry.npmjs.org; {ASK_PERSON_HINT}"
                                    ),
                                    bundle: None,
                                    version: None,
                                    warnings: base_warnings,
                                });
                            }
                            PendingApprover::Person => {}
                        }
                        reach_gated = true;
                        post_network = network_policy_for(&tool);
                    }
                    let preview_net = if reach_gated {
                        Some(&post_network)
                    } else {
                        None
                    };
                    let post_preview = if let Some(err) = post_refine_error {
                        Err(err)
                    } else {
                        prepare_local_source_and_hash_with_network(
                            &source,
                            &source_dir,
                            preview_net,
                        )
                        .await
                    };
                    match post_preview {
                        Ok(values) => values,
                        Err((code, msg)) => {
                            record_outcome(call, "install", None, None, None, "failed");
                            return change_with_outcome(ChangeOutcome {
                                application: "failed",
                                stage: "install",
                                target: raw_target,
                                changed: false,
                                enabled: None,
                                code,
                                diagnostic: scrub_message(&msg, &scrub),
                                bundle: None,
                                version: None,
                                warnings: base_warnings,
                            });
                        }
                    }
                }
                PreGateLocal::Ready {
                    plugin_name,
                    source_package,
                    version,
                    pre_hash,
                    ..
                } => {
                    if !reach_gated
                        && matches!(source, PluginInstallSource::Dsh(_))
                        && has_unresolved_npm_dependencies(&source_dir).unwrap_or(false)
                    {
                        record_outcome(
                            call,
                            "install",
                            Some(&plugin_name),
                            Some(&pre_hash),
                            None,
                            "failed",
                        );
                        return change_with_outcome(ChangeOutcome {
                            application: "failed",
                            stage: "install",
                            target: raw_target,
                            changed: false,
                            enabled: None,
                            code: "operation-error",
                            diagnostic: "changed after review".to_string(),
                            bundle: Some(plugin_name),
                            version,
                            warnings: base_warnings,
                        });
                    }
                    let preview_net = if reach_gated {
                        Some(&post_network)
                    } else {
                        None
                    };
                    match prepare_local_source_and_hash_with_network(
                        &source,
                        &source_dir,
                        preview_net,
                    )
                    .await
                    {
                        Ok((cur_name, _, _, cur_hash)) => {
                            if cur_hash != pre_hash || cur_name != plugin_name {
                                record_outcome(
                                    call,
                                    "install",
                                    Some(&plugin_name),
                                    Some(&cur_hash),
                                    None,
                                    "failed",
                                );
                                return change_with_outcome(ChangeOutcome {
                                    application: "failed",
                                    stage: "install",
                                    target: raw_target,
                                    changed: false,
                                    enabled: None,
                                    code: "operation-error",
                                    diagnostic: "changed after review".to_string(),
                                    bundle: Some(plugin_name),
                                    version,
                                    warnings: base_warnings,
                                });
                            }
                        }
                        Err((code, msg)) => {
                            record_outcome(
                                call,
                                "install",
                                Some(&plugin_name),
                                Some(&pre_hash),
                                None,
                                "failed",
                            );
                            return change_with_outcome(ChangeOutcome {
                                application: "failed",
                                stage: "install",
                                target: raw_target,
                                changed: false,
                                enabled: None,
                                code,
                                diagnostic: scrub_message(&msg, &scrub),
                                bundle: Some(plugin_name),
                                version,
                                warnings: base_warnings,
                            });
                        }
                    }
                    (plugin_name, source_package, version, pre_hash)
                }
            };

            let collision = {
                let fresh_ref = Arc::clone(&fresh2);
                let pname = plugin_name.clone();
                let spkg = source_package.clone();
                tokio::task::spawn_blocking(move || {
                    #[cfg(any(test, feature = "test-support"))]
                    let _env_scope = crate::test_support::join_env_scope(env_scope);
                    let _scope = PolicyScope::propagate(policy);
                    if let Some(p) = fresh_ref.get(&pname) {
                        return Some((p.name().to_string(), p.scope.as_str()));
                    }
                    if let Some(pkg) = spkg.as_deref() {
                        match resolve_plugin_by_target(&fresh_ref, pkg) {
                            TargetLookup::Found(p) => {
                                return Some((p.name().to_string(), p.scope.as_str()));
                            }
                            TargetLookup::Ambiguous { first, .. } => {
                                return Some((first.name().to_string(), first.scope.as_str()));
                            }
                            TargetLookup::Missing => {}
                        }
                    }
                    None
                })
                .await
                .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?
            };
            if let Some((existing_name, scope)) = collision {
                record_outcome(
                    call,
                    "install",
                    Some(&existing_name),
                    Some(&pre_hash),
                    None,
                    "failed",
                );
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "install",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "already-installed",
                    diagnostic: format!(
                        "plugin `{existing_name}` is already installed ({scope} scope); remove it first, then install"
                    ),
                    bundle: Some(existing_name),
                    version,
                    warnings: base_warnings,
                });
            }

            if call.context.cancel().is_cancelled() {
                record_outcome(
                    call,
                    "install",
                    Some(&plugin_name),
                    Some(&pre_hash),
                    None,
                    "cancelled",
                );
                return cancelled_result(
                    "install",
                    raw_target,
                    Some(plugin_name),
                    version,
                    base_warnings,
                );
            }

            let exec_network = if reach_gated {
                post_network
            } else {
                NetworkPolicy::default()
            };
            let mut reg_for_exec = (*fresh2).clone();
            let exec_result = execute(
                PluginMutationRequest::InstallExact {
                    source,
                    expected_content_hash: pre_hash.clone(),
                },
                &PluginMutationContext {
                    network: &exec_network,
                    max_size: DEFAULT_MAX_SIZE_BYTES,
                },
                &mut reg_for_exec,
            )
            .await;

            let receipt = match exec_result {
                Ok(receipt) => receipt,
                Err(error) => {
                    let is_mismatch = error.downcast_ref::<PluginContentMismatch>().is_some()
                        || error.downcast_ref::<PluginArchiveMismatch>().is_some();
                    let code = classify_install_error(&error);
                    let msg = if is_mismatch {
                        "changed after review".to_string()
                    } else if code == "already-installed" {
                        format!(
                            "plugin `{plugin_name}` is already installed; remove it first, then install"
                        )
                    } else {
                        scrub_message(&format!("{error:#}"), &scrub)
                    };
                    record_outcome(
                        call,
                        "install",
                        Some(&plugin_name),
                        Some(&pre_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code,
                        diagnostic: msg,
                        bundle: Some(plugin_name),
                        version,
                        warnings: base_warnings,
                    });
                }
            };

            match receipt.outcome {
                PluginMutationOutcome::NeedsApproval(blocked_host) => {
                    record_outcome(
                        call,
                        "install",
                        Some(&plugin_name),
                        Some(&pre_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: format!("network approval required for {blocked_host}"),
                        bundle: Some(plugin_name),
                        version,
                        warnings: base_warnings,
                    });
                }
                PluginMutationOutcome::NetworkDenied(denied_host) => {
                    record_outcome(
                        call,
                        "install",
                        Some(&plugin_name),
                        Some(&pre_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: format!("network access denied for {denied_host}"),
                        bundle: Some(plugin_name),
                        version,
                        warnings: base_warnings,
                    });
                }
                PluginMutationOutcome::Installed => {}
                _ => {
                    record_outcome(
                        call,
                        "install",
                        Some(&plugin_name),
                        Some(&pre_hash),
                        None,
                        "failed",
                    );
                    return change_with_outcome(ChangeOutcome {
                        application: "failed",
                        stage: "install",
                        target: raw_target,
                        changed: false,
                        enabled: None,
                        code: "operation-error",
                        diagnostic: "unexpected local install outcome".to_string(),
                        bundle: Some(plugin_name),
                        version,
                        warnings: base_warnings,
                    });
                }
            }

            if let Some(installed_dir) = receipt.path.as_deref() {
                #[cfg(test)]
                fire_post_install_hook_for_test(installed_dir, call.context.cancel());
                let _ = installed_dir;
            }
            let installed_hash = receipt.installed_content_hash.unwrap_or_default();
            finish_post_install(
                call,
                PostInstallInput {
                    reg_after_place: Arc::new(reg_for_exec),
                    installed_name: receipt.name,
                    expected_installed_hash: installed_hash,
                    version,
                    raw_target,
                    want_enabled: input.enabled,
                    warnings: base_warnings,
                    is_own: still_own,
                    source_dir: Some(source_dir),
                    canon_workspace: effective_canon_ws,
                    registry_ignored,
                },
            )
            .await
        }
    }
}

struct PostInstallInput {
    reg_after_place: Arc<PluginRegistry>,
    installed_name: String,
    expected_installed_hash: String,
    version: Option<String>,
    raw_target: String,
    want_enabled: bool,
    warnings: Vec<String>,
    is_own: bool,
    source_dir: Option<PathBuf>,
    canon_workspace: Option<PathBuf>,
    registry_ignored: bool,
}

async fn finish_post_install(
    call: &BrokerCall,
    input: PostInstallInput,
) -> Result<Value, BrokerRefusal> {
    let PostInstallInput {
        reg_after_place,
        installed_name,
        expected_installed_hash,
        version,
        raw_target,
        want_enabled,
        mut warnings,
        is_own: allow_agent,
        source_dir,
        canon_workspace,
        registry_ignored,
    } = input;
    let policy = extension_host_policy_enabled();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = crate::test_support::env_scope_ticket();

    let (tool, authority) = live(call);
    let Some(authority) = authority else {
        return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
    };
    let pending = approver_from_posture(&authority);
    if call.context.cancel().is_cancelled() {
        let fresh = tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _env_scope = crate::test_support::join_env_scope(env_scope);
            let _scope = PolicyScope::propagate(policy);
            reg_after_place.rediscover_for_workspace(reg_after_place.workspace())
        })
        .await
        .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;
        super::plugins_changed(fresh);
        record_outcome(
            call,
            "install",
            Some(&installed_name),
            Some(&expected_installed_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: "install",
            target: raw_target,
            changed: true,
            enabled: Some(false),
            code: "operation-error",
            diagnostic:
                "cancelled after installation; the plugin remains installed on disk, disabled and untrusted"
                    .to_string(),
            bundle: Some(installed_name),
            version,
            warnings,
        });
    }

    let Some(approver) = pending.as_approver(call, &tool, reg_after_place.workspace()) else {
        let fresh = tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _env_scope = crate::test_support::join_env_scope(env_scope);
            let _scope = PolicyScope::propagate(policy);
            reg_after_place.rediscover_for_workspace(reg_after_place.workspace())
        })
        .await
        .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;
        super::plugins_changed(fresh);
        record_outcome(
            call,
            "install",
            Some(&installed_name),
            Some(&expected_installed_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: "install",
            target: raw_target,
            changed: true,
            enabled: Some(false),
            code: "operation-error",
            diagnostic:
                "permission mode changed after installation, so the plugin remains disabled and untrusted"
                    .to_string(),
            bundle: Some(installed_name),
            version,
            warnings,
        });
    };
    if !allow_agent && matches!(approver, PluginApprover::Agent(_)) {
        let fresh = tokio::task::spawn_blocking(move || {
            #[cfg(any(test, feature = "test-support"))]
            let _env_scope = crate::test_support::join_env_scope(env_scope);
            let _scope = PolicyScope::propagate(policy);
            reg_after_place.rediscover_for_workspace(reg_after_place.workspace())
        })
        .await
        .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;
        super::plugins_changed(fresh);
        record_outcome(
            call,
            "install",
            Some(&installed_name),
            Some(&expected_installed_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: "install",
            target: raw_target,
            changed: true,
            enabled: Some(false),
            code: "operation-error",
            diagnostic: format!(
                "permission mode changed to agent approval on a remote plugin, so it remains disabled and untrusted; {ASK_PERSON_HINT}"
            ),
            bundle: Some(installed_name),
            version,
            warnings,
        });
    }

    let approver_for_commit = approver.clone();
    let name_for_commit = installed_name.clone();
    let expected_hash_clone = expected_installed_hash.clone();
    enum PostCommit {
        Mismatch(Arc<PluginRegistry>),
        Failed {
            registry: Arc<PluginRegistry>,
            enabled: bool,
            err: String,
        },
        Overridden {
            registry: Arc<PluginRegistry>,
            enabled: bool,
            err: String,
        },
        Done {
            registry: Arc<PluginRegistry>,
            loaded: Box<LoadedPlugin>,
            warnings: Vec<String>,
        },
    }
    let outcome = tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let _scope = PolicyScope::propagate(policy);
        let fresh_arc = reg_after_place.rediscover_for_workspace(reg_after_place.workspace());
        let Some(loaded) = fresh_arc.get(&name_for_commit).cloned() else {
            return PostCommit::Mismatch(fresh_arc);
        };
        if loaded.content_hash != expected_hash_clone {
            return PostCommit::Mismatch(fresh_arc);
        }
        let mut reg = (*fresh_arc).clone();
        let selector = loaded.id.as_str().to_string();
        let host_services = match &approver_for_commit {
            PluginApprover::Agent(_) => reg
                .host_service_review_for(&selector, &approver_for_commit)
                .map(|review| review.token),
            PluginApprover::Person { .. } | PluginApprover::CommandLine => None,
        };
        let approval = ReviewApproval {
            approved_by: approver_for_commit.clone(),
            host_services,
        };
        let commit_res = if want_enabled {
            reg.trust_and_enable_by(&selector, approval, approver_for_commit.clone())
        } else {
            reg.trust_reviewed(&selector, approval)
        };
        if let Err(err) = commit_res {
            let latest = reg.rediscover_for_workspace(reg.workspace());
            let persisted_enabled = latest.get(&selector).is_some_and(|p| p.enabled);
            if want_enabled && is_managed_policy_error(&err) {
                return PostCommit::Overridden {
                    registry: latest,
                    enabled: persisted_enabled,
                    err,
                };
            }
            return PostCommit::Failed {
                registry: latest,
                enabled: persisted_enabled,
                err,
            };
        }
        let final_reg = reg.rediscover_for_workspace(reg.workspace());
        let final_loaded = final_reg.get(&selector).cloned().unwrap_or(loaded);
        let scrub = ScrubContext {
            workspace: final_reg.workspace(),
            canon_workspace: canon_workspace.as_deref(),
            plugin_root: Some(&final_loaded.canonical_root),
            user_plugins_dir: final_reg.user_plugins_dir(),
            state_path: final_reg.state_path(),
        };
        let extra_warnings = collect_enable_warnings(
            &final_reg,
            &final_loaded,
            &approver_for_commit,
            final_loaded.enabled,
            registry_ignored,
            &scrub,
        );
        PostCommit::Done {
            registry: final_reg,
            loaded: Box::new(final_loaded),
            warnings: extra_warnings,
        }
    })
    .await
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;

    match outcome {
        PostCommit::Mismatch(fresh) => {
            super::plugins_changed(fresh);
            record_outcome(
                call,
                "install",
                Some(&installed_name),
                Some(&expected_installed_hash),
                Some(&approver),
                "failed",
            );
            change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "install",
                target: raw_target,
                changed: true,
                enabled: Some(false),
                code: "operation-error",
                diagnostic: "changed after review".to_string(),
                bundle: Some(installed_name),
                version,
                warnings,
            })
        }
        PostCommit::Overridden {
            registry: fresh,
            enabled,
            err,
        } => {
            let scrub = ScrubContext {
                workspace: fresh.workspace(),
                canon_workspace: None,
                plugin_root: source_dir.as_deref(),
                user_plugins_dir: fresh.user_plugins_dir(),
                state_path: fresh.state_path(),
            };
            let diag = scrub_message(&err, &scrub);
            super::plugins_changed(fresh);
            record_outcome(
                call,
                "install",
                Some(&installed_name),
                Some(&expected_installed_hash),
                Some(&approver),
                "overridden",
            );
            change_with_outcome(ChangeOutcome {
                application: "overridden",
                stage: "enable",
                target: raw_target,
                changed: true,
                enabled: Some(enabled),
                code: "operation-error",
                diagnostic: diag,
                bundle: Some(installed_name),
                version,
                warnings,
            })
        }
        PostCommit::Failed {
            registry: fresh,
            enabled,
            err,
        } => {
            let scrub = ScrubContext {
                workspace: fresh.workspace(),
                canon_workspace: None,
                plugin_root: source_dir.as_deref(),
                user_plugins_dir: fresh.user_plugins_dir(),
                state_path: fresh.state_path(),
            };
            let diag = scrub_message(&err, &scrub);
            super::plugins_changed(fresh);
            record_outcome(
                call,
                "install",
                Some(&installed_name),
                Some(&expected_installed_hash),
                Some(&approver),
                "failed",
            );
            change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "enable",
                target: raw_target,
                changed: true,
                enabled: Some(enabled),
                code: "operation-error",
                diagnostic: diag,
                bundle: Some(installed_name),
                version,
                warnings,
            })
        }
        PostCommit::Done {
            registry: final_reg,
            loaded: final_loaded,
            warnings: built_warnings,
        } => {
            warnings = built_warnings;
            super::plugins_changed(Arc::clone(&final_reg));
            let scrub = ScrubContext {
                workspace: final_reg.workspace(),
                canon_workspace: None,
                plugin_root: Some(&final_loaded.canonical_root),
                user_plugins_dir: final_reg.user_plugins_dir(),
                state_path: final_reg.state_path(),
            };
            if want_enabled && final_loaded.enabled {
                match finish_enabled_activation(
                    call,
                    EnableActivation {
                        method: "install",
                        stage: "enable",
                        target: raw_target.clone(),
                        changed: true,
                        registry: &final_reg,
                        loaded: &final_loaded,
                        approver: &approver,
                        version: version.clone(),
                        warnings,
                    },
                    &scrub,
                )
                .await
                {
                    std::ops::ControlFlow::Continue(updated) => warnings = updated,
                    std::ops::ControlFlow::Break(early) => return early,
                }
            }
            record_outcome(
                call,
                "install",
                Some(final_loaded.name()),
                Some(&final_loaded.content_hash),
                Some(&approver),
                "applied",
            );
            change_ok(
                if want_enabled { "enable" } else { "install" },
                raw_target,
                true,
                Some(final_loaded.enabled),
                Some(final_loaded.name().to_string()),
                non_empty_version(&final_loaded.manifest.plugin.version).or(version),
                warnings,
            )
        }
    }
}

async fn load_existing_plugin(
    registry: Arc<PluginRegistry>,
    target: String,
    row: Option<String>,
) -> Result<PreparedExisting, BrokerRefusal> {
    let policy = extension_host_policy_enabled();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = crate::test_support::env_scope_ticket();
    tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let _scope = PolicyScope::propagate(policy);
        let fresh = registry.rediscover_for_workspace(registry.workspace());
        let canon_ws = std::fs::canonicalize(fresh.workspace()).ok();
        let all_plugins = fresh.list();
        let counts = name_counts(&all_plugins);

        let matching_row = |plugin: &LoadedPlugin, wanted: &str| -> (usize, bool) {
            let prefix = id_prefix(plugin, &counts);
            let mut truncated = false;
            let rows = collect_component_rows(plugin, &mut truncated);
            let total = rows.len();
            let hit = rows.into_iter().any(|item| {
                let (row_id, module_name, entry_id) =
                    row_identities(prefix, &item.label, &mut truncated);
                row_id == wanted || module_name == wanted || entry_id == wanted
            });
            (total, hit)
        };

        let found = match resolve_plugin_by_target(&fresh, &target) {
            TargetLookup::Ambiguous { message, .. } => {
                return PreparedExisting::Ambiguous(message);
            }
            TargetLookup::Found(plugin) => {
                if let Some(ref requested_row) = row {
                    let (total, hit) = matching_row(plugin, requested_row);
                    if !hit {
                        return PreparedExisting::Missing;
                    }
                    if total != 1 {
                        return PreparedExisting::NotSoleRow(plugin.name().to_string());
                    }
                }
                Some(plugin.clone())
            }
            TargetLookup::Missing => {
                if row.is_none() {
                    let mut resolved: Option<LoadedPlugin> = None;
                    let mut resolved_total = 0usize;
                    for plugin in &all_plugins {
                        let prefix = id_prefix(plugin, &counts);
                        let mut truncated = false;
                        let rows = collect_component_rows(plugin, &mut truncated);
                        let total = rows.len();
                        let hit = rows.into_iter().any(|item| {
                            let (_, _, entry_id) =
                                row_identities(prefix, &item.label, &mut truncated);
                            entry_id == target
                        });
                        if hit {
                            if resolved.is_some() {
                                return PreparedExisting::Ambiguous(format!(
                                    "target `{target}` matches multiple installed plugins"
                                ));
                            }
                            resolved = Some((*plugin).clone());
                            resolved_total = total;
                        }
                    }
                    if let Some(plugin) = &resolved
                        && resolved_total != 1
                    {
                        return PreparedExisting::NotSoleRow(plugin.name().to_string());
                    }
                    resolved
                } else {
                    None
                }
            }
        };

        let Some(plugin) = found else {
            if row.is_none()
                && codewhale_skill_install::install::validate_skill_name_segment(&target).is_ok()
                && let Some(user_plugins_dir) = fresh.user_plugins_dir()
            {
                let candidate = user_plugins_dir.join(&target);
                if let Ok(meta) = std::fs::symlink_metadata(&candidate) {
                    let link_or_non_dir = metadata_is_link_or_reparse(&meta) || !meta.is_dir();
                    let marker_present = std::fs::symlink_metadata(
                        candidate.join(codewhale_skill_install::install::INSTALLED_FROM_MARKER),
                    )
                    .is_ok();
                    let canon_dir = std::fs::canonicalize(user_plugins_dir)
                        .ok()
                        .map(|root| root.join(&target));
                    let has_bundle_diag = fresh.diagnostics().iter().any(|diag| {
                        diag.path.as_ref().is_some_and(|path| {
                            path.starts_with(&candidate)
                                || canon_dir.as_ref().is_some_and(|dir| path.starts_with(dir))
                        })
                    });
                    if link_or_non_dir
                        || (!has_regular_install_marker(&candidate)
                            && (marker_present || has_bundle_diag))
                    {
                        return PreparedExisting::Unremovable(target);
                    }
                }
            }
            return PreparedExisting::Missing;
        };
        let (is_own, own_denial_reason) = is_existing_own(&fresh, &plugin);
        let marker_ok = has_regular_install_marker(&plugin.canonical_root);
        PreparedExisting::Found {
            plugin: Box::new(plugin),
            is_own,
            own_denial_reason,
            has_marker: marker_ok,
            canon_workspace: canon_ws,
        }
    })
    .await
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))
}

async fn remove(
    call: &BrokerCall,
    mut tool: ToolContext,
    authority: Option<LiveRuntimeAuthority>,
    input: RemoveInput,
) -> Result<Value, BrokerRefusal> {
    let raw_target = validate_required_str(&input.target, "target", MAX_TARGET_BYTES)?;
    let initial_registry = live_registry(call, &tool)?;
    let Some(current_authority) = authority else {
        return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
    };

    let (initial_plugin, is_own, own_denial_reason, has_marker) = match load_existing_plugin(
        Arc::clone(&initial_registry),
        raw_target.clone(),
        None,
    )
    .await?
    {
        PreparedExisting::Missing => {
            record_outcome(call, "remove", None, None, None, "failed");
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "remove",
                target: raw_target.clone(),
                changed: false,
                enabled: None,
                code: "unknown-plugin",
                diagnostic: format!("no plugin `{raw_target}` is installed"),
                bundle: None,
                version: None,
                warnings: Vec::new(),
            });
        }
        PreparedExisting::Unremovable(plugin_name) => {
            record_outcome(call, "remove", Some(&plugin_name), None, None, "failed");
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "remove",
                target: raw_target,
                changed: false,
                enabled: None,
                code: "not-removable",
                diagnostic: format!(
                    "plugin `{plugin_name}` has no valid regular .installed-from record, so Codewhale refuses to remove it"
                ),
                bundle: Some(plugin_name),
                version: None,
                warnings: Vec::new(),
            });
        }
        PreparedExisting::NotSoleRow(plugin_name) => {
            record_outcome(call, "remove", Some(&plugin_name), None, None, "failed");
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "remove",
                target: raw_target,
                changed: false,
                enabled: None,
                code: "operation-error",
                diagnostic: "Codewhale removes a plugin as a whole; use the bundle name"
                    .to_string(),
                bundle: Some(plugin_name),
                version: None,
                warnings: Vec::new(),
            });
        }
        PreparedExisting::Ambiguous(msg) => {
            record_outcome(call, "remove", None, None, None, "failed");
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "remove",
                target: raw_target,
                changed: false,
                enabled: None,
                code: "operation-error",
                diagnostic: msg,
                bundle: None,
                version: None,
                warnings: Vec::new(),
            });
        }
        PreparedExisting::Found {
            plugin,
            is_own,
            own_denial_reason,
            has_marker,
            canon_workspace: _,
        } => (plugin, is_own, own_denial_reason, has_marker),
    };

    let bundle_name = initial_plugin.name().to_string();
    let version = non_empty_version(&initial_plugin.manifest.plugin.version);
    match initial_plugin.scope {
        PluginScope::Builtin => {
            record_outcome(
                call,
                "remove",
                Some(&bundle_name),
                Some(&initial_plugin.content_hash),
                None,
                "failed",
            );
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "remove",
                target: raw_target,
                changed: false,
                enabled: Some(initial_plugin.enabled),
                code: "management-required",
                diagnostic: format!(
                    "built-in plugin `{bundle_name}` ships with Codewhale and cannot be removed"
                ),
                bundle: Some(bundle_name),
                version,
                warnings: Vec::new(),
            });
        }
        PluginScope::Workspace => {
            record_outcome(
                call,
                "remove",
                Some(&bundle_name),
                Some(&initial_plugin.content_hash),
                None,
                "failed",
            );
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "remove",
                target: raw_target,
                changed: false,
                enabled: Some(initial_plugin.enabled),
                code: "not-removable",
                diagnostic: format!(
                    "workspace plugin `{bundle_name}` lives in the project repository and is not removable here"
                ),
                bundle: Some(bundle_name),
                version,
                warnings: Vec::new(),
            });
        }
        PluginScope::User if !has_marker => {
            record_outcome(
                call,
                "remove",
                Some(&bundle_name),
                Some(&initial_plugin.content_hash),
                None,
                "failed",
            );
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "remove",
                target: raw_target,
                changed: false,
                enabled: Some(initial_plugin.enabled),
                code: "not-removable",
                diagnostic: format!(
                    "plugin `{bundle_name}` has no .installed-from record, so Codewhale refuses to remove it"
                ),
                bundle: Some(bundle_name),
                version,
                warnings: Vec::new(),
            });
        }
        PluginScope::User => {}
    }

    let pending = approver_from_posture(&current_authority);
    if !is_own && matches!(pending, PendingApprover::Agent(_)) {
        let reason =
            own_denial_reason.unwrap_or("this plugin was not installed and approved by this agent");
        record_outcome(
            call,
            "remove",
            Some(&bundle_name),
            Some(&initial_plugin.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: "remove",
            target: raw_target,
            changed: false,
            enabled: Some(initial_plugin.enabled),
            code: "operation-error",
            diagnostic: format!("{reason}; {ASK_PERSON_HINT}"),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }

    let gate_target = format!("{bundle_name} {}", initial_plugin.id.as_str());
    call.context
        .act("remove", BrokerEffect::Write, Some(&gate_target))
        .await?;

    if call.context.cancel().is_cancelled() {
        record_outcome(
            call,
            "remove",
            Some(&bundle_name),
            Some(&initial_plugin.content_hash),
            None,
            "cancelled",
        );
        return cancelled_result("remove", raw_target, Some(bundle_name), version, Vec::new());
    }

    let (refreshed_tool, refreshed_auth) = live(call);
    tool = refreshed_tool;
    let Some(refreshed_auth) = refreshed_auth else {
        return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
    };
    let after_pending = approver_from_posture(&refreshed_auth);
    if matches!(after_pending, PendingApprover::Nobody) {
        record_outcome(
            call,
            "remove",
            Some(&bundle_name),
            Some(&initial_plugin.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: "remove",
            target: raw_target,
            changed: false,
            enabled: Some(initial_plugin.enabled),
            code: "operation-error",
            diagnostic:
                "permission mode changed during review and no longer allows removing plugins"
                    .to_string(),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }

    let (plugin_now, still_own, reason_now, still_has_marker, canon_ws_now) =
        match load_existing_plugin(Arc::clone(&initial_registry), raw_target.clone(), None).await? {
            PreparedExisting::Missing => {
                record_outcome(call, "remove", Some(&bundle_name), None, None, "failed");
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "remove",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "unknown-plugin",
                    diagnostic: format!("plugin `{bundle_name}` is no longer installed"),
                    bundle: Some(bundle_name),
                    version,
                    warnings: Vec::new(),
                });
            }
            PreparedExisting::Unremovable(plugin_name) => {
                record_outcome(call, "remove", Some(&plugin_name), None, None, "failed");
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "remove",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "not-removable",
                    diagnostic: format!(
                        "plugin `{plugin_name}` has no valid regular .installed-from record, so Codewhale refuses to remove it"
                    ),
                    bundle: Some(plugin_name),
                    version,
                    warnings: Vec::new(),
                });
            }
            PreparedExisting::NotSoleRow(name) => {
                record_outcome(call, "remove", Some(&name), None, None, "failed");
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "remove",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "operation-error",
                    diagnostic: "Codewhale removes a plugin as a whole; use the bundle name"
                        .to_string(),
                    bundle: Some(name),
                    version,
                    warnings: Vec::new(),
                });
            }
            PreparedExisting::Ambiguous(msg) => {
                record_outcome(call, "remove", Some(&bundle_name), None, None, "failed");
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: "remove",
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "operation-error",
                    diagnostic: msg,
                    bundle: Some(bundle_name),
                    version,
                    warnings: Vec::new(),
                });
            }
            PreparedExisting::Found {
                plugin,
                is_own,
                own_denial_reason,
                has_marker,
                canon_workspace,
            } => (
                plugin,
                is_own,
                own_denial_reason,
                has_marker,
                canon_workspace,
            ),
        };

    if matches!(after_pending, PendingApprover::Agent(_)) && !still_own {
        let reason = reason_now.unwrap_or("this plugin is not owned by this agent");
        record_outcome(
            call,
            "remove",
            Some(&bundle_name),
            Some(&plugin_now.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: "remove",
            target: raw_target,
            changed: false,
            enabled: Some(plugin_now.enabled),
            code: "operation-error",
            diagnostic: format!("{reason}; {ASK_PERSON_HINT}"),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }
    if plugin_now.scope != PluginScope::User || !still_has_marker {
        record_outcome(
            call,
            "remove",
            Some(&bundle_name),
            Some(&plugin_now.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: "remove",
            target: raw_target,
            changed: false,
            enabled: Some(plugin_now.enabled),
            code: "not-removable",
            diagnostic: format!("plugin `{bundle_name}` is no longer removable"),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }
    if plugin_now.id != initial_plugin.id
        || plugin_now.install_nonce != initial_plugin.install_nonce
        || plugin_now.content_hash != initial_plugin.content_hash
    {
        record_outcome(
            call,
            "remove",
            Some(&bundle_name),
            Some(&plugin_now.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: "remove",
            target: raw_target,
            changed: false,
            enabled: Some(plugin_now.enabled),
            code: "operation-error",
            diagnostic: "changed after review".to_string(),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }

    let policy = extension_host_policy_enabled();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = crate::test_support::env_scope_ticket();
    let selector = plugin_now.id.as_str().to_string();
    let plugin_ref_copy = (*plugin_now).clone();
    let base_reg = Arc::clone(&initial_registry);
    let step_disable = tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let _scope = PolicyScope::propagate(policy);
        let fresh = base_reg.rediscover_for_workspace(base_reg.workspace());
        let mut reg = (*fresh).clone();
        let was_enabled = reg.get(&selector).is_some_and(|p| p.enabled);
        let disable_res = if was_enabled {
            reg.disable(&selector).map(|()| true)
        } else {
            Ok(false)
        };
        (reg, disable_res)
    })
    .await
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;

    let (mut reg_for_uninstall, disable_res) = step_disable;
    let turned_off = match disable_res {
        Ok(changed_flag) => changed_flag,
        Err(err) => {
            let scrub = ScrubContext {
                workspace: reg_for_uninstall.workspace(),
                canon_workspace: canon_ws_now.as_deref(),
                plugin_root: Some(&plugin_now.canonical_root),
                user_plugins_dir: reg_for_uninstall.user_plugins_dir(),
                state_path: reg_for_uninstall.state_path(),
            };
            record_outcome(
                call,
                "remove",
                Some(&bundle_name),
                Some(&plugin_now.content_hash),
                None,
                "failed",
            );
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "remove",
                target: raw_target,
                changed: false,
                enabled: Some(plugin_now.enabled),
                code: "operation-error",
                diagnostic: scrub_message(&err, &scrub),
                bundle: Some(bundle_name),
                version,
                warnings: Vec::new(),
            });
        }
    };

    #[cfg(test)]
    fire_post_disable_hook_for_test(&plugin_ref_copy);
    let _ = &plugin_ref_copy;

    let network = network_policy_for(&tool);
    let uninstall_res = execute(
        PluginMutationRequest::Uninstall {
            selector: plugin_now.id.as_str().to_string(),
        },
        &PluginMutationContext {
            network: &network,
            max_size: DEFAULT_MAX_SIZE_BYTES,
        },
        &mut reg_for_uninstall,
    )
    .await;

    let final_reg = tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let _scope = PolicyScope::propagate(policy);
        reg_for_uninstall.rediscover_for_workspace(reg_for_uninstall.workspace())
    })
    .await
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;
    super::plugins_changed(Arc::clone(&final_reg));

    match uninstall_res {
        Ok(_) => {
            let leaf = super::supervisor::plugin_data_dir(
                Path::new("~"),
                plugin_now.id.as_str(),
                &bundle_name,
            )
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| bundle_name.clone());
            let mut truncated = false;
            let safe_leaf = sanitize_inline(&leaf, MAX_NAME_BYTES, &mut truncated);
            let data_warning = format!(
                "kept plugin data folder at ~/.codewhale/extension-host/data/plugins/{safe_leaf}"
            );
            let approver_used =
                after_pending.as_approver(call, &tool, initial_registry.workspace());
            record_outcome(
                call,
                "remove",
                Some(&bundle_name),
                Some(&plugin_now.content_hash),
                approver_used.as_ref(),
                "applied",
            );
            change_ok(
                "remove",
                raw_target,
                true,
                Some(false),
                Some(bundle_name),
                version,
                vec![data_warning],
            )
        }
        Err(err) => {
            let scrub = ScrubContext {
                workspace: final_reg.workspace(),
                canon_workspace: canon_ws_now.as_deref(),
                plugin_root: Some(&plugin_now.canonical_root),
                user_plugins_dir: final_reg.user_plugins_dir(),
                state_path: final_reg.state_path(),
            };
            let code = classify_install_error(&err);
            let detail = scrub_message(&format!("{err:#}"), &scrub);
            let msg = if turned_off {
                format!("plugin was turned off, then removal failed: {detail}")
            } else {
                detail
            };
            let persisted_enabled = final_reg
                .get(plugin_now.id.as_str())
                .is_some_and(|p| p.enabled);
            record_outcome(
                call,
                "remove",
                Some(&bundle_name),
                Some(&plugin_now.content_hash),
                None,
                "failed",
            );
            change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: "remove",
                target: raw_target,
                changed: turned_off,
                enabled: Some(persisted_enabled),
                code,
                diagnostic: msg,
                bundle: Some(bundle_name),
                version,
                warnings: Vec::new(),
            })
        }
    }
}

async fn toggle(
    call: &BrokerCall,
    mut tool: ToolContext,
    authority: Option<LiveRuntimeAuthority>,
    input: ToggleInput,
    want_enabled: bool,
) -> Result<Value, BrokerRefusal> {
    let method_name = if want_enabled { "enable" } else { "disable" };
    let raw_target = validate_required_str(&input.target, "target", MAX_TARGET_BYTES)?;
    let row = validate_optional_str(input.row.as_deref(), "row", MAX_ROW_BYTES)?;
    let initial_registry = live_registry(call, &tool)?;
    let Some(current_authority) = authority else {
        return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
    };

    let (initial_plugin, is_own, own_denial_reason, _) = match load_existing_plugin(
        Arc::clone(&initial_registry),
        raw_target.clone(),
        row.clone(),
    )
    .await?
    {
        PreparedExisting::Missing | PreparedExisting::Unremovable(_) => {
            record_outcome(call, method_name, None, None, None, "failed");
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: method_name,
                target: raw_target.clone(),
                changed: false,
                enabled: None,
                code: "unknown-plugin",
                diagnostic: format!("no plugin `{raw_target}` is installed"),
                bundle: None,
                version: None,
                warnings: Vec::new(),
            });
        }
        PreparedExisting::NotSoleRow(plugin_name) => {
            record_outcome(call, method_name, Some(&plugin_name), None, None, "failed");
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: method_name,
                target: raw_target,
                changed: false,
                enabled: None,
                code: "operation-error",
                diagnostic: "Codewhale switches a plugin as a whole; use set_bundle".to_string(),
                bundle: Some(plugin_name),
                version: None,
                warnings: Vec::new(),
            });
        }
        PreparedExisting::Ambiguous(msg) => {
            record_outcome(call, method_name, None, None, None, "failed");
            return change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: method_name,
                target: raw_target,
                changed: false,
                enabled: None,
                code: "operation-error",
                diagnostic: msg,
                bundle: None,
                version: None,
                warnings: Vec::new(),
            });
        }
        PreparedExisting::Found {
            plugin,
            is_own,
            own_denial_reason,
            has_marker: _,
            canon_workspace,
        } => (plugin, is_own, own_denial_reason, canon_workspace),
    };

    let bundle_name = initial_plugin.name().to_string();
    let version = non_empty_version(&initial_plugin.manifest.plugin.version);
    if initial_plugin.scope == PluginScope::Builtin {
        record_outcome(
            call,
            method_name,
            Some(&bundle_name),
            Some(&initial_plugin.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: method_name,
            target: raw_target,
            changed: false,
            enabled: Some(initial_plugin.enabled),
            code: "management-required",
            diagnostic: format!(
                "built-in plugin `{bundle_name}` ships with Codewhale and cannot be switched here"
            ),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }

    let pending = approver_from_posture(&current_authority);
    if matches!(pending, PendingApprover::Agent(_)) && !has_agent_call_identity(call, &tool) {
        record_outcome(
            call,
            method_name,
            Some(&bundle_name),
            Some(&initial_plugin.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: method_name,
            target: raw_target,
            changed: false,
            enabled: Some(initial_plugin.enabled),
            code: "operation-error",
            diagnostic:
                "An approval by the agent must name the thread, turn and tool call that made it"
                    .to_string(),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }
    if !is_own && matches!(pending, PendingApprover::Agent(_)) {
        let reason =
            own_denial_reason.unwrap_or("this plugin was not installed and approved by this agent");
        record_outcome(
            call,
            method_name,
            Some(&bundle_name),
            Some(&initial_plugin.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: method_name,
            target: raw_target,
            changed: false,
            enabled: Some(initial_plugin.enabled),
            code: "operation-error",
            diagnostic: format!("{reason}; {ASK_PERSON_HINT}"),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }

    let gate_target = if want_enabled {
        format!(
            "{bundle_name} {} {}",
            initial_plugin.content_hash,
            initial_plugin.id.as_str()
        )
    } else {
        format!("{bundle_name} {}", initial_plugin.id.as_str())
    };
    call.context
        .act(method_name, BrokerEffect::Write, Some(&gate_target))
        .await?;

    if call.context.cancel().is_cancelled() {
        record_outcome(
            call,
            method_name,
            Some(&bundle_name),
            Some(&initial_plugin.content_hash),
            None,
            "cancelled",
        );
        return cancelled_result(
            method_name,
            raw_target,
            Some(bundle_name),
            version,
            Vec::new(),
        );
    }

    let (refreshed_tool, refreshed_auth) = live(call);
    tool = refreshed_tool;
    let Some(refreshed_auth) = refreshed_auth else {
        return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
    };
    let after_pending = approver_from_posture(&refreshed_auth);
    if matches!(after_pending, PendingApprover::Nobody) {
        record_outcome(
            call,
            method_name,
            Some(&bundle_name),
            Some(&initial_plugin.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: method_name,
            target: raw_target,
            changed: false,
            enabled: Some(initial_plugin.enabled),
            code: "operation-error",
            diagnostic:
                "permission mode changed during review and no longer allows changing plugin state"
                    .to_string(),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }

    let (plugin_now, still_own, reason_now, canon_ws_now) =
        match load_existing_plugin(Arc::clone(&initial_registry), raw_target.clone(), row).await? {
            PreparedExisting::Missing | PreparedExisting::Unremovable(_) => {
                record_outcome(call, method_name, Some(&bundle_name), None, None, "failed");
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: method_name,
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "unknown-plugin",
                    diagnostic: format!("plugin `{bundle_name}` is no longer installed"),
                    bundle: Some(bundle_name),
                    version,
                    warnings: Vec::new(),
                });
            }
            PreparedExisting::NotSoleRow(name) => {
                record_outcome(call, method_name, Some(&name), None, None, "failed");
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: method_name,
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "operation-error",
                    diagnostic: "Codewhale switches a plugin as a whole; use set_bundle"
                        .to_string(),
                    bundle: Some(name),
                    version,
                    warnings: Vec::new(),
                });
            }
            PreparedExisting::Ambiguous(msg) => {
                record_outcome(call, method_name, Some(&bundle_name), None, None, "failed");
                return change_with_outcome(ChangeOutcome {
                    application: "failed",
                    stage: method_name,
                    target: raw_target,
                    changed: false,
                    enabled: None,
                    code: "operation-error",
                    diagnostic: msg,
                    bundle: Some(bundle_name),
                    version,
                    warnings: Vec::new(),
                });
            }
            PreparedExisting::Found {
                plugin,
                is_own,
                own_denial_reason,
                has_marker: _,
                canon_workspace,
            } => (plugin, is_own, own_denial_reason, canon_workspace),
        };

    if matches!(after_pending, PendingApprover::Agent(_)) && !has_agent_call_identity(call, &tool) {
        record_outcome(
            call,
            method_name,
            Some(&bundle_name),
            Some(&plugin_now.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: method_name,
            target: raw_target,
            changed: false,
            enabled: Some(plugin_now.enabled),
            code: "operation-error",
            diagnostic:
                "An approval by the agent must name the thread, turn and tool call that made it"
                    .to_string(),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }
    if matches!(after_pending, PendingApprover::Agent(_)) && !still_own {
        let reason = reason_now.unwrap_or("this plugin is not owned by this agent");
        record_outcome(
            call,
            method_name,
            Some(&bundle_name),
            Some(&plugin_now.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: method_name,
            target: raw_target,
            changed: false,
            enabled: Some(plugin_now.enabled),
            code: "operation-error",
            diagnostic: format!("{reason}; {ASK_PERSON_HINT}"),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }
    if plugin_now.id != initial_plugin.id
        || plugin_now.install_nonce != initial_plugin.install_nonce
        || plugin_now.content_hash != initial_plugin.content_hash
        || plugin_now.capability_hash != initial_plugin.capability_hash
    {
        record_outcome(
            call,
            method_name,
            Some(&bundle_name),
            Some(&plugin_now.content_hash),
            None,
            "failed",
        );
        return change_with_outcome(ChangeOutcome {
            application: "failed",
            stage: method_name,
            target: raw_target,
            changed: false,
            enabled: Some(plugin_now.enabled),
            code: "operation-error",
            diagnostic: "changed after review".to_string(),
            bundle: Some(bundle_name),
            version,
            warnings: Vec::new(),
        });
    }

    let policy = extension_host_policy_enabled();
    #[cfg(any(test, feature = "test-support"))]
    let env_scope = crate::test_support::env_scope_ticket();
    let Some(approver) = after_pending.as_approver(call, &tool, initial_registry.workspace())
    else {
        return Err(BrokerRefusal::NotAvailable(DshService::Plugins));
    };
    let approver_for_commit = approver.clone();
    let selector = plugin_now.id.as_str().to_string();
    let was_enabled = plugin_now.enabled;
    let was_trusted = plugin_now.trusted();
    let base_reg = Arc::clone(&initial_registry);
    let canon_ws_clone = canon_ws_now.clone();

    enum ToggleOutcome {
        Failed {
            registry: Arc<PluginRegistry>,
            changed: bool,
            enabled: bool,
            err: String,
        },
        Overridden {
            registry: Arc<PluginRegistry>,
            changed: bool,
            enabled: bool,
            err: String,
        },
        Done {
            registry: Arc<PluginRegistry>,
            loaded: Box<LoadedPlugin>,
            changed: bool,
            warnings: Vec<String>,
        },
    }

    let outcome = tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _env_scope = crate::test_support::join_env_scope(env_scope);
        let _scope = PolicyScope::propagate(policy);
        let fresh = base_reg.rediscover_for_workspace(base_reg.workspace());
        let mut reg = (*fresh).clone();
        let needs_person_upgrade = matches!(approver_for_commit, PluginApprover::Person { .. })
            && matches!(
                fresh.get(&selector).and_then(|p| p.approved_by.as_ref()),
                Some(PluginApprover::Agent(_))
            );
        let changed = if want_enabled {
            let commit_res = if !was_trusted || needs_person_upgrade {
                let host_services = match &approver_for_commit {
                    PluginApprover::Agent(_) => reg
                        .host_service_review_for(&selector, &approver_for_commit)
                        .map(|review| review.token),
                    PluginApprover::Person { .. } | PluginApprover::CommandLine => None,
                };
                reg.trust_and_enable_by(
                    &selector,
                    ReviewApproval {
                        approved_by: approver_for_commit.clone(),
                        host_services,
                    },
                    approver_for_commit.clone(),
                )
            } else {
                reg.enable_by(&selector, approver_for_commit.clone())
            };
            if let Err(err) = commit_res {
                let latest = reg.rediscover_for_workspace(reg.workspace());
                let actual_enabled = latest.get(&selector).is_some_and(|p| p.enabled);
                if is_managed_policy_error(&err) {
                    return ToggleOutcome::Overridden {
                        registry: latest,
                        changed: false,
                        enabled: actual_enabled,
                        err,
                    };
                }
                return ToggleOutcome::Failed {
                    registry: latest,
                    changed: false,
                    enabled: actual_enabled,
                    err,
                };
            }
            !was_enabled || !was_trusted || needs_person_upgrade
        } else {
            if let Err(err) = reg.disable(&selector) {
                let latest = reg.rediscover_for_workspace(reg.workspace());
                let actual_enabled = latest.get(&selector).is_some_and(|p| p.enabled);
                return ToggleOutcome::Failed {
                    registry: latest,
                    changed: false,
                    enabled: actual_enabled,
                    err,
                };
            }
            was_enabled
        };
        let final_reg = reg.rediscover_for_workspace(reg.workspace());
        let Some(final_loaded) = final_reg.get(&selector).cloned() else {
            return ToggleOutcome::Failed {
                registry: final_reg,
                changed,
                enabled: false,
                err: "plugin vanished while updating state".to_string(),
            };
        };
        let scrub = ScrubContext {
            workspace: final_reg.workspace(),
            canon_workspace: canon_ws_clone.as_deref(),
            plugin_root: Some(&final_loaded.canonical_root),
            user_plugins_dir: final_reg.user_plugins_dir(),
            state_path: final_reg.state_path(),
        };
        let warnings = if want_enabled {
            let effective_approver = final_loaded
                .approved_by
                .as_ref()
                .unwrap_or(&approver_for_commit);
            collect_enable_warnings(
                &final_reg,
                &final_loaded,
                effective_approver,
                final_loaded.enabled,
                false,
                &scrub,
            )
        } else {
            vec![approver_label(final_loaded.approved_by.as_ref()).to_string()]
        };
        ToggleOutcome::Done {
            registry: final_reg,
            loaded: Box::new(final_loaded),
            changed,
            warnings,
        }
    })
    .await
    .map_err(|_| BrokerRefusal::NotAvailable(DshService::Plugins))?;

    match outcome {
        ToggleOutcome::Overridden {
            registry: fresh,
            changed,
            enabled,
            err,
        } => {
            let scrub = ScrubContext {
                workspace: fresh.workspace(),
                canon_workspace: canon_ws_now.as_deref(),
                plugin_root: Some(&plugin_now.canonical_root),
                user_plugins_dir: fresh.user_plugins_dir(),
                state_path: fresh.state_path(),
            };
            let diag = scrub_message(&err, &scrub);
            super::plugins_changed(fresh);
            record_outcome(
                call,
                method_name,
                Some(&bundle_name),
                Some(&plugin_now.content_hash),
                Some(&approver),
                "overridden",
            );
            change_with_outcome(ChangeOutcome {
                application: "overridden",
                stage: method_name,
                target: raw_target,
                changed,
                enabled: Some(enabled),
                code: "operation-error",
                diagnostic: diag,
                bundle: Some(bundle_name),
                version,
                warnings: Vec::new(),
            })
        }
        ToggleOutcome::Failed {
            registry: fresh,
            changed,
            enabled,
            err,
        } => {
            let scrub = ScrubContext {
                workspace: fresh.workspace(),
                canon_workspace: canon_ws_now.as_deref(),
                plugin_root: Some(&plugin_now.canonical_root),
                user_plugins_dir: fresh.user_plugins_dir(),
                state_path: fresh.state_path(),
            };
            let diag = scrub_message(&err, &scrub);
            super::plugins_changed(fresh);
            record_outcome(
                call,
                method_name,
                Some(&bundle_name),
                Some(&plugin_now.content_hash),
                Some(&approver),
                "failed",
            );
            change_with_outcome(ChangeOutcome {
                application: "failed",
                stage: method_name,
                target: raw_target,
                changed,
                enabled: Some(enabled),
                code: "operation-error",
                diagnostic: diag,
                bundle: Some(bundle_name),
                version,
                warnings: Vec::new(),
            })
        }
        ToggleOutcome::Done {
            registry: final_reg,
            loaded: final_loaded,
            changed,
            warnings,
        } => {
            super::plugins_changed(Arc::clone(&final_reg));
            let scrub = ScrubContext {
                workspace: final_reg.workspace(),
                canon_workspace: canon_ws_now.as_deref(),
                plugin_root: Some(&final_loaded.canonical_root),
                user_plugins_dir: final_reg.user_plugins_dir(),
                state_path: final_reg.state_path(),
            };
            let warnings = if want_enabled && final_loaded.enabled {
                match finish_enabled_activation(
                    call,
                    EnableActivation {
                        method: method_name,
                        stage: method_name,
                        target: raw_target.clone(),
                        changed,
                        registry: &final_reg,
                        loaded: &final_loaded,
                        approver: &approver,
                        version: version.clone(),
                        warnings,
                    },
                    &scrub,
                )
                .await
                {
                    std::ops::ControlFlow::Continue(updated) => updated,
                    std::ops::ControlFlow::Break(early) => return early,
                }
            } else {
                warnings
            };
            record_outcome(
                call,
                method_name,
                Some(final_loaded.name()),
                Some(&final_loaded.content_hash),
                Some(&approver),
                "applied",
            );
            change_ok(
                method_name,
                raw_target,
                changed,
                Some(final_loaded.enabled),
                Some(final_loaded.name().to_string()),
                non_empty_version(&final_loaded.manifest.plugin.version).or(version),
                warnings,
            )
        }
    }
}

fn live(call: &BrokerCall) -> (ToolContext, Option<LiveRuntimeAuthority>) {
    let mut tool = call.context.tool().clone();
    let authority = tool.refresh_live_posture();
    (tool, authority)
}

fn refused() -> BrokerRefusal {
    BrokerRefusal::Refused {
        service: DshService::Plugins,
        message: "the plugins service is not available to this caller".to_string(),
    }
}

#[cfg(test)]
#[path = "dsh_plugins_tests.rs"]
mod tests;

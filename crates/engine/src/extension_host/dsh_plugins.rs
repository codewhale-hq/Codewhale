use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use codewhale_config::AppMode;
use codewhale_execpolicy::ApprovalMode;
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::activation::extension_host_policy_enabled;
use codewhale_plugins::host_services::{
    AgentPosture, PersonVia, PluginApprover, approver_label, offered,
};
use codewhale_plugins::types::{LoadedPlugin, PluginDiagnosticLevel, PluginScope};
use codewhale_runtime::runtime_policy::approval_wire;
use codewhale_sanitize::redact::redact_model_bound_secrets;
use codewhale_sanitize::sanitize::{is_bidi_control, sanitize_text, strip_ansi_into};
use codewhale_skill_install::install::INSTALLED_FROM_MARKER;
use codewhale_util::path_identity::metadata_is_link_or_reparse;
use codewhale_util::utils::truncate_with_ellipsis;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::dsh_broker::{
    BrokerCall, BrokerEffect, BrokerFuture, BrokerMethods, BrokerRefusal, builtin_services, decode,
};
use super::protocol::DshService;
use super::registry::OwnerState;
use crate::core::engine::LiveRuntimeAuthority;
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
const HOST_OFF_DIAGNOSTIC: &str =
    "the extension host is switched off in this process, so native plugin code did not start";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoInput {}

#[derive(Deserialize)]
#[serde(tag = "method", content = "input", deny_unknown_fields)]
enum PluginsMethod {
    #[serde(rename = "list")]
    List(NoInput),
    #[serde(rename = "status")]
    Status(NoInput),
    #[serde(rename = "posture")]
    Posture(NoInput),
    #[serde(rename = "install")]
    Install(Map<String, Value>),
    #[serde(rename = "remove")]
    Remove(Map<String, Value>),
    #[serde(rename = "enable")]
    Enable(Map<String, Value>),
    #[serde(rename = "disable")]
    Disable(Map<String, Value>),
}

impl BrokerMethods for PluginsMethod {
    const NAMES: &'static [&'static str] = &[
        "list", "status", "posture", "install", "remove", "enable", "disable",
    ];
}

impl PluginsMethod {
    fn name(&self) -> &'static str {
        match self {
            Self::List(_) => "list",
            Self::Status(_) => "status",
            Self::Posture(_) => "posture",
            Self::Install(_) => "install",
            Self::Remove(_) => "remove",
            Self::Enable(_) => "enable",
            Self::Disable(_) => "disable",
        }
    }
}

pub(super) fn serve(call: BrokerCall) -> BrokerFuture {
    Box::pin(async move {
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
            ref other @ (PluginsMethod::Install(ref input)
            | PluginsMethod::Remove(ref input)
            | PluginsMethod::Enable(ref input)
            | PluginsMethod::Disable(ref input)) => {
                if input.len() > MAX_INPUT_FIELDS {
                    return Err(BrokerRefusal::InvalidInput {
                        service: DshService::Plugins,
                        message: format!(
                            "a plugins {} input has at most {MAX_INPUT_FIELDS} fields",
                            other.name()
                        ),
                    });
                }
                Err(BrokerRefusal::NotAvailable(DshService::Plugins))
            }
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
    let marker = canonical_root.join(INSTALLED_FROM_MARKER);
    match tokio::fs::symlink_metadata(&marker).await {
        Ok(meta) => !metadata_is_link_or_reparse(&meta) && meta.file_type().is_file(),
        Err(_) => false,
    }
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

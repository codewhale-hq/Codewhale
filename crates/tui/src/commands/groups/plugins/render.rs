//! Presentation for `/plugin`: bundle detail, the capability review body,
//! and diagnostics.
//!
//! Everything here is a pure portable transform — no registry mutation, no
//! disk access. [`escape_review_text`] is the security-relevant part:
//! manifest fields are attacker-controlled, so they are escaped before they
//! reach a review the user is about to approve.
//!
//! FEAT-020: render helpers consume portable `PluginDetail` values and the
//! presentation facet; the concrete `LoadedPlugin` never crosses the
//! boundary.

use std::fmt::Write as _;
use std::path::Path;

use codewhale_command_contract::facets::{
    CommandPresentationContext, PluginDetail, PluginDiagnostic, PluginDiagnosticLevel,
    PluginMcpServerDetail,
};

use super::append_diagnostics;

pub(super) fn render_bundle_detail(
    presentation: &mut dyn CommandPresentationContext,
    detail: &PluginDetail,
    include_hashes: bool,
    context: ReviewTextContext,
) -> String {
    let unsupported = if detail.unsupported_labels.is_empty() {
        "none".to_string()
    } else {
        display_component_labels(&detail.unsupported_labels)
    };
    let active_components = if detail.active {
        let labels = &detail.supported_labels;
        if labels.is_empty() {
            "none".to_string()
        } else {
            display_component_labels(labels)
        }
    } else {
        "none".to_string()
    };
    let extension_host = codewhale_plugins::activation::extension_host_policy_enabled();
    let (content_hash, capability_hash) = if include_hashes {
        (
            detail.content_hash.as_str(),
            detail.capability_hash.as_str(),
        )
    } else {
        ("hidden", "hidden")
    };
    let mut output = presentation
        .translate(
            "cmd_plugin_bundle_detail",
            &[
                ("name", &escape_review_text(&detail.name, context)),
                ("id", &escape_review_text(&detail.id, context)),
                ("version", &escape_review_text(&detail.version, context)),
                ("origin", &detail.origin),
                ("scope", &detail.scope),
                ("state", &detail.state_label),
                (
                    "trust",
                    detail
                        .trust_display
                        .as_deref()
                        .unwrap_or(&detail.trust_status),
                ),
                ("inventory", &detail.inventory_summary),
                ("permissions", &render_permissions(detail, context)),
                ("mcp", &render_mcp_inventory(detail, context)),
                ("unsupported", &unsupported),
                ("content_hash", content_hash),
                ("capability_hash", capability_hash),
                ("path", &escape_review_path(&detail.canonical_root, context)),
            ],
        )
        .unwrap_or_default();
    let skills = detail
        .skills
        .iter()
        .map(|skill| escape_review_text(skill, context))
        .collect::<Vec<_>>();
    let commands = detail
        .commands
        .iter()
        .map(|command| escape_review_text(command, context))
        .collect::<Vec<_>>();
    let profiles = detail
        .profiles
        .iter()
        .map(|profile| escape_review_text(profile, context))
        .collect::<Vec<_>>();
    let _ = write!(
        output,
        "\nCompatibility: {}\nActive components: [{active_components}]\nInactive components: [{unsupported}]\nQualified skills: [{}]\nCommands: [{}]\nProfiles: [{}]\nActivation boundary: trust stages the exact reviewed content but does not activate it; enable rebuilds this workspace's Skills, MCP, Commands, Agents, and Hooks immediately. Every plugin command dispatch, Agent spawn, Hook process start, Skill use, and MCP call rechecks current authority. {}",
        detail.compatibility,
        if skills.is_empty() {
            "none".to_string()
        } else {
            skills.join(", ")
        },
        if commands.is_empty() {
            "none".to_string()
        } else {
            commands.join(", ")
        },
        if profiles.is_empty() {
            "none".to_string()
        } else {
            profiles.join(", ")
        },
        if extension_host {
            "Native host code (JavaScript) runs in the experimental extension host with your user permissions (sandboxed where the OS sandbox is available); extension tools are never auto-approved by the plugin itself, and each call needs approval under your approval mode. LSP, filesystem-roots, and lifecycle-mutation stay inventoried and inactive."
        } else {
            "LSP, native, filesystem-roots, and lifecycle-mutation stay inventoried and inactive."
        }
    );
    if let Some(approver) = detail.approver.as_deref() {
        if let Some(date) = detail.reviewed_date.as_deref() {
            let safe_date = escape_review_text(date, ReviewTextContext::Plain);
            let approved_date = presentation
                .translate("cmd_plugin_audit_approved_date", &[("date", &safe_date)])
                .unwrap_or_else(|_| format!("approved {safe_date}"));
            let _ = write!(output, "\nApproval: {approver} ({approved_date})");
        } else {
            let _ = write!(output, "\nApproval: {approver}");
        }
    }
    if let Some(thread_id) = detail.agent_thread_id.as_deref() {
        let _ = write!(
            output,
            "\nApproval thread: {}",
            escape_review_text(thread_id, context)
        );
    }
    if let Some(enabled_by) = detail.enabled_by.as_deref() {
        let _ = write!(output, "\nTurned on by: {enabled_by}");
    }
    if detail.outside_reach
        || detail.reach_folder.is_some()
        || detail.agent_workspace_path.is_some()
    {
        let folder =
            escape_review_text(detail.reach_folder.as_deref().unwrap_or("unknown"), context);
        let key = if detail.outside_reach {
            "cmd_plugin_audit_reach_only_in"
        } else {
            "cmd_plugin_audit_reach_here"
        };
        let reach = presentation
            .translate(key, &[("folder", &folder)])
            .unwrap_or(folder);
        if let Some(workspace_path) = detail.agent_workspace_path.as_deref() {
            let _ = write!(
                output,
                "\nReach: {reach} — {}",
                escape_review_text(workspace_path, context)
            );
        } else {
            let _ = write!(output, "\nReach: {reach}");
        }
    }
    if let Some(note) = detail.process_note.as_deref() {
        let _ = write!(output, "\nProcess: {note}");
    }
    if !detail.host_services.is_empty() {
        output.push_str("\nHost services:");
        for service in &detail.host_services {
            let line = if !service.withheld_by.is_empty() && service.can_use {
                let peer_context = if context == ReviewTextContext::Prose {
                    ReviewTextContext::InlineCode
                } else {
                    context
                };
                let peers = service
                    .withheld_by
                    .iter()
                    .map(|peer| format!("`{}`", escape_review_text(peer, peer_context)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let key = if service.withheld_by.len() == 1 {
                    "cmd_plugin_audit_service_withheld_one"
                } else {
                    "cmd_plugin_audit_service_withheld_many"
                };
                presentation
                    .translate(key, &[("service", &service.service), ("peers", &peers)])
                    .unwrap_or_else(|_| format!("{}: off for now: {peers}", service.service))
            } else if service.can_use {
                presentation
                    .translate(
                        "cmd_plugin_audit_service_can_use",
                        &[("service", &service.service)],
                    )
                    .unwrap_or_else(|_| format!("{}: can use", service.service))
            } else {
                presentation
                    .translate(
                        "cmd_plugin_audit_service_not_offered",
                        &[("service", &service.service)],
                    )
                    .unwrap_or_else(|_| {
                        format!("{}: this version does not offer it", service.service)
                    })
            };
            let _ = write!(output, "\n  - {line}");
        }
    }
    if !detail.earlier_reviews.is_empty() {
        output.push_str("\nEarlier reviews:");
        for entry in &detail.earlier_reviews {
            let safe_date = escape_review_text(&entry.reviewed_at, ReviewTextContext::Plain);
            let _ = write!(output, "\n  - {safe_date}: {}", entry.approver);
        }
    }
    append_diagnostics(presentation, &mut output, &detail.diagnostics, context);
    output
}

/// Component labels for review text. With the experimental extension host
/// on, `native` is shown by what it is; with it off the text is unchanged.
/// The hashed label itself (`PluginActivationCapability::as_str`) never
/// changes.
fn display_component_labels(labels: &[String]) -> String {
    let extension_host = codewhale_plugins::activation::extension_host_policy_enabled();
    labels
        .iter()
        .map(|label| match label.as_str() {
            "native" if extension_host => "native (host code: JavaScript)",
            other => other,
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_permissions(detail: &PluginDetail, context: ReviewTextContext) -> String {
    let filesystem = if detail.filesystem_roots.is_empty() {
        "none".to_string()
    } else {
        detail
            .filesystem_roots
            .iter()
            .map(|value| escape_review_text(value, context))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let network = if detail.network_hosts.is_empty() {
        "none".to_string()
    } else {
        detail
            .network_hosts
            .iter()
            .map(|value| escape_review_text(value, context))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let stdio_authority = if detail.stdio_mcp_servers == 0 {
        "none".to_string()
    } else {
        format!(
            "{} local child process(es) with host-user filesystem/network authority; MCP tool approvals still apply",
            detail.stdio_mcp_servers
        )
    };
    format!(
        "filesystem_roots=[{filesystem}] network_hosts=[{network}] (exact allowlist for Codewhale-managed remote requests; redirects stay same-origin) lifecycle_mutation={} stdio_runtime=[{stdio_authority}]",
        detail.lifecycle_mutation
    )
}

fn render_mcp_inventory(detail: &PluginDetail, context: ReviewTextContext) -> String {
    if detail.mcp_servers.is_empty() {
        return "none".to_string();
    }
    detail
        .mcp_servers
        .iter()
        .map(|server| render_mcp_server(server, context))
        .collect::<Vec<_>>()
        .join("; ")
}

fn render_mcp_server(server: &PluginMcpServerDetail, context: ReviewTextContext) -> String {
    let enabled = if server.enabled {
        "configured-on"
    } else {
        "configured-off"
    };
    if let Some(command) = server.command.as_deref() {
        let mut env_provenance = server
            .env
            .iter()
            .map(|(destination, source)| {
                let source = source
                    .strip_prefix("${")
                    .and_then(|source| source.strip_suffix('}'))
                    .unwrap_or("invalid");
                format!(
                    "{} <- {}",
                    escape_review_text(destination, context),
                    escape_review_text(source, context)
                )
            })
            .collect::<Vec<_>>();
        env_provenance.sort_unstable();
        let cwd = server
            .cwd
            .as_deref()
            .map(|value| escape_review_path(value, context))
            .unwrap_or_else(|| "plugin-root".to_string());
        let argv = render_review_argv(&server.argv, context);
        format!(
            "{}: transport=stdio command={} argv=[{}] cwd={cwd} env=[{}] timeouts={} required={} enabled_tools=[{}] disabled_tools=[{}] host-user-filesystem/network-authority {enabled}",
            escape_review_text(&server.name, context),
            escape_review_text(command, context),
            argv.join(", "),
            if env_provenance.is_empty() {
                "none".to_string()
            } else {
                env_provenance.join(", ")
            },
            render_mcp_timeouts(server),
            server.required,
            render_review_values(&server.enabled_tools, context),
            render_review_values(&server.disabled_tools, context),
        )
    } else if let Some(url) = server.url.as_deref() {
        let endpoint = reqwest::Url::parse(url)
            .ok()
            .map(|url| escape_review_text(url.as_str(), context))
            .unwrap_or_else(|| "invalid-url".to_string());
        let mut env_headers = server
            .env_headers
            .iter()
            .map(|(header, source)| {
                format!(
                    "{} <- {}",
                    escape_review_text(header, context),
                    escape_review_text(source, context)
                )
            })
            .collect::<Vec<_>>();
        env_headers.sort_unstable();
        let bearer = server
            .bearer_token_env_var
            .as_deref()
            .map(|value| escape_review_text(value, context))
            .unwrap_or_else(|| "none".to_string());
        let transport = transport_label(&server.transport);
        format!(
            "{}: transport={} endpoint={} redirects=same-origin-only env_headers=[{}] bearer_env={} oauth=disabled timeouts={} required={} enabled_tools=[{}] disabled_tools=[{}] {enabled}",
            escape_review_text(&server.name, context),
            escape_review_text(transport, context),
            endpoint,
            if env_headers.is_empty() {
                "none".to_string()
            } else {
                env_headers.join(", ")
            },
            bearer,
            render_mcp_timeouts(server),
            server.required,
            render_review_values(&server.enabled_tools, context),
            render_review_values(&server.disabled_tools, context),
        )
    } else {
        format!("{}: invalid", escape_review_text(&server.name, context))
    }
}

fn transport_label(
    transport: &codewhale_command_contract::facets::PluginMcpTransport,
) -> &'static str {
    match transport {
        codewhale_command_contract::facets::PluginMcpTransport::Stdio => "stdio",
        codewhale_command_contract::facets::PluginMcpTransport::Http => "http",
        codewhale_command_contract::facets::PluginMcpTransport::Invalid => "invalid",
    }
}

fn render_review_argv(arguments: &[String], context: ReviewTextContext) -> Vec<String> {
    arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            let position = index + 1;
            format!(
                "#{position} value={}",
                render_review_argv_value(argument, context)
            )
        })
        .collect()
}

fn render_review_argv_value(value: &str, context: ReviewTextContext) -> String {
    // JSON string syntax is a lossless, unambiguous terminal representation:
    // whitespace, quotes, backslashes, and punctuation retain their exact
    // argv semantics without hiding arbitrary values behind redaction.
    let rendered = serde_json::to_string(value).expect("serializing a Rust string cannot fail");
    if context != ReviewTextContext::Prose {
        return rendered;
    }
    let mut escaped = String::with_capacity(rendered.len());
    let mut chars = rendered.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some(next @ ('\\' | '"')) => {
                    let _ = write!(escaped, "\\u{:04x}", next as u32);
                }
                Some(next) => {
                    escaped.push('\\');
                    escaped.push(next);
                }
                None => escaped.push(ch),
            }
        } else if "&`*_[](){}#+-.!|~<>:".contains(ch)
            || codewhale_sanitize::sanitize::is_bidi_control(ch)
        {
            let _ = write!(escaped, "\\u{:04x}", ch as u32);
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

fn render_review_values(values: &[String], context: ReviewTextContext) -> String {
    if values.is_empty() {
        return "none".to_string();
    }
    values
        .iter()
        .map(|value| escape_review_text(value, context))
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_mcp_timeouts(server: &PluginMcpServerDetail) -> String {
    format!(
        "connect={}/execute={}/read={}",
        server
            .connect_timeout_secs
            .map_or_else(|| "default".to_string(), |value| format!("{value}s")),
        server
            .execute_timeout_secs
            .map_or_else(|| "default".to_string(), |value| format!("{value}s")),
        server
            .read_timeout_secs
            .map_or_else(|| "default".to_string(), |value| format!("{value}s")),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewTextContext {
    Prose,
    InlineCode,
    Plain,
}

pub(crate) fn escape_review_path(path: &Path, context: ReviewTextContext) -> String {
    escape_review_text(&path.to_string_lossy(), context)
}

pub(crate) fn escape_review_text(value: &str, context: ReviewTextContext) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control()
            || codewhale_sanitize::sanitize::is_bidi_control(ch)
            || (context == ReviewTextContext::InlineCode && ch == '`')
        {
            let _ = write!(escaped, "\\u{{{:x}}}", ch as u32);
        } else if context == ReviewTextContext::Prose && ch == '&' {
            escaped.push_str("&amp;");
        } else if context == ReviewTextContext::Prose
            && matches!(
                ch,
                '\\' | '`'
                    | '*'
                    | '_'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '<'
                    | '>'
                    | '('
                    | ')'
                    | '#'
                    | '+'
                    | '-'
                    | '.'
                    | '!'
                    | '|'
            )
        {
            escaped.push('\\');
            escaped.push(ch);
        } else {
            escaped.push(ch);
        }
    }
    escaped
}

fn _diagnostic_level_label(level: PluginDiagnosticLevel) -> &'static str {
    match level {
        PluginDiagnosticLevel::Warning => "warning",
        PluginDiagnosticLevel::Error => "error",
    }
}

fn _diagnostic_path(diagnostic: &PluginDiagnostic) -> Option<String> {
    diagnostic
        .path
        .as_ref()
        .map(|path| path.display().to_string())
}

/// `/plugin doctor` output: what is superseded, what was kept and why, and
/// the state-file effect. Every dynamic string is escaped; paths and ids come
/// from the filesystem and from plugin state, which a bundle can influence.
pub(super) fn render_gc_report(
    report: &codewhale_plugins::registry::gc::GcReport,
    fixed: bool,
) -> String {
    use codewhale_plugins::registry::gc::format_bytes;

    const SHOWN: usize = 40;
    let mut out = String::new();
    let title = if fixed {
        "Plugin doctor: applied"
    } else {
        "Plugin doctor: report (nothing was changed)"
    };
    let _ = writeln!(out, "{title}\n");
    if report.items.is_empty() {
        let _ = writeln!(
            out,
            "Nothing to retire. {} state records, {}.",
            report.records_before,
            format_bytes(report.state_bytes_before)
        );
    } else {
        let verb = if fixed { "Retired" } else { "Would retire" };
        let records = report
            .items
            .iter()
            .filter(|item| item.kind == codewhale_plugins::registry::gc::GcKind::Record)
            .count();
        let _ = writeln!(
            out,
            "{verb} {records} records and {} directories ({} on disk). state.json: {} -> {} records, {} -> {}.\n",
            report.items.len() - records,
            format_bytes(report.reclaimable_bytes()),
            report.records_before,
            report.records_after,
            format_bytes(report.state_bytes_before),
            format_bytes(report.state_bytes_after),
        );
        for item in report.items.iter().take(SHOWN) {
            let gate = if item.explicit && !fixed {
                " [--fix only]"
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "  {}{gate} {}: {}",
                item.kind.label(),
                escape_review_text(&item.target, ReviewTextContext::Prose),
                escape_review_text(&item.reason, ReviewTextContext::Prose)
            );
        }
        if report.items.len() > SHOWN {
            let _ = writeln!(out, "  ... and {} more", report.items.len() - SHOWN);
        }
    }
    if !report.kept.is_empty() {
        let _ = writeln!(out, "\nKept ({}):", report.kept.len());
        for line in report.kept.iter().take(10) {
            let _ = writeln!(
                out,
                "  {}",
                escape_review_text(line, ReviewTextContext::Prose)
            );
        }
        if report.kept.len() > 10 {
            let _ = writeln!(out, "  ... and {} more", report.kept.len() - 10);
        }
    }
    for note in &report.notes {
        let _ = writeln!(
            out,
            "\nNote: {}",
            escape_review_text(note, ReviewTextContext::Prose)
        );
    }
    for failure in &report.failures {
        let _ = writeln!(
            out,
            "\nCould not remove {}",
            escape_review_text(failure, ReviewTextContext::Prose)
        );
    }
    if fixed {
        if !report.items.is_empty() {
            out.push_str("\nThe previous state.json is kept as state.json.pre-gc.");
        }
    } else if !report.items.is_empty() {
        out.push_str(
            "\nRun /plugin doctor --fix to apply. Items not marked [--fix only] are also retired automatically at startup.",
        );
    }
    out
}

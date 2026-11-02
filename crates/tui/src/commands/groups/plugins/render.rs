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
                ("name", &escape_review_text(&detail.name)),
                ("id", &escape_review_text(&detail.id)),
                ("version", &escape_review_text(&detail.version)),
                ("origin", &detail.origin),
                ("scope", &detail.scope),
                ("state", &detail.state_label),
                ("trust", &detail.trust_status),
                ("inventory", &detail.inventory_summary),
                ("permissions", &render_permissions(detail)),
                ("mcp", &render_mcp_inventory(detail)),
                ("unsupported", &unsupported),
                ("content_hash", content_hash),
                ("capability_hash", capability_hash),
                ("path", &escape_review_path(&detail.canonical_root)),
            ],
        )
        .unwrap_or_default();
    let skills = detail
        .skills
        .iter()
        .map(|skill| escape_review_text(skill))
        .collect::<Vec<_>>();
    let _ = write!(
        output,
        "\nCompatibility: {}\nActive components: [{active_components}]\nInactive components: [{unsupported}]\nQualified skills: [{}]\nActivation boundary: trust stages the exact reviewed content but does not activate it; enable rebuilds this workspace's Skills, MCP, Commands, Agents, and Hooks immediately. Every plugin command dispatch, Agent spawn, Hook process start, Skill use, and MCP call rechecks current authority. {}",
        detail.compatibility,
        if skills.is_empty() {
            "none".to_string()
        } else {
            skills.join(", ")
        },
        if extension_host {
            "Native host code (JavaScript) runs in the experimental extension host with your user permissions (sandboxed where the OS sandbox is available); extension tools are never auto-approved by the plugin itself, and each call needs approval under your approval mode. LSP, filesystem-roots, and lifecycle-mutation stay inventoried and inactive."
        } else {
            "LSP, native, filesystem-roots, and lifecycle-mutation stay inventoried and inactive."
        }
    );
    append_diagnostics(presentation, &mut output, &detail.diagnostics);
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

fn render_permissions(detail: &PluginDetail) -> String {
    let filesystem = if detail.filesystem_roots.is_empty() {
        "none".to_string()
    } else {
        detail
            .filesystem_roots
            .iter()
            .map(|value| escape_review_text(value))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let network = if detail.network_hosts.is_empty() {
        "none".to_string()
    } else {
        detail
            .network_hosts
            .iter()
            .map(|value| escape_review_text(value))
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

fn render_mcp_inventory(detail: &PluginDetail) -> String {
    if detail.mcp_servers.is_empty() {
        return "none".to_string();
    }
    detail
        .mcp_servers
        .iter()
        .map(render_mcp_server)
        .collect::<Vec<_>>()
        .join("; ")
}

fn render_mcp_server(server: &PluginMcpServerDetail) -> String {
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
                    escape_review_text(destination),
                    escape_review_text(source)
                )
            })
            .collect::<Vec<_>>();
        env_provenance.sort_unstable();
        let cwd = server
            .cwd
            .as_deref()
            .map(escape_review_path)
            .unwrap_or_else(|| "plugin-root".to_string());
        let argv = render_review_argv(server, &server.argv);
        format!(
            "{}: transport=stdio command={} argv=[{}] cwd={cwd} env=[{}] timeouts={} required={} enabled_tools=[{}] disabled_tools=[{}] host-user-filesystem/network-authority {enabled}",
            escape_review_text(&server.name),
            escape_review_text(command),
            argv.join(", "),
            if env_provenance.is_empty() {
                "none".to_string()
            } else {
                env_provenance.join(", ")
            },
            render_mcp_timeouts(server),
            server.required,
            render_review_values(&server.enabled_tools),
            render_review_values(&server.disabled_tools),
        )
    } else if let Some(url) = server.url.as_deref() {
        let endpoint = reqwest::Url::parse(url)
            .ok()
            .map(|url| escape_review_text(url.as_str()))
            .unwrap_or_else(|| "invalid-url".to_string());
        let mut env_headers = server
            .env_headers
            .iter()
            .map(|(header, source)| {
                format!(
                    "{} <- {}",
                    escape_review_text(header),
                    escape_review_text(source)
                )
            })
            .collect::<Vec<_>>();
        env_headers.sort_unstable();
        let bearer = server
            .bearer_token_env_var
            .as_deref()
            .map(escape_review_text)
            .unwrap_or_else(|| "none".to_string());
        let transport = transport_label(&server.transport);
        format!(
            "{}: transport={} endpoint={} redirects=same-origin-only env_headers=[{}] bearer_env={} oauth=disabled timeouts={} required={} enabled_tools=[{}] disabled_tools=[{}] {enabled}",
            escape_review_text(&server.name),
            escape_review_text(transport),
            endpoint,
            if env_headers.is_empty() {
                "none".to_string()
            } else {
                env_headers.join(", ")
            },
            bearer,
            render_mcp_timeouts(server),
            server.required,
            render_review_values(&server.enabled_tools),
            render_review_values(&server.disabled_tools),
        )
    } else {
        format!("{}: invalid", server.name)
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

fn render_review_argv(server: &PluginMcpServerDetail, arguments: &[String]) -> Vec<String> {
    // Portable argv rendering: plugin-path classification requires the
    // canonical root, which is carried in the detail. Keep the exact
    // semantics of the legacy renderer.
    let root = &server.cwd.clone().unwrap_or_default();
    arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            let position = index + 1;
            let candidate = root.join(argument);
            if candidate.exists()
                && candidate
                    .canonicalize()
                    .is_ok_and(|path| path.starts_with(root))
            {
                return format!(
                    "#{position} plugin-path={}",
                    render_review_argv_value(argument)
                );
            }
            format!("#{position} value={}", render_review_argv_value(argument))
        })
        .collect()
}

fn render_review_argv_value(value: &str) -> String {
    // JSON string syntax is a lossless, unambiguous terminal representation:
    // whitespace, quotes, backslashes, and punctuation retain their exact
    // argv semantics without hiding arbitrary values behind redaction.
    serde_json::to_string(value).expect("serializing a Rust string cannot fail")
}

fn render_review_values(values: &[String]) -> String {
    if values.is_empty() {
        return "none".to_string();
    }
    values
        .iter()
        .map(|value| escape_review_text(value))
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

pub(crate) fn escape_review_path(path: &Path) -> String {
    escape_review_text(&path.to_string_lossy())
}

pub(crate) fn escape_review_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_control() || codewhale_sanitize::sanitize::is_bidi_control(ch) {
            let _ = write!(escaped, "\\u{{{:x}}}", ch as u32);
        } else if matches!(
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
        ) {
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
                escape_review_text(&item.target),
                escape_review_text(&item.reason)
            );
        }
        if report.items.len() > SHOWN {
            let _ = writeln!(out, "  ... and {} more", report.items.len() - SHOWN);
        }
    }
    if !report.kept.is_empty() {
        let _ = writeln!(out, "\nKept ({}):", report.kept.len());
        for line in report.kept.iter().take(10) {
            let _ = writeln!(out, "  {}", escape_review_text(line));
        }
        if report.kept.len() > 10 {
            let _ = writeln!(out, "  ... and {} more", report.kept.len() - 10);
        }
    }
    for note in &report.notes {
        let _ = writeln!(out, "\nNote: {}", escape_review_text(note));
    }
    for failure in &report.failures {
        let _ = writeln!(out, "\nCould not remove {}", escape_review_text(failure));
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

//! `/plugin import dsh`: review, then install, a DeepSeek Harness bundle
//! package through the ordinary reviewed installer.
//!
//! Review converts the package into scratch and shows what converts, what is
//! skipped (each a manual port), the authority the bundle will request, and
//! the content hash. Approval installs only that exact converted bundle; it
//! lands disabled and untrusted like every other install, and the existing
//! trust and enable commands take it from there.

use std::fmt::Write as _;
use std::path::Path;

use codewhale_command_contract::facets::{CommandPluginContext, CommandPresentationContext};

use super::{escape_review_path, escape_review_text};
use crate::commands::CommandResult;

pub(super) const USAGE: &str =
    "/plugin import dsh <package-dir>\n/plugin import dsh approve <package-dir> <content-hash>";

pub(super) fn dispatch(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    words: &[&str],
) -> CommandResult {
    match words {
        ["approve", path @ .., hash] if !path.is_empty() => {
            approve(presentation, plugin, &path.join(" "), hash)
        }
        path if !path.is_empty() && path[0] != "approve" => review(plugin, &path.join(" ")),
        _ => CommandResult::error(format!("Usage:\n{USAGE}")),
    }
}

fn list_line(output: &mut String, label: &str, items: &[String]) {
    let shown = if items.is_empty() {
        "none".to_string()
    } else {
        items
            .iter()
            .map(|item| escape_review_text(item))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let _ = writeln!(output, "  {label}: {shown}");
}

fn review(plugin: &dyn CommandPluginContext, path: &str) -> CommandResult {
    let preview = match plugin.dsh_preview(Path::new(path)) {
        Ok(preview) => preview,
        Err(error) => return CommandResult::error(format!("DSH import refused: {error}")),
    };
    let mut output = format!(
        "DeepSeek Harness package {}{} → plugin '{}'\n  Source: {}\n",
        escape_review_text(preview.source_package.as_deref().unwrap_or("unnamed")),
        preview
            .source_version
            .as_deref()
            .map(|version| format!("@{}", escape_review_text(version)))
            .unwrap_or_default(),
        preview.plugin_name,
        escape_review_path(&preview.package_path),
    );
    list_line(&mut output, "Skills", &preview.skills);
    list_line(&mut output, "Remote MCP servers", &preview.remote_servers);
    list_line(
        &mut output,
        "Local MCP servers (run with your user's authority)",
        &preview.local_servers,
    );
    list_line(
        &mut output,
        "Network hosts it will request",
        &preview.network_hosts,
    );
    if !preview.native_rows.is_empty() {
        list_line(
            &mut output,
            "Native host code (JavaScript; runs in the experimental extension host)",
            &preview.native_rows,
        );
        if !codewhale_plugins::activation::extension_host_policy_enabled() {
            output.push_str(
                "  Requires: [features] extension_host = true (off now, so this bundle cannot be enabled)\n",
            );
        }
    }
    if preview.requires_node {
        output.push_str("  Requires: node on PATH\n");
    }
    if preview.manual_ports.is_empty() {
        output.push_str("  Skipped: none\n");
    } else {
        let _ = writeln!(
            output,
            "  Skipped ({}; each needs a manual port and is not imported):",
            preview.manual_ports.len()
        );
        for line in &preview.manual_ports {
            let _ = writeln!(output, "    - {}", escape_review_text(line));
        }
    }
    let _ = writeln!(
        output,
        "  Content hash: {}\n\nNothing was installed. The full conversion receipt ships in the bundle as CONVERSION.md.\nTo import exactly this bundle (it lands disabled and untrusted):\n  /plugin import dsh approve {} {}",
        preview.content_hash,
        preview.package_path.display(),
        preview.content_hash,
    );
    CommandResult::message(output)
}

fn approve(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    path: &str,
    expected_hash: &str,
) -> CommandResult {
    // The installer re-converts and refuses unless the staged bytes match the
    // reviewed hash, so a package edited after review installs nothing.
    match plugin.install(&format!("dsh:{path}"), Some(expected_hash)) {
        Ok(receipt) => {
            super::render_install_receipt(presentation, plugin, receipt, Some(expected_hash))
        }
        Err(error) => super::action_error(presentation, &format!("DSH import failed: {error}")),
    }
}

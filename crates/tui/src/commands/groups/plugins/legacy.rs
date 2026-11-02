//! Legacy executable plugin-tool inventory (`[tools].plugin_dir`).
//!
//! These are scripts, not declarative bundles: they are discovered by
//! scanning a directory, they carry their own approval requirement, and
//! they never share bundle trust state. `/plugin tools` reports them
//! read-only — nothing here installs, trusts, or executes anything.
//!
//! FEAT-020: this module consumes the portable `PluginLegacyScan` from the
//! plugin facet; no concrete `App` or `PluginMetadata` crosses the boundary.

use super::render::ReviewTextContext;
use codewhale_command_contract::facets::{CommandPluginContext, CommandPresentationContext};
use std::fmt::Write as _;

use crate::commands::CommandResult;

pub(super) fn legacy_tools(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &dyn CommandPluginContext,
    name: Option<&str>,
) -> CommandResult {
    let scan = match plugin.legacy_scan() {
        Ok(Some(scan)) => scan,
        Ok(None) | Err(_) => {
            return super::action_error(
                presentation,
                "Could not resolve the legacy executable plugin-tool directory",
            );
        }
    };
    match name {
        Some(name) => show_legacy_tool_detail(presentation, name, &scan),
        None => list_legacy_tools(presentation, &scan),
    }
}

fn list_legacy_tools(
    presentation: &mut dyn CommandPresentationContext,
    scan: &codewhale_command_contract::facets::PluginLegacyScan,
) -> CommandResult {
    if scan.tools.is_empty() {
        return CommandResult::message(
            presentation
                .translate(
                    "cmd_plugin_none_found",
                    &[("dir", &scan.dir.display().to_string())],
                )
                .unwrap_or_default(),
        );
    }
    let mut output = presentation
        .translate(
            "cmd_plugin_legacy_list_header",
            &[
                ("count", &scan.tools.len().to_string()),
                ("dir", &scan.dir.display().to_string()),
            ],
        )
        .unwrap_or_default();
    output.push('\n');
    for tool in &scan.tools {
        let _ = writeln!(
            output,
            "• {} — {}\n  {}",
            tool.name,
            tool.description,
            tool.path.display()
        );
    }
    super::append_diagnostics(
        presentation,
        &mut output,
        &scan.diagnostics,
        ReviewTextContext::Prose,
    );
    CommandResult::message(output)
}

fn show_legacy_tool_detail(
    presentation: &mut dyn CommandPresentationContext,
    name: &str,
    scan: &codewhale_command_contract::facets::PluginLegacyScan,
) -> CommandResult {
    let Some(tool) = scan.tools.iter().find(|tool| tool.name == name) else {
        return CommandResult::error(
            presentation
                .translate("cmd_plugin_not_found", &[("name", name)])
                .unwrap_or_default(),
        );
    };
    let schema = tool
        .input_schema
        .clone()
        .unwrap_or_else(|| "{}".to_string());
    let mut output = format!("{}\n{:=<40}\n", tool.name, "");
    let _ = writeln!(
        output,
        "{}",
        presentation
            .translate(
                "cmd_plugin_detail_description",
                &[("description", &tool.description)],
            )
            .unwrap_or_default()
    );
    let _ = writeln!(
        output,
        "{}",
        presentation
            .translate("cmd_plugin_detail_schema", &[("schema", &schema)])
            .unwrap_or_default()
    );
    let _ = writeln!(
        output,
        "{}",
        presentation
            .translate(
                "cmd_plugin_detail_approval",
                &[("approval", &tool.approval)]
            )
            .unwrap_or_default()
    );
    let _ = writeln!(
        output,
        "{}",
        presentation
            .translate(
                "cmd_plugin_detail_path",
                &[("path", &tool.path.display().to_string())]
            )
            .unwrap_or_default()
    );
    let diagnostics: Vec<_> = scan
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.path.as_deref() == Some(tool.path.as_path()))
        .cloned()
        .collect();
    super::append_diagnostics(
        presentation,
        &mut output,
        &diagnostics,
        ReviewTextContext::Prose,
    );
    CommandResult::message(output)
}

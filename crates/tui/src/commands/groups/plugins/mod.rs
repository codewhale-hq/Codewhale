//! Codewhale bundle lifecycle and legacy executable plugin-tool inventory.
//!
//! `/plugin` owns declarative bundles (`plugin.toml`). Script tools under
//! `[tools].plugin_dir` remain supported, but are labeled as legacy executable
//! tools and never share bundle trust state.
//!
//! # Module map
//!
//! This file is the command surface: registration, the `/plugin` verb
//! dispatch, and the bundle lifecycle verbs (list/show/trust/validate/
//! install/update/uninstall/enable/disable/revoke). Two seams live next
//! door:
//!
//! * [`render`] — every string the user reads: bundle detail, the
//!   capability review body, diagnostics, and the escaping that keeps
//!   manifest-controlled text from forging review output.
//! * [`legacy`] — the separate `[tools].plugin_dir` executable inventory,
//!   which shares no trust state with declarative bundles.
//!
//! FEAT-020 converts this group to the portable command contract: every
//! production handler consumes workspace, presentation, and plugin facets —
//! never concrete `App`, `PluginRegistry`, or `Config`. Production registration
//! uses `ContextualCommand::from_contract`; a test-only shell builds the same
//! capability bundle for focused parity tests. `CommandResult` and `AppAction`
//! remain temporary TUI-owned references until FEAT-037.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use codewhale_command_contract::facets::{
    CommandPluginContext, CommandPresentationContext, PluginDetail, PluginDiagnosticLevel,
    PluginMutationOutcome, PluginMutationReceipt,
};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{CommandInfo, RegisterCommand};

use crate::commands::CommandResult;
use crate::commands::traits::{CommandGroup, ContextualCommand};
#[cfg(test)]
use crate::tui::app::App;
use crate::tui::app::AppAction;

pub(crate) mod dsh_import;
mod kimi_import;
pub(crate) mod legacy;
pub(crate) mod marketplace;
#[cfg(test)]
mod marketplace_tests;
pub(crate) mod render;

#[cfg(test)]
mod tests;

use legacy::legacy_tools;
use render::ReviewTextContext;

pub struct PluginsCommands;

impl CommandGroup for PluginsCommands {
    fn commands(&self) -> &'static [Box<dyn crate::commands::traits::Command>] {
        cached_command_list!(vec![Box::new(
            ContextualCommand::from_contract::<PluginsCmd>().expect("plugin registration"),
        )])
    }
}

pub(in crate::commands) const PLUGINS_INFO: CommandInfo = CommandInfo {
    name: "plugin",
    aliases: &["plugins", "extensions"],
    usage: "/plugin [list|show|suggest|validate|export|install|import|update|uninstall|trust|enable|disable|revoke|reload|doctor|tools|marketplace|dismissals]",
    description_key: "cmd_plugin_description",
};

pub(in crate::commands) struct PluginsCmd;

impl RegisterCommand<CommandResult> for PluginsCmd {
    fn info() -> &'static CommandInfo {
        &PLUGINS_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::WORKSPACE
                .union(CommandCapabilities::PRESENTATION)
                .union(CommandCapabilities::PLUGIN),
            handler: plugins_contextual,
        }
    }
}

fn plugins_contextual(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(workspace) = parts.workspace.as_deref() else {
        return CommandResult::error("Command capability unavailable: workspace");
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let Some(plugin) = parts.plugin.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: plugin");
    };
    plugins(&workspace.workspace(), presentation, plugin, arg, None)
}

#[cfg(test)]
fn plugins_with_kimi_home(app: &mut App, arg: Option<&str>, home: &Path) -> CommandResult {
    plugins_with_kimi_home_override(app, arg, Some(home))
}

#[cfg(test)]
fn plugins_with_kimi_home_override(
    app: &mut App,
    arg: Option<&str>,
    kimi_home: Option<&Path>,
) -> CommandResult {
    let mut bundle = app.command_contexts();
    let capabilities = CommandCapabilities::WORKSPACE
        .union(CommandCapabilities::PRESENTATION)
        .union(CommandCapabilities::PLUGIN);
    let mut contexts = bundle.contexts(capabilities).into_parts();
    let Some(workspace) = contexts.workspace.as_deref() else {
        return CommandResult::error("Command capability unavailable: workspace");
    };
    let Some(presentation) = contexts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    let Some(plugin) = contexts.plugin.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: plugin");
    };
    plugins(&workspace.workspace(), presentation, plugin, arg, kimi_home)
}

/// Portable `/plugin` dispatch (FEAT-020 Phase 4).
///
/// The handler consumes only portable facets; all concrete host access lives
/// in the TUI adapter. `kimi_home` is a test-only home override for the Kimi
/// managed-import scan.
pub(super) fn plugins(
    workspace: &Path,
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    arg: Option<&str>,
    kimi_home: Option<&Path>,
) -> CommandResult {
    let words = arg
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>();
    match words.as_slice() {
        [] => CommandResult::action(AppAction::OpenExtensions {
            tab: crate::tui::views::extensions::ExtensionsTab::Plugins,
        }),
        ["list"] => list_bundles_and_legacy_tools(presentation, plugin),
        ["help"] => CommandResult::message(format!(
            "{}\n\n/plugin import kimi [list]\n/plugin import kimi approve <name> <content-hash>\n{}\n{}",
            translate(presentation, "cmd_plugin_bundle_usage"),
            dsh_import::USAGE,
            DOCTOR_USAGE
        )),
        ["marketplace", rest @ ..] => marketplace::dispatch(presentation, plugin, rest),
        ["import", "kimi", rest @ ..] => {
            kimi_import::dispatch(presentation, plugin, rest, kimi_home)
        }
        ["import", "dsh", rest @ ..] => dsh_import::dispatch(presentation, plugin, rest),
        ["import", ..] => CommandResult::error(format!(
            "{}\n{}",
            kimi_import::usage(presentation),
            dsh_import::USAGE
        )),
        ["show", selector] => show_bundle(presentation, plugin, selector),
        ["suggest"] | ["recommend"] => CommandResult::error("Usage: /plugin suggest <task>"),
        ["suggest", task @ ..] | ["recommend", task @ ..] => {
            suggest_bundles(presentation, plugin, &task.join(" "))
        }
        ["validate"] => validate_bundles(presentation, plugin, None),
        ["validate", selector] => validate_bundles(presentation, plugin, Some(selector)),
        ["export"] => CommandResult::error("Usage: /plugin export <name> <target-dir>"),
        ["export", selector, target @ ..] => {
            export_bundle(workspace, presentation, plugin, selector, &target.join(" "))
        }
        ["install"] => CommandResult::error(translate(presentation, "cmd_plugin_bundle_usage")),
        ["install", rest @ ..] => install_bundle(presentation, plugin, &rest.join(" ")),
        ["update"] | ["uninstall"] => {
            CommandResult::error(translate(presentation, "cmd_plugin_bundle_usage"))
        }
        ["update", selector] => update_bundle(presentation, plugin, selector),
        ["uninstall", selector] => uninstall_bundle(presentation, plugin, selector),
        ["trust", selector] => review_bundle(presentation, plugin, selector),
        ["trust", selector, token] => {
            mutate_bundle(presentation, plugin, selector, Mutation::Trust(token))
        }
        ["enable", selector] => mutate_bundle(presentation, plugin, selector, Mutation::Enable),
        ["disable", selector] => mutate_bundle(presentation, plugin, selector, Mutation::Disable),
        ["revoke", selector] => mutate_bundle(presentation, plugin, selector, Mutation::Revoke),
        ["reload"] => reload(presentation, plugin),
        ["doctor"] => doctor(presentation, plugin, false),
        ["doctor", "--fix"] => doctor(presentation, plugin, true),
        ["doctor", ..] => CommandResult::error(DOCTOR_USAGE),
        ["dismissals"] => list_dismissals(plugin),
        ["dismissals", "reset"] => reset_dismissals(plugin, None),
        ["dismissals", "reset", name] => reset_dismissals(plugin, Some(name)),
        ["dismissals", ..] => CommandResult::error("Usage: /plugin dismissals [reset [<name>]]"),
        ["tools"] => legacy_tools(presentation, plugin, None),
        ["tools", name] => legacy_tools(presentation, plugin, Some(name)),
        [selector] => {
            if plugin.detail(selector).is_ok() {
                show_bundle(presentation, plugin, selector)
            } else {
                // Preserve `/plugin <script-tool>` compatibility while making
                // its distinct execution model explicit in the output.
                legacy_tools(presentation, plugin, Some(selector))
            }
        }
        _ => CommandResult::error(translate(presentation, "cmd_plugin_bundle_usage")),
    }
}

/// `/plugin dismissals`: which plugins suggestions skip, and for how long
/// (plugin policy rule 9: dismissal is reversible).
fn list_dismissals(plugin: &dyn CommandPluginContext) -> CommandResult {
    let dismissals = match plugin.suggestion_dismissals() {
        Ok(dismissals) => dismissals,
        Err(error) => return CommandResult::error(error),
    };
    if dismissals.persisted.is_empty() && dismissals.session.is_empty() {
        return CommandResult::message("No plugins are hidden from suggestions.".to_string());
    }
    let mut output = String::from("Plugins hidden from suggestions:\n");
    if !dismissals.persisted.is_empty() {
        output.push_str("  Don't suggest again (kept across sessions):\n");
        for name in &dismissals.persisted {
            let _ = writeln!(
                output,
                "    {}",
                escape_review_text(name, ReviewTextContext::Prose)
            );
        }
    }
    if !dismissals.session.is_empty() {
        output.push_str("  This session only:\n");
        for name in &dismissals.session {
            let _ = writeln!(
                output,
                "    {}",
                escape_review_text(name, ReviewTextContext::Prose)
            );
        }
    }
    output.push_str(
        "\nReset with /plugin dismissals reset [<name>]. Manual /plugin commands work either way.",
    );
    CommandResult::message(output)
}

/// `/plugin dismissals reset [<name>]`: let suggestions offer a plugin again.
fn reset_dismissals(plugin: &mut dyn CommandPluginContext, name: Option<&str>) -> CommandResult {
    match plugin.reset_suggestion_dismissals(name) {
        Ok(cleared) if cleared.is_empty() => CommandResult::message(match name {
            Some(name) => format!(
                "`{}` was not hidden from suggestions.",
                escape_review_text(name, ReviewTextContext::InlineCode)
            ),
            None => "No plugins were hidden from suggestions.".to_string(),
        }),
        Ok(cleared) => CommandResult::message(format!(
            "Suggestions may offer {} again.",
            cleared
                .iter()
                .map(|name| escape_review_text(name, ReviewTextContext::Prose))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        Err(error) => CommandResult::error(error),
    }
}

/// Translate one stable plugin key through the presentation facet.
fn translate(presentation: &mut dyn CommandPresentationContext, key: &str) -> String {
    presentation.translate(key, &[]).unwrap_or_default()
}

fn reload(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
) -> CommandResult {
    match plugin.reload() {
        Ok(count) => {
            let message = presentation
                .translate(
                    "cmd_plugin_bundle_reloaded",
                    &[("count", &count.to_string())],
                )
                .unwrap_or_default();
            CommandResult::with_message_and_action(message, AppAction::PluginRegistryChanged)
        }
        Err(error) => action_error(presentation, &format!("Plugin reload failed: {error}")),
    }
}

const DOCTOR_USAGE: &str =
    "Usage: /plugin doctor [--fix]  (report superseded plugin state; --fix applies it)";

/// `/plugin doctor [--fix]`: report, or retire, plugin state that no longer
/// points at anything: records and snapshots of superseded built-in builds,
/// inert records for vanished workspaces, orphaned runtime snapshots. The
/// report is read-only; `--fix` backs `state.json` up, then rewrites it
/// atomically. The same safe subset also runs automatically at startup.
fn doctor(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    fix: bool,
) -> CommandResult {
    use codewhale_plugins::registry::gc;

    let Some(state_path) = plugin.state_path() else {
        return CommandResult::error("Plugin registry has no persistence store; nothing to check");
    };
    let live = match plugin.summaries() {
        Ok(summaries) => summaries.into_iter().map(|summary| summary.id).collect(),
        Err(error) => return CommandResult::error(error),
    };
    let options = gc::GcOptions::default();
    // Path identity is resolved off this thread. Reloading the session
    // registry stays here: that is session state, not the directory walk.
    let report = gc::run(
        state_path,
        live,
        options,
        fix.then_some(gc::GcTier::Explicit),
    );
    if !fix {
        return match report {
            Ok(report) => CommandResult::message(render::render_gc_report(&report, false)),
            Err(error) => {
                CommandResult::error(format!("Plugin doctor could not read state: {error}"))
            }
        };
    }
    match report {
        Ok(report) => {
            let message = render::render_gc_report(&report, true);
            // Rediscover so the session stops listing what was just retired.
            match plugin.reload() {
                Ok(_) => CommandResult::with_message_and_action(
                    message,
                    AppAction::PluginRegistryChanged,
                ),
                Err(error) => action_error(
                    presentation,
                    &format!("{message}\nPlugin reload failed: {error}"),
                ),
            }
        }
        Err(error) => CommandResult::error(format!("Plugin doctor changed nothing: {error}")),
    }
}

/// Rank installed bundles and locally-added marketplace candidates for a task
/// without changing trust, enablement, disk state, or network state.
fn suggest_bundles(
    _presentation: &mut dyn CommandPresentationContext,
    plugin: &dyn CommandPluginContext,
    task: &str,
) -> CommandResult {
    let task = task.trim();
    if task.chars().count() < 3 {
        return CommandResult::error("Usage: /plugin suggest <task of at least 3 characters>");
    }
    let suggestions = plugin.suggest(task).unwrap_or_default();
    if suggestions.is_empty() {
        return CommandResult::message(format!(
            "No installed or catalog plugin matched `{}`.\n\nInstall a reviewed bundle with /plugin install <source>, or add a catalog with /plugin marketplace add. Nothing was installed, trusted, or enabled.",
            escape_review_text(task, ReviewTextContext::InlineCode)
        ));
    }
    let mut output = format!(
        "Suggested plugins for `{}`:\n",
        escape_review_text(task, ReviewTextContext::InlineCode)
    );
    output.push_str("─────────────────────────────\n");
    for suggestion in suggestions {
        let why = suggestion
            .why
            .iter()
            .map(|term| escape_review_text(term, ReviewTextContext::Prose))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            output,
            "  {} — {} · {}",
            escape_review_text(&suggestion.name, ReviewTextContext::Prose),
            suggestion.state_label,
            escape_review_text(&suggestion.description, ReviewTextContext::Prose)
        );
        let _ = writeln!(output, "    Why: {why}");
        let _ = writeln!(output, "    {}", suggestion.next_step);
    }
    output.push_str("\nNothing was installed, trusted, or enabled.");
    CommandResult::message(output)
}

fn list_bundles_and_legacy_tools(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
) -> CommandResult {
    let summaries = plugin.summaries().unwrap_or_default();
    let mut output = if summaries.is_empty() {
        translate(presentation, "cmd_plugin_bundle_none_found")
    } else {
        let mut output = presentation
            .translate(
                "cmd_plugin_bundle_list_header",
                &[("count", &summaries.len().to_string())],
            )
            .unwrap_or_default();
        output.push('\n');
        for summary in &summaries {
            let _ = writeln!(
                output,
                "• {} — {}\n  {} · {} · compatibility={} · {}\n  {}",
                escape_review_text(&summary.name, ReviewTextContext::Prose),
                summary.state_label,
                summary.scope,
                summary.trust_status,
                summary.compatibility,
                summary.inventory,
                escape_review_text(&summary.id, ReviewTextContext::Prose)
            );
        }
        output
    };
    append_diagnostics(
        presentation,
        &mut output,
        &plugin.registry_diagnostics(),
        ReviewTextContext::Prose,
    );
    // Diagnostics and the stderr tail carry plugin-controlled text.
    for line in codewhale_engine::extension_host::status_report().lines() {
        output.push('\n');
        output.push_str(&escape_review_text(line, ReviewTextContext::Prose));
    }

    if let Ok(Some(scan)) = plugin.legacy_scan() {
        output.push('\n');
        output.push_str(
            &presentation
                .translate(
                    "cmd_plugin_legacy_list_header",
                    &[
                        ("count", &scan.tools.len().to_string()),
                        ("dir", &scan.dir.display().to_string()),
                    ],
                )
                .unwrap_or_default(),
        );
        output.push('\n');
        for tool in &scan.tools {
            let _ = writeln!(
                output,
                "• {} — {}\n  {}",
                escape_review_text(&tool.name, ReviewTextContext::Prose),
                escape_review_text(&tool.description, ReviewTextContext::Prose),
                escape_review_path(&tool.path, ReviewTextContext::Prose)
            );
        }
        append_diagnostics(
            presentation,
            &mut output,
            &scan.diagnostics,
            ReviewTextContext::Prose,
        );
    }

    if let Some(nudge) = plugin.reload_nudge() {
        output.push('\n');
        output.push_str(&nudge);
    }

    CommandResult::message(output)
}

fn show_bundle(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &dyn CommandPluginContext,
    selector: &str,
) -> CommandResult {
    let detail = match plugin.detail(selector) {
        Ok(detail) => detail,
        Err(_) => {
            return CommandResult::error(
                presentation
                    .translate("cmd_plugin_bundle_not_found", &[("name", selector)])
                    .unwrap_or_default(),
            );
        }
    };
    let mut output =
        render::render_bundle_detail(presentation, &detail, true, ReviewTextContext::Prose);
    if let Some(report) = codewhale_engine::extension_host::owner_report(&detail.id) {
        append_host_owner_report(presentation, &mut output, &report);
    }
    // What the user configured for the plugin is data its code will read, so
    // it is part of what `/plugin show` puts in front of them (keys only: a
    // value can be anything, including something they would rather not echo).
    if let Some(summary) = codewhale_engine::extension_host::plugin_config_summary(&detail.name) {
        append_plugin_config_summary(&mut output, &detail.name, &summary);
    }
    CommandResult::message(output)
}

/// The `[plugins."<name>".config]` line of `/plugin show`: the configured
/// keys, never the values, or why the config is refused. Every part is escaped.
fn append_plugin_config_summary(
    output: &mut String,
    plugin_name: &str,
    summary: &Result<Vec<String>, String>,
) {
    let name = escape_review_text(plugin_name, ReviewTextContext::Prose);
    match summary {
        Ok(keys) => {
            let keys = keys
                .iter()
                .map(|key| escape_review_text(key, ReviewTextContext::Prose))
                .collect::<Vec<_>>()
                .join(", ");
            let _ = write!(
                output,
                "\n  config from [plugins.\"{name}\".config] in your config.toml (values not shown): {keys}"
            );
        }
        Err(reason) => {
            let _ = write!(
                output,
                "\n  config from [plugins.\"{name}\".config] is refused: {}",
                escape_review_text(reason, ReviewTextContext::Prose)
            );
        }
    }
}

fn append_host_owner_report(
    presentation: &dyn CommandPresentationContext,
    output: &mut String,
    report: &codewhale_engine::extension_host::OwnerReport,
) {
    use codewhale_engine::extension_host::registry::OwnerState;

    let (state_key, reason) = match &report.state {
        Some(OwnerState::Activating) => ("cmd_plugin_owner_activating", None),
        Some(OwnerState::Active) => ("cmd_plugin_owner_active", None),
        Some(OwnerState::Failed(reason)) => ("cmd_plugin_owner_failed", Some(reason)),
        Some(OwnerState::Faulted(reason)) => ("cmd_plugin_owner_faulted", Some(reason)),
        Some(OwnerState::Revoked) => ("cmd_plugin_owner_revoked", None),
        None => ("cmd_plugin_owner_inactive", None),
    };
    let reason = reason.map(|value| escape_review_text(value, ReviewTextContext::Prose));
    let replacements = reason
        .as_deref()
        .map(|value| vec![("reason", value)])
        .unwrap_or_default();
    let state = presentation
        .translate(state_key, &replacements)
        .unwrap_or_default();
    let tools = report
        .tools
        .iter()
        .map(|name| escape_review_text(name, ReviewTextContext::Prose))
        .collect::<Vec<_>>();
    let tool_names = if tools.is_empty() {
        "—".to_string()
    } else {
        tools.join(", ")
    };
    output.push('\n');
    output.push_str(
        &presentation
            .translate(
                "cmd_plugin_owner_report",
                &[
                    ("state", &state),
                    ("count", &tools.len().to_string()),
                    ("tools", &tool_names),
                ],
            )
            .unwrap_or_default(),
    );
    for line in &report.diagnostics {
        let _ = write!(
            output,
            "\n  · {}",
            escape_review_text(line, ReviewTextContext::Prose)
        );
    }
}

/// `/plugin export <name> <target-dir>` — publish a loaded bundle as a
/// spec-valid Agent Plugins v1.0.0 directory.
fn export_bundle(
    workspace: &Path,
    presentation: &mut dyn CommandPresentationContext,
    plugin: &dyn CommandPluginContext,
    selector: &str,
    target: &str,
) -> CommandResult {
    if plugin.detail(selector).is_err() {
        return CommandResult::error(
            presentation
                .translate("cmd_plugin_bundle_not_found", &[("name", selector)])
                .unwrap_or_default(),
        );
    }
    let target = target.trim();
    if target.is_empty() {
        return CommandResult::error("Usage: /plugin export <name> <target-dir>");
    }
    let target = PathBuf::from(target);
    let target = if target.is_absolute() {
        target
    } else {
        workspace.join(target)
    };
    match plugin.export(selector, &target) {
        Ok(receipt) => {
            let mut output = format!(
                "Exported `{}` as an Agent Plugins v1.0.0 bundle:\n  {}\n",
                escape_review_text(&receipt.exported_name, ReviewTextContext::InlineCode),
                escape_review_path(&receipt.target, ReviewTextContext::Prose),
            );
            if let Some(display_name) = &receipt.display_name {
                let _ = writeln!(
                    output,
                    "  Published under a slugified name; `{}` is preserved as the display name.",
                    escape_review_text(display_name, ReviewTextContext::InlineCode)
                );
            }
            let _ = writeln!(
                output,
                "  plugin.json{} · {} file(s) copied{}",
                if receipt.wrote_mcp_json {
                    " + mcp.json"
                } else {
                    ""
                },
                receipt.files_copied,
                if receipt.skills_normalized {
                    " · skills moved to the standard skills/ layout"
                } else {
                    ""
                }
            );
            output.push_str("The installed bundle was not modified.");
            CommandResult::message(output)
        }
        Err(error) => CommandResult::error(format!(
            "Export of `{}` failed: {}",
            escape_review_text(selector, ReviewTextContext::InlineCode),
            escape_review_text(&error, ReviewTextContext::Prose)
        )),
    }
}

fn review_bundle(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &dyn CommandPluginContext,
    selector: &str,
) -> CommandResult {
    let detail = match plugin.detail(selector) {
        Ok(detail) => detail,
        Err(_) => {
            return CommandResult::error(
                presentation
                    .translate("cmd_plugin_bundle_not_found", &[("name", selector)])
                    .unwrap_or_default(),
            );
        }
    };
    let mut output =
        render::render_bundle_detail(presentation, &detail, true, ReviewTextContext::Prose);
    let content =
        render::render_bundle_detail(presentation, &detail, true, ReviewTextContext::Plain);
    let command = format!("/plugin trust {} {}", detail.name, review_token(&detail));
    let _ = writeln!(output, "\n{command}");
    CommandResult::with_message_and_action(
        output,
        AppAction::OpenCommandReview {
            title: escape_review_text(&detail.name, ReviewTextContext::Plain),
            content,
            command,
        },
    )
}

pub(crate) fn review_token(detail: &PluginDetail) -> String {
    // This is an explicit user confirmation, not cosmetic display text. Bind
    // the command to both complete SHA-256 receipts so a same-inventory bundle
    // cannot collide through the former 48-bit content prefix.
    format!("{}.{}", detail.content_hash, detail.capability_hash)
}

fn validate_bundles(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &dyn CommandPluginContext,
    selector: Option<&str>,
) -> CommandResult {
    if plugin.is_empty() && selector.is_none() {
        return CommandResult::error(translate(presentation, "cmd_plugin_bundle_none_found"));
    }

    let mut output = String::new();
    if let Some(selector) = selector {
        match plugin.detail(selector) {
            Ok(detail) => {
                let invalid = detail
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.level == PluginDiagnosticLevel::Error);
                let _ = writeln!(
                    output,
                    "{} — {} — {}",
                    detail.name,
                    if invalid { "invalid" } else { "valid" },
                    detail.inventory_summary
                );
                append_diagnostics(
                    presentation,
                    &mut output,
                    &detail.diagnostics,
                    ReviewTextContext::Prose,
                );
            }
            Err(_) => {
                return CommandResult::error(
                    presentation
                        .translate("cmd_plugin_bundle_not_found", &[("name", selector)])
                        .unwrap_or_default(),
                );
            }
        }
    } else {
        for summary in plugin.summaries().unwrap_or_default() {
            let _ = writeln!(
                output,
                "{} — {} — {}",
                summary.name, summary.state_label, summary.inventory
            );
        }
        append_diagnostics(
            presentation,
            &mut output,
            &plugin.registry_diagnostics(),
            ReviewTextContext::Prose,
        );
    }
    if output.is_empty() {
        output.push_str(if plugin.validation_is_clean() {
            "valid"
        } else {
            "invalid"
        });
    }
    CommandResult::message(output)
}

// ─── /plugin install | update | uninstall (#5182) ──────────────────────────

fn install_bundle(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    spec: &str,
) -> CommandResult {
    match plugin.install(spec, None) {
        Ok(receipt) => render_install_receipt(presentation, plugin, receipt, None),
        Err(error) => action_error(presentation, &format!("Plugin install failed: {error}")),
    }
}

fn install_bundle_with_expected_hash(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    path: &Path,
    expected_content_hash: &str,
) -> CommandResult {
    match plugin.install(
        path.to_str().unwrap_or_default(),
        Some(expected_content_hash),
    ) {
        Ok(receipt) => {
            render_install_receipt(presentation, plugin, receipt, Some(expected_content_hash))
        }
        Err(error) => action_error(presentation, &format!("Plugin install failed: {error}")),
    }
}

fn render_install_receipt(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    receipt: PluginMutationReceipt,
    expected_content_hash: Option<&str>,
) -> CommandResult {
    match receipt.outcome {
        PluginMutationOutcome::Installed => {
            let name = receipt.name.clone();
            let installed_path = receipt.path.clone();
            let path = installed_path
                .as_deref()
                .map(|path| path.display().to_string())
                .unwrap_or_default();
            if let Some(expected) = expected_content_hash
                && receipt.content_hash.as_deref() != Some(expected)
            {
                return rollback_hash_mismatch(
                    presentation,
                    plugin,
                    &name,
                    installed_path.as_deref(),
                    expected,
                    receipt.content_hash.as_deref(),
                );
            }
            let mut output = format!(
                "Installed plugin '{name}' to {path}.\n\
                 It is disabled and untrusted. Review its requested authority below, then trust and enable it.\n"
            );
            if let Some(review) = review_bundle(presentation, plugin, &name).message {
                output.push('\n');
                output.push_str(&review);
            }
            CommandResult::with_message_and_action(output, AppAction::PluginRegistryChanged)
        }
        PluginMutationOutcome::NeedsApproval(host) => {
            CommandResult::error(needs_approval_message(&host))
        }
        PluginMutationOutcome::NetworkDenied(host) => {
            CommandResult::error(network_denied_message(&host))
        }
        other => CommandResult::error(format!("Unexpected install outcome: {other:?}")),
    }
}

fn rollback_hash_mismatch(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    name: &str,
    installed_path: Option<&Path>,
    expected: &str,
    actual: Option<&str>,
) -> CommandResult {
    let missing_destination = translate(presentation, "plugin_kimi_rollback_destination_missing");
    // File-level rollback removal crosses the boundary through the plugin
    // facet (D1); the host adapter owns the `codewhale_plugins::install::uninstall`
    // call.
    let rollback = installed_path
        .and_then(Path::parent)
        .ok_or_else(|| anyhow::anyhow!(missing_destination))
        .and_then(|plugins_dir| {
            plugin
                .uninstall_path(name, plugins_dir)
                .map_err(anyhow::Error::msg)
        });
    let actual = actual
        .map(|value| escape_review_text(value, ReviewTextContext::Prose))
        .unwrap_or_else(|| translate(presentation, "plugin_kimi_hash_unavailable"));
    let name = escape_review_text(name, ReviewTextContext::InlineCode);
    let expected = escape_review_text(expected, ReviewTextContext::Prose);
    match rollback {
        Ok(()) => CommandResult::error(
            presentation
                .translate(
                    "plugin_kimi_mismatch_removed",
                    &[
                        ("name", &name),
                        ("expected", &expected),
                        ("actual", &actual),
                    ],
                )
                .unwrap_or_default(),
        ),
        Err(error) => {
            let error_text = escape_review_text(&format!("{error:#}"), ReviewTextContext::Prose);
            let path_text = installed_path
                .map(|value| escape_review_path(value, ReviewTextContext::Prose))
                .unwrap_or_else(|| translate(presentation, "plugin_kimi_user_plugin_directory"));
            CommandResult {
                message: Some(
                    presentation
                        .translate(
                            "plugin_kimi_mismatch_rollback_failed",
                            &[
                                ("name", &name),
                                ("expected", &expected),
                                ("actual", &actual),
                                ("error", &error_text),
                                ("path", &path_text),
                            ],
                        )
                        .unwrap_or_default(),
                ),
                action: Some(AppAction::PluginRegistryChanged),
                is_error: true,
            }
        }
    }
}

fn update_bundle(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    selector: &str,
) -> CommandResult {
    match plugin.update(selector) {
        Ok(receipt) => match receipt.outcome {
            PluginMutationOutcome::Updated => {
                let name = receipt.name.clone();
                let mut output = format!(
                    "Updated plugin '{name}'. Review its current capabilities and trust it \
                     if needed before enabling.\n"
                );
                if let Some(review) = review_bundle(presentation, plugin, &name).message {
                    output.push('\n');
                    output.push_str(&review);
                }
                CommandResult::with_message_and_action(output, AppAction::PluginRegistryChanged)
            }
            PluginMutationOutcome::NoChange => {
                CommandResult::message(format!("Plugin '{}' is already up to date.", receipt.name))
            }
            PluginMutationOutcome::NeedsApproval(host) => {
                CommandResult::error(needs_approval_message(&host))
            }
            PluginMutationOutcome::NetworkDenied(host) => {
                CommandResult::error(network_denied_message(&host))
            }
            other => CommandResult::error(format!("Unexpected update outcome: {other:?}")),
        },
        Err(error) => action_error(presentation, &format!("Plugin update failed: {error}")),
    }
}

fn uninstall_bundle(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    selector: &str,
) -> CommandResult {
    match plugin.uninstall(selector) {
        Ok(receipt) => CommandResult::with_message_and_action(
            format!("Uninstalled plugin '{}'.", receipt.name),
            AppAction::PluginRegistryChanged,
        ),
        Err(error) => action_error(presentation, &format!("Plugin uninstall failed: {error}")),
    }
}

/// Read the active network policy for plugin downloads (host-side, D11).
pub(crate) fn plugin_network_policy() -> codewhale_net::network_policy::NetworkPolicy {
    codewhale_plugins::providers::load_config(None, None)
        .unwrap_or_default()
        .network
        .map(codewhale_net::network_policy::NetworkPolicy::from)
        .unwrap_or_default()
}

fn needs_approval_message(host: &str) -> String {
    format!(
        "Network policy requires approval for {host}.\n\
         Add it to your allow list with `/network allow {host}` (or set [network].default = \"allow\" in ~/.codewhale/config.toml), then retry."
    )
}

fn network_denied_message(host: &str) -> String {
    format!(
        "Network policy denied access to {host}.\n\
         Remove the deny entry from ~/.codewhale/config.toml under [network] or contact your administrator."
    )
}

#[derive(Clone, Copy)]
enum Mutation<'a> {
    Trust(&'a str),
    Enable,
    Disable,
    Revoke,
}

fn mutate_bundle(
    presentation: &mut dyn CommandPresentationContext,
    plugin: &mut dyn CommandPluginContext,
    selector: &str,
    mutation: Mutation<'_>,
) -> CommandResult {
    if matches!(mutation, Mutation::Enable) {
        let needs_review = plugin
            .detail(selector)
            .map(|detail| !detail.trusted)
            .unwrap_or(false);
        if needs_review {
            // Enabling is the natural entry point. Open the exact capability
            // review instead of leaving the user at an opaque denial.
            return review_bundle(presentation, plugin, selector);
        }
    }

    let result = match mutation {
        Mutation::Trust(token) => plugin.trust(selector, token).map(|()| "trusted"),
        Mutation::Enable => plugin.enable(selector).map(|()| "enabled"),
        Mutation::Disable => plugin.disable(selector).map(|()| "disabled"),
        Mutation::Revoke => plugin.revoke_trust(selector).map(|()| "trust-revoked"),
    };
    match result {
        Ok(action) => {
            let mut message = presentation
                .translate(
                    "cmd_plugin_bundle_mutation_success",
                    &[("name", selector), ("action", action)],
                )
                .unwrap_or_default();
            if matches!(mutation, Mutation::Enable)
                && let Ok(detail) = plugin.detail(selector)
            {
                let inactive = detail.unsupported_labels;
                if !inactive.is_empty() {
                    message.push(' ');
                    message.push_str(&format!(
                        "Compatibility: {}. Supported declarative components are active; inactive: {}.",
                        detail.compatibility,
                        inactive.join(", ")
                    ));
                }
            }
            CommandResult::with_message_and_action(message, AppAction::PluginRegistryChanged)
        }
        Err(error) => action_error(presentation, &error),
    }
}

fn action_error(presentation: &mut dyn CommandPresentationContext, error: &str) -> CommandResult {
    CommandResult::error(
        presentation
            .translate("cmd_plugin_action_failed", &[("error", error)])
            .unwrap_or_default(),
    )
}

pub(super) fn append_diagnostics(
    presentation: &mut dyn CommandPresentationContext,
    output: &mut String,
    diagnostics: &[codewhale_command_contract::facets::PluginDiagnostic],
    context: ReviewTextContext,
) {
    if diagnostics.is_empty() {
        return;
    }
    if !output.ends_with('\n') {
        output.push('\n');
    }
    output.push_str(
        &presentation
            .translate(
                "cmd_plugin_bundle_diagnostics_header",
                &[("count", &diagnostics.len().to_string())],
            )
            .unwrap_or_default(),
    );
    output.push('\n');
    for diagnostic in diagnostics {
        let level = match diagnostic.level {
            PluginDiagnosticLevel::Warning => "warning",
            PluginDiagnosticLevel::Error => "error",
        };
        let path = diagnostic
            .path
            .as_deref()
            .map(|path| format!(" ({})", escape_review_path(path, context)))
            .unwrap_or_default();
        let _ = writeln!(
            output,
            "• {level} [{}]: {}{path}",
            diagnostic.code,
            escape_review_text(&diagnostic.message, context)
        );
    }
}

pub(super) fn escape_review_path(path: &Path, context: ReviewTextContext) -> String {
    render::escape_review_path(path, context)
}

pub(super) fn escape_review_text(value: &str, context: ReviewTextContext) -> String {
    render::escape_review_text(value, context)
}

//! `/cache` command — portable turn/call cache telemetry and inspection.
//!
//! The host resolves the request route and pricing partitions once. This
//! handler checks flags, observes, formats and commits that same inspection;
//! no prompt builder or App state is reachable from the command leaf.

use super::CommandResult;
use super::DebugAction as AppAction;
use super::cache_format::{
    format_cache_calls, format_cache_history, format_cache_stats, format_cache_zones,
    format_first_divergence, format_static_prefix_status, format_verbose_diff,
    format_warmup_status, session_cache_rates_line,
};
use codewhale_command_contract::facets::{
    CommandDebugDiagnosticsContext, CommandPresentationContext, DebugCacheInspectionObservation,
    DebugCacheInspectionUnavailable,
};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};

pub struct CacheCmd;

const CONTRACT_INFO: ContractInfo = ContractInfo {
    name: "cache",
    aliases: &[],
    usage: "/cache [count|calls [count]|inspect|stats|zones|warmup]",
    description_key: "cmd_cache_description",
};

impl ContractRegisterCommand<CommandResult> for CacheCmd {
    fn info() -> &'static ContractInfo {
        &CONTRACT_INFO
    }

    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::DEBUG_DIAGNOSTICS
                .union(CommandCapabilities::PRESENTATION),
            handler: cache,
        }
    }
}

/// Show turn/call prefix-cache telemetry, a static status or an inspection.
pub fn cache(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(diagnostics) = parts.debug_diagnostics.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: debug_diagnostics");
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error("Command capability unavailable: presentation");
    };
    cache_portable(diagnostics, presentation, arg)
}

fn cache_portable(
    diagnostics: &mut dyn CommandDebugDiagnosticsContext,
    presentation: &dyn CommandPresentationContext,
    arg: Option<&str>,
) -> CommandResult {
    let arg = arg.map(str::trim).filter(|s| !s.is_empty());
    if let Some(count) = arg.and_then(|value| {
        value
            .strip_prefix("calls")
            .filter(|rest| rest.is_empty() || rest.starts_with(char::is_whitespace))
    }) {
        let want = if count.trim().is_empty() {
            10
        } else {
            match count.trim().parse::<usize>() {
                Ok(count) => count,
                Err(_) => return CommandResult::error("Usage: /cache calls [count]"),
            }
        };
        return CommandResult::message(format_cache_calls(&diagnostics.cache_telemetry(), want));
    }
    let inspect_flags = arg.and_then(|a| {
        if a == "inspect" {
            Some("")
        } else {
            a.strip_prefix("inspect")
                .filter(|rest| rest.starts_with(char::is_whitespace))
        }
    });
    if let Some(flags) = inspect_flags {
        let flags = flags.trim();
        let verbose = flags.split_whitespace().any(|flag| flag == "--verbose");
        let json_mode = flags.split_whitespace().any(|flag| flag == "--json");
        return CommandResult::message(format_cache_inspect(diagnostics, verbose, json_mode));
    }
    if matches!(arg, Some("warmup")) {
        return CommandResult::action(AppAction::CacheWarmup);
    }
    if matches!(arg, Some("stats")) {
        return CommandResult::message(format_cache_stats(&diagnostics.cache_telemetry()));
    }
    if matches!(arg, Some("zones")) {
        return CommandResult::message(format_cache_zones(&diagnostics.cache_telemetry()));
    }

    let want = match arg {
        None => 10,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                return CommandResult::error(format!(
                    "Unknown /cache argument `{raw}`. Usage: /cache [count|calls [count]|inspect [--verbose|--json]|stats|zones|warmup]"
                ));
            }
        },
    };
    let telemetry = diagnostics.cache_telemetry();
    if telemetry.history.is_empty() {
        let message = (|| {
            let mut text = presentation.translate("cmd_cache_no_data", &[])?;
            if let Some(line) = session_cache_rates_line(&telemetry, presentation)? {
                text.push_str("\n\n");
                text.push_str(&line);
            }
            Ok::<_, String>(text)
        })();
        return match message {
            Ok(text) => CommandResult::message(text),
            Err(error) => CommandResult::error(error),
        };
    }
    let count = want
        .min(telemetry.history.len())
        .min(telemetry.history_capacity);
    let cost = diagnostics.cost_projection();
    match format_cache_history(&telemetry, count, &cost, presentation) {
        Ok(text) => CommandResult::message(text),
        Err(error) => CommandResult::error(error),
    }
}

fn format_cache_inspect(
    diagnostics: &mut dyn CommandDebugDiagnosticsContext,
    verbose: bool,
    json_mode: bool,
) -> String {
    if verbose && json_mode {
        return "cache inspect: --json and --verbose cannot be combined".to_string();
    }
    let observation = match diagnostics.inspect_cache() {
        Ok(value) => value,
        Err(DebugCacheInspectionUnavailable::NoConcreteRoute) => {
            return "cache inspect: Auto has no concrete route yet; send a turn first".to_string();
        }
        Err(DebugCacheInspectionUnavailable::MissingCapturedEndpoint) => {
            return "cache inspect: the restored Auto route has no captured endpoint; send a turn first"
                .to_string();
        }
    };
    let output = render_inspection(&observation, verbose, json_mode);
    commit_rendered_inspection(diagnostics, observation, output)
}

pub(super) fn commit_rendered_inspection(
    diagnostics: &mut dyn CommandDebugDiagnosticsContext,
    observation: DebugCacheInspectionObservation,
    output: String,
) -> String {
    diagnostics.remember_cache_inspection(observation.current);
    output
}

pub(super) fn json_or_fallback(rendered: Result<String, serde_json::Error>) -> String {
    rendered.unwrap_or_else(|_| "{\"error\":\"cache inspection serialization failed\"}".to_string())
}

fn render_inspection(
    observation: &DebugCacheInspectionObservation,
    verbose: bool,
    json_mode: bool,
) -> String {
    let inspection = &observation.current;
    let previous = observation.previous.as_ref();
    let warmup_status = format_warmup_status(
        observation.last_warmup_key.as_ref(),
        &observation.current_warmup_key,
        observation.last_warmup_hash_short.as_deref(),
        &observation.current_warmup_hash_short,
    );
    if json_mode {
        return json_or_fallback(serde_json::to_value(inspection).and_then(|mut value| {
            if let serde_json::Value::Object(ref mut object) = value {
                object.insert(
                    "current_warmup_key".to_string(),
                    serde_json::to_value(&observation.current_warmup_key)?,
                );
                object.insert(
                    "warmup_status".to_string(),
                    serde_json::Value::String(warmup_status.trim_end().to_string()),
                );
            }
            serde_json::to_string_pretty(&value)
        }));
    }

    let mut out = String::new();
    out.push_str("Cache Inspect\n");
    out.push_str("Full prompt text is not printed. Hashes are SHA-256 of each rendered layer.\n");
    out.push_str(&format!(
        "Base static prefix hash: {}\n",
        inspection.base_static_prefix_hash
    ));
    out.push_str(&format!(
        "Full request prefix hash: {}\n",
        inspection.full_request_prefix_hash
    ));
    out.push_str(&format!(
        "Tool catalog hash: {}\n",
        if inspection.tool_catalog_hash.is_empty() {
            "(no tools registered)".to_string()
        } else {
            inspection.tool_catalog_hash.clone()
        }
    ));
    out.push_str(&format_static_prefix_status(previous, inspection));
    out.push_str(&format_first_divergence(previous, inspection));
    out.push_str(&warmup_status);
    let total_tokens: usize = inspection
        .layers
        .iter()
        .map(|layer| layer.token_estimate)
        .sum();
    out.push_str(&format!("Estimated reusable tokens: ~{total_tokens}\n"));
    out.push('\n');

    for layer in &inspection.layers {
        let mut line = format!(
            "{}: {}, chars={}, bytes={}, ~{}tok, hash={}\n",
            layer.name,
            layer.stability.label(),
            layer.char_len,
            layer.byte_len,
            layer.token_estimate,
            layer.sha256
        );
        if let Some(tool_result) = &layer.tool_result {
            let trimmed = line.trim_end_matches('\n').to_string();
            line = format!(
                "{trimmed}, original_chars={}, sent_chars={}, truncated={}, deduplicated={}\n",
                tool_result.original_chars,
                tool_result.sent_chars,
                tool_result.truncated,
                tool_result.deduplicated
            );
        }
        if let Some(turn_meta) = &layer.turn_meta {
            let trimmed = line.trim_end_matches('\n').to_string();
            line = format!(
                "{trimmed}, turn_meta_original_chars={}, turn_meta_sent_chars={}, turn_meta_deduplicated={}, turn_meta_sha256={}\n",
                turn_meta.original_chars,
                turn_meta.sent_chars,
                turn_meta.deduplicated,
                turn_meta.sha256
            );
        }
        out.push_str(&line);
    }
    if verbose {
        out.push_str("\nVerbose diff\n");
        if let Some(previous) = previous {
            out.push_str(&format_verbose_diff(previous, inspection));
        } else {
            out.push_str("No previous inspection to compare against.\n");
        }
    }
    out
}

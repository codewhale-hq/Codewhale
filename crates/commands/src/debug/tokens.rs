//! Portable token, cost, system-prompt and context diagnostics.
//!
//! The host owns counting, route pricing, pressure classification and prompt
//! construction; these handlers compose only declared semantic projections.

use codewhale_command_contract::facets::{
    CommandPresentationContext, DebugCostProjection, DebugSystemPrompt, DebugTokenProjection,
};
use codewhale_command_contract::handler::{CommandCapabilities, CommandContexts, CommandHandler};
use codewhale_command_contract::metadata::{
    CommandInfo as ContractInfo, RegisterCommand as ContractRegisterCommand,
};

use super::CommandResult;
use super::DebugAction as AppAction;
use crate::diagnostics_reports as reports;

pub struct TokensCmd;
pub struct CostCmd;
pub struct SystemCmd;
pub struct ContextCmd;

const TOKENS_INFO: ContractInfo = ContractInfo {
    name: "tokens",
    aliases: &[],
    usage: "/tokens",
    description_key: "cmd_tokens_description",
};
const COST_INFO: ContractInfo = ContractInfo {
    name: "cost",
    aliases: &[],
    usage: "/cost [details]",
    description_key: "cmd_cost_description",
};
const SYSTEM_INFO: ContractInfo = ContractInfo {
    name: "system",
    aliases: &["xitong"],
    usage: "/system",
    description_key: "cmd_system_description",
};
const CONTEXT_INFO: ContractInfo = ContractInfo {
    name: "context",
    aliases: &["ctx"],
    usage: "/context [report|json|prompt-json|summary]",
    description_key: "cmd_context_description",
};

impl ContractRegisterCommand<CommandResult> for TokensCmd {
    fn info() -> &'static ContractInfo {
        &TOKENS_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::DEBUG_DIAGNOSTICS
                .union(CommandCapabilities::PRESENTATION),
            handler: |contexts, _| tokens(contexts),
        }
    }
}
impl ContractRegisterCommand<CommandResult> for CostCmd {
    fn info() -> &'static ContractInfo {
        &COST_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::DEBUG_DIAGNOSTICS
                .union(CommandCapabilities::PRESENTATION),
            handler: cost,
        }
    }
}
impl ContractRegisterCommand<CommandResult> for SystemCmd {
    fn info() -> &'static ContractInfo {
        &SYSTEM_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::DEBUG_DIAGNOSTICS,
            handler: |contexts, _| system_prompt(contexts),
        }
    }
}
impl ContractRegisterCommand<CommandResult> for ContextCmd {
    fn info() -> &'static ContractInfo {
        &CONTEXT_INFO
    }
    fn handler() -> CommandHandler<CommandResult> {
        CommandHandler::Contextual {
            capabilities: CommandCapabilities::DEBUG_DIAGNOSTICS,
            handler: context,
        }
    }
}

const DIAGNOSTICS_UNAVAILABLE: &str = "Command capability unavailable: debug_diagnostics";
const PRESENTATION_UNAVAILABLE: &str = "Command capability unavailable: presentation";

fn localized(
    presentation: &dyn CommandPresentationContext,
    key: &str,
    replacements: &[(&str, &str)],
) -> Result<String, String> {
    presentation.translate(key, replacements)
}

fn token_count(
    value: Option<u32>,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    value.map_or_else(
        || localized(presentation, "cmd_tokens_not_reported", &[]),
        |tokens| Ok(tokens.to_string()),
    )
}

fn active_context_summary(
    usage: &DebugTokenProjection,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    let percent = (usage.active_context_used as f64 / f64::from(usage.context_window) * 100.0)
        .clamp(0.0, 100.0);
    localized(
        presentation,
        "cmd_tokens_context_with_window",
        &[
            ("used", &usage.active_context_used.to_string()),
            ("window", &usage.context_window.to_string()),
            ("percent", &format!("{percent:.1}")),
        ],
    )
}

fn cache_summary(
    usage: &DebugTokenProjection,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    match (usage.cache_hit, usage.cache_miss) {
        (Some(hit), Some(miss)) => localized(
            presentation,
            "cmd_tokens_cache_both",
            &[("hit", &hit.to_string()), ("miss", &miss.to_string())],
        ),
        (Some(hit), None) => localized(
            presentation,
            "cmd_tokens_cache_hit_only",
            &[("hit", &hit.to_string())],
        ),
        (None, Some(miss)) => localized(
            presentation,
            "cmd_tokens_cache_miss_only",
            &[("miss", &miss.to_string())],
        ),
        (None, None) => localized(presentation, "cmd_tokens_not_reported", &[]),
    }
}

/// Show token usage for the current session, without opening host state.
pub fn tokens(contexts: CommandContexts<'_>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(diagnostics) = parts.debug_diagnostics.as_deref_mut() else {
        return CommandResult::error(DIAGNOSTICS_UNAVAILABLE);
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error(PRESENTATION_UNAVAILABLE);
    };
    let usage = diagnostics.token_projection();
    match tokens_report(&usage, presentation) {
        Ok(report) => CommandResult::message(report),
        Err(error) => CommandResult::error(error),
    }
}

fn tokens_report(
    usage: &DebugTokenProjection,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    let mut report = localized(
        presentation,
        "cmd_tokens_report",
        &[
            ("active", &active_context_summary(usage, presentation)?),
            ("input", &token_count(usage.last_input, presentation)?),
            ("output", &token_count(usage.last_output, presentation)?),
            ("cache", &cache_summary(usage, presentation)?),
            ("total", &usage.total_tokens.to_string()),
            ("cost", &cost_report_amount(&usage.cost, presentation)?),
            ("api_messages", &usage.api_message_count.to_string()),
            ("chat_messages", &usage.chat_message_count.to_string()),
            ("model", &usage.model),
        ],
    )?;
    report.push('\n');
    report.push_str(&localized(
        presentation,
        "cmd_tokens_cache_write_total",
        &[(
            "write",
            &if usage.cache_write_tokens > 0 {
                usage.cache_write_tokens.to_string()
            } else {
                localized(presentation, "cmd_tokens_not_reported", &[])?
            },
        )],
    )?);
    report.push_str(&cost_coverage_report(&usage.cost, presentation)?);
    Ok(report)
}

/// Show session cost with the same coverage disclaimer as `/tokens`.
pub fn cost(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(diagnostics) = parts.debug_diagnostics.as_deref_mut() else {
        return CommandResult::error(DIAGNOSTICS_UNAVAILABLE);
    };
    let Some(presentation) = parts.presentation.as_deref_mut() else {
        return CommandResult::error(PRESENTATION_UNAVAILABLE);
    };
    let projection = diagnostics.cost_projection();
    let details = arg.is_some_and(|arg| arg.trim().eq_ignore_ascii_case("details"));
    let plan_only = projection.total == 0.0
        && projection.priced_turns == 0
        && projection.unpriced_turns == 0
        && !projection.legacy_coverage_unknown
        && !projection.user_declared_estimates
        && !projection.route_receipts.is_empty()
        && projection.route_receipts.iter().all(|receipt| {
            receipt
                .split_whitespace()
                .any(|field| field == "billing_mode=subscription")
        });
    if plan_only && !details {
        return CommandResult::message(
            "Session Cost:\n ─────────────────────────────\n These routes use subscription billing. Codewhale does not know the amount charged by your provider.\n\n Run /cost details for the route receipts.".to_string(),
        );
    }
    match cost_report(&projection, presentation) {
        Ok(report) => CommandResult::message(report),
        Err(error) => CommandResult::error(error),
    }
}

fn cost_report(
    cost: &DebugCostProjection,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    let saved_legacy_subtotal = cost.legacy_coverage_unknown && cost.total > 0.0;
    let headline = if cost.priced_turns == 0 && !saved_legacy_subtotal {
        "cmd_cost_report_unknown"
    } else if cost.legacy_coverage_unknown || cost.unpriced_turns > 0 {
        "cmd_cost_report_subtotal"
    } else {
        "cmd_cost_report"
    };
    let mut report = if cost.user_declared_estimates {
        format!(
            "Session cost estimate (priced subtotal): {}",
            cost_report_amount(cost, presentation)?
        )
    } else if headline == "cmd_cost_report_unknown" {
        // The unknown template has no `{cost}` placeholder; the legacy
        // renderer's no-op replacement did not require one.
        localized(presentation, headline, &[])?
    } else {
        localized(
            presentation,
            headline,
            &[("cost", &cost_report_amount(cost, presentation)?)],
        )?
    };
    if cost.priced_turns > 0 || saved_legacy_subtotal {
        report.push_str(&cost_breakdown_report(cost));
    }
    report.push_str(&cost_coverage_report(cost, presentation)?);
    Ok(report)
}

fn cost_report_amount(
    cost: &DebugCostProjection,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    if cost.priced_turns > 0 || (cost.legacy_coverage_unknown && cost.total > 0.0) {
        Ok(reports::format_cost_amount_precise(
            cost.total,
            cost.currency,
        ))
    } else {
        localized(presentation, "cmd_cost_unknown_value", &[])
    }
}

fn cost_breakdown_report(cost: &DebugCostProjection) -> String {
    let format = |value| reports::format_cost_amount_precise(value, cost.currency);
    let mut out = String::from("\n\nBreakdown (components sum to the total above):");
    out.push_str(&format!("\n  Parent turns: {}", format(cost.parent_turns)));
    if cost.subagents > 0.0 {
        out.push_str(&format!("\n  Sub-agents: {}", format(cost.subagents)));
    }
    if cost.display_floor > 0.0 {
        out.push_str(&format!(
            "\n  Reconciliation floor: {} (monotonic display guarantee, kept after a downward cost reconciliation)",
            format(cost.display_floor)
        ));
    }
    if !cost.route_amounts.is_empty() {
        out.push_str(&format!(
            "\n  Parent-turn spend by route ({} of {} priced turns itemized):",
            cost.itemized_turns, cost.priced_turns
        ));
        for route in &cost.route_amounts {
            out.push_str(&format!("\n    {}: {}", route.route, format(route.amount)));
        }
        if cost.itemized_turns < cost.priced_turns {
            out.push_str(&format!(
                "\n    (earlier turns not itemized: turn telemetry keeps the last {})",
                cost.turn_history_capacity
            ));
        }
    }
    out
}

fn formatted_unpriced_reasons(
    cost: &DebugCostProjection,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    let mut descriptions: Vec<String> = Vec::new();
    if cost.unpriced_reason_labels.is_empty() {
        return localized(presentation, "cost_reason_unrecorded_coverage", &[]);
    }
    for reason in &cost.unpriced_reason_labels {
        // Classification comes from the host's UnpricedReason::from_label;
        // duplicate localized explanations collapse exactly as before.
        let description = localized(presentation, &format!("cost_reason_{reason}"), &[])?;
        if !descriptions.contains(&description) {
            descriptions.push(description);
        }
    }
    Ok(descriptions.join(", "))
}

/// The identical honesty block used by both diagnostic monetary surfaces.
fn cost_coverage_report(
    cost: &DebugCostProjection,
    presentation: &dyn CommandPresentationContext,
) -> Result<String, String> {
    let mut out = String::from("\n\n");
    if cost.user_declared_estimates {
        out.push_str("Includes user-declared, unverified price estimates calculated from recorded usage. These amounts do not establish provider prices, billing mode, or an invoice.");
    } else {
        out.push_str(&localized(presentation, "cmd_cost_estimate_only", &[])?);
    }
    out.push('\n');
    if cost.legacy_coverage_unknown {
        out.push_str(&localized(
            presentation,
            "cmd_cost_coverage_unknown_legacy",
            &[],
        )?);
    } else if cost.user_declared_estimates {
        out.push_str(&format!(
            "Coverage: {} of {} tracked turns priced or estimated.",
            cost.priced_turns,
            cost.priced_turns.saturating_add(cost.unpriced_turns)
        ));
    } else {
        out.push_str(&localized(
            presentation,
            "cmd_cost_coverage",
            &[
                ("priced", &cost.priced_turns.to_string()),
                (
                    "turns",
                    &cost
                        .priced_turns
                        .saturating_add(cost.unpriced_turns)
                        .to_string(),
                ),
            ],
        )?);
    }
    if cost.unpriced_turns > 0 {
        out.push('\n');
        let reasons = formatted_unpriced_reasons(cost, presentation)?;
        if cost.user_declared_estimates {
            out.push_str(&format!(
                "Excluded: {} turns have incomplete prices ({}); their cost is unknown.",
                cost.unpriced_turns, reasons
            ));
        } else {
            out.push_str(&localized(
                presentation,
                "cmd_cost_unpriced_turns",
                &[
                    ("unpriced", &cost.unpriced_turns.to_string()),
                    ("reasons", &reasons),
                ],
            )?);
        }
    }
    for (key, placeholder, values) in [
        (
            "cmd_cost_unpriced_classes",
            "classes",
            &cost.unpriced_classes,
        ),
        (
            "cmd_cost_pricing_provenance",
            "sources",
            &cost.pricing_provenances,
        ),
        (
            "cmd_cost_live_pricing_downgraded",
            "defects",
            &cost.live_pricing_defects,
        ),
        (
            "cmd_cost_live_pricing_unavailable",
            "defects",
            &cost.unusable_pricing_defects,
        ),
    ] {
        if !values.is_empty() {
            out.push('\n');
            out.push_str(&localized(
                presentation,
                key,
                &[(placeholder, &values.join(", "))],
            )?);
        }
    }
    if !cost.route_receipts.is_empty() {
        out.push('\n');
        out.push_str(&localized(presentation, "cmd_cost_routes_header", &[])?);
        for receipt in &cost.route_receipts {
            out.push_str("\n  ");
            out.push_str(receipt);
        }
    }
    Ok(out)
}

/// The system command is the deliberate prompt-disclosing operation.
pub fn system_prompt(contexts: CommandContexts<'_>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(diagnostics) = parts.debug_diagnostics.as_deref_mut() else {
        return CommandResult::error(DIAGNOSTICS_UNAVAILABLE);
    };
    let system = diagnostics.system_projection();
    let prompt_text = match system.prompt {
        DebugSystemPrompt::Text(text) => text,
        DebugSystemPrompt::Blocks(blocks) => blocks.join("\n\n---\n\n"),
        DebugSystemPrompt::None => "(no system prompt)".to_string(),
    };
    let display = if prompt_text.len() > 500 {
        let truncate_at = prompt_text
            .char_indices()
            .take_while(|(i, _)| *i <= 500)
            .last()
            .map_or(0, |(i, _)| i);
        format!(
            "{}...\n\n(truncated, {} chars total)",
            &prompt_text[..truncate_at],
            prompt_text.len()
        )
    } else {
        prompt_text
    };
    CommandResult::message(format!(
        "System Prompt ({} mode):\n─────────────────────────────\n{}",
        system.mode_label, display
    ))
}

/// Bare `/context` emits an inspector action without invoking a report builder.
pub fn context(contexts: CommandContexts<'_>, arg: Option<&str>) -> CommandResult {
    let mut parts = contexts.into_parts();
    let Some(diagnostics) = parts.debug_diagnostics.as_deref_mut() else {
        return CommandResult::error(DIAGNOSTICS_UNAVAILABLE);
    };
    let Some(subcommand) = arg.map(str::trim).filter(|arg| !arg.is_empty()) else {
        return CommandResult::action(AppAction::OpenContextInspector);
    };
    match subcommand {
        "prompt-json" | "prompt_json" | "prompt" => {
            CommandResult::message(reports::prompt_context_json(&diagnostics.prompt_context()))
        }
        "report" | "json" | "summary" => {
            let report = diagnostics.context_source_map();
            match subcommand {
                "report" => CommandResult::message(reports::format_context_report(&report)),
                "json" => CommandResult::message(reports::context_report_json(&report)),
                "summary" => CommandResult::message(reports::format_context_summary(&report)),
                _ => unreachable!(),
            }
        }
        other => CommandResult::error(format!(
            "Unknown /context subcommand: {other}. Use report, json, prompt-json, or summary."
        )),
    }
}

use std::collections::BTreeMap;

use codewhale_command_contract::facets::{
    DebugCostProjection, ReceiptCost, ReceiptCostState, ReceiptTokens, ReceiptUsage,
};
use codewhale_command_contract::types::CommandCurrency;
use codewhale_commands::debug::tokens::{
    SUBSCRIPTION_ONLY_COST_NOTE, cost_is_subscription_only, cost_report,
};

use super::Receipt;
use crate::cost_status::UsageSourceContribution;
use crate::diagnostics_messages::EnglishDiagnostics;
use crate::llm_client::ProviderUsageCoverage;
use crate::session_manager::{SessionCostSnapshot, SessionMetadata};

const TURN_SCOPE_NOTE: &str = "Usage: token counts and cost are kept for the whole session, not per turn, so a single-turn receipt leaves them out.";
const NO_TOKENS_NOTE: &str = "Tokens: the session saved no provider token counts, so none are shown; that is not zero usage.";
const THREAD_COST_NOTE: &str = "Cost: a Runtime thread receipt carries token counts only; GET /v1/threads/{id}/usage reports the thread's cost and what it covers.";

fn sanitize(amount: f64) -> f64 {
    if amount.is_finite() && amount >= 0.0 {
        amount
    } else {
        0.0
    }
}

pub fn cost_projection_from_snapshot(cost: &SessionCostSnapshot) -> DebugCostProjection {
    let cny = cost.cny_priced_turns > 0 && cost.priced_turns == 0;
    let (parent, auxiliary, high_water, priced, unpriced, reasons) = if cny {
        (
            cost.session_cost_cny,
            cost.subagent_cost_cny,
            cost.displayed_cost_high_water_cny,
            cost.cny_priced_turns,
            cost.cny_unpriced_turns,
            &cost.cny_unpriced_reasons,
        )
    } else {
        (
            cost.session_cost_usd,
            cost.subagent_cost_usd,
            cost.displayed_cost_high_water_usd,
            cost.priced_turns,
            cost.unpriced_turns,
            &cost.unpriced_reasons,
        )
    };
    let parent_turns = sanitize(parent);
    let subagents = sanitize(auxiliary);
    let current = parent_turns + subagents;
    let total = current.max(sanitize(high_water));
    DebugCostProjection {
        currency: if cny {
            CommandCurrency::Cny
        } else {
            CommandCurrency::Usd
        },
        total,
        parent_turns,
        subagents,
        display_floor: (total - current).max(0.0),
        priced_turns: priced,
        unpriced_turns: unpriced,
        legacy_coverage_unknown: cost.coverage_is_legacy_unknown(),
        user_declared_estimates: cost.pricing_provenances.contains("user_override"),
        itemized_turns: 0,
        route_amounts: Vec::new(),
        turn_history_capacity: 0,
        unpriced_reason_labels: reasons.iter().cloned().collect(),
        unpriced_classes: cost.unpriced_classes.iter().cloned().collect(),
        pricing_provenances: cost.pricing_provenances.iter().cloned().collect(),
        live_pricing_defects: cost.live_pricing_defects.iter().cloned().collect(),
        unusable_pricing_defects: cost.live_pricing_unusable_defects.iter().cloned().collect(),
        route_receipts: cost.route_receipts.iter().cloned().collect(),
    }
}

pub fn receipt_cost(projection: &DebugCostProjection) -> ReceiptCost {
    let has_amount = projection.priced_turns > 0
        || (projection.legacy_coverage_unknown && projection.total > 0.0);
    let state = if !has_amount {
        ReceiptCostState::Unknown
    } else if projection.unpriced_turns > 0 || projection.legacy_coverage_unknown {
        ReceiptCostState::Partial
    } else {
        ReceiptCostState::Priced
    };
    let report = if cost_is_subscription_only(projection) {
        SUBSCRIPTION_ONLY_COST_NOTE.to_string()
    } else {
        cost_report(projection, &EnglishDiagnostics)
            .unwrap_or_else(|error| format!("The cost report is unavailable: {error}"))
    };
    ReceiptCost {
        state,
        amount: has_amount.then_some(projection.total),
        currency: match projection.currency {
            CommandCurrency::Usd => "USD",
            CommandCurrency::Cny => "CNY",
        },
        priced_turns: projection.priced_turns,
        unpriced_turns: projection.unpriced_turns,
        report,
    }
}

fn add(total: &mut Option<u64>, value: Option<u32>) {
    if let Some(value) = value {
        *total = Some(total.unwrap_or(0).saturating_add(u64::from(value)));
    }
}

pub fn tokens_from_sources(
    sources: &BTreeMap<String, UsageSourceContribution>,
    saved_total: u64,
) -> Option<ReceiptTokens> {
    let mut tokens = ReceiptTokens::default();
    let (mut input, mut output, mut calls) = (0u64, 0u64, 0usize);
    for contribution in sources.values() {
        let record = &contribution.record;
        let reported = record.settlement.as_ref().map_or_else(
            || record.usage.usage != Default::default(),
            |revision| {
                matches!(
                    revision.coverage,
                    ProviderUsageCoverage::Reported | ProviderUsageCoverage::Incomplete
                )
            },
        );
        if !reported {
            continue;
        }
        let usage = &record.usage.usage;
        input = input.saturating_add(u64::from(usage.input_tokens));
        output = output.saturating_add(u64::from(usage.output_tokens));
        add(&mut tokens.cache_read, usage.prompt_cache_hit_tokens);
        add(&mut tokens.cache_write, usage.prompt_cache_write_tokens);
        add(&mut tokens.reasoning, usage.reasoning_tokens);
        calls += 1;
    }
    if calls > 0 {
        tokens.input = Some(input);
        tokens.output = Some(output);
        tokens.total = input.saturating_add(output);
        tokens.calls = Some(calls);
        Some(tokens)
    } else if saved_total > 0 {
        tokens.total = saved_total;
        Some(tokens)
    } else {
        None
    }
}

pub fn session_usage(
    projection: &DebugCostProjection,
    sources: &BTreeMap<String, UsageSourceContribution>,
    saved_total_tokens: u64,
) -> ReceiptUsage {
    ReceiptUsage {
        tokens: tokens_from_sources(sources, saved_total_tokens),
        cost: Some(receipt_cost(projection)),
    }
}

pub fn saved_session_usage(metadata: &SessionMetadata) -> ReceiptUsage {
    session_usage(
        &cost_projection_from_snapshot(&metadata.cost),
        &metadata.cost.accounted_usage_sources,
        metadata.total_tokens,
    )
}

#[must_use]
pub fn with_usage(mut receipt: Receipt, usage: ReceiptUsage) -> Receipt {
    if receipt.turn.is_some() {
        receipt.not_recorded.push(TURN_SCOPE_NOTE.to_string());
        return receipt;
    }
    if usage.tokens.is_none() {
        receipt.not_recorded.push(NO_TOKENS_NOTE.to_string());
    }
    receipt.usage = Some(usage);
    receipt
}

pub fn thread_tokens(turns: &[&crate::runtime_threads::TurnRecord]) -> Option<ReceiptTokens> {
    let mut tokens = ReceiptTokens::default();
    let (mut input, mut output, mut turns_with_usage) = (0u64, 0u64, 0usize);
    for usage in turns.iter().filter_map(|turn| turn.usage.as_ref()) {
        input = input.saturating_add(u64::from(usage.input_tokens));
        output = output.saturating_add(u64::from(usage.output_tokens));
        add(&mut tokens.cache_read, usage.prompt_cache_hit_tokens);
        add(&mut tokens.cache_write, usage.prompt_cache_write_tokens);
        add(&mut tokens.reasoning, usage.reasoning_tokens);
        turns_with_usage += 1;
    }
    (turns_with_usage > 0).then(|| {
        tokens.input = Some(input);
        tokens.output = Some(output);
        tokens.total = input.saturating_add(output);
        tokens
    })
}

pub fn thread_cost_note() -> &'static str {
    THREAD_COST_NOTE
}

//! Host-owned diagnostics projections for FEAT-029.
//!
//! Only this adapter reads App, pricing, request inspection and prepared-tool
//! evidence. No completed command message or rendered report crosses the facet.
//! The cache observation and final store are separate synchronous operations;
//! in particular observation never mutates the remembered inspection.

use std::collections::BTreeMap;
use std::time::Instant;

use codewhale_command_contract::facets::*;
use codewhale_models::{MessageRequest, SystemPrompt, Usage};

use super::SharedCommandHost;
use crate::client::{
    CacheWarmupKey, PromptInspection, PromptLayerStability, inspect_prompt_for_request,
};
use crate::context_report::project_source_map as source_map;
use crate::pricing::token_usage_for_pricing;
use crate::tool_inspection::project_snapshot as tool_snapshot;
use crate::tui::app::App;
use codewhale_config::pricing::CostCurrency;
use codewhale_runtime::config::provider_has_balance_api;

pub(super) struct DebugDiagnosticsAdapter<'a> {
    pub(super) host: SharedCommandHost<'a>,
}

impl CommandDebugDiagnosticsContext for DebugDiagnosticsAdapter<'_> {
    fn balance_projection(&self) -> DebugBalanceProjection {
        let app = self.host.app.borrow();
        DebugBalanceProjection {
            provider_display_name: app
                .admitted_provider_identity()
                .map_or("unavailable", |identity| {
                    identity
                        .compatibility()
                        .map_or(identity.key.as_str(), |row| row.label)
                })
                .to_string(),
            supports_balance_api: provider_has_balance_api(app.api_provider),
        }
    }

    fn system_projection(&self) -> DebugSystemProjection {
        let app = self.host.app.borrow();
        let prompt = match app.system_prompt.as_ref() {
            None => DebugSystemPrompt::None,
            Some(SystemPrompt::Text(text)) => DebugSystemPrompt::Text(text.clone()),
            Some(SystemPrompt::Blocks(blocks)) => {
                DebugSystemPrompt::Blocks(blocks.iter().map(|block| block.text.clone()).collect())
            }
        };
        DebugSystemProjection {
            mode_label: app.mode.label().to_string(),
            prompt,
        }
    }

    fn token_projection(&self) -> DebugTokenProjection {
        let app = self.host.app.borrow();
        let window = crate::route_budget::route_context_window_tokens(
            app.api_provider,
            app.effective_model_for_budget(),
            app.active_route_limits,
        );
        let estimated = crate::compaction::estimate_input_tokens_conservative(
            &app.api_messages,
            app.system_prompt.as_ref(),
        );
        DebugTokenProjection {
            active_context_used: estimated.min(window as usize),
            context_window: window,
            last_input: app.session.last_prompt_tokens,
            last_output: app.session.last_completion_tokens,
            cache_hit: app.session.last_prompt_cache_hit_tokens,
            cache_miss: app.session.last_prompt_cache_miss_tokens,
            total_tokens: u64::from(app.session.displayed_total_tokens()),
            cache_write_tokens: u64::from(app.session.displayed_total_cache_write_tokens()),
            api_message_count: app.api_messages.len(),
            chat_message_count: app.history.len(),
            model: app.model.clone(),
            cost: cost_projection(&app),
        }
    }

    fn cost_projection(&self) -> DebugCostProjection {
        cost_projection(&self.host.app.borrow())
    }

    fn cache_telemetry(&self) -> DebugCacheTelemetry {
        let app = self.host.app.borrow();
        let currency = app.cost_display_currency(app.cost_currency);
        let now = Instant::now();
        let history = app
            .session
            .turn_cache_history
            .iter()
            .map(|rec| {
                let classes = token_usage_for_pricing(&Usage {
                    input_tokens: rec.input_tokens,
                    output_tokens: rec.output_tokens,
                    prompt_cache_hit_tokens: rec.cache_hit_tokens,
                    prompt_cache_miss_tokens: rec.cache_miss_tokens,
                    prompt_cache_write_tokens: rec.cache_write_tokens,
                    reasoning_tokens: rec.reasoning_tokens,
                    reasoning_replay_tokens: rec.reasoning_replay_tokens,
                    server_tool_use: None,

                    input_audio_tokens: None,
                    output_audio_tokens: None,

                    request_usages: Vec::new(),
                });
                DebugCacheTurn {
                    provider: rec.provider.map(|provider| provider.as_str().to_string()),
                    provider_identity: rec.provider_identity.clone(),
                    model: rec.model.clone(),
                    auto_model: rec.auto_model,
                    input_tokens: rec.input_tokens,
                    output_tokens: rec.output_tokens,
                    cache_hit_tokens: rec.cache_hit_tokens,
                    cache_miss_tokens: rec.cache_miss_tokens,
                    cache_write_tokens: rec.cache_write_tokens,
                    reasoning_tokens: rec.reasoning_tokens,
                    reasoning_replay_tokens: rec.reasoning_replay_tokens,
                    priced_amount: rec.cost_audit.as_ref().and_then(|audit| {
                        audit
                            .is_priced_in(currency)
                            .then(|| audit.estimate.map(|value| value.amount(currency)))
                            .flatten()
                    }),
                    unpriced_reason_key: rec.cost_audit.as_ref().and_then(|audit| {
                        audit
                            .unpriced_reason
                            .map(|reason| reason.label().to_string())
                    }),
                    unpriced_reason_sort_rank: rec
                        .cost_audit
                        .as_ref()
                        .and_then(|audit| audit.unpriced_reason.map(|reason| reason as u8)),
                    unpriced_classes: rec.cost_audit.as_ref().map_or_else(Vec::new, |audit| {
                        audit
                            .unpriced_classes
                            .iter()
                            .map(|class| class.label().to_string())
                            .collect()
                    }),
                    priced_cache_read: classes.cache_read,
                    priced_cache_miss: classes.input,
                    priced_cache_write: classes.cache_write,
                    age_seconds: now.saturating_duration_since(rec.recorded_at).as_secs(),
                }
            })
            .collect();
        DebugCacheTelemetry {
            model: app.model.clone(),
            session_cache_rates: crate::tui::session_metrics::cache_rates(&app),
            history,
            history_capacity: App::TURN_CACHE_HISTORY_CAP,
            prefix_stability_pct: app.prefix_stability_pct,
            prefix_checks_total: app.prefix_checks_total,
            prefix_change_count: app.prefix_change_count,
            prefix_drift_count: app.prefix_drift_count,
            prefix_context_updates: app.prefix_context_updates,
            prefix_pin_reason: app.prefix_pin_reason.clone(),
            prefix_last_miss_reason: app.prefix_last_miss_reason.clone(),
            last_prefix_change_desc: app.last_prefix_change_desc.clone(),
            last_pinned_prefix_hash: app.last_pinned_prefix_hash.clone(),
            api_message_count: app.api_messages.len(),
            non_system_message_count: app
                .api_messages
                .iter()
                .filter(|m| m.role != "system")
                .count(),
        }
    }

    fn context_source_map(&self) -> DebugPromptSourceMap {
        source_map(crate::context_report::build_context_report(
            &self.host.app.borrow(),
        ))
    }

    fn prompt_context(&self) -> DebugPromptContext {
        prompt_context(crate::context_report::build_prompt_context(
            &self.host.app.borrow(),
        ))
    }

    fn tool_snapshot(&self) -> Option<DebugToolSnapshot> {
        self.host
            .app
            .borrow()
            .session
            .last_tool_request_snapshot
            .as_ref()
            .map(tool_snapshot)
    }

    fn inspect_cache(
        &self,
    ) -> Result<DebugCacheInspectionObservation, DebugCacheInspectionUnavailable> {
        let app = self.host.app.borrow();
        let (inspection, key) = observe_cache_for_app(&app)?;
        let current_warmup_hash_short = key.hash_short();
        let last_warmup_hash_short = app
            .session
            .last_warmup_key
            .as_ref()
            .map(CacheWarmupKey::hash_short);
        Ok(DebugCacheInspectionObservation {
            current: prompt_inspection(inspection),
            previous: app
                .session
                .last_cache_inspection
                .clone()
                .map(prompt_inspection),
            current_warmup_key: warmup_key(key),
            last_warmup_key: app.session.last_warmup_key.clone().map(warmup_key),
            current_warmup_hash_short,
            last_warmup_hash_short,
        })
    }

    fn remember_cache_inspection(&mut self, inspection: DebugPromptInspection) {
        self.host.app.borrow_mut().session.last_cache_inspection =
            Some(host_inspection(inspection));
    }
}

/// Resolve the route and build the inspection exactly once using the existing
/// client authority. Both the original handler (until Phase 4 adoption) and
/// the diagnostics facet use this host-owned builder. This function is read-only;
/// the caller explicitly stores the inspected snapshot only after rendering.
pub(crate) fn observe_cache_for_app(
    app: &App,
) -> Result<(PromptInspection, CacheWarmupKey), DebugCacheInspectionUnavailable> {
    let target = app
        .cache_replay_target()
        .ok_or(DebugCacheInspectionUnavailable::NoConcreteRoute)?;
    let replay_base_url = target
        .base_url
        .as_deref()
        .ok_or(DebugCacheInspectionUnavailable::MissingCapturedEndpoint)?;
    let request = MessageRequest {
        model: target.model.clone(),
        messages: app.api_messages.as_ref().clone(),
        max_tokens: 0,
        system: app.system_prompt.clone(),
        tools: app.session.last_tool_catalog.clone(),
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: app
            .reasoning_effort_api_value_for_replay(
                target.provider,
                &target.provider_identity,
                replay_base_url,
                &target.model,
            )
            .map(str::to_string),
        stream: Some(true),
        temperature: None,
        top_p: None,
    };
    let inspection = inspect_prompt_for_request(&request);
    let key = CacheWarmupKey::from_inspection(
        &target.provider_identity,
        &target.model,
        replay_base_url,
        &inspection,
    );
    Ok((inspection, key))
}

/// Authoritative host cost components shared with the original `/cost`
/// command while its presentation moves to portable code in Phase 4.
/// The #244 high-water floor is not a separate billable amount.
pub(crate) struct CostComponents {
    pub(crate) parent_turns: f64,
    pub(crate) subagents: f64,
    pub(crate) display_floor: f64,
}

impl CostComponents {
    pub(crate) fn compute(app: &App) -> Self {
        fn sanitize(amount: f64) -> f64 {
            if amount.is_finite() && amount >= 0.0 {
                amount
            } else {
                0.0
            }
        }
        let currency = app.cost_display_currency(app.cost_currency);
        let parent_turns = sanitize(app.session_cost_for_currency(currency));
        let subagents = sanitize(app.subagent_cost_for_currency(currency));
        let sum = parent_turns + subagents;
        let current = if sum.is_finite() { sum } else { f64::MAX };
        let total = app.displayed_session_cost_for_currency(app.cost_currency);
        Self {
            parent_turns,
            subagents,
            display_floor: (total - current).max(0.0),
        }
    }

    #[cfg(test)]
    pub(crate) fn sum(&self) -> f64 {
        self.parent_turns + self.subagents + self.display_floor
    }
}

fn cost_projection(app: &App) -> DebugCostProjection {
    let currency = app.cost_display_currency(app.cost_currency);
    let total = app.displayed_session_cost_for_currency(app.cost_currency);
    let components = CostComponents::compute(app);
    let (priced_turns, unpriced_turns, reasons) = match currency {
        CostCurrency::Usd => (
            app.session.cost_priced_turns,
            app.session.cost_unpriced_turns,
            &app.session.cost_unpriced_reasons,
        ),
        CostCurrency::Cny => (
            app.session.cost_cny_priced_turns,
            app.session.cost_cny_unpriced_turns,
            &app.session.cost_cny_unpriced_reasons,
        ),
    };
    let mut by_route = BTreeMap::<String, f64>::new();
    let mut itemized_turns = 0u32;
    for rec in &app.session.turn_cache_history {
        let Some(audit) = rec.cost_audit.as_ref() else {
            continue;
        };
        if !audit.is_priced_in(currency) {
            continue;
        }
        let Some(estimate) = audit.estimate else {
            continue;
        };
        let provider = rec.provider_identity.clone().unwrap_or_else(|| {
            rec.provider.map_or_else(
                || "unknown-provider".to_string(),
                |provider| provider.as_str().to_string(),
            )
        });
        let route = format!(
            "{provider}/{}",
            rec.model.as_deref().unwrap_or("unknown-model")
        );
        *by_route.entry(route).or_default() += estimate.amount(currency);
        itemized_turns = itemized_turns.saturating_add(1);
    }
    DebugCostProjection {
        currency: super::to_command_currency(currency),
        total,
        parent_turns: components.parent_turns,
        subagents: components.subagents,
        display_floor: components.display_floor,
        priced_turns,
        unpriced_turns,
        legacy_coverage_unknown: app.session.cost_coverage_unknown_legacy,
        user_declared_estimates: app
            .session
            .cost_pricing_provenances
            .contains("user_override"),
        itemized_turns,
        route_amounts: by_route
            .into_iter()
            .map(|(route, amount)| DebugRouteCost { route, amount })
            .collect(),
        turn_history_capacity: App::TURN_CACHE_HISTORY_CAP,
        unpriced_reason_labels: reasons.iter().cloned().collect(),
        unpriced_classes: app.session.cost_unpriced_classes.iter().cloned().collect(),
        pricing_provenances: app
            .session
            .cost_pricing_provenances
            .iter()
            .cloned()
            .collect(),
        live_pricing_defects: app
            .session
            .cost_live_pricing_defects
            .iter()
            .cloned()
            .collect(),
        unusable_pricing_defects: app
            .session
            .cost_live_pricing_unusable_defects
            .iter()
            .cloned()
            .collect(),
        route_receipts: app.session.cost_route_receipts.iter().cloned().collect(),
    }
}

pub(crate) fn prompt_context(context: crate::context_report::PromptContext) -> DebugPromptContext {
    DebugPromptContext {
        schema_version: context.schema_version,
        provider: context.provider,
        model: context.model,
        system_prompt_state: context.system_prompt_state.to_string(),
        tool_catalog_state: context.tool_catalog_state.to_string(),
        sections: context
            .sections
            .into_iter()
            .map(|section| DebugPromptContextSection {
                index: section.index,
                block_type: section.block_type,
                cache_control: section.cache_control.map(|control| DebugCacheControl {
                    cache_type: control.cache_type,
                }),
                estimated_tokens: section.estimated_tokens,
                text: section.text,
            })
            .collect(),
        tools: context
            .tools
            .into_iter()
            .map(|tool| DebugPromptTool {
                tool_type: tool.tool_type,
                name: tool.name,
                description: tool.description,
                input_schema: tool.input_schema,
                allowed_callers: tool.allowed_callers,
                defer_loading: tool.defer_loading,
                input_examples: tool.input_examples,
                strict: tool.strict,
                cache_control: tool.cache_control.map(|control| DebugCacheControl {
                    cache_type: control.cache_type,
                }),
            })
            .collect(),
        source_map: source_map(context.source_map),
    }
}

fn prompt_inspection(inspection: PromptInspection) -> DebugPromptInspection {
    DebugPromptInspection {
        base_static_prefix_hash: inspection.base_static_prefix_hash,
        full_request_prefix_hash: inspection.full_request_prefix_hash,
        tool_catalog_hash: inspection.tool_catalog_hash,
        layers: inspection
            .layers
            .into_iter()
            .map(|layer| DebugPromptLayer {
                name: layer.name,
                stability: match layer.stability {
                    PromptLayerStability::Static => DebugPromptLayerStability::Static,
                    PromptLayerStability::History => DebugPromptLayerStability::History,
                    PromptLayerStability::Dynamic => DebugPromptLayerStability::Dynamic,
                },
                char_len: layer.char_len,
                byte_len: layer.byte_len,
                token_estimate: layer.token_estimate,
                sha256: layer.sha256,
                tool_result: layer.tool_result.map(|result| DebugToolResultInspection {
                    original_chars: result.original_chars,
                    sent_chars: result.sent_chars,
                    truncated: result.truncated,
                    deduplicated: result.deduplicated,
                }),
                turn_meta: layer.turn_meta.map(|meta| DebugTurnMetaInspection {
                    original_chars: meta.original_chars,
                    sent_chars: meta.sent_chars,
                    deduplicated: meta.deduplicated,
                    sha256: meta.sha256,
                }),
            })
            .collect(),
    }
}

fn host_inspection(inspection: DebugPromptInspection) -> PromptInspection {
    use crate::client::{PromptLayerInspection, ToolResultInspection, TurnMetaInspection};
    PromptInspection {
        base_static_prefix_hash: inspection.base_static_prefix_hash,
        full_request_prefix_hash: inspection.full_request_prefix_hash,
        tool_catalog_hash: inspection.tool_catalog_hash,
        layers: inspection
            .layers
            .into_iter()
            .map(|layer| PromptLayerInspection {
                name: layer.name,
                stability: match layer.stability {
                    DebugPromptLayerStability::Static => PromptLayerStability::Static,
                    DebugPromptLayerStability::History => PromptLayerStability::History,
                    DebugPromptLayerStability::Dynamic => PromptLayerStability::Dynamic,
                },
                char_len: layer.char_len,
                byte_len: layer.byte_len,
                token_estimate: layer.token_estimate,
                sha256: layer.sha256,
                tool_result: layer.tool_result.map(|result| ToolResultInspection {
                    original_chars: result.original_chars,
                    sent_chars: result.sent_chars,
                    truncated: result.truncated,
                    deduplicated: result.deduplicated,
                }),
                turn_meta: layer.turn_meta.map(|meta| TurnMetaInspection {
                    original_chars: meta.original_chars,
                    sent_chars: meta.sent_chars,
                    deduplicated: meta.deduplicated,
                    sha256: meta.sha256,
                }),
            })
            .collect(),
    }
}

pub(crate) fn warmup_key(key: CacheWarmupKey) -> DebugWarmupKey {
    DebugWarmupKey {
        provider: key.provider,
        model: key.model,
        base_url: key.base_url,
        static_prefix_hash: key.static_prefix_hash,
        tool_catalog_hash: key.tool_catalog_hash,
        project_pack_hash: key.project_pack_hash,
        skills_hash: key.skills_hash,
    }
}

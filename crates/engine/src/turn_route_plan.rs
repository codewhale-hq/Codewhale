//! The single shared turn-route planner (#1004).
//!
//! One function decides which provider, model, route identity, client, limits,
//! compaction policy, and reasoning tier a turn will use.
//! `spawned_dispatch_inner` calls it to *send* a turn; `/preview-request`
//! calls it with a hypothetical prompt to *describe* one. Because there is a
//! single implementation, a preview cannot report a route different from the
//! one dispatch would pick for the same prompt — which is the whole point of
//! previewing a route before spending anything on it.
//!
//! It lives outside the TUI module so the engine-side preview tests can drive
//! the same planner the UI drives, provider-free.
//!
//! The planner mutates no engine or session state. Its one outbound call is
//! the auto-router classifier, which only runs when auto model routing is on.
//! Production may use the deterministic response cache for that call;
//! `/preview-request` explicitly bypasses it so inspection does not perturb
//! later routing.

use crate::compaction::CompactionConfig;
use crate::reasoning_preference::ReasoningEffort;
use crate::route_runtime::{ResolvedRuntimeRoute, resolve_runtime_route_for_identity};
use codewhale_config::AppMode;
use codewhale_config::ProviderKind;
use codewhale_runtime::config::{Config, ProviderIdentity};

/// Everything the shared turn-route planner needs.
///
/// Borrowed rather than owned so the dispatch path can pass its already
/// captured `UserDispatchPrepare` fields and `/preview-request` can pass a
/// hypothetical prompt, without either one duplicating the other's logic.
pub struct TurnRoutePlanRequest<'a> {
    pub route_config: &'a Config,
    pub app_route_identity: &'a ProviderIdentity,
    pub api_provider: ProviderKind,
    pub app_model: &'a str,
    pub auto_model: bool,
    pub reasoning_effort: ReasoningEffort,
    pub mode: AppMode,
    /// Model-facing content of the next user message (file mentions and skill
    /// wrapping already resolved). This is what the auto router classifies.
    pub content: &'a str,
    pub auto_router_context: &'a str,
    pub should_auto_resolve: bool,
    /// Production dispatch may use the deterministic response cache for the
    /// auxiliary Auto classifier. Read-only previews must set this to false.
    pub allow_auto_router_response_cache: bool,
    pub preflight_required: bool,
    pub auto_compact_user_configured: bool,
    pub auto_compact: bool,
    pub auto_compact_threshold_percent: f64,
}

/// The exact route, limits, compaction policy, and reasoning normalization one
/// turn would use.
pub struct PlannedTurnRoute {
    pub route: ResolvedRuntimeRoute,
    pub compaction: CompactionConfig,
    pub effective_provider: ProviderKind,
    pub effective_model: String,
    pub effective_provider_identity: String,
    pub effective_provider_label: String,
    pub selected_reasoning_effort: Option<ReasoningEffort>,
    /// Normalized api value for the resolved route — the string that reaches
    /// the wire.
    pub effective_reasoning_effort: Option<String>,
    pub auto_controls_reasoning: bool,
    pub auto_selection: Option<crate::model_routing::AutoRouteSelection>,
    /// Bounded auxiliary classifier usage that must enter the accepted turn
    /// under its own frozen routes. It is moved out of `auto_selection` so a
    /// UI-only receipt consumer cannot accidentally become the accounting
    /// owner or price it under the parent route.
    pub initial_routed_usage: crate::cost_status::RuntimeUsageBatch,
    /// Why this concrete route was selected. This is captured by the planner,
    /// not inferred later from the resulting provider/model pair.
    pub routing_source: TurnRoutingSource,
}

/// Durable provenance for the route selected for one turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnRoutingSource {
    /// The active fixed route was used unchanged. This intentionally does not
    /// guess whether an earlier UI action or persisted config installed it.
    ActiveFixedRoute,
    /// Auto model routing used its provider-backed classifier.
    AutoProviderClassifier,
    /// Auto model routing fell back to the local declared default (no
    /// classifier signal; request wording never inspected).
    AutoLocalFallback,
}

impl TurnRoutingSource {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ActiveFixedRoute => "active-fixed-route",
            Self::AutoProviderClassifier => "auto-provider-classifier",
            Self::AutoLocalFallback => "auto-local-fallback",
        }
    }
}

fn reasoning_effort_for_route_selection(
    auto_model: bool,
    provider: ProviderKind,
    effort: ReasoningEffort,
) -> &'static str {
    if auto_model {
        effort.as_setting()
    } else {
        effort.as_setting_for_provider(provider)
    }
}

/// Settle the classifier usage of a turn whose route then failed, into the
/// cost scope that was current when the classifier call was made. A fresh
/// token taken here would bill it to whatever session is current by now:
/// after a `/new` or session load during the call, a different one.
fn settle_failed_parent_route(
    cost_scope: crate::cost_status::CostScopeToken,
    error: String,
    initial_routed_usage: &crate::cost_status::RuntimeUsageBatch,
) -> String {
    crate::cost_status::report_runtime_usage_batch(cost_scope, None, initial_routed_usage);
    error
}

/// Resolve the route for one turn.
///
/// This is *the* route planner (#1004). `spawned_dispatch_inner` calls it to
/// send a turn; `/preview-request` calls it with a hypothetical prompt to
/// describe one. Because there is a single implementation, a preview cannot
/// report a provider, model, route identity, client, reasoning tier, limit,
/// tool budget, billing basis, or endpoint different from the one dispatch
/// would pick for the same prompt.
///
/// It mutates no engine or session state: it reads config, resolves a route,
/// and returns a value. Its one outbound call is the auto-router classifier,
/// which is the same auxiliary call a real turn makes and only runs when auto
/// model routing is on. The caller chooses whether that auxiliary call may
/// touch the process-global deterministic response cache.
pub async fn plan_turn_route(
    request: TurnRoutePlanRequest<'_>,
) -> Result<PlannedTurnRoute, String> {
    // Taken before the classifier call so its usage settles into the scope
    // it was spent in, not the one current when a failure is noticed.
    let cost_scope = crate::cost_status::scope_token();
    let mut auto_selection = if request.should_auto_resolve {
        Some(
            crate::model_routing::resolve_auto_route_with_inventory_for_session_and_cache_policy(
                request.route_config,
                request.content,
                request.auto_router_context,
                request.mode.as_setting(),
                if request.auto_model { "auto" } else { "fixed" },
                reasoning_effort_for_route_selection(
                    request.auto_model,
                    request.api_provider,
                    request.reasoning_effort,
                ),
                request.allow_auto_router_response_cache,
            )
            .await
            .map_err(|err| err.to_string())?,
        )
    } else {
        None
    };

    // Move classifier accounting out immediately. Every later parent-route
    // failure must settle this already-incurred auxiliary call instead of
    // returning an error that silently drops its exact quote/usage.
    let initial_routed_usage = auto_selection
        .as_mut()
        .map(|selection| crate::cost_status::RuntimeUsageBatch {
            decisions: Vec::new(),
            records: std::mem::take(&mut selection.routed_usage),
            drop_records: std::mem::take(&mut selection.routed_usage_drop_records),
            dropped_records: std::mem::take(&mut selection.routed_usage_dropped_records),
        })
        .unwrap_or_default();

    let selected_identity = auto_selection
        .as_ref()
        .map_or(request.app_route_identity, |selection| &selection.provider);
    let effective_provider = selected_identity.provider;

    // Without an Auto selection there is no per-request signal, so the
    // route is the configured model — the same declared default the local
    // fallback uses. Request wording is never inspected (#6290 rework).
    let selected_model = if request.auto_model {
        auto_selection
            .as_ref()
            .map(|selection| selection.model.clone())
            .unwrap_or_else(|| request.app_model.to_string())
    } else {
        request.app_model.to_string()
    };

    let turn_route = resolve_runtime_route_for_identity(
        request.route_config,
        selected_identity,
        Some(&selected_model),
    );

    let turn_route = match turn_route {
        Ok(route) => route,
        Err(err) => {
            return Err(settle_failed_parent_route(
                cost_scope,
                err.to_string(),
                &initial_routed_usage,
            ));
        }
    };
    let turn_route = if request.preflight_required {
        match turn_route.preflight() {
            Ok(route) => route,
            Err(err) => {
                return Err(settle_failed_parent_route(
                    cost_scope,
                    err,
                    &initial_routed_usage,
                ));
            }
        }
    } else {
        turn_route
    };

    let turn_route_limits = crate::route_budget::known_route_limits(turn_route.candidate.limits());
    let effective_provider_identity = turn_route.identity.key.to_string();
    let effective_provider_label = if effective_provider == ProviderKind::Custom {
        effective_provider_identity.clone()
    } else {
        turn_route
            .identity
            .compatibility()
            .map_or(effective_provider.as_str(), |row| row.label)
            .to_string()
    };

    let turn_compaction = CompactionConfig {
        enabled: if request.auto_compact_user_configured {
            request.auto_compact
        } else {
            crate::route_budget::auto_compact_default_for_route(
                turn_route.identity.provider,
                &turn_route.model,
                turn_route_limits,
            )
        },
        token_threshold: crate::route_budget::compaction_threshold_for_route_at_percent_with_window(
            turn_route.identity.provider,
            &turn_route.model,
            turn_route_limits,
            Some(turn_route.context_window.tokens),
            request.auto_compact_threshold_percent,
        ),
        model: turn_route.model.clone(),
        image_input: turn_route.candidate.capabilities().image_input,
        effective_context_window: Some(turn_route.context_window.tokens),
        summary_instructions: request.route_config.compaction_summary_instructions(),
        retained_user_message_tokens: request
            .route_config
            .compaction_retained_user_message_tokens(),
        ..Default::default()
    };

    // Model selection and reasoning selection are independent. A fixed
    // reasoning preference survives auto model routing and is normalized
    // against the concrete route below; only an explicit `auto` delegates the
    // tier to the classifier/declared fallback.
    let auto_controls_reasoning = request.reasoning_effort == ReasoningEffort::Auto;
    let selected_reasoning_effort = if auto_controls_reasoning {
        Some(
            auto_selection
                .as_ref()
                .and_then(|selection| selection.reasoning_effort)
                .unwrap_or_else(crate::auto_reasoning::select),
        )
    } else {
        None
    };

    let effective_reasoning_effort = selected_reasoning_effort
        .unwrap_or(request.reasoning_effort)
        .normalize_for_route_identity(
            effective_provider,
            turn_route.identity.key.as_str(),
            &turn_route.candidate.endpoint().base_url,
            &turn_route.model,
        )
        .api_value_for_provider(effective_provider)
        .map(str::to_string);

    let routing_source = if !request.auto_model {
        TurnRoutingSource::ActiveFixedRoute
    } else if auto_selection.is_some() {
        TurnRoutingSource::AutoProviderClassifier
    } else {
        TurnRoutingSource::AutoLocalFallback
    };

    Ok(PlannedTurnRoute {
        effective_model: turn_route.model.clone(),
        route: turn_route,
        compaction: turn_compaction,
        effective_provider,
        effective_provider_identity,
        effective_provider_label,
        selected_reasoning_effort,
        effective_reasoning_effort,
        auto_controls_reasoning,
        auto_selection,
        initial_routed_usage,
        routing_source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL;

    fn deepseek_identity() -> ProviderIdentity {
        codewhale_runtime::config::Config::default()
            .builtin_provider_identity(ProviderKind::Deepseek)
            .unwrap()
    }

    #[test]
    fn failed_parent_route_settles_classifier_batch_once() {
        let _cost_scope = crate::cost_status::test_scope();
        let route = crate::cost_status::EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Deepseek,
            "deepseek",
            "classifier-model",
            Some(ProviderKind::Deepseek.provider().default_base_url()),
            chrono::Utc::now(),
        );
        let batch = crate::cost_status::RuntimeUsageBatch {
            decisions: Vec::new(),
            records: vec![crate::cost_status::RuntimeUsageRecord {
                source_id: "auto-router:plan-usage".to_string(),
                usage: crate::cost_status::EffectiveRouteUsage {
                    route: route.clone(),
                    usage: codewhale_models::Usage {
                        input_tokens: 4,
                        output_tokens: 2,
                        ..Default::default()
                    },
                },
            }],
            drop_records: vec![crate::cost_status::RuntimeUsageDropRecord {
                reason: crate::cost_status::RuntimeUsageMissingReason::default(),
                source_id: "auto-router:plan-drop".to_string(),
                route,
            }],
            dropped_records: 1,
        };

        let scope = crate::cost_status::scope_token();
        assert_eq!(
            settle_failed_parent_route(scope, "route failed".to_string(), &batch),
            "route failed"
        );
        settle_failed_parent_route(scope, "route failed".to_string(), &batch);
        let pending = crate::cost_status::drain();
        assert_eq!(
            pending.usage_source_fingerprints.len(),
            2,
            "both exact classifier outcomes persist, and replay is idempotent"
        );

        // The classifier ran in one session; `/new` closed it before the
        // route failed. Its cost belongs to the closed session, never the new.
        let spent_in = crate::cost_status::scope_token();
        let _closed = crate::cost_status::close_current_scope();
        let batch = crate::cost_status::RuntimeUsageBatch {
            decisions: Vec::new(),
            records: batch
                .records
                .iter()
                .cloned()
                .map(|mut record| {
                    record.source_id = "auto-router:plan-usage-after-new".to_string();
                    record
                })
                .collect(),
            drop_records: Vec::new(),
            dropped_records: 0,
        };
        settle_failed_parent_route(spent_in, "route failed".to_string(), &batch);
        assert!(
            crate::cost_status::drain()
                .usage_source_fingerprints
                .is_empty(),
            "classifier cost leaked into the session opened after it was spent"
        );
    }

    #[test]
    fn auto_model_route_selection_keeps_raw_reasoning_preference() {
        assert_eq!(
            reasoning_effort_for_route_selection(
                true,
                ProviderKind::OpenaiCodex,
                ReasoningEffort::Off,
            ),
            "off"
        );
        assert_eq!(
            reasoning_effort_for_route_selection(
                false,
                ProviderKind::OpenaiCodex,
                ReasoningEffort::Off,
            ),
            "low"
        );
    }

    #[tokio::test]
    async fn auto_model_route_respects_fixed_reasoning_preference() {
        let config = Config::default();
        let identity = deepseek_identity();

        let planned = plan_turn_route(TurnRoutePlanRequest {
            route_config: &config,
            app_route_identity: &identity,
            api_provider: ProviderKind::Deepseek,
            app_model: DEFAULT_TEXT_MODEL,
            auto_model: true,
            reasoning_effort: ReasoningEffort::Low,
            mode: AppMode::Agent,
            content: "explain this function",
            auto_router_context: "",
            should_auto_resolve: false,
            allow_auto_router_response_cache: false,
            preflight_required: false,
            auto_compact_user_configured: false,
            auto_compact: true,
            auto_compact_threshold_percent: 80.0,
        })
        .await
        .expect("plan auto-model turn");

        assert_eq!(planned.routing_source, TurnRoutingSource::AutoLocalFallback);
        assert!(!planned.auto_controls_reasoning);
        assert_eq!(planned.selected_reasoning_effort, None);
        // First-party DeepSeek routes carry low as the real wire tier
        // (`reasoning_effort` low/high/max are documented); the App keeps the
        // unresolved preference as Low either way.
        assert_eq!(planned.effective_reasoning_effort.as_deref(), Some("low"));
    }
}

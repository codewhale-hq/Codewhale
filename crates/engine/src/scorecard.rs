//! Token / cache / cost scorecard (#3388).
//!
//! A release-gate view of an agent run's token economics: per-turn input /
//! output / cache-read tokens and cost, aggregate totals + cache-hit ratio, and
//! regression detection against a committed baseline. This is the measurement
//! layer the "token, cache, and context discipline" EPIC asks for — it makes a
//! cost/token regression visible instead of silently shipping.
//!
//! The core here is pure and offline: it turns already-recorded per-turn
//! [`Usage`] (captured on every turn, persisted in `TurnRecord`) into a
//! scorecard, reusing the existing pricing layer rather than reinventing cost
//! math. The `scorecard` subcommand is a thin I/O wrapper over this module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::pricing::{
    CostEstimate, TurnCostAudit, audit_turn_cost_for_route_at, token_usage_for_pricing,
};
use codewhale_config::ProviderKind;
use codewhale_models::Usage;
#[cfg(test)]
use codewhale_runtime::config::DEEPSEEK_ALIAS_RETIREMENT_UTC;

/// One turn's normalized token economics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnScore {
    pub turn_id: String,
    /// Timestamp used for historical/time-window pricing. `None` means the
    /// recorder did not preserve when the turn occurred.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,
    /// Effective provider recorded for this turn. `None` means legacy or
    /// otherwise unknown provenance, so cost must remain unpriced.
    #[serde(default)]
    pub provider: Option<String>,
    /// Non-secret discriminator when one provider/model pair spans multiple
    /// billing systems. Missing provenance keeps ambiguous routes unpriced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_surface: Option<String>,
    pub model: String,
    /// Non-cached (billable) input tokens.
    pub input_tokens: u64,
    /// Output tokens, including reasoning output.
    pub output_tokens: u64,
    /// Cache-read (cache-hit) input tokens.
    pub cache_read_tokens: u64,
    /// Cache-write (cache-creation) input tokens. Billed at a premium on the
    /// providers that publish one, so it is audited as its own class rather
    /// than folded into input. Defaults to 0 for legacy records.
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// Reasoning tokens reported for the turn. **Informational only** — every
    /// provider counts these inside `output_tokens`, so adding them here would
    /// double-bill. Kept so a reasoning-heavy run can still be inspected.
    #[serde(default)]
    pub reasoning_tokens: u64,
    pub cost_usd: f64,
    pub cost_cny: f64,
    /// True when provider provenance is missing/unknown or no authoritative USD
    /// pricing row exists: numeric cost stays 0 for compatibility, while this
    /// flag prevents it from being represented as a real zero-dollar charge.
    pub cost_unpriced: bool,
    /// Same availability marker for CNY. Most catalog offerings publish only
    /// USD, so their CNY value is unavailable rather than a real zero.
    #[serde(default)]
    pub cost_cny_unpriced: bool,
    /// Why USD cost is unavailable, when it is (`no_pricing_row`,
    /// `missing_class_price`, `not_money_metered`, …). `None` for priced turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_unpriced_reason: Option<String>,
    /// Token classes this turn used that carry no published price. Non-empty
    /// means the estimate failed closed on purpose.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unpriced_classes: Vec<String>,
    /// Provenance of the pricing row that was applied or attempted
    /// (`models_dev_bundled`, `provider_live`, `provider_docs`,
    /// `user_override`). `None` when no row was found at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing_provenance: Option<String>,
    /// Live-pricing downgrade receipt: the live catalog row for this route could
    /// not be verified (stale, or fetched from a different endpoint), so the
    /// bundled published rates were used. Present even on priced turns, because
    /// it explains *which* row the number came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_pricing_defect: Option<String>,
    /// Whether this turn is inside the money-metered coverage denominator.
    ///
    /// False only for routes exactly identified as non-metered. Serialized so a
    /// re-read scorecard can reproduce the coverage split without re-deriving it
    /// from `provider` + `billing_surface`, which are also preserved above.
    #[serde(default)]
    pub money_metered: bool,
}

/// Aggregate metrics for a run. Serializes/deserializes as the baseline file.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ScorecardMetrics {
    pub turns: usize,
    /// Turns whose route meters money, or whose billing basis could not be
    /// established. This — not `turns` — is the denominator the USD total is
    /// meant to cover: a local or subscription turn owes no dollars, so counting
    /// it would understate coverage, while an unknown-basis turn must stay in
    /// (#4318). Defaults to zero so existing baseline JSON stays readable.
    #[serde(default)]
    pub money_metered_turns: usize,
    /// Money-metered turns that could not be priced authoritatively in USD.
    /// Defaults to zero so existing baseline JSON remains readable.
    #[serde(default)]
    pub unpriced_turns: usize,
    /// Turns without authoritative CNY pricing.
    #[serde(default)]
    pub cny_unpriced_turns: usize,
    /// Whether every turn contributed authoritative USD pricing. Legacy
    /// baselines lack this field and therefore default to `false`, preventing
    /// comparisons against totals that may have been inferred from model ids
    /// alone.
    #[serde(default)]
    pub cost_complete: bool,
    /// Whether every turn contributed authoritative CNY pricing.
    #[serde(default)]
    pub cny_cost_complete: bool,
    /// Token classes used somewhere in the run that had no published price, in
    /// stable order. Non-empty means `cost_complete` is false *because* of a
    /// class-level pricing gap, not merely an unknown route.
    #[serde(default)]
    pub unpriced_classes: Vec<String>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_read_tokens: u64,
    /// Cache-write (cache-creation) tokens across the run. Defaults to zero so
    /// existing baseline JSON stays readable.
    #[serde(default)]
    pub total_cache_write_tokens: u64,
    /// Reasoning tokens across the run. Informational: already inside
    /// `total_output_tokens`, never added to it.
    #[serde(default)]
    pub total_reasoning_tokens: u64,
    pub total_cost_usd: f64,
    pub total_cost_cny: f64,
    /// `cache_read / (input + cache_read)`; `0.0` when there are no input
    /// tokens. Higher is better (more of the prompt was served from cache).
    pub cache_hit_ratio: f64,
}

/// A metric that grew beyond the allowed threshold versus the baseline.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Regression {
    pub metric: String,
    pub baseline: f64,
    pub current: f64,
    /// Percent increase over baseline. `f64::INFINITY` when baseline was 0.
    pub pct_increase: f64,
}

fn cacheable_token_total(input: u64, cache_read: u64, cache_write: u64) -> u64 {
    input.saturating_add(cache_read).saturating_add(cache_write)
}

/// Full scorecard: per-turn breakdown plus aggregates.
#[derive(Debug, Clone, Serialize)]
pub struct Scorecard {
    pub per_turn: Vec<TurnScore>,
    pub metrics: ScorecardMetrics,
}

/// One row of input to the scorecard: a turn id, the model that served it, and
/// the turn's recorded usage.
///
/// `billing_surface` is explicit and has no default. The scorecard has two
/// entry modes, and they must agree: if this fixture mode could silently supply
/// an official first-party surface, every scorecard test would be asserting
/// against a route classification that `from_recorded_turns` never invents, and
/// the fail-closed path would go unexercised in the mode the tests use.
#[cfg(any(test, feature = "test-support"))]
pub struct TurnInput<'a> {
    pub turn_id: String,
    pub created_at: Option<&'a DateTime<Utc>>,
    pub provider: Option<&'a str>,
    /// The route's recorded billing surface, or `None` when the recording did
    /// not establish one. `None` must price exactly as it does for a recorded
    /// turn: unknown, never official.
    pub billing_surface: Option<&'a str>,
    pub model: String,
    pub usage: &'a Usage,
}

#[derive(Debug, Clone, Copy)]
struct ScorecardTurnRef<'a> {
    turn_id: &'a str,
    created_at: Option<&'a DateTime<Utc>>,
    provider: Option<&'a str>,
    billing_surface: Option<&'a str>,
    model: &'a str,
    usage: &'a Usage,
}

/// A recorded turn as read from a scorecard input file (a JSON array of these).
/// The base shape matches the per-turn data a `TurnEnd` hook emits. Recorders
/// and persisted runtime exports can add `provider` / `effective_provider` plus
/// non-secret billing-surface provenance. Legacy model-only recordings remain
/// readable but deliberately unpriced.
#[derive(Debug, Clone, Deserialize)]
pub struct RecordedTurn {
    #[serde(default, alias = "id")]
    pub turn_id: String,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// New `turn_end` hooks mark shell-only lifecycle records false so the
    /// model-cost scorecard can ignore them. Missing stays compatible with
    /// legacy hook rows and persisted runtime turns, which are model-backed.
    #[serde(default)]
    pub model_backed: Option<bool>,
    #[serde(default, alias = "effective_provider")]
    pub provider: Option<String>,
    #[serde(default, alias = "effective_billing_surface")]
    pub billing_surface: Option<String>,
    #[serde(default, alias = "effective_model")]
    pub model: String,
    #[serde(default)]
    pub usage: Option<Usage>,
}

impl RecordedTurn {
    #[must_use]
    pub fn contributes_to_scorecard(&self) -> bool {
        self.model_backed.unwrap_or(true) && self.usage.is_some() && !self.model.trim().is_empty()
    }
}

#[derive(Debug, Clone, Default)]
struct AvailableCost {
    usd: Option<f64>,
    cny: Option<f64>,
    unpriced_reason: Option<String>,
    unpriced_classes: Vec<String>,
    provenance: Option<String>,
    /// Live-pricing downgrade receipt, when the row used was a bundled fallback
    /// for an unverifiable live row.
    live_pricing_defect: Option<String>,
    /// Whether this turn belongs in the money-metered coverage denominator.
    /// False only for routes *exactly* identified as non-metered.
    counts_toward_money_coverage: bool,
}

impl AvailableCost {
    /// Legacy/unknown provenance: no route to price against at all.
    ///
    /// This still counts toward money coverage. A recording whose provider text
    /// CodeWhale cannot parse is a turn whose spend is unknown, not a turn that
    /// cost nothing — excusing it would let a legacy input file report a complete
    /// total (#4318).
    fn unknown_route() -> Self {
        Self {
            unpriced_reason: Some("unknown_route".to_string()),
            counts_toward_money_coverage: true,
            ..Self::default()
        }
    }

    /// Fails closed with an explicit reason, still inside money coverage.
    fn failed_closed(reason: &str) -> Self {
        Self {
            unpriced_reason: Some(reason.to_string()),
            counts_toward_money_coverage: true,
            ..Self::default()
        }
    }

    fn from_audit(audit: &TurnCostAudit) -> Self {
        Self {
            usd: audit
                .estimate
                .and_then(|cost| audit.usd_priced.then_some(cost.usd)),
            cny: audit
                .estimate
                .and_then(|cost| audit.cny_priced.then_some(cost.cny)),
            unpriced_reason: audit
                .unpriced_reason
                .map(|reason| reason.label().to_string()),
            unpriced_classes: audit
                .unpriced_classes
                .iter()
                .map(|class| class.label().to_string())
                .collect(),
            provenance: audit
                .provenance
                .as_ref()
                .map(|provenance| provenance.label().to_string()),
            live_pricing_defect: audit
                .live_pricing_defect
                .as_ref()
                .map(|defect| defect.label().to_string()),
            counts_toward_money_coverage: audit.counts_toward_money_coverage(),
        }
    }
}

fn provider_scoped_cost(
    provider: ProviderKind,
    model: &str,
    usage: &Usage,
    created_at: Option<&DateTime<Utc>>,
    billing_surface: Option<&str>,
) -> AvailableCost {
    // These provider identities are themselves exact billing provenance and
    // override stale/junk recorded surfaces: they cannot become PAYG merely
    // because an older recorder wrote a bogus endpoint classification.
    let intrinsic_surface = match provider {
        ProviderKind::Ollama | ProviderKind::Sglang | ProviderKind::Vllm => {
            Some(crate::pricing::LOCAL_BILLING_SURFACE)
        }
        ProviderKind::OpenaiCodex | ProviderKind::OpencodeGo => {
            Some(crate::pricing::OAUTH_SUBSCRIPTION_BILLING_SURFACE)
        }
        _ => None,
    };
    let billing_surface = intrinsic_surface.or(billing_surface);
    // Every provider that supports both PAYG and plan/OAuth routes needs the
    // recorded surface to choose between them. A model id or provider name is
    // not sufficient evidence in an offline scorecard.
    if billing_surface.is_none()
        && matches!(
            provider,
            ProviderKind::Zai
                | ProviderKind::Moonshot
                | ProviderKind::Anthropic
                | ProviderKind::XiaomiMimo
                | ProviderKind::Xai
                | ProviderKind::Minimax
                | ProviderKind::MinimaxAnthropic
                | ProviderKind::Stepfun
                | ProviderKind::Custom
        )
    {
        return AvailableCost::failed_closed("missing_billing_surface");
    }
    let direct_deepseek = matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    );
    let normalized_model = model.trim();
    let model_lower = normalized_model.to_ascii_lowercase();
    // Every direct DeepSeek first-party rate is time-windowed now — the
    // V4 flash/pro rows carry peak/off-peak tiers (01:00–04:00 and
    // 06:00–10:00 UTC on weekdays, with the whole of a Beijing-time Saturday
    // and Sunday billing off-peak from 2026-08-23), and the retired
    // `deepseek-chat` / `deepseek-reasoner` aliases price through them — so an
    // undated DeepSeek turn cannot be resolved to one price. The weekend is
    // bounded in Beijing time, which is why the window it covers is not the
    // one a UTC weekday would give. `claude-sonnet-5` keeps the same recorded-time
    // contract it had during its introductory window (Anthropic later made
    // that $2/$10 rate permanent; the row still prices at the turn's own time
    // rather than the wall clock, and undated turns still fail closed).
    let needs_recorded_time = direct_deepseek
        || (provider == ProviderKind::Anthropic && model_lower == "claude-sonnet-5");
    let recorded_at = match (created_at, needs_recorded_time) {
        (Some(recorded_at), _) => recorded_at.to_owned(),
        // A time-windowed rate without a recorded time cannot be resolved to a
        // single price; fail closed rather than guess a window.
        (None, true) => return AvailableCost::failed_closed("missing_recorded_time"),
        (None, false) => Utc::now(),
    };

    // The billing surface recorded with the turn is authoritative over any
    // provider-level assumption, and it now covers every classification a route
    // can carry — Z.ai Coding Plan, Kimi Code, MiniMax Token Plan, MiMo token
    // plan, OAuth brokers, local runtimes, aggregators, first-party PAYG, and
    // "unclassified" — not just StepFun's two surfaces (#4318).
    match crate::pricing::endpoint_metering_for_billing_surface(billing_surface) {
        // Exactly identified as non-metered: no dollar figure is owed, and the
        // turn leaves the money-coverage denominator.
        crate::pricing::EndpointMetering::ExactSubscription
        | crate::pricing::EndpointMetering::LocalNoBill => {
            return AvailableCost {
                unpriced_reason: Some(
                    crate::pricing::UnpricedReason::NotMoneyMetered
                        .label()
                        .to_string(),
                ),
                counts_toward_money_coverage: false,
                ..AvailableCost::default()
            };
        }
        // A recorded surface CodeWhale cannot place must not inherit the
        // provider's default rates.
        crate::pricing::EndpointMetering::Unknown if billing_surface.is_some() => {
            return AvailableCost::failed_closed(
                crate::pricing::UnpricedReason::UnknownBillingBasis.label(),
            );
        }
        crate::pricing::EndpointMetering::Unknown | crate::pricing::EndpointMetering::Money => {}
    }

    // The pricing layer owns the exact provider/model catalog gate, explicit
    // first-party hand-price allowlist, cache-class completeness checks, and
    // endpoint-derived billing surfaces. Keeping one route-aware path prevents
    // the scorecard from drifting back to model-only pricing.
    let audit = audit_turn_cost_for_route_at(
        provider,
        normalized_model,
        billing_surface,
        usage,
        recorded_at,
    );
    AvailableCost::from_audit(&audit)
}

impl Scorecard {
    /// Build a scorecard from recorded per-turn usage. Pure + offline; cost is
    /// computed via the shared pricing layer (`None` pricing → unpriced, 0 cost).
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_turns(turns: &[TurnInput<'_>]) -> Self {
        Self::from_turn_refs(turns.iter().map(|turn| ScorecardTurnRef {
            turn_id: &turn.turn_id,
            created_at: turn.created_at,
            provider: turn.provider,
            billing_surface: turn.billing_surface,
            model: &turn.model,
            usage: turn.usage,
        }))
    }

    /// Build directly from hook/runtime records, retaining billing provenance
    /// while excluding explicitly non-model lifecycle rows.
    #[must_use]
    pub fn from_recorded_turns(turns: &[RecordedTurn]) -> Self {
        Self::from_turn_refs(turns.iter().filter_map(|turn| {
            if !turn.contributes_to_scorecard() {
                return None;
            }
            let usage = turn.usage.as_ref()?;
            Some(ScorecardTurnRef {
                turn_id: &turn.turn_id,
                created_at: turn.created_at.as_ref(),
                provider: turn.provider.as_deref(),
                billing_surface: turn.billing_surface.as_deref(),
                model: &turn.model,
                usage,
            })
        }))
    }

    fn from_turn_refs<'a>(turns: impl IntoIterator<Item = ScorecardTurnRef<'a>>) -> Self {
        let turns = turns.into_iter();
        let mut per_turn = Vec::with_capacity(turns.size_hint().0);
        let mut metrics = ScorecardMetrics::default();
        let mut unpriced_classes = std::collections::BTreeSet::new();

        for turn in turns {
            // Normalize provider usage into canonical billable classes once.
            let classes = token_usage_for_pricing(turn.usage);
            let provider = turn
                .provider
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let cost = provider.and_then(ProviderKind::parse).map_or_else(
                AvailableCost::unknown_route,
                |provider| {
                    provider_scoped_cost(
                        provider,
                        turn.model,
                        turn.usage,
                        turn.created_at,
                        turn.billing_surface,
                    )
                },
            );
            let cost_unpriced = cost.usd.is_none();
            let cost_cny_unpriced = cost.cny.is_none();
            let cost_usd = cost.usd.unwrap_or(0.0);
            let cost_cny = cost.cny.unwrap_or(0.0);
            let reasoning_tokens = u64::from(turn.usage.reasoning_tokens.unwrap_or(0));
            unpriced_classes.extend(cost.unpriced_classes.iter().cloned());

            metrics.turns = metrics.turns.saturating_add(1);
            // Only money-metered turns can make a dollar total incomplete. A
            // local or plan turn is not an unpriced dollar; an *unknown* one is.
            if cost.counts_toward_money_coverage {
                metrics.money_metered_turns = metrics.money_metered_turns.saturating_add(1);
                metrics.unpriced_turns = metrics
                    .unpriced_turns
                    .saturating_add(usize::from(cost_unpriced));
                metrics.cny_unpriced_turns = metrics
                    .cny_unpriced_turns
                    .saturating_add(usize::from(cost_cny_unpriced));
            }
            metrics.total_input_tokens = metrics.total_input_tokens.saturating_add(classes.input);
            metrics.total_output_tokens =
                metrics.total_output_tokens.saturating_add(classes.output);
            metrics.total_cache_read_tokens = metrics
                .total_cache_read_tokens
                .saturating_add(classes.cache_read);
            metrics.total_cache_write_tokens = metrics
                .total_cache_write_tokens
                .saturating_add(classes.cache_write);
            metrics.total_reasoning_tokens = metrics
                .total_reasoning_tokens
                .saturating_add(reasoning_tokens);
            metrics.total_cost_usd = CostEstimate::usd_only(metrics.total_cost_usd)
                .saturating_add(CostEstimate::usd_only(cost_usd))
                .usd;
            metrics.total_cost_cny = CostEstimate {
                usd: 0.0,
                cny: metrics.total_cost_cny,
            }
            .saturating_add(CostEstimate {
                usd: 0.0,
                cny: cost_cny,
            })
            .cny;

            per_turn.push(TurnScore {
                turn_id: turn.turn_id.to_string(),
                created_at: turn.created_at.cloned(),
                provider: provider.map(str::to_string),
                billing_surface: turn.billing_surface.map(str::to_string),
                model: turn.model.to_string(),
                input_tokens: classes.input,
                output_tokens: classes.output,
                cache_read_tokens: classes.cache_read,
                cache_write_tokens: classes.cache_write,
                reasoning_tokens,
                cost_usd,
                cost_cny,
                cost_unpriced,
                cost_cny_unpriced,
                cost_unpriced_reason: cost.unpriced_reason,
                unpriced_classes: cost.unpriced_classes,
                pricing_provenance: cost.provenance,
                live_pricing_defect: cost.live_pricing_defect,
                money_metered: cost.counts_toward_money_coverage,
            });
        }
        metrics.unpriced_classes = unpriced_classes.into_iter().collect();

        // Canonical denominator: hit / (non-cached input + hit + write).
        // `total_input_tokens` here is already the *non-cached* input, because
        // `token_usage_for_pricing` splits hits and writes out of the reported
        // prompt total. Cache-write tokens were previously missing from the
        // denominator, which reported a better hit ratio on precisely the turns
        // that paid to populate the cache (#4318). Write stays a separate
        // reported total so the premium is not hidden inside the ratio.
        let cacheable = cacheable_token_total(
            metrics.total_input_tokens,
            metrics.total_cache_read_tokens,
            metrics.total_cache_write_tokens,
        );
        metrics.cache_hit_ratio = if cacheable > 0 {
            metrics.total_cache_read_tokens as f64 / cacheable as f64
        } else {
            0.0
        };
        metrics.cost_complete = metrics.unpriced_turns == 0;
        metrics.cny_cost_complete = metrics.cny_unpriced_turns == 0;

        Self { per_turn, metrics }
    }

    /// Render a compact human-readable summary (used for non-JSON output).
    #[must_use]
    pub fn to_summary(&self) -> String {
        let m = &self.metrics;
        let mut out = String::new();
        out.push_str("Token / cache / cost scorecard\n");
        out.push_str(&format!(
            "turns: {}  money_metered_turns: {}\n",
            m.turns, m.money_metered_turns
        ));
        out.push_str(&format!(
            "input_tokens: {}  output_tokens: {}  cache_read_tokens: {}  cache_write_tokens: {}\n",
            m.total_input_tokens,
            m.total_output_tokens,
            m.total_cache_read_tokens,
            m.total_cache_write_tokens
        ));
        out.push_str(&format!(
            "reasoning_tokens: {} (informational; already inside output_tokens)\n",
            m.total_reasoning_tokens
        ));
        out.push_str(&format!(
            "cache_hit_ratio: {:.1}%\n",
            m.cache_hit_ratio * 100.0
        ));
        append_currency_summary(
            &mut out,
            "cost_usd",
            "priced_cost_subtotal_usd",
            "$",
            m.total_cost_usd,
            m.unpriced_turns,
            // Coverage is reported against the money-metered turns, not every
            // turn: a local or plan turn owes no dollars, so including it in the
            // denominator would understate how complete the figure is.
            m.money_metered_turns,
        );
        append_currency_summary(
            &mut out,
            "cost_cny",
            "priced_cost_subtotal_cny",
            "¥",
            m.total_cost_cny,
            m.cny_unpriced_turns,
            m.money_metered_turns,
        );
        if m.unpriced_turns > 0 {
            out.push_str(&format!(
                "note: {} turn(s) had missing/unknown provider provenance or no authoritative USD pricing row; their USD cost is unavailable and excluded.\n",
                m.unpriced_turns
            ));
        }
        if m.cny_unpriced_turns > 0 {
            out.push_str(&format!(
                "note: {} turn(s) had no authoritative CNY pricing row; their CNY cost is unavailable and excluded.\n",
                m.cny_unpriced_turns
            ));
        }
        if !m.unpriced_classes.is_empty() {
            out.push_str(&format!(
                "note: token class(es) with no published price on a used route: {}. Those turns fail closed rather than under-report.\n",
                m.unpriced_classes.join(", ")
            ));
        }
        out
    }
}

fn append_currency_summary(
    out: &mut String,
    complete_label: &str,
    subtotal_label: &str,
    symbol: &str,
    total: f64,
    unpriced_turns: usize,
    turns: usize,
) {
    if unpriced_turns == 0 {
        out.push_str(&format!("{complete_label}: {symbol}{total:.4}\n"));
    } else if unpriced_turns == turns {
        out.push_str(&format!("{complete_label}: unavailable\n"));
    } else {
        out.push_str(&format!("{subtotal_label}: {symbol}{total:.4}\n"));
    }
}

impl ScorecardMetrics {
    /// Flag metrics that grew more than `threshold_pct` over `baseline`. Cost
    /// and token counts are "lower is better", so only *increases* are
    /// regressions. (Cache-hit ratio is the opposite, reported separately.)
    ///
    /// Fails closed on values that cannot be compared: a non-finite threshold
    /// admits no growth at all (it is treated as 0%), and a metric whose
    /// change is `NaN` counts as regressed. Every `NaN` comparison is false,
    /// so without this a `NaN` threshold would pass any run.
    #[must_use]
    pub fn regressions_against(
        &self,
        baseline: &ScorecardMetrics,
        threshold_pct: f64,
    ) -> Vec<Regression> {
        let threshold_pct = if threshold_pct.is_finite() {
            threshold_pct
        } else {
            0.0
        };
        let mut out = Vec::new();
        // A partial/unknown subtotal is not comparable to a complete baseline,
        // but losing completeness is itself a regression. Otherwise removing
        // provider provenance could turn real spend into a smaller subtotal
        // and silently bypass the release gate.
        if baseline.cost_complete && !self.cost_complete {
            out.push(Regression {
                metric: "cost_completeness_drop".to_string(),
                baseline: 1.0,
                current: 0.0,
                pct_increase: 100.0,
            });
        } else if self.cost_complete && baseline.cost_complete {
            push_regression(
                &mut out,
                "total_cost_usd",
                baseline.total_cost_usd,
                self.total_cost_usd,
                threshold_pct,
            );
        }
        if baseline.cny_cost_complete && !self.cny_cost_complete {
            out.push(Regression {
                metric: "cny_cost_completeness_drop".to_string(),
                baseline: 1.0,
                current: 0.0,
                pct_increase: 100.0,
            });
        } else if self.cny_cost_complete && baseline.cny_cost_complete {
            push_regression(
                &mut out,
                "total_cost_cny",
                baseline.total_cost_cny,
                self.total_cost_cny,
                threshold_pct,
            );
        }
        push_regression(
            &mut out,
            "total_input_tokens",
            baseline.total_input_tokens as f64,
            self.total_input_tokens as f64,
            threshold_pct,
        );
        push_regression(
            &mut out,
            "total_output_tokens",
            baseline.total_output_tokens as f64,
            self.total_output_tokens as f64,
            threshold_pct,
        );
        // Cache-hit ratio regresses when it *drops*; express the drop as a
        // positive percentage so it reads like the others.
        if baseline.cache_hit_ratio > 0.0 {
            let drop_pct = (baseline.cache_hit_ratio - self.cache_hit_ratio)
                / baseline.cache_hit_ratio
                * 100.0;
            if drop_pct.is_nan() || drop_pct > threshold_pct {
                out.push(Regression {
                    metric: "cache_hit_ratio_drop".to_string(),
                    baseline: baseline.cache_hit_ratio,
                    current: self.cache_hit_ratio,
                    pct_increase: drop_pct,
                });
            }
        }
        out
    }
}

fn push_regression(
    out: &mut Vec<Regression>,
    metric: &str,
    base: f64,
    cur: f64,
    threshold_pct: f64,
) {
    if base > 0.0 {
        let pct = (cur - base) / base * 100.0;
        if pct.is_nan() || pct > threshold_pct {
            out.push(Regression {
                metric: metric.to_string(),
                baseline: base,
                current: cur,
                pct_increase: pct,
            });
        }
    } else if cur > 0.0 {
        out.push(Regression {
            metric: metric.to_string(),
            baseline: base,
            current: cur,
            pct_increase: f64::INFINITY,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u32, output: u32, cache_hit: u32) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            prompt_cache_hit_tokens: Some(cache_hit),
            ..Default::default()
        }
    }

    /// The scorecard has two entry modes and they must classify identically.
    ///
    /// `from_turns` is the fixture mode used by this test module;
    /// `from_recorded_turns` is the mode that reads real recordings. The
    /// fixture mode used to inject `first-party-payg` for every row, which
    /// meant the whole suite was asserting against a route classification the
    /// real mode never produces — the fail-closed path was untested precisely
    /// where it mattered. Neither mode may invent an official surface.
    #[test]
    fn both_scorecard_entry_modes_agree_on_an_unestablished_billing_surface() {
        let sample = usage(10_000, 1_000, 0);

        let fixture = Scorecard::from_turns(&[TurnInput {
            turn_id: "t1".into(),
            created_at: None,
            provider: Some("anthropic"),
            billing_surface: None,
            model: "claude-haiku-4-5".into(),
            usage: &sample,
        }]);
        let recorded = Scorecard::from_recorded_turns(&[RecordedTurn {
            turn_id: "t1".to_string(),
            created_at: None,
            model_backed: None,
            provider: Some("anthropic".to_string()),
            billing_surface: None,
            model: "claude-haiku-4-5".to_string(),
            usage: Some(sample.clone()),
        }]);

        assert_eq!(fixture.per_turn, recorded.per_turn);
        assert_eq!(fixture.metrics, recorded.metrics);
        assert!(
            fixture.per_turn[0].cost_unpriced,
            "a route with no established surface must not be priced"
        );
        assert_eq!(fixture.per_turn[0].cost_usd, 0.0);
        assert!(!fixture.metrics.cost_complete);
        assert_eq!(fixture.metrics.money_metered_turns, 1);
        assert_eq!(fixture.metrics.unpriced_turns, 1);
        let summary = fixture.to_summary();
        assert!(summary.contains("cost_usd: unavailable"), "{summary}");
        assert!(summary.contains("cost_cny: unavailable"), "{summary}");
        assert!(
            !summary.contains('$') && !summary.contains('¥'),
            "an unpriced-only run must name no amount at all: {summary}"
        );

        // With the surface actually established, both modes price it — and
        // still agree.
        let priced_fixture = Scorecard::from_turns(&[TurnInput {
            turn_id: "t1".into(),
            created_at: None,
            provider: Some("anthropic"),
            billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
            model: "claude-haiku-4-5".into(),
            usage: &sample,
        }]);
        let priced_recorded = Scorecard::from_recorded_turns(&[RecordedTurn {
            turn_id: "t1".to_string(),
            created_at: None,
            model_backed: None,
            provider: Some("anthropic".to_string()),
            billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE.to_string()),
            model: "claude-haiku-4-5".to_string(),
            usage: Some(sample),
        }]);
        assert_eq!(priced_fixture.per_turn, priced_recorded.per_turn);
        assert!(!priced_fixture.per_turn[0].cost_unpriced);
        assert!(priced_fixture.metrics.cost_complete);
    }

    #[test]
    fn dual_mode_routes_require_surface_but_intrinsic_routes_override_junk() {
        let usage = usage(10_000, 1_000, 0);
        for (provider, model) in [
            (ProviderKind::Anthropic, "claude-haiku-4-5"),
            (ProviderKind::Moonshot, "kimi-k2.7-code"),
            (ProviderKind::Zai, "glm-5.2"),
            (ProviderKind::Minimax, "minimax-m3"),
        ] {
            let cost = provider_scoped_cost(provider, model, &usage, None, None);
            assert_eq!(
                cost.unpriced_reason.as_deref(),
                Some("missing_billing_surface"),
                "{provider:?}"
            );
            assert!(cost.usd.is_none(), "{provider:?}");
        }

        for provider in [ProviderKind::OpenaiCodex, ProviderKind::OpencodeGo] {
            let cost = provider_scoped_cost(
                provider,
                "gpt-5.5",
                &usage,
                None,
                Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
            );
            assert_eq!(cost.unpriced_reason.as_deref(), Some("not_money_metered"));
            assert!(!cost.counts_toward_money_coverage);
        }
        let local = provider_scoped_cost(
            ProviderKind::Ollama,
            "llama3.2",
            &usage,
            None,
            Some(crate::pricing::UNCLASSIFIED_BILLING_SURFACE),
        );
        assert_eq!(local.unpriced_reason.as_deref(), Some("not_money_metered"));
        assert!(!local.counts_toward_money_coverage);

        let cloud = provider_scoped_cost(
            ProviderKind::OllamaCloud,
            codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_CLOUD_MODEL,
            &usage,
            None,
            Some(crate::pricing::UNCLASSIFIED_BILLING_SURFACE),
        );
        assert_eq!(
            cloud.unpriced_reason.as_deref(),
            Some("unknown_billing_basis")
        );
        assert!(
            cloud.counts_toward_money_coverage,
            "hosted Cloud usage must never disappear as local/free"
        );
    }

    fn cache_write_usage(input: u32, output: u32, cache_hit: u32, cache_write: u32) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            prompt_cache_hit_tokens: Some(cache_hit),
            prompt_cache_write_tokens: Some(cache_write),
            reasoning_tokens: Some(output / 2),
            ..Default::default()
        }
    }

    /// A mixed-route run: one fully-priced cache-write turn, one turn whose
    /// route publishes no cache-write rate, and one non-metered OAuth turn.
    /// The priced subtotal stays honest, `cost_complete` fails closed, and the
    /// audit names the class and provenance behind each gap.
    #[test]
    fn mixed_route_run_audits_cache_write_classes_and_fails_closed() {
        // 1M input of which 200k is a cache read, 100k is a cache write.
        let priced = cache_write_usage(1_000_000, 100_000, 200_000, 100_000);
        let unpriced_write = cache_write_usage(1_000_000, 100_000, 200_000, 100_000);
        let oauth = cache_write_usage(1_000_000, 100_000, 200_000, 100_000);
        let turns = [
            TurnInput {
                turn_id: "anthropic".into(),
                created_at: None,
                provider: Some("anthropic"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "claude-haiku-4-5".into(),
                usage: &priced,
            },
            TurnInput {
                turn_id: "moonshot".into(),
                created_at: None,
                provider: Some("moonshot"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "kimi-k2.7-code".into(),
                usage: &unpriced_write,
            },
            TurnInput {
                turn_id: "oauth".into(),
                created_at: None,
                provider: Some("openai-codex"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5.5".into(),
                usage: &oauth,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        // Cache-write tokens are their own audited class on every turn.
        for turn in &card.per_turn {
            assert_eq!(turn.cache_write_tokens, 100_000, "{}", turn.turn_id);
            assert_eq!(turn.input_tokens, 700_000, "{}", turn.turn_id);
            assert_eq!(turn.cache_read_tokens, 200_000, "{}", turn.turn_id);
            // Reasoning stays informational: never added to billable output.
            assert_eq!(turn.output_tokens, 100_000, "{}", turn.turn_id);
            assert_eq!(turn.reasoning_tokens, 50_000, "{}", turn.turn_id);
        }
        assert_eq!(card.metrics.total_cache_write_tokens, 300_000);
        assert_eq!(card.metrics.total_output_tokens, 300_000);
        assert_eq!(card.metrics.total_reasoning_tokens, 150_000);

        // Anthropic publishes a 1.25/M cache-write rate, so the write premium
        // is billed rather than silently charged at the input rate.
        let anthropic = &card.per_turn[0];
        assert!(!anthropic.cost_unpriced);
        assert_eq!(anthropic.cost_unpriced_reason, None);
        assert!(anthropic.unpriced_classes.is_empty());
        // Provenance is recorded (bundled snapshot offline, live after a
        // catalog refresh); the point is that it is never absent for a
        // priced turn.
        assert!(anthropic.pricing_provenance.is_some());
        let expected = 0.7 * 1.0 + 0.1 * 5.0 + 0.2 * 0.1 + 0.1 * 1.25;
        assert!((anthropic.cost_usd - expected).abs() < 1e-9);

        // Moonshot's row has no published cache-write rate: the whole turn
        // fails closed instead of under-reporting the write tokens.
        let moonshot = &card.per_turn[1];
        assert!(moonshot.cost_unpriced);
        assert_eq!(moonshot.cost_usd, 0.0);
        assert_eq!(
            moonshot.cost_unpriced_reason.as_deref(),
            Some("missing_class_price")
        );
        assert_eq!(moonshot.unpriced_classes, vec!["cache_write".to_string()]);
        assert!(moonshot.pricing_provenance.is_some());

        // A subscription route is not "free"; it is not money-metered.
        let oauth = &card.per_turn[2];
        assert!(oauth.cost_unpriced);
        assert_eq!(
            oauth.cost_unpriced_reason.as_deref(),
            Some("not_money_metered")
        );
        assert!(oauth.unpriced_classes.is_empty());
        assert_eq!(oauth.pricing_provenance, None);

        // Aggregates stay honest about what the number covers. Two of the three
        // turns owe money (Anthropic and Moonshot); the OAuth turn does not, so
        // it is outside the denominator rather than counted as an unpriced dollar.
        assert_eq!(card.metrics.turns, 3);
        assert_eq!(card.metrics.money_metered_turns, 2);
        assert_eq!(card.metrics.unpriced_turns, 1);
        assert!(!card.metrics.cost_complete);
        assert_eq!(
            card.metrics.unpriced_classes,
            vec!["cache_write".to_string()]
        );
        assert!((card.metrics.total_cost_usd - expected).abs() < 1e-9);
        assert!(card.per_turn[0].money_metered);
        assert!(card.per_turn[1].money_metered);
        assert!(!card.per_turn[2].money_metered);

        let summary = card.to_summary();
        assert!(summary.contains("priced_cost_subtotal_usd"));
        assert!(summary.contains("cache_write_tokens: 300000"));
        assert!(summary.contains("no published price"));
        // Coverage reads against the money-metered turns, not all three.
        assert!(summary.contains("money_metered_turns: 2"), "{summary}");

        let json = serde_json::to_value(&card).expect("serialize scorecard");
        assert_eq!(json["per_turn"][1]["unpriced_classes"][0], "cache_write");
        assert_eq!(json["metrics"]["total_cache_write_tokens"], 300_000);
        assert_eq!(json["metrics"]["cost_complete"], false);
        assert_eq!(json["metrics"]["money_metered_turns"], 2);
        // Route identity survives serialization, so a re-read scorecard can be
        // re-explained without the original input file.
        assert_eq!(json["per_turn"][1]["provider"], "moonshot");
        assert_eq!(json["per_turn"][2]["money_metered"], false);
    }

    /// Legacy baselines and per-turn records that predate the class audit must
    /// still deserialize; the new fields default rather than fail.
    #[test]
    fn legacy_turn_score_json_defaults_the_new_audit_fields() {
        let score: TurnScore = serde_json::from_value(serde_json::json!({
            "turn_id": "t1",
            "model": "gpt-5.5",
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_tokens": 0,
            "cost_usd": 0.1,
            "cost_cny": 0.0,
            "cost_unpriced": false
        }))
        .expect("legacy per-turn record stays readable");
        assert_eq!(score.cache_write_tokens, 0);
        assert_eq!(score.reasoning_tokens, 0);
        assert!(score.unpriced_classes.is_empty());
        assert_eq!(score.pricing_provenance, None);
        assert_eq!(score.cost_unpriced_reason, None);
        assert_eq!(score.live_pricing_defect, None);
        // A legacy row carries no coverage evidence, so `money_metered` defaults
        // to false rather than asserting the row was inside a complete total.
        assert!(!score.money_metered);

        // Legacy aggregate baselines stay readable too, defaulting the new
        // coverage denominator rather than failing the parse.
        let metrics: ScorecardMetrics = serde_json::from_value(serde_json::json!({
            "turns": 3,
            "total_input_tokens": 10,
            "total_output_tokens": 5,
            "total_cache_read_tokens": 0,
            "total_cost_usd": 0.1,
            "total_cost_cny": 0.0,
            "cache_hit_ratio": 0.0
        }))
        .expect("legacy baseline stays readable");
        assert_eq!(metrics.money_metered_turns, 0);
        assert_eq!(metrics.total_cache_write_tokens, 0);
    }

    #[test]
    fn aggregates_tokens_and_cache_hit_ratio_independent_of_pricing() {
        // input_tokens includes cache hits; token_usage_for_pricing splits them:
        // non-cached input = 1000-200 = 800, cache_read = 200.
        let u1 = usage(1000, 500, 200);
        let u2 = usage(2000, 100, 800); // non-cached = 1200, cache_read = 800
        let turns = [
            TurnInput {
                turn_id: "t1".into(),
                created_at: None,
                provider: None,
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "unpriced-x".into(),
                usage: &u1,
            },
            TurnInput {
                turn_id: "t2".into(),
                created_at: None,
                provider: None,
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "unpriced-x".into(),
                usage: &u2,
            },
        ];
        let card = Scorecard::from_turns(&turns);

        assert_eq!(card.metrics.turns, 2);
        assert_eq!(card.metrics.total_input_tokens, 800 + 1200);
        assert_eq!(card.metrics.total_output_tokens, 600); // 500 + 100
        assert_eq!(card.metrics.total_cache_read_tokens, 1000); // 200 + 800
        assert_eq!(card.metrics.unpriced_turns, 2);
        // cache_read / (input + cache_read) = 1000 / (2000 + 1000)
        let expected = 1000.0 / 3000.0;
        assert!((card.metrics.cache_hit_ratio - expected).abs() < 1e-9);
    }

    /// The canonical cache-efficiency denominator is
    /// `hit / (non-cached input + hit + write)`. Cache-write tokens are prompt
    /// tokens that were not served from cache, so omitting them reported a
    /// flattering ratio on exactly the turns that paid to populate the cache.
    #[test]
    fn cache_hit_ratio_counts_cache_write_in_the_denominator() {
        fn card_for(usage: &Usage) -> Scorecard {
            Scorecard::from_turns(&[TurnInput {
                turn_id: "t1".into(),
                created_at: None,
                provider: None,
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "unpriced-x".into(),
                usage,
            }])
        }

        // Zero everything: a ratio is undefined, reported as 0.0 rather than NaN.
        let empty = Usage::default();
        let card = card_for(&empty);
        assert_eq!(card.metrics.cache_hit_ratio, 0.0);
        assert_eq!(card.metrics.total_cache_write_tokens, 0);

        // Write-only: a turn that populated the cache and read nothing from it
        // has a 0% hit ratio, not an undefined-and-therefore-zero one that a
        // write-blind denominator would produce by accident.
        let write_only = Usage {
            input_tokens: 1_000,
            output_tokens: 10,
            prompt_cache_hit_tokens: Some(0),
            prompt_cache_write_tokens: Some(1_000),
            ..Default::default()
        };
        let card = card_for(&write_only);
        assert_eq!(card.metrics.total_cache_write_tokens, 1_000);
        assert_eq!(card.metrics.total_cache_read_tokens, 0);
        assert_eq!(card.metrics.cache_hit_ratio, 0.0);

        // Mixed: 1000 prompt tokens = 200 read + 300 write + 500 non-cached.
        let mixed = Usage {
            input_tokens: 1_000,
            output_tokens: 10,
            prompt_cache_hit_tokens: Some(200),
            prompt_cache_write_tokens: Some(300),
            ..Default::default()
        };
        let card = card_for(&mixed);
        assert_eq!(card.metrics.total_input_tokens, 500);
        assert_eq!(card.metrics.total_cache_read_tokens, 200);
        assert_eq!(card.metrics.total_cache_write_tokens, 300);
        let expected = 200.0 / (500.0 + 200.0 + 300.0);
        assert!(
            (card.metrics.cache_hit_ratio - expected).abs() < 1e-9,
            "got {}, want {expected}",
            card.metrics.cache_hit_ratio
        );
        // The write-blind denominator would have said 200/700 — assert the two
        // are distinguishable so a regression is unambiguous.
        let write_blind = 200.0 / 700.0;
        assert!((card.metrics.cache_hit_ratio - write_blind).abs() > 1e-6);
    }

    #[test]
    fn unknown_model_is_marked_unpriced_with_zero_cost() {
        let u = usage(1000, 500, 0);
        let turns = [TurnInput {
            turn_id: "t1".into(),
            created_at: None,
            provider: Some("openai"),
            billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
            model: "definitely-not-a-real-model".into(),
            usage: &u,
        }];
        let card = Scorecard::from_turns(&turns);
        assert!(card.per_turn[0].cost_unpriced);
        assert_eq!(card.per_turn[0].cost_usd, 0.0);
        assert_eq!(card.metrics.total_cost_usd, 0.0);
        assert!(card.to_summary().contains("cost_usd: unavailable"));
    }

    #[test]
    fn same_model_is_priced_only_for_its_authoritative_provider_route() {
        let u = usage(1000, 500, 0);
        let turns = [
            TurnInput {
                turn_id: "api".into(),
                created_at: None,
                provider: Some("openai"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5.5".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "oauth".into(),
                created_at: None,
                provider: Some("openai-codex"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5.5".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "local".into(),
                created_at: None,
                provider: Some("ollama"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5.5".into(),
                usage: &u,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        assert!(!card.per_turn[0].cost_unpriced);
        assert!(card.per_turn[0].cost_usd > 0.0);
        assert!(card.per_turn[1].cost_unpriced);
        assert_eq!(card.per_turn[1].cost_usd, 0.0);
        assert!(card.per_turn[2].cost_unpriced);
        assert_eq!(card.per_turn[2].cost_usd, 0.0);
        // Codex OAuth and Ollama are *exactly* non-metered, so they leave the
        // money-coverage denominator entirely rather than counting as unpriced
        // dollars: only the OpenAI turn owes money, and it is priced. The USD
        // total is therefore genuinely complete for the spend it covers (#4318).
        assert_eq!(card.metrics.money_metered_turns, 1);
        assert_eq!(card.metrics.unpriced_turns, 0);
        assert!(card.metrics.cost_complete);
        for (index, expected) in [(1_usize, false), (2_usize, false)] {
            assert_eq!(
                card.per_turn[index].money_metered, expected,
                "turn {index} money-metered"
            );
            assert_eq!(
                card.per_turn[index].cost_unpriced_reason.as_deref(),
                Some("not_money_metered"),
                "turn {index} reason"
            );
        }
        assert!(card.per_turn[0].money_metered);
        // CNY is only published by direct DeepSeek, so the single metered turn
        // still has no authoritative CNY figure.
        assert_eq!(card.metrics.cny_unpriced_turns, 1);
        assert!(!card.metrics.cny_cost_complete);
        assert!(card.to_summary().contains("money_metered_turns: 1"));
        assert!(card.to_summary().contains("cost_cny: unavailable"));

        let json = serde_json::to_value(&card).expect("serialize scorecard");
        assert_eq!(json["per_turn"][0]["provider"], "openai");
        assert_eq!(json["per_turn"][1]["provider"], "openai-codex");
        assert_eq!(json["per_turn"][2]["provider"], "ollama");
        assert_eq!(json["metrics"]["money_metered_turns"], 1);
        assert_eq!(json["metrics"]["unpriced_turns"], 0);
        assert_eq!(json["metrics"]["cost_complete"], true);
        assert_eq!(json["metrics"]["cny_cost_complete"], false);
    }

    #[test]
    fn first_party_hand_price_survives_a_missing_catalog_offering() {
        let u = usage(1_000_000, 0, 0);
        let turns = [
            TurnInput {
                turn_id: "openai-api".into(),
                created_at: None,
                provider: Some("openai"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5-codex".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "foreign-route".into(),
                created_at: None,
                provider: Some("ollama"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5-codex".into(),
                usage: &u,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        assert!(!card.per_turn[0].cost_unpriced);
        assert!((card.per_turn[0].cost_usd - 1.25).abs() < f64::EPSILON);
        assert!(card.per_turn[1].cost_unpriced);
    }

    #[test]
    fn documented_no_cache_discount_uses_input_without_generalizing_missing_rates() {
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 0,
            prompt_cache_hit_tokens: Some(250_000),
            prompt_cache_write_tokens: Some(100_000),
            ..Default::default()
        };
        let turns = [
            TurnInput {
                turn_id: "documented-no-discount".into(),
                created_at: None,
                provider: Some("openai"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5.5-pro".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "missing-cache-rate".into(),
                created_at: None,
                provider: Some("meta"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "muse-spark-1.1".into(),
                usage: &u,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        // 1M total input crosses the 272K context tier, so every input class
        // (miss, cache read, cache write) bills at the tier input rate of
        // $60/M on this documented no-cache-discount route: 1M * 60 = $60.
        assert!(!card.per_turn[0].cost_unpriced);
        assert!((card.per_turn[0].cost_usd - 60.0).abs() < f64::EPSILON);
        assert!(card.per_turn[1].cost_unpriced);
        assert!(!card.metrics.cost_complete);
    }

    #[test]
    fn anthropic_sonnet_5_uses_the_recorded_turn_time() {
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            prompt_cache_hit_tokens: Some(250_000),
            prompt_cache_write_tokens: Some(100_000),
            ..Default::default()
        };
        // Sonnet 5's $2/$10 launch rate became the standard price (Anthropic
        // pricing page, 2026-08-17: the 2026-09-01 increase "will not
        // occur"), so both sides of the former boundary price identically:
        // 650K miss * 2.00 + 250K hit * 0.20 + 100K write * 2.50 + 500K out
        // * 10.00 = 1.30 + 0.05 + 0.25 + 5.00 = 6.60. A turn with no recorded
        // time still fails closed rather than guessing a window.
        let before_boundary: DateTime<Utc> = "2026-08-31T23:59:59Z".parse().expect("time");
        let after_boundary: DateTime<Utc> = "2026-09-01T00:00:00Z".parse().expect("time");
        let turns = [
            TurnInput {
                turn_id: "sonnet-before".into(),
                created_at: Some(&before_boundary),
                provider: Some("anthropic"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: " claude-sonnet-5 ".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "sonnet-after".into(),
                created_at: Some(&after_boundary),
                provider: Some("anthropic"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "claude-sonnet-5".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "sonnet-missing-time".into(),
                created_at: None,
                provider: Some("anthropic"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "claude-sonnet-5".into(),
                usage: &u,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        assert!(!card.per_turn[0].cost_unpriced);
        assert!((card.per_turn[0].cost_usd - 6.60).abs() < 1e-12);
        assert_eq!(card.per_turn[0].created_at.as_ref(), Some(&before_boundary));
        assert!(card.per_turn[0].cost_cny_unpriced);
        assert!(!card.per_turn[1].cost_unpriced);
        assert!((card.per_turn[1].cost_usd - 6.60).abs() < 1e-12);
        assert!(card.per_turn[1].cost_cny_unpriced);
        assert!(card.per_turn[2].cost_unpriced);
    }

    #[test]
    fn known_zero_usage_is_zero_cost_not_unavailable() {
        let u = usage(0, 0, 0);
        let turns = [TurnInput {
            turn_id: "zero".into(),
            created_at: None,
            provider: Some("openai"),
            billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
            model: "gpt-5.5".into(),
            usage: &u,
        }];

        let card = Scorecard::from_turns(&turns);

        assert!(!card.per_turn[0].cost_unpriced);
        assert_eq!(card.per_turn[0].cost_usd, 0.0);
        assert!(card.per_turn[0].cost_cny_unpriced);
        assert_eq!(card.metrics.unpriced_turns, 0);
        assert_eq!(card.metrics.cny_unpriced_turns, 1);
        assert!(card.metrics.cost_complete);
        assert!(!card.metrics.cny_cost_complete);
        assert!(card.to_summary().contains("cost_usd: $0.0000"));
        assert!(card.to_summary().contains("cost_cny: unavailable"));
    }

    #[test]
    fn direct_deepseek_route_keeps_authoritative_dual_currency_pricing() {
        let u = usage(1000, 500, 0);
        let recorded_at: DateTime<Utc> = "2026-08-17T15:00:00Z".parse().expect("recorded time");
        let turns = [TurnInput {
            turn_id: "deepseek".into(),
            created_at: Some(&recorded_at),
            provider: Some("deepseek"),
            billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
            model: "deepseek-v4-pro".into(),
            usage: &u,
        }];

        let card = Scorecard::from_turns(&turns);

        assert!(!card.per_turn[0].cost_unpriced);
        assert!(!card.per_turn[0].cost_cny_unpriced);
        assert!(card.per_turn[0].cost_usd > 0.0);
        assert!(card.per_turn[0].cost_cny > 0.0);
        assert!(card.metrics.cost_complete);
        assert!(card.metrics.cny_cost_complete);
    }

    #[test]
    fn undated_direct_deepseek_v4_turns_fail_closed_on_the_time_window() {
        // V4 flash/pro are peak/off-peak tiered by the turn's recorded time; a
        // turn the recorder did not date cannot be resolved to one price and
        // must not be silently priced at whatever tier `now` happens to be.
        let u = usage(1000, 500, 0);
        let turns = [
            TurnInput {
                turn_id: "undated-pro".into(),
                created_at: None,
                provider: Some("deepseek"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "deepseek-v4-pro".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "undated-flash".into(),
                created_at: None,
                provider: Some("deepseek"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "deepseek-v4-flash".into(),
                usage: &u,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        assert!(card.per_turn.iter().all(|turn| turn.cost_unpriced));
        assert!(card.per_turn.iter().all(|turn| turn.cost_cny_unpriced));
        assert!(!card.metrics.cost_complete);
    }

    #[test]
    fn direct_deepseek_compact_aliases_use_canonical_pricing() {
        let u = usage(1000, 500, 100);
        let recorded_at: DateTime<Utc> = "2026-08-17T15:00:00Z".parse().expect("recorded time");
        let models = [
            "deepseek-v4-pro",
            "pro",
            " DeepSeek-V4Pro ",
            "deepseek-v4-flash",
            "flash",
            "DEEPSEEK-V4FLASH",
        ];
        let turns: Vec<_> = models
            .iter()
            .map(|model| TurnInput {
                turn_id: (*model).into(),
                created_at: Some(&recorded_at),
                provider: Some("deepseek"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: (*model).into(),
                usage: &u,
            })
            .collect();

        let card = Scorecard::from_turns(&turns);

        for alias in [1, 2] {
            assert_eq!(card.per_turn[alias].cost_usd, card.per_turn[0].cost_usd);
            assert_eq!(card.per_turn[alias].cost_cny, card.per_turn[0].cost_cny);
        }
        for alias in [4, 5] {
            assert_eq!(card.per_turn[alias].cost_usd, card.per_turn[3].cost_usd);
            assert_eq!(card.per_turn[alias].cost_cny, card.per_turn[3].cost_cny);
        }
        assert!(card.per_turn.iter().all(|turn| !turn.cost_unpriced));
        assert!(card.per_turn.iter().all(|turn| !turn.cost_cny_unpriced));
    }

    #[test]
    fn direct_deepseek_compatibility_aliases_use_the_flash_route() {
        let u = usage(1000, 500, 100);
        let before_retirement: DateTime<Utc> =
            "2026-07-24T15:58:59Z".parse().expect("pre-retirement time");
        let at_retirement: DateTime<Utc> = DEEPSEEK_ALIAS_RETIREMENT_UTC
            .parse()
            .expect("retirement time");
        let turns = [
            TurnInput {
                turn_id: "chat-alias".into(),
                created_at: Some(&before_retirement),
                provider: Some("deepseek"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "deepseek-chat".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "reasoner-alias".into(),
                created_at: Some(&before_retirement),
                provider: Some("deepseek"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "deepseek-reasoner".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "canonical".into(),
                created_at: Some(&before_retirement),
                provider: Some("deepseek"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "deepseek-v4-flash".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "retired-alias".into(),
                created_at: Some(&at_retirement),
                provider: Some("deepseek"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "deepseek-chat".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "undated-alias".into(),
                created_at: None,
                provider: Some("deepseek"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "deepseek-reasoner".into(),
                usage: &u,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        assert_eq!(card.per_turn[0].cost_usd, card.per_turn[2].cost_usd);
        assert_eq!(card.per_turn[1].cost_usd, card.per_turn[2].cost_usd);
        assert_eq!(card.per_turn[0].cost_cny, card.per_turn[2].cost_cny);
        assert_eq!(card.per_turn[1].cost_cny, card.per_turn[2].cost_cny);
        assert!(card.per_turn[..3].iter().all(|turn| !turn.cost_unpriced));
        assert!(
            card.per_turn[..3]
                .iter()
                .all(|turn| !turn.cost_cny_unpriced)
        );
        assert!(card.per_turn[3].cost_unpriced);
        assert!(card.per_turn[4].cost_unpriced);
    }

    #[test]
    fn direct_arcee_aliases_do_not_cross_the_openrouter_namespace() {
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            prompt_cache_hit_tokens: Some(250_000),
            prompt_cache_write_tokens: Some(100_000),
            ..Default::default()
        };
        let turns = [
            TurnInput {
                turn_id: "canonical-direct".into(),
                created_at: None,
                provider: Some("arcee"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "trinity-large-thinking".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "direct-alias".into(),
                created_at: None,
                provider: Some("arcee"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "arcee-trinity-large-thinking".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "openrouter-namespace".into(),
                created_at: None,
                provider: Some("arcee"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "arcee-ai/trinity-large-thinking".into(),
                usage: &u,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        assert!(!card.per_turn[0].cost_unpriced);
        assert!((card.per_turn[0].cost_usd - 0.65).abs() < f64::EPSILON);
        assert_eq!(card.per_turn[1].cost_usd, card.per_turn[0].cost_usd);
        assert!(!card.per_turn[1].cost_unpriced);
        assert!(card.per_turn[2].cost_unpriced);
    }

    #[test]
    fn costless_catalog_rows_fall_back_only_to_verified_provider_prices() {
        let u = Usage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            prompt_cache_hit_tokens: Some(250_000),
            prompt_cache_write_tokens: Some(100_000),
            ..Default::default()
        };
        let turns = [
            TurnInput {
                turn_id: "arcee-mini".into(),
                created_at: None,
                provider: Some("arcee"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "trinity-mini".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "minimax-m2.7".into(),
                created_at: None,
                provider: Some("minimax"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "minimax-m2.7".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "foreign-route".into(),
                created_at: None,
                provider: Some("ollama"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "trinity-mini".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "openai-hosted-deepseek".into(),
                created_at: None,
                provider: Some("openai"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "deepseek-v4-pro".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "openrouter-hosted-zai".into(),
                created_at: None,
                provider: Some("openrouter"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "z-ai/glm-5.2".into(),
                usage: &u,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        // Trinity Mini has no verified provider rate in the release metadata;
        // a removed hand-written estimate must stay unknown, not become zero
        // or leak through from a similarly named route.
        assert_eq!(card.per_turn[0].cost_usd, 0.0);
        assert!(card.per_turn[0].cost_unpriced);
        // MiniMax-M2.7 publishes a distinct cache-write rate (0.375/M),
        // retained by the provider-owned fallback even without a priced
        // catalog offering.
        assert!((card.per_turn[1].cost_usd - 0.8475).abs() < f64::EPSILON);
        assert!(!card.per_turn[1].cost_unpriced);
        assert!(card.per_turn[..2].iter().all(|turn| turn.cost_cny_unpriced));
        assert!(card.per_turn[2..].iter().all(|turn| turn.cost_unpriced));
    }

    #[test]
    fn stepfun_legacy_route_keeps_pricing_without_a_catalog_row() {
        let u = usage(1000, 500, 250);
        let recorded = |turn_id: &str,
                        provider: &str,
                        model: &str,
                        billing_surface: Option<&str>| RecordedTurn {
            turn_id: turn_id.to_string(),
            created_at: None,
            model_backed: Some(true),
            provider: Some(provider.to_string()),
            billing_surface: billing_surface.map(str::to_string),
            model: model.to_string(),
            usage: Some(u.clone()),
        };
        let turns = [
            recorded(
                "stepfun-default",
                "stepfun",
                " STEP-3.7-FLASH ",
                Some(crate::pricing::STEPFUN_PAYG_BILLING_SURFACE),
            ),
            recorded(
                "stepfun-plan",
                "stepfun",
                "step-3.7-flash",
                Some(crate::pricing::STEPFUN_PLAN_BILLING_SURFACE),
            ),
            recorded("stepfun-missing-surface", "stepfun", "step-3.7-flash", None),
            recorded("stepfun-unknown-model", "stepfun", "step-3.5-flash", None),
            recorded(
                "openrouter-stepfun-name",
                "openrouter",
                "step-3.7-flash",
                None,
            ),
            recorded("local-stepfun-name", "ollama", "step-3.7-flash", None),
            recorded(
                "sakana-incomplete-tier-price",
                "sakana",
                "fugu-ultra-20260615",
                None,
            ),
            recorded(
                "foreign-deepseek-name",
                "openmodel",
                "deepseek-v4-flash",
                None,
            ),
        ];

        let card = Scorecard::from_recorded_turns(&turns);

        assert!((card.per_turn[0].cost_usd - 0.000_735).abs() < 1e-12);
        assert!(!card.per_turn[0].cost_unpriced);
        assert!(card.per_turn[0].cost_cny_unpriced);
        assert_eq!(
            card.per_turn[0].billing_surface.as_deref(),
            Some(crate::pricing::STEPFUN_PAYG_BILLING_SURFACE)
        );
        assert!(card.per_turn[1..].iter().all(|turn| turn.cost_unpriced));
    }

    #[test]
    fn legacy_model_only_record_is_readable_but_unpriced() {
        let recorded: RecordedTurn = serde_json::from_value(serde_json::json!({
            "turn_id": "legacy",
            "model": "gpt-5.5",
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0
            }
        }))
        .expect("parse legacy scorecard turn");
        assert_eq!(recorded.provider, None);
        assert_eq!(recorded.billing_surface, None);

        let card = Scorecard::from_recorded_turns(&[recorded]);

        assert!(card.per_turn[0].cost_unpriced);
        assert_eq!(card.per_turn[0].cost_usd, 0.0);
        assert_eq!(card.metrics.unpriced_turns, 1);
        assert!(card.to_summary().contains("cost_usd: unavailable"));
    }

    #[test]
    fn recorded_turn_accepts_runtime_route_aliases() {
        let recorded: RecordedTurn = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "id": "runtime-turn",
            "thread_id": "thread-1",
            "status": "completed",
            "input_summary": "score this turn",
            "created_at": "2026-07-12T10:30:00Z",
            "effective_provider": "openai-codex",
            "effective_billing_surface": "account-subscription",
            "effective_model": "gpt-5.5",
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1
            }
        }))
        .expect("parse runtime scorecard turn");

        assert_eq!(recorded.turn_id, "runtime-turn");
        assert_eq!(
            recorded.created_at.as_ref().map(DateTime::to_rfc3339),
            Some("2026-07-12T10:30:00+00:00".to_string())
        );
        assert_eq!(recorded.provider.as_deref(), Some("openai-codex"));
        assert_eq!(
            recorded.billing_surface.as_deref(),
            Some("account-subscription")
        );
        assert_eq!(recorded.model, "gpt-5.5");
        assert!(recorded.contributes_to_scorecard());
    }

    #[test]
    fn runtime_turn_without_usage_is_readable_and_filtered() {
        let recorded: RecordedTurn = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "id": "queued-runtime-turn",
            "thread_id": "thread-1",
            "status": "queued",
            "input_summary": "waiting to run",
            "created_at": "2026-07-12T10:30:00Z",
            "effective_provider": "openai",
            "effective_model": "gpt-5.5"
        }))
        .expect("parse runtime row before usage is recorded");

        assert!(recorded.usage.is_none());
        assert!(!recorded.contributes_to_scorecard());
        let card = Scorecard::from_recorded_turns(&[recorded]);
        assert_eq!(card.metrics.turns, 0);
        assert!(card.per_turn.is_empty());
    }

    #[test]
    fn recorded_non_model_hook_turn_is_excluded_from_model_scorecard() {
        let recorded: RecordedTurn = serde_json::from_value(serde_json::json!({
            "turn_id": "shell-turn",
            "created_at": "2026-07-12T10:30:00Z",
            "model_backed": false,
            "provider": null,
            "model": "gpt-5.5",
            "usage": {
                "input_tokens": 0,
                "output_tokens": 0
            }
        }))
        .expect("parse non-model turn_end record");

        assert!(!recorded.contributes_to_scorecard());
    }

    #[test]
    fn blank_unknown_and_custom_providers_fail_closed_as_unpriced() {
        let u = usage(1000, 500, 0);
        let turns = [
            TurnInput {
                turn_id: "blank".into(),
                created_at: None,
                provider: Some("   "),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5.5".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "named-custom".into(),
                created_at: None,
                provider: Some("my-openai-proxy"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5.5".into(),
                usage: &u,
            },
            TurnInput {
                turn_id: "generic-custom".into(),
                created_at: None,
                provider: Some("custom"),
                billing_surface: Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
                model: "gpt-5.5".into(),
                usage: &u,
            },
        ];

        let card = Scorecard::from_turns(&turns);

        assert_eq!(card.per_turn[0].provider, None);
        assert_eq!(
            card.per_turn[1].provider.as_deref(),
            Some("my-openai-proxy")
        );
        assert_eq!(card.per_turn[2].provider.as_deref(), Some("custom"));
        assert!(card.per_turn.iter().all(|turn| turn.cost_unpriced));
        assert_eq!(card.metrics.unpriced_turns, 3);
        assert!(!card.metrics.cost_complete);
        assert!(card.to_summary().contains("cost_usd: unavailable"));
    }

    #[test]
    fn regression_flags_cost_and_token_increases_over_threshold() {
        let baseline = ScorecardMetrics {
            turns: 1,
            money_metered_turns: 1,
            unpriced_turns: 0,
            cny_unpriced_turns: 0,
            cost_complete: true,
            cny_cost_complete: true,
            unpriced_classes: Vec::new(),
            total_input_tokens: 1000,
            total_output_tokens: 1000,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            total_cost_usd: 0.10,
            total_cost_cny: 0.7,
            cache_hit_ratio: 0.5,
        };
        let current = ScorecardMetrics {
            total_cost_usd: 0.20,      // +100% → regression
            total_input_tokens: 1010,  // +1% → under 5% threshold, no regression
            total_output_tokens: 2000, // +100% → regression
            cache_hit_ratio: 0.5,      // unchanged
            ..baseline.clone()
        };
        let regs = current.regressions_against(&baseline, 5.0);
        let names: Vec<&str> = regs.iter().map(|r| r.metric.as_str()).collect();
        assert!(names.contains(&"total_cost_usd"));
        assert!(names.contains(&"total_output_tokens"));
        assert!(!names.contains(&"total_input_tokens")); // under threshold
    }

    #[test]
    fn a_non_finite_threshold_cannot_pass_a_regressed_run() {
        let baseline = ScorecardMetrics {
            turns: 1,
            money_metered_turns: 1,
            unpriced_turns: 0,
            cny_unpriced_turns: 0,
            cost_complete: true,
            cny_cost_complete: true,
            unpriced_classes: Vec::new(),
            total_input_tokens: 1000,
            total_output_tokens: 1000,
            total_cache_read_tokens: 0,
            total_cache_write_tokens: 0,
            total_reasoning_tokens: 0,
            total_cost_usd: 0.10,
            total_cost_cny: 0.7,
            cache_hit_ratio: 0.5,
        };
        let current = ScorecardMetrics {
            total_output_tokens: 2000,
            ..baseline.clone()
        };
        for threshold in [f64::NAN, f64::INFINITY] {
            let names: Vec<String> = current
                .regressions_against(&baseline, threshold)
                .into_iter()
                .map(|r| r.metric)
                .collect();
            assert!(
                names.iter().any(|name| name == "total_output_tokens"),
                "threshold {threshold} passed a +100% regression: {names:?}"
            );
        }
        // A metric whose change cannot be computed is not a pass either.
        let unknown = ScorecardMetrics {
            cache_hit_ratio: f64::NAN,
            ..baseline.clone()
        };
        assert!(
            unknown
                .regressions_against(&baseline, 5.0)
                .iter()
                .any(|r| r.metric == "cache_hit_ratio_drop")
        );
    }

    #[test]
    fn regression_flags_loss_of_cost_completeness_without_comparing_subtotals() {
        let baseline = ScorecardMetrics {
            cost_complete: true,
            total_cost_usd: 0.10,
            ..Default::default()
        };
        let current = ScorecardMetrics {
            turns: 1,
            unpriced_turns: 1,
            total_cost_usd: 0.20,
            ..Default::default()
        };

        let regs = current.regressions_against(&baseline, 5.0);
        assert!(!regs.iter().any(|r| r.metric == "total_cost_usd"));
        assert!(regs.iter().any(|r| r.metric == "cost_completeness_drop"));
    }

    #[test]
    fn regression_flags_loss_of_cny_cost_completeness() {
        let baseline = ScorecardMetrics {
            cny_cost_complete: true,
            total_cost_cny: 0.70,
            ..Default::default()
        };
        let current = ScorecardMetrics {
            turns: 1,
            cny_unpriced_turns: 1,
            total_cost_cny: 0.0,
            ..Default::default()
        };

        let regs = current.regressions_against(&baseline, 5.0);
        assert!(
            regs.iter()
                .any(|r| r.metric == "cny_cost_completeness_drop")
        );
    }

    #[test]
    fn regression_flags_complete_cny_cost_increase() {
        let baseline = ScorecardMetrics {
            cny_cost_complete: true,
            total_cost_cny: 0.70,
            ..Default::default()
        };
        let current = ScorecardMetrics {
            total_cost_cny: 1.40,
            ..baseline.clone()
        };

        let regs = current.regressions_against(&baseline, 5.0);
        assert!(regs.iter().any(|r| r.metric == "total_cost_cny"));
    }

    #[test]
    fn legacy_baseline_is_readable_but_cost_is_not_comparable() {
        let baseline: ScorecardMetrics = serde_json::from_value(serde_json::json!({
            "turns": 1,
            "total_input_tokens": 10,
            "total_output_tokens": 5,
            "total_cache_read_tokens": 0,
            "total_cost_usd": 0.10,
            "total_cost_cny": 0.0,
            "cache_hit_ratio": 0.0
        }))
        .expect("parse legacy scorecard baseline");
        assert!(!baseline.cost_complete);

        let current = ScorecardMetrics {
            cost_complete: true,
            total_cost_usd: 0.20,
            total_input_tokens: 10,
            total_output_tokens: 5,
            ..Default::default()
        };
        let regs = current.regressions_against(&baseline, 5.0);
        assert!(!regs.iter().any(|r| r.metric == "total_cost_usd"));
    }

    #[test]
    fn regression_flags_cache_hit_ratio_drop() {
        let baseline = ScorecardMetrics {
            cache_hit_ratio: 0.80,
            ..Default::default()
        };
        let current = ScorecardMetrics {
            cache_hit_ratio: 0.40,
            ..Default::default()
        };
        let regs = current.regressions_against(&baseline, 10.0);
        assert!(regs.iter().any(|r| r.metric == "cache_hit_ratio_drop"));
    }

    #[test]
    fn no_regressions_when_within_threshold() {
        let baseline = ScorecardMetrics {
            total_cost_usd: 1.0,
            total_input_tokens: 1000,
            total_output_tokens: 1000,
            cache_hit_ratio: 0.5,
            ..Default::default()
        };
        let current = baseline.clone();
        assert!(current.regressions_against(&baseline, 5.0).is_empty());
    }

    #[test]
    fn cache_hit_denominator_saturates_instead_of_wrapping() {
        assert_eq!(cacheable_token_total(u64::MAX, 1, 1), u64::MAX);
        assert_eq!(cacheable_token_total(1, u64::MAX, 1), u64::MAX);
    }
}

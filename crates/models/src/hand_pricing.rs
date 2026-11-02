use chrono::{DateTime, Datelike, FixedOffset, TimeZone, Timelike, Utc, Weekday};
use codewhale_config::ProviderKind;
use codewhale_config::pricing::{Currency, OfferingPricing, PricingProvenance};

/// How a hand-sourced row bills cache-creation (cache-write) tokens.
///
/// The distinction matters because "no separate write rate published" and
/// "documented to cost the same as ordinary input" are different facts that used
/// to collapse onto the same `None`. Folding the unknown case into the input
/// rate invents a price; this enum keeps the invention impossible (#4318).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CacheWritePolicy {
    /// The provider publishes a distinct cache-creation rate (per million).
    Rate(f64),
    /// Provider documentation states that cache creation carries **no separate
    /// charge** beyond the ordinary cache-miss input rate, so the miss rate is
    /// the published write rate rather than a substitute for a missing one.
    ///
    /// The `&'static str` is the documentation receipt this claim rests on, so
    /// the policy is auditable instead of asserted.
    DocumentedAsInputRate(&'static str),
    /// No published cache-write rate was found for this row. A turn that
    /// actually wrote to cache fails closed rather than being billed at a rate
    /// CodeWhale made up.
    Unpublished,
}

/// DeepSeek's context-caching docs: tokens that miss the cache are billed once
/// at the cache-miss rate and writing them into the cache costs nothing extra.
/// <https://api-docs.deepseek.com/guides/kv_cache>
pub const DEEPSEEK_CACHE_WRITE_IS_FREE: &str = "deepseek-kv-cache-no-write-charge";

impl CacheWritePolicy {
    /// The rate to bill cache-write tokens at, given the row's input rate.
    ///
    /// `None` means the row cannot price cache-write tokens at all.
    pub fn rate(self, input_cache_miss_per_million: f64) -> Option<f64> {
        match self {
            Self::Rate(rate) => Some(rate),
            Self::DocumentedAsInputRate(_) => Some(input_cache_miss_per_million),
            Self::Unpublished => None,
        }
    }
}

/// Per-million-token pricing for a model.
#[derive(Debug, Clone, Copy)]
pub struct CurrencyPricing {
    pub input_cache_hit_per_million: f64,
    pub input_cache_miss_per_million: f64,
    pub output_per_million: f64,
    /// How cache-creation tokens are billed on this row.
    pub cache_write: CacheWritePolicy,
}

/// Per-million-token pricing for a model.
#[derive(Debug, Clone, Copy)]
pub struct ModelPricing {
    pub usd: CurrencyPricing,
    pub cny: Option<CurrencyPricing>,
}

pub fn pricing_for_model_at(model: &str, now: DateTime<Utc>) -> Option<ModelPricing> {
    let lower = model.to_lowercase();
    if lower.starts_with("deepseek-ai/") {
        // NVIDIA NIM-hosted DeepSeek uses NVIDIA's catalog/account terms, not
        // DeepSeek Platform pricing. Avoid showing misleading DeepSeek costs.
        return None;
    }
    if lower == "claude-sonnet-5" {
        // Resolved ahead of the catalog through the recorded-time helper so
        // the first-party Anthropic override path (`hand_priced_audit`)
        // and this metadata lookup stay one contract (see
        // `claude_sonnet_5_pricing`).
        return Some(claude_sonnet_5_pricing(now));
    }
    if let Some(pricing) = known_pricing_for_model(&lower) {
        return Some(pricing);
    }
    // A new or expiring model ID does not inherit a neighboring model's
    // rates. Keep this metadata lookup as exact as the billing route owner.
    deepseek_time_aware_pricing(&lower, now)
}

pub fn deepseek_time_aware_pricing(model_lower: &str, now: DateTime<Utc>) -> Option<ModelPricing> {
    match model_lower {
        "deepseek-v4-pro" => Some(deepseek_v4_pro_pricing(now)),
        "deepseek-v4-flash" | "deepseek-flash" => Some(deepseek_v4_flash_pricing(now)),
        "deepseek-v4-flash-vision-exp" => {
            (now >= deepseek_flash_effective_from()).then(|| deepseek_flash_pricing(now))
        }
        _ => None,
    }
}

fn known_pricing_for_model(model_lower: &str) -> Option<ModelPricing> {
    let reviewed = codewhale_config::catalog::reviewed::bundled_reviewed();
    if let Some(policy) = reviewed.reference_price_policies.get(model_lower) {
        return match policy.as_str() {
            "grok" => grok_tiered_pricing(model_lower, false),
            "minimax_m3" => Some(minimax_m3_standard_pricing(false)),
            _ => None,
        };
    }
    reviewed.reference_prices.get(model_lower).map(|row| {
        usd_pricing(
            row.cache_read,
            row.input,
            row.output,
            row.cache_write
                .map_or(CacheWritePolicy::Unpublished, CacheWritePolicy::Rate),
        )
    })
}

/// A USD row whose provider publishes input/cache-read/output rates but **no**
/// cache-creation rate. Cache-write tokens on such a row are unpriced, not free
/// and not silently charged at the input rate (#4318).
pub fn usd_only_pricing(
    input_cache_hit_per_million: f64,
    input_cache_miss_per_million: f64,
    output_per_million: f64,
) -> ModelPricing {
    usd_pricing(
        input_cache_hit_per_million,
        input_cache_miss_per_million,
        output_per_million,
        CacheWritePolicy::Unpublished,
    )
}

fn usd_pricing_with_write(
    input_cache_hit_per_million: f64,
    input_cache_miss_per_million: f64,
    output_per_million: f64,
    cache_write_per_million: f64,
) -> ModelPricing {
    usd_pricing(
        input_cache_hit_per_million,
        input_cache_miss_per_million,
        output_per_million,
        CacheWritePolicy::Rate(cache_write_per_million),
    )
}

pub fn usd_pricing(
    input_cache_hit_per_million: f64,
    input_cache_miss_per_million: f64,
    output_per_million: f64,
    cache_write: CacheWritePolicy,
) -> ModelPricing {
    ModelPricing {
        usd: CurrencyPricing {
            input_cache_hit_per_million,
            input_cache_miss_per_million,
            output_per_million,
            cache_write,
        },
        cny: None,
    }
}

pub const MINIMAX_M3_LONG_CONTEXT_THRESHOLD: u32 = 512_000;

pub const GROK_4_6_LONG_CONTEXT_THRESHOLD: u32 = 200_000;

pub fn minimax_m3_standard_pricing(long_context: bool) -> ModelPricing {
    if long_context {
        usd_only_pricing(0.12, 0.60, 2.40)
    } else {
        usd_only_pricing(0.06, 0.30, 1.20)
    }
}

pub fn is_minimax_m3(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "minimax-m3" | "minimax/minimax-m3"
    )
}

/// xAI Grok standard-tier rates (cache-read, input, output per 1M) and the
/// doubled tier once a prompt reaches 200K tokens. Verified 2026-08-17 against
/// the model pages, whose embedded price tables carry both the standard and
/// `LongContext` columns at exactly 2x:
/// - <https://docs.x.ai/docs/models/grok-4.7>: 0.50 / 2.00 / 6.00 (verified
///   2026-09-23: $4.00 / $1.00 / $12.00 at or above 200K)
/// - <https://docs.x.ai/docs/models/grok-4.6>: 0.50 / 2.00 / 6.00
/// - <https://docs.x.ai/docs/models/grok-4.5>: 0.30 / 2.00 / 6.00
/// - <https://docs.x.ai/docs/models/grok-4.3>: 0.20 / 1.25 / 2.50
pub fn grok_tiered_pricing(model_lower: &str, long_context: bool) -> Option<ModelPricing> {
    let (cache_read, input, output) = match model_lower {
        "grok-4.7" | "grok-4.6" => (0.50, 2.00, 6.00),
        "grok-4.5" => (0.30, 2.00, 6.00),
        "grok-4.3" => (0.20, 1.25, 2.50),
        _ => return None,
    };
    let multiplier = if long_context { 2.0 } else { 1.0 };
    Some(usd_only_pricing(
        cache_read * multiplier,
        input * multiplier,
        output * multiplier,
    ))
}

pub fn is_grok_tiered(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "grok-4.7" | "grok-4.6" | "grok-4.5" | "grok-4.3"
    )
}

/// Claude Sonnet 5 pricing (<https://platform.claude.com/docs/en/about-claude/pricing>,
/// re-verified 2026-08-17): 2.00 / 10.00 (cache-read 0.20, 5m cache-write
/// 2.50) is now the standard price. Anthropic's pricing page states the
/// previously scheduled increase to 3.00 / 15.00 on 2026-09-01 "will not
/// occur" (release notes, 2026-08-10), so the former time-windowed flip is
/// gone; the recorded-time signature is kept so callers that price turns at
/// their recorded time (scorecard, usage aggregation) keep one contract for
/// every first-party time-aware row.
fn claude_sonnet_5_pricing(_now: DateTime<Utc>) -> ModelPricing {
    usd_pricing_with_write(0.20, 2.00, 10.00, 2.50)
}

/// DeepSeek publishes only cache-hit and cache-miss input rates *because* its
/// context cache charges nothing extra to write: a token that misses the cache
/// is billed once at the miss rate and is cached as a side effect. That makes
/// the miss rate the documented write rate, not a stand-in for a missing one.
///
/// Peak/off-peak tiers (verified against
/// <https://api-docs.deepseek.com/quick_start/pricing> on 2026-08-17):
/// off-peak rates are half the peak rates, and peak hours are 01:00–04:00
/// and 06:00–10:00 UTC (half-open). From 00:00 Beijing time on 2026-08-23 the
/// whole of Saturday and Sunday bills off-peak, peak hours included. Each turn
/// resolves its tier from its own recorded time, mirroring
/// `claude_sonnet_5_pricing`'s time-aware precedent.
fn deepseek_peak_hour(hour_utc: u32) -> bool {
    (1..4).contains(&hour_utc) || (6..10).contains(&hour_utc)
}

/// Beijing time, the zone DeepSeek states its weekend rule in. China has run a
/// fixed UTC+08:00 with no daylight saving since 1991, so a fixed offset is
/// exact here and needs no tzdata on the host.
fn deepseek_billing_offset() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("+08:00 is a valid UTC offset")
}

/// The instant the weekend-wide off-peak rule takes effect: 00:00 Beijing time
/// on Sunday 2026-08-23, which is 2026-08-22T16:00Z.
fn deepseek_weekend_off_peak_from() -> DateTime<Utc> {
    deepseek_billing_offset()
        .with_ymd_and_hms(2026, 8, 23, 0, 0, 0)
        .single()
        .expect("2026-08-23 00:00 exists in a fixed offset")
        .with_timezone(&Utc)
}

/// Whether `now` falls on a Beijing-time Saturday or Sunday with the weekend-wide
/// off-peak rule already in force.
///
/// The weekend is bounded in Beijing time, so it runs 16:00Z Friday to 16:00Z
/// Sunday; `now.weekday()` taken in UTC covers a different 48 hours. Both
/// spellings agree on today's tiers, because the peak windows sit entirely
/// outside the 16 hours they disagree over. This one keeps agreeing if the
/// windows move.
fn deepseek_weekend_off_peak(now: DateTime<Utc>) -> bool {
    now >= deepseek_weekend_off_peak_from()
        && matches!(
            now.with_timezone(&deepseek_billing_offset()).weekday(),
            Weekday::Sat | Weekday::Sun
        )
}

/// Whether a turn recorded at `now` is billed at DeepSeek's peak tier.
pub fn deepseek_is_peak(now: DateTime<Utc>) -> bool {
    !deepseek_weekend_off_peak(now) && deepseek_peak_hour(now.hour())
}

fn deepseek_v4_pro_pricing(now: DateTime<Utc>) -> ModelPricing {
    // September 11 vendor reversal: Pro remains available at its own rates.
    let peak = deepseek_is_peak(now);
    let (hit, miss, out) = if peak {
        (0.044, 1.32, 3.96)
    } else {
        (0.022, 0.66, 1.98)
    };
    let (cny_hit, cny_miss, cny_out) = if peak {
        (0.30, 9.0, 27.0)
    } else {
        (0.15, 4.5, 13.5)
    };
    ModelPricing {
        usd: CurrencyPricing {
            input_cache_hit_per_million: hit,
            input_cache_miss_per_million: miss,
            output_per_million: out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        },
        cny: Some(CurrencyPricing {
            input_cache_hit_per_million: cny_hit,
            input_cache_miss_per_million: cny_miss,
            output_per_million: cny_out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        }),
    }
}

/// DeepSeek V4.1 Flash, shipped as the unversioned id `deepseek-flash`.
///
/// Rates and effective time are the vendor's own 2026-09-10 notice, not a
/// relay: cache hit $0.003, cache miss $0.15, output $0.60 per 1M off-peak,
/// doubling at peak, effective 04:00 UTC on 2026-09-10.
/// V4 Pro remains available at its own rates after the September 11 reversal.
///
/// CNY rates are the vendor's own Chinese-language notice: cache hit 0.02 元,
/// cache miss 1 元, output 4 元 off-peak, doubling at peak. Taken from the
/// published table rather than converted from USD — a converted rate would be
/// a receipt the vendor never issued.
///
/// That notice states the peak windows in Beijing time (Mon-Fri 09:00-12:00 and
/// 14:00-18:00), which is UTC+8 and therefore exactly the 01:00-04:00 and
/// 06:00-10:00 UTC the English notice gives. Both agree, so `deepseek_is_peak`
/// needs no change.
fn deepseek_flash_pricing(now: DateTime<Utc>) -> ModelPricing {
    let peak = deepseek_is_peak(now);
    let (hit, miss, out) = if peak {
        (0.006, 0.30, 1.20)
    } else {
        (0.003, 0.15, 0.60)
    };
    let (cny_hit, cny_miss, cny_out) = if peak {
        (0.04, 2.0, 8.0)
    } else {
        (0.02, 1.0, 4.0)
    };
    ModelPricing {
        usd: CurrencyPricing {
            input_cache_hit_per_million: hit,
            input_cache_miss_per_million: miss,
            output_per_million: out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        },
        cny: Some(CurrencyPricing {
            input_cache_hit_per_million: cny_hit,
            input_cache_miss_per_million: cny_miss,
            output_per_million: cny_out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        }),
    }
}

pub fn deepseek_flash_effective_from() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 10, 4, 0, 0)
        .single()
        .expect("2026-09-10T04:00Z is a valid UTC instant")
}

fn deepseek_v4_flash_pricing(now: DateTime<Utc>) -> ModelPricing {
    if now >= deepseek_flash_effective_from() {
        return deepseek_flash_pricing(now);
    }
    let peak = deepseek_is_peak(now);
    let (hit, miss, out) = if peak {
        (0.014, 0.44, 1.32)
    } else {
        (0.007, 0.22, 0.66)
    };
    let (cny_hit, cny_miss, cny_out) = if peak {
        (0.10, 3.0, 9.0)
    } else {
        (0.05, 1.5, 4.5)
    };
    ModelPricing {
        usd: CurrencyPricing {
            input_cache_hit_per_million: hit,
            input_cache_miss_per_million: miss,
            output_per_million: out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        },
        cny: Some(CurrencyPricing {
            input_cache_hit_per_million: cny_hit,
            input_cache_miss_per_million: cny_miss,
            output_per_million: cny_out,
            cache_write: CacheWritePolicy::DocumentedAsInputRate(DEEPSEEK_CACHE_WRITE_IS_FREE),
        }),
    }
}

pub fn provider_owned_hand_quote_rows(
    provider: ProviderKind,
    model: &str,
    recorded_at: DateTime<Utc>,
) -> Option<(OfferingPricing, Option<OfferingPricing>)> {
    let pricing = provider_owned_hand_pricing_at(provider, model, recorded_at)?;
    let mut primary = currency_offering_pricing(pricing.usd, Currency::Usd);
    let mut secondary = pricing
        .cny
        .map(|row| currency_offering_pricing(row, Currency::Cny));
    let tier = if provider == ProviderKind::Xai && is_grok_tiered(model) {
        Some((
            u64::from(GROK_4_6_LONG_CONTEXT_THRESHOLD),
            true,
            grok_tiered_pricing(&model.to_ascii_lowercase(), true)?,
        ))
    } else if matches!(
        provider,
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
    ) && is_minimax_m3(model)
    {
        Some((
            u64::from(MINIMAX_M3_LONG_CONTEXT_THRESHOLD),
            false,
            minimax_m3_standard_pricing(true),
        ))
    } else {
        None
    };
    if let Some((size, inclusive, higher)) = tier {
        primary
            .context_tiers
            .push(codewhale_config::models_dev::ModelsDevCostTier {
                tier: codewhale_config::models_dev::ModelsDevContextTier {
                    kind: "context".into(),
                    size,
                },
                cost: currency_offering_pricing(higher.usd, Currency::Usd).cost_structure(),
            });
        primary.context_tiers_inclusive = inclusive;
        if let Some(row) = secondary.as_mut()
            && let Some(higher) = higher.cny
        {
            row.context_tiers
                .push(codewhale_config::models_dev::ModelsDevCostTier {
                    tier: codewhale_config::models_dev::ModelsDevContextTier {
                        kind: "context".into(),
                        size,
                    },
                    cost: currency_offering_pricing(higher, Currency::Cny).cost_structure(),
                });
            row.context_tiers_inclusive = inclusive;
        }
    }
    for row in std::iter::once(&mut primary).chain(secondary.iter_mut()) {
        row.provider = provider.as_str().into();
        row.wire_model_id = model.into();
        row.source_ref = Some(
            match provider {
                ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic => {
                    "https://api-docs.deepseek.com/quick_start/pricing"
                }
                ProviderKind::Xai => "https://docs.x.ai/docs/models",
                ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => {
                    "https://platform.minimax.io/docs/guides/pricing-paygo"
                }
                ProviderKind::Anthropic => {
                    "https://platform.claude.com/docs/en/about-claude/pricing"
                }
                _ => "codewhale:reviewed-provider-price",
            }
            .into(),
        );
    }
    Some((primary, secondary))
}

pub fn provider_owned_hand_pricing_at(
    provider: ProviderKind,
    model: &str,
    recorded_at: DateTime<Utc>,
) -> Option<ModelPricing> {
    let model_lower = model.trim().to_ascii_lowercase();
    // Hosted Fireworks / OpenCode Zen rates are provider-owned docs rows, not
    // first-party DeepSeek's $0.0028 cache-hit card and not Models.dev.
    if provider == ProviderKind::Fireworks {
        return fireworks_bundled_fallback_pricing(&model_lower);
    }
    if provider == ProviderKind::OpencodeZen {
        return opencode_zen_bundled_fallback_pricing(&model_lower);
    }
    let provider_owns_row = match provider {
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic => {
            // `deepseek-flash` is V4.1 Flash on the first-party API. Only the
            // first-party family gains it: third-party hosts below keep their
            // own published tables and must not be assumed to serve a model
            // just because DeepSeek does.
            matches!(
                model_lower.as_str(),
                "deepseek-v4-pro"
                    | "deepseek-v4-flash"
                    | "deepseek-v4-flash-vision-exp"
                    | "deepseek-flash"
            )
        }
        ProviderKind::Openai => matches!(
            model_lower.as_str(),
            "gpt-5-codex"
                | "gpt-5.3-codex"
                | "gpt-5.5"
                | "gpt-5.5-pro"
                | "gpt-5.6"
                | "gpt-5.6-sol"
                | "gpt-5.6-terra"
                | "gpt-5.6-luna"
                | "gpt-6-sol"
                | "gpt-6.1-sol"
                | "gpt-6-luna"
        ),
        ProviderKind::Anthropic => matches!(
            model_lower.as_str(),
            "claude-opus-4-8"
                | "claude-sonnet-4-6"
                | "claude-haiku-4-5"
                | "claude-fable-5"
                | "claude-sonnet-5"
                | "claude-opus-5"
                | "claude-opus-5-5"
                | "claude-sonnet-5-5"
                | "claude-fable-5-1"
        ),
        ProviderKind::Xai => is_grok_tiered(&model_lower),
        // GLM-5.3 is deliberately absent: this allowlist declares that Z.ai
        // owns a *hand-written price row* for the model, and no GLM-5.3 rate
        // has been published. An absent price is honest; an owned-but-empty
        // row is not. See `glm_5_3_has_no_hardcoded_price` below.
        // GLM-5.3-Flash *does* have a published USD list (2026-08-26).
        ProviderKind::Zai => matches!(
            model_lower.as_str(),
            "glm-5.1" | "glm-5.2" | "glm-5.3-flash" | "glm-5-turbo"
        ),
        // `k3` (Kimi Code membership) is deliberately absent: it is quota
        // billed and must never inherit the direct-platform kimi-k3 rate.
        ProviderKind::Moonshot => matches!(
            model_lower.as_str(),
            "kimi-k2.6" | "kimi-k2.7-code" | "kimi-k2.7-code-highspeed" | "kimi-k3"
        ),
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => matches!(
            model_lower.as_str(),
            "minimax-m3" | "minimax-m2.7" | "minimax-m2.7-highspeed"
        ),
        ProviderKind::Mistral => matches!(
            model_lower.as_str(),
            "mistral-medium-latest"
                | "mistral-medium-3-5"
                | "mistral-medium-3.5"
                | "mistral-medium-2604"
                | "mistral-large-latest"
                | "mistral-large-2512"
                | "mistral-small-latest"
                | "mistral-small-2603"
                | "mistral-code-latest"
                | "codestral-latest"
                | "codestral"
        ),
        ProviderKind::Arcee => model_lower == "trinity-large-thinking",
        // Every Muse tier owns a hand-written row the same way 1.1 does (see
        // `pricing_for_model_at`), so `DEFAULT_META_MODEL` (1.3) always has a
        // provider-owned fallback row.
        ProviderKind::Meta => matches!(
            model_lower.as_str(),
            "muse-spark-1.1"
                | "muse-spark-1.2"
                | "muse-spark-1.2-contributor"
                | "muse-spark-1.3"
                | "muse-spark-1.3-contributor"
                | "muse-wonder-1.0"
        ),
        // Deployment-style ids (Fireworks account prefix, OpenCode Zen
        // gateway) have no Models.dev cost fields. When the live control
        // plane 503s, these bundled family rates keep the session priced
        // instead of `unverified_live_pricing` forever (#5241).
        ProviderKind::Fireworks => {
            let bare = model_lower
                .strip_prefix("accounts/fireworks/models/")
                .unwrap_or(model_lower.as_str());
            matches!(bare, "deepseek-v4-flash" | "deepseek-v4-pro")
        }
        ProviderKind::OpencodeZen => {
            matches!(
                model_lower.as_str(),
                "deepseek-v4-flash" | "deepseek-v4-pro"
            )
        }
        _ => false,
    };
    let lookup = if provider == ProviderKind::Fireworks {
        model_lower
            .strip_prefix("accounts/fireworks/models/")
            .unwrap_or(model_lower.as_str())
            .to_string()
    } else {
        model_lower
    };
    provider_owns_row
        .then(|| pricing_for_model_at(&lookup, recorded_at))
        .flatten()
}

/// Fireworks serverless Standard rates (2026-08-15 audit).
/// <https://docs.fireworks.ai/serverless/pricing>
///
/// Cache-write is unpublished on that table. Do not inherit first-party
/// DeepSeek's $0.0028 cache-hit card — Fireworks publishes $0.028.
fn fireworks_bundled_fallback_pricing(model_lower: &str) -> Option<ModelPricing> {
    match fireworks_deployment_id(model_lower) {
        "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-0731" => {
            Some(hosted_deepseek_v4_flash_standard_pricing())
        }
        "deepseek-v4-pro" => Some(hosted_deepseek_v4_pro_standard_pricing()),
        // kimi-k3 stays unpriced until Fireworks publishes a rate for it
        // (see `fireworks_and_zen_flash_use_bundled_family_rates`).
        _ => None,
    }
}

fn fireworks_deployment_id(model_lower: &str) -> &str {
    model_lower
        .strip_prefix("accounts/fireworks/models/")
        .or_else(|| model_lower.strip_prefix("accounts/fireworks/routers/"))
        .unwrap_or(model_lower)
}

/// OpenCode Zen PAYG rates (2026-08-15 audit).
/// <https://opencode.ai/docs/zen/>
///
/// Cached write is unpublished (`-` on the Zen table). Flash cache-read is
/// $0.028, not first-party DeepSeek's $0.0028.
fn opencode_zen_bundled_fallback_pricing(model_lower: &str) -> Option<ModelPricing> {
    match model_lower {
        "deepseek-v4-flash" | "deepseek-v4-flash-0731" => {
            Some(hosted_deepseek_v4_flash_standard_pricing())
        }
        _ => None,
    }
}

fn hosted_deepseek_v4_flash_standard_pricing() -> ModelPricing {
    usd_only_pricing(0.028, 0.14, 0.28)
}

fn hosted_deepseek_v4_pro_standard_pricing() -> ModelPricing {
    usd_only_pricing(0.145, 1.74, 3.48)
}

pub fn currency_offering_pricing(pricing: CurrencyPricing, currency: Currency) -> OfferingPricing {
    OfferingPricing {
        currency,
        input_per_million: Some(pricing.input_cache_miss_per_million),
        output_per_million: Some(pricing.output_per_million),
        cache_read_per_million: Some(pricing.input_cache_hit_per_million),
        cache_write_per_million: pricing
            .cache_write
            .rate(pricing.input_cache_miss_per_million),
        provenance: PricingProvenance::ProviderDocs,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_peak_window_is_half_open_on_utc_hours() {
        for hour in 0..24 {
            let expected = matches!(hour, 1..=3 | 6..=9);
            assert_eq!(deepseek_peak_hour(hour), expected, "hour {hour}");
        }
    }

    #[test]
    fn deepseek_weekend_edges_are_bounded_in_beijing_time_not_utc() {
        // All four instants are off-peak by the hour, so `deepseek_is_peak`
        // cannot tell them apart today. Pinning the predicate keeps the 16:00Z
        // edges right if the peak windows ever move.
        let cases = [
            (Utc.with_ymd_and_hms(2026, 8, 28, 15, 0, 0).unwrap(), false), // Fri 23:00 Beijing
            (Utc.with_ymd_and_hms(2026, 8, 28, 16, 0, 0).unwrap(), true),  // Sat 00:00 Beijing
            (Utc.with_ymd_and_hms(2026, 8, 30, 15, 0, 0).unwrap(), true),  // Sun 23:00 Beijing
            (Utc.with_ymd_and_hms(2026, 8, 30, 16, 0, 0).unwrap(), false), // Mon 00:00 Beijing
        ];
        for (at, weekend) in cases {
            assert_eq!(deepseek_weekend_off_peak(at), weekend, "weekend at {at}");
        }
    }
}

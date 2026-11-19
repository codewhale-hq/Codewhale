//! Behavior tests for the offering pricing projection (#3085).

use super::*;
use crate::catalog::{CatalogOffering, CatalogSource, bundled_offerings_from_models_dev};
use crate::models_dev::{ModelsDevCatalog, ModelsDevCost};
use crate::route::PricingSku;

/// A DeepSeek-shaped priced offering (input/output/cache-read known,
/// cache-write deliberately unknown) tagged with the given provenance source.
fn priced(source: CatalogSource) -> CatalogOffering {
    CatalogOffering {
        provider: "deepseek".into(),
        wire_model_id: "deepseek-v4-pro".into(),
        canonical_model: Some("deepseek-v4-pro".into()),
        endpoint_key: "chat".into(),
        cost: Some(ModelsDevCost {
            input: Some(0.28),
            output: Some(0.42),
            cache_read: Some(0.028),
            cache_write: None,
            ..Default::default()
        }),
        source,
        ..Default::default()
    }
}

#[test]
fn maps_models_dev_cost_with_bundled_provenance_in_usd() {
    let p =
        OfferingPricing::from_catalog_offering(&priced(CatalogSource::Bundled)).expect("priced");
    assert_eq!(p.currency, Currency::Usd);
    assert_eq!(p.input_per_million, Some(0.28));
    assert_eq!(p.output_per_million, Some(0.42));
    assert_eq!(p.cache_read_per_million, Some(0.028));
    assert_eq!(p.cache_write_per_million, None);
    assert_eq!(p.provenance, PricingProvenance::ModelsDevBundled);
    assert_eq!(p.effective_at, None);
    assert!(p.has_any_price());
}

#[test]
fn malformed_catalog_prices_fail_closed_at_every_projection() {
    for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.01] {
        for field in 0..7 {
            let mut offering = priced(CatalogSource::Bundled);
            offering.provider = "openrouter".to_string();
            offering.wire_model_id = "openai/gpt-5.5".to_string();
            let cost = offering.cost.as_mut().expect("cost fixture");
            match field {
                0 => cost.input = Some(invalid),
                1 => cost.output = Some(invalid),
                2 => cost.cache_read = Some(invalid),
                3 => cost.cache_write = Some(invalid),
                4 => cost.reasoning = Some(invalid),
                5 => cost.input_audio = Some(invalid),
                6 => cost.output_audio = Some(invalid),
                _ => unreachable!(),
            }
            assert!(
                OfferingPricing::from_catalog_offering(&offering).is_none(),
                "field {field} accepted {invalid:?}"
            );
            assert_eq!(
                route_pricing_sku(&offering),
                PricingSku::UnknownOrStale,
                "route projection accepted field {field} = {invalid:?}"
            );
        }
    }

    let zero = priced(CatalogSource::Bundled);
    let mut zero = zero;
    let cost = zero.cost.as_mut().expect("cost fixture");
    cost.input = Some(0.0);
    cost.output = Some(0.0);
    cost.cache_read = Some(0.0);
    cost.cache_write = Some(0.0);
    assert!(OfferingPricing::from_catalog_offering(&zero).is_some());
}

/// A price of an impossible magnitude is a unit error, not an expensive model.
///
/// The two ways this happens in the wild are a per-token price parsed as
/// per-million (10^6 too large) and a minor-unit integer read as a major unit
/// (10^2 too large). Both would bill the user orders of magnitude over, so the
/// row is rejected at the same boundary that rejects NaN and negatives — and
/// rejected *whole*, so the surviving fields cannot produce a quietly
/// under-counted estimate instead.
#[test]
fn absurd_catalog_prices_fail_closed_at_every_projection() {
    let absurd = [
        // $3/token, mistakenly published as a per-million rate.
        3_000_000.0,
        // The declared bound itself is out, one ulp above is far out.
        MAX_PLAUSIBLE_PRICE_PER_MILLION + 1.0,
        f64::MAX,
    ];
    for price in absurd {
        for field in 0..7 {
            let mut offering = priced(CatalogSource::Bundled);
            let cost = offering.cost.as_mut().expect("cost fixture");
            match field {
                0 => cost.input = Some(price),
                1 => cost.output = Some(price),
                2 => cost.cache_read = Some(price),
                3 => cost.cache_write = Some(price),
                4 => cost.reasoning = Some(price),
                5 => cost.input_audio = Some(price),
                6 => cost.output_audio = Some(price),
                _ => unreachable!(),
            }
            assert!(
                !catalog_cost_is_valid(cost),
                "field {field} accepted absurd price {price}"
            );
            assert!(
                OfferingPricing::from_catalog_offering(&offering).is_none(),
                "field {field} produced pricing from absurd price {price}"
            );
            assert_eq!(
                route_pricing_sku(&offering),
                PricingSku::UnknownOrStale,
                "route projection accepted field {field} = {price}"
            );
        }
    }

    // The bound is generous on purpose: a genuinely expensive published rate
    // stays priced. Rejecting a real price would be its own kind of lie.
    let mut expensive = priced(CatalogSource::Bundled);
    let cost = expensive.cost.as_mut().expect("cost fixture");
    cost.input = Some(600.0);
    cost.output = Some(2_400.0);
    assert!(
        OfferingPricing::from_catalog_offering(&expensive).is_some(),
        "a plausible frontier rate must survive the magnitude bound"
    );

    // Exactly at the bound is still accepted; only values above it are not.
    let mut boundary = priced(CatalogSource::Bundled);
    let cost = boundary.cost.as_mut().expect("cost fixture");
    cost.input = Some(MAX_PLAUSIBLE_PRICE_PER_MILLION);
    assert!(OfferingPricing::from_catalog_offering(&boundary).is_some());
}

#[test]
fn live_source_carries_provider_live_provenance_and_fetch_time() {
    let src = CatalogSource::Live {
        base_url_fingerprint: "fp".into(),
        fetched_at: 1_700,
    };
    let p = OfferingPricing::from_catalog_offering(&priced(src)).expect("priced");
    assert_eq!(p.provenance, PricingProvenance::ProviderLive);
    assert_eq!(p.effective_at, None);
    assert_eq!(p.fetched_at, Some(1_700));
    assert_eq!(p.endpoint_fingerprint.as_deref(), Some("fp"));
}

/// A live row is only authoritative while it is *both* fresh and fetched from
/// the endpoint the turn was served on. Every other combination is a defect the
/// caller must fail closed on rather than billing against.
#[test]
fn live_pricing_defect_gates_stale_and_mismatched_rows() {
    let live = OfferingPricing::from_catalog_offering(&priced(CatalogSource::Live {
        base_url_fingerprint: "route-fp".into(),
        fetched_at: 1_000,
    }))
    .expect("priced");

    // Fresh + fingerprint-matched: authoritative.
    assert_eq!(
        live.live_pricing_defect(Some("route-fp"), Some(1_500), 1_000),
        None
    );

    // Same age, at the window boundary: stale is inclusive, matching
    // `is_stale`, so a row exactly at the TTL is not authoritative.
    assert_eq!(
        live.live_pricing_defect(Some("route-fp"), Some(2_000), 1_000),
        Some(LivePricingDefect::Stale {
            age_secs: 1_000,
            max_age_secs: 1_000,
        })
    );

    // A row fetched from a different endpoint prices a different billing
    // surface; it is never a "fresher price" for this route.
    assert_eq!(
        live.live_pricing_defect(Some("other-fp"), Some(1_500), 1_000),
        Some(LivePricingDefect::EndpointMismatch {
            row_fingerprint: "route-fp".into(),
            route_fingerprint: "other-fp".into(),
        })
    );

    // An unknown route endpoint cannot confirm any live row.
    assert_eq!(
        live.live_pricing_defect(None, Some(1_500), 1_000),
        Some(LivePricingDefect::UnknownRouteEndpoint)
    );

    // No clock means the age is unknowable, so the row stays unproven rather
    // than being assumed fresh.
    assert_eq!(
        live.live_pricing_defect(Some("route-fp"), None, 1_000),
        Some(LivePricingDefect::MissingTimestamp)
    );

    // Non-live provenances carry no fetch clock and are not age-gated here.
    for source in [CatalogSource::Bundled, CatalogSource::UserOverride] {
        let row = OfferingPricing::from_catalog_offering(&priced(source)).expect("priced");
        assert_eq!(row.live_pricing_defect(None, Some(u64::MAX), 1), None);
        assert!(row.provenance.is_authoritative_without_freshness_check());
    }
    assert!(!PricingProvenance::ProviderLive.is_authoritative_without_freshness_check());
}

/// A live row that claims live provenance but lost its fingerprint (hand-built
/// or migrated from an older schema) cannot be matched to a route.
#[test]
fn live_row_without_a_fingerprint_is_never_authoritative() {
    let mut live = OfferingPricing::from_catalog_offering(&priced(CatalogSource::Live {
        base_url_fingerprint: "fp".into(),
        fetched_at: 1_000,
    }))
    .expect("priced");
    live.endpoint_fingerprint = None;
    assert_eq!(
        live.live_pricing_defect(Some("fp"), Some(1_001), 1_000),
        Some(LivePricingDefect::MissingEndpointFingerprint)
    );

    // Defect labels are stable, non-localized, and carry no URL — only the
    // non-secret FNV digests the catalog already scopes caches on.
    let mismatch = LivePricingDefect::EndpointMismatch {
        row_fingerprint: "a".into(),
        route_fingerprint: "b".into(),
    };
    assert_eq!(mismatch.label(), "live_pricing_endpoint_mismatch");
    let json = serde_json::to_string(&mismatch).expect("serialize defect");
    assert!(!json.contains("http"), "{json}");
}

#[test]
fn no_cost_or_empty_cost_object_is_unknown() {
    let mut offering = priced(CatalogSource::Bundled);
    offering.cost = None;
    assert!(
        OfferingPricing::from_catalog_offering(&offering).is_none(),
        "absent cost is unknown, not free"
    );

    // A cost object present but with no concrete price is still unknown.
    offering.cost = Some(ModelsDevCost::default());
    let withheld =
        OfferingPricing::from_catalog_offering(&offering).expect("explicit withheld rate");
    assert!(withheld.rate_withheld);
    assert_eq!(withheld.estimate_cost(&TokenUsage::default()), None);
}

#[test]
fn estimate_cost_sums_priced_classes() {
    let p = OfferingPricing::from_catalog_offering(&priced(CatalogSource::Bundled)).unwrap();
    // 1M input @0.28 + 0.5M output @0.42 + 2M cache_read @0.028 = 0.546
    let usage = TokenUsage {
        input: 1_000_000,
        output: 500_000,
        cache_read: 2_000_000,
        cache_write: 0,

        reasoning: None,
        input_audio: None,
        output_audio: None,

        request_usages: Vec::new(),
    };
    let cost = p.estimate_cost(&usage).expect("priced classes estimate");
    assert!((cost - 0.546).abs() < 1e-9, "got {cost}");
}

#[test]
fn estimate_cost_is_none_when_a_used_class_is_unpriced() {
    // cache_write price is unknown; charging cache-write tokens cannot be
    // estimated honestly, so the whole estimate is None rather than under-reported.
    let p = OfferingPricing::from_catalog_offering(&priced(CatalogSource::Bundled)).unwrap();
    let usage = TokenUsage {
        input: 100,
        output: 0,
        cache_read: 0,
        cache_write: 10,

        reasoning: None,
        input_audio: None,
        output_audio: None,

        request_usages: Vec::new(),
    };
    assert!(p.estimate_cost(&usage).is_none());
}

#[test]
fn estimate_cost_with_zero_usage_is_zero() {
    let p = OfferingPricing::from_catalog_offering(&priced(CatalogSource::Bundled)).unwrap();
    assert_eq!(p.estimate_cost(&TokenUsage::default()), Some(0.0));
}

#[test]
fn finite_rates_that_overflow_the_computed_total_fail_closed() {
    // Constructed directly rather than through `from_catalog_offering`: the
    // magnitude bound now rejects a rate this large at the catalog boundary, so
    // the only way to reach the estimator with one is to bypass that boundary.
    // The estimator keeps its own overflow guard regardless — it is the last
    // check before a number becomes money, and it must not depend on an earlier
    // layer having run.
    let pricing = OfferingPricing {
        provider: "deepseek".to_string(),
        wire_model_id: "deepseek-v4-pro".to_string(),
        canonical_model: Some("deepseek-v4-pro".to_string()),
        currency: Currency::Usd,
        input_per_million: Some(f64::MAX),
        output_per_million: None,
        cache_read_per_million: None,
        cache_write_per_million: None,
        provenance: PricingProvenance::ModelsDevBundled,
        effective_at: None,
        endpoint_fingerprint: None,

        ..Default::default()
    };
    let usage = TokenUsage {
        input: u64::MAX,
        ..TokenUsage::default()
    };
    assert_eq!(pricing.estimate_cost(&usage), None);

    // And the boundary itself refuses to hand such a row over in the first
    // place, so the guard above is defence in depth, not the only defence.
    let mut offering = priced(CatalogSource::Bundled);
    offering.cost.as_mut().expect("cost fixture").input = Some(f64::MAX);
    assert!(OfferingPricing::from_catalog_offering(&offering).is_none());
}

#[test]
fn route_pricing_sku_is_token_when_priced_and_unknown_otherwise() {
    match route_pricing_sku(&priced(CatalogSource::Bundled)) {
        PricingSku::Token {
            input_per_mtok,
            output_per_mtok,
        } => {
            assert_eq!(input_per_mtok, Some(0.28));
            assert_eq!(output_per_mtok, Some(0.42));
        }
        other => panic!("expected Token, got {other:?}"),
    }

    // No cost → honest UnknownOrStale, never a fabricated zero price.
    let mut unpriced = priced(CatalogSource::Bundled);
    unpriced.cost = None;
    assert!(matches!(
        route_pricing_sku(&unpriced),
        PricingSku::UnknownOrStale
    ));
}

#[test]
fn currency_round_trips_including_other() {
    for currency in [Currency::Usd, Currency::Cny, Currency::Other("eur".into())] {
        let json = serde_json::to_string(&currency).expect("serialize");
        let back: Currency = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(currency, back);
    }
}

#[test]
fn user_override_pricing_round_trips_and_carries_no_secrets() {
    let pricing = OfferingPricing {
        provider: "custom".into(),
        wire_model_id: "house-model".into(),
        canonical_model: None,
        currency: Currency::Cny,
        input_per_million: Some(8.0),
        output_per_million: Some(16.0),
        cache_read_per_million: None,
        cache_write_per_million: None,
        provenance: PricingProvenance::UserOverride,
        effective_at: None,
        endpoint_fingerprint: None,

        ..Default::default()
    };
    let json = serde_json::to_string_pretty(&pricing).expect("serialize");
    let back: OfferingPricing = serde_json::from_str(&json).expect("round-trip");
    assert_eq!(pricing, back);

    let lower = json.to_lowercase();
    for needle in [
        "api_key",
        "apikey",
        "authorization",
        "secret",
        "password",
        "bearer",
        "access_token",
    ] {
        assert!(!lower.contains(needle), "pricing JSON contains `{needle}`");
    }
}

#[test]
fn staleness_applies_to_live_rows_only() {
    let live = CatalogSource::Live {
        base_url_fingerprint: "fp".into(),
        fetched_at: 1_000,
    };
    let live_price = OfferingPricing::from_catalog_offering(&priced(live)).unwrap();
    assert!(!live_price.is_stale(1_500, 3_600), "within TTL");
    assert!(live_price.is_stale(5_000, 3_600), "past TTL");

    // A bundled price has no fetch clock and is not age-stale.
    let bundled = OfferingPricing::from_catalog_offering(&priced(CatalogSource::Bundled)).unwrap();
    assert!(!bundled.is_stale(u64::MAX, 1));
}

#[test]
fn pricing_flows_from_the_models_dev_parser() {
    let raw = r#"{
      "providers": {
        "zai": {
          "models": {
            "glm-5.2": {
              "id": "glm-5.2",
              "modalities": { "input": ["text"], "output": ["text"] },
              "cost": { "input": 1.4, "output": 4.4, "cache_read": 0.26 }
            }
          }
        }
      }
    }"#;
    let catalog = ModelsDevCatalog::parse_json(raw).expect("fixture parses");
    let rows = bundled_offerings_from_models_dev(&catalog);
    let pricing = OfferingPricing::from_catalog_offering(&rows[0]).expect("zai glm-5.2 is priced");

    assert_eq!(pricing.provider, "zai");
    assert_eq!(pricing.wire_model_id, "glm-5.2");
    assert_eq!(pricing.input_per_million, Some(1.4));
    assert_eq!(pricing.output_per_million, Some(4.4));
    assert_eq!(pricing.cache_read_per_million, Some(0.26));
    assert_eq!(pricing.provenance, PricingProvenance::ModelsDevBundled);
}

#[test]
fn cache_only_offering_is_unknown_at_the_route_layer() {
    // Priced only on cache classes (no input/output): the route Token badge
    // would have no visible rates, so route_pricing_sku degrades to
    // UnknownOrStale — yet the cache rate is still usable for estimate_cost.
    let mut offering = priced(CatalogSource::Bundled);
    offering.cost = Some(ModelsDevCost {
        input: None,
        output: None,
        cache_read: Some(0.028),
        cache_write: None,
        ..Default::default()
    });

    assert!(matches!(
        route_pricing_sku(&offering),
        PricingSku::UnknownOrStale
    ));

    let pricing =
        OfferingPricing::from_catalog_offering(&offering).expect("cache-only row is still priced");
    assert!(pricing.has_any_price());
    let usage = TokenUsage {
        cache_read: 1_000_000,
        ..Default::default()
    };
    assert_eq!(pricing.estimate_cost(&usage), Some(0.028));
}

#[test]
fn user_override_source_maps_through_from_catalog_offering() {
    // Exercises provenance_from_source / effective_at_from_source for the
    // override arm via the hydration path (not direct construction).
    let pricing = OfferingPricing::from_catalog_offering(&priced(CatalogSource::UserOverride))
        .expect("priced");
    assert_eq!(pricing.provenance, PricingProvenance::UserOverride);
    assert_eq!(pricing.effective_at, None);
}

#[test]
fn staleness_is_inclusive_at_the_ttl_boundary() {
    let live = CatalogSource::Live {
        base_url_fingerprint: "fp".into(),
        fetched_at: 1_000,
    };
    let p = OfferingPricing::from_catalog_offering(&priced(live)).unwrap();
    // age == max_age_secs counts as stale (`>=` semantics)...
    assert!(p.is_stale(1_100, 100));
    // ...one second younger is still fresh.
    assert!(!p.is_stale(1_099, 100));
}

#[test]
fn unpriced_used_classes_names_exactly_what_makes_an_estimate_fail_closed() {
    // The DeepSeek-shaped row publishes input/output/cache-read but no
    // cache-write rate.
    let pricing = OfferingPricing::from_catalog_offering(&priced(CatalogSource::Bundled))
        .expect("priced row");

    // A turn that never wrote to cache is fully priced.
    let no_write = TokenUsage {
        input: 1_000_000,
        output: 1_000_000,
        cache_read: 1_000_000,
        cache_write: 0,

        reasoning: None,
        input_audio: None,
        output_audio: None,

        request_usages: Vec::new(),
    };
    assert!(pricing.unpriced_used_classes(&no_write).is_empty());
    assert_eq!(pricing.estimate_cost(&no_write), Some(0.28 + 0.42 + 0.028));

    // The moment cache-write tokens appear, the estimate fails closed and the
    // audit names the single class responsible.
    let with_write = TokenUsage {
        cache_write: 1,
        ..no_write
    };
    assert_eq!(
        pricing.unpriced_used_classes(&with_write),
        vec![TokenClass::CacheWrite]
    );
    assert_eq!(pricing.estimate_cost(&with_write), None);

    // Zero-token classes never count as unpriced.
    let empty = TokenUsage::default();
    assert!(pricing.unpriced_used_classes(&empty).is_empty());
    assert_eq!(pricing.estimate_cost(&empty), Some(0.0));
}

#[test]
fn token_class_labels_and_counts_stay_aligned_with_token_usage() {
    let usage = TokenUsage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,

        reasoning: None,
        input_audio: None,
        output_audio: None,

        request_usages: Vec::new(),
    };
    let seen: Vec<(&str, u64)> = TokenClass::ALL
        .into_iter()
        .map(|class| (class.label(), class.tokens(&usage)))
        .collect();
    assert_eq!(
        seen,
        vec![
            ("input", 1),
            ("output", 2),
            ("cache_read", 3),
            ("cache_write", 4),
            ("reasoning", 0),
            ("input_audio", 0),
            ("output_audio", 0),
        ]
    );
}

#[test]
fn text_usage_prices_multimodal_and_equal_rate_reasoning_while_distinct_reasoning_fails_closed() {
    let gemini_carbon = OfferingPricing {
        provider: "google".into(),
        wire_model_id: "gemini-4-carbon".into(),
        currency: Currency::Usd,
        input_per_million: Some(2.0),
        output_per_million: Some(10.0),
        cache_read_per_million: Some(0.1),
        input_audio_per_million: Some(2.0),
        provenance: PricingProvenance::ModelsDevBundled,
        ..Default::default()
    };
    let text_turn = TokenUsage {
        input: 4_963,
        output: 46,
        cache_read: 131_324,
        ..Default::default()
    };
    let carbon_cost = gemini_carbon
        .estimate_cost(&text_turn)
        .expect("equal-rate input_audio must not block text turns");
    let expected_carbon = 4_963.0 * 2e-6 + 131_324.0 * 0.1e-6 + 46.0 * 10e-6;
    assert!(
        (carbon_cost - expected_carbon).abs() < 1e-12,
        "{carbon_cost} != {expected_carbon}"
    );

    let flash_lite = OfferingPricing {
        provider: "google".into(),
        wire_model_id: "gemini-3.1-flash-lite".into(),
        currency: Currency::Usd,
        input_per_million: Some(0.25),
        output_per_million: Some(1.5),
        cache_read_per_million: Some(0.025),
        input_audio_per_million: Some(0.5),
        provenance: PricingProvenance::ModelsDevBundled,
        ..Default::default()
    };
    let plain_text = TokenUsage {
        input: 1_000_000,
        output: 100_000,
        ..Default::default()
    };
    assert!(
        (flash_lite
            .estimate_cost(&plain_text)
            .expect("text turn on audio-capable model prices")
            - 0.4)
            .abs()
            < 1e-12
    );
    let audio_turn = TokenUsage {
        input: 1_000_000,
        output: 0,
        input_audio: Some(400_000),
        ..Default::default()
    };
    assert!(
        (flash_lite
            .estimate_cost(&audio_turn)
            .expect("explicit audio tokens bill at audio rate")
            - (0.6 * 0.25 + 0.4 * 0.5))
            .abs()
            < 1e-12
    );
    let audio_with_cache = TokenUsage {
        input: 1_000_000,
        cache_read: 100_000,
        input_audio: Some(400_000),
        ..Default::default()
    };
    assert_eq!(
        flash_lite.quote_cost(&audio_with_cache),
        Err(PricingQuoteError::MissingUsage(TokenClass::InputAudio))
    );

    let equal_reasoning = OfferingPricing {
        provider: "openrouter".into(),
        wire_model_id: "google/gemini-4-carbon".into(),
        currency: Currency::Usd,
        input_per_million: Some(2.0),
        output_per_million: Some(10.0),
        reasoning_per_million: Some(10.0),
        input_audio_per_million: Some(2.0),
        provenance: PricingProvenance::ModelsDevLive,
        ..Default::default()
    };
    assert!(
        (equal_reasoning
            .estimate_cost(&plain_text)
            .expect("equal reasoning/output rate prices without reasoning breakdown")
            - 3.0)
            .abs()
            < 1e-12
    );

    let distinct_reasoning = OfferingPricing {
        provider: "openrouter".into(),
        wire_model_id: "perplexity/sonar-deep-research".into(),
        currency: Currency::Usd,
        input_per_million: Some(2.0),
        output_per_million: Some(8.0),
        reasoning_per_million: Some(3.0),
        provenance: PricingProvenance::ModelsDevLive,
        ..Default::default()
    };
    assert_eq!(
        distinct_reasoning.quote_cost(&plain_text),
        Err(PricingQuoteError::MissingUsage(TokenClass::Reasoning))
    );
    let with_reasoning = TokenUsage {
        input: 0,
        output: 1_000_000,
        reasoning: Some(600_000),
        ..Default::default()
    };
    assert!(
        (distinct_reasoning
            .estimate_cost(&with_reasoning)
            .expect("reported reasoning breakdown prices at distinct rate")
            - (0.4 * 8.0 + 0.6 * 3.0))
            .abs()
            < 1e-12
    );
}

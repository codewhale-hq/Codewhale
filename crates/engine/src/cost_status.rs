//! Usage projections and source attribution receipts.

use codewhale_runtime::safe_label::{safe_receipt_field, sanitize_persisted_route_label};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{DateTime, Utc};

use crate::pricing::{CostEstimate, TurnCostAudit};
use crate::route_billing::BillingPresentation;
use codewhale_config::ProviderKind;
use codewhale_models::Usage;

/// Everything a drained background accrual needs to be explained.
///
/// The money and the coverage/provenance that qualify it are drained together,
/// so `/cost` can never show a background subtotal whose completeness came from
/// a different observation.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PendingBackgroundCost {
    /// Summed cost of the background turns that were priced.
    pub estimate: CostEstimate,
    /// Background turns that produced an authoritative price.
    pub priced_turns: u32,
    /// Background turns that were money-metered (or of unknown basis) but
    /// produced no authoritative price, so their spend is missing.
    pub unpriced_turns: u32,
    /// Money-metered turns authoritatively priced in CNY.
    pub cny_priced_turns: u32,
    /// Money-metered turns missing authoritative CNY pricing.
    pub cny_unpriced_turns: u32,
    /// Stable reason labels for the unpriced turns.
    pub unpriced_reasons: BTreeSet<&'static str>,
    pub cny_unpriced_reasons: BTreeSet<&'static str>,
    /// Token classes used on a background route that carry no published price.
    pub unpriced_classes: BTreeSet<&'static str>,
    /// Provenance labels of the pricing rows that were applied or attempted.
    pub pricing_provenances: BTreeSet<&'static str>,
    /// Live-pricing downgrade receipts, when a live catalog row could not be
    /// verified for the endpoint that served the turn.
    pub live_pricing_defects: BTreeSet<&'static str>,
    /// Live pricing failed and no bundled row could price the turn. Kept
    /// separate so `/cost` never claims a bundled fallback was used when the
    /// result is actually unavailable.
    pub live_pricing_unusable_defects: BTreeSet<&'static str>,
    /// One redacted receipt per distinct background route that reported.
    ///
    /// See [`EffectiveRouteEnvelope::receipt`] for the exact contents; these carry
    /// provider identity, endpoint *fingerprint*, billing surface, wire model,
    /// and currency — never a URL, key, token, or filesystem path.
    pub route_receipts: BTreeSet<String>,
    /// Durable, redacted identities of provider responses folded into this
    /// batch. These travel with the money so a session snapshot can make a
    /// replay idempotent after reload.
    pub usage_source_fingerprints: BTreeSet<String>,
    /// Prompt-cache classes the background routes reported, through
    /// [`crate::pricing::token_usage_for_pricing`] so they never exceed the
    /// input they partition (#6565). `None` until a child reports cache
    /// telemetry at all: no report is not a 0% hit rate.
    pub cache_hit_tokens: Option<u64>,
    pub cache_miss_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub source_records: Vec<RuntimeUsageRecord>,
}

/// Immutable, non-secret route evidence captured before a provider request.
/// It contains enough information to audit the eventual usage without reading
/// mutable parent/app config at completion time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveRouteEnvelope {
    pub provider: ProviderKind,
    pub provider_identity: String,
    pub model: String,
    /// Requested OpenRouter upstream, frozen with the client that dispatched.
    pub openrouter_vendor: Option<String>,
    pub billing_surface: Option<String>,
    pub endpoint_fingerprint: Option<String>,
    /// Frozen provider-live or signed cloud rates captured from the exact catalog scope
    /// at CodeWhale's pre-permit application-dispatch boundary. Legacy
    /// receipts omit this and therefore cannot meter a reviewed custom route
    /// retroactively.
    pub provider_live_pricing: Option<crate::provider_catalog_live::ProviderLivePricingQuote>,
    pub billing_mode: RouteBillingMode,
    pub dispatched_at: DateTime<Utc>,
}

#[derive(serde::Deserialize)]
struct EffectiveRouteEnvelopeWire {
    provider: String,
    provider_identity: String,
    model: String,
    /// Requested OpenRouter upstream, frozen with the client that dispatched.
    #[serde(default)]
    openrouter_vendor: Option<String>,
    billing_surface: Option<String>,
    endpoint_fingerprint: Option<String>,
    /// Frozen provider-live or signed cloud rates captured from the exact catalog scope
    /// at CodeWhale's pre-permit application-dispatch boundary. Legacy
    /// receipts omit this and therefore cannot meter a reviewed custom route
    /// retroactively.
    #[serde(
        default,
        deserialize_with = "crate::provider_catalog_live::deserialize_optional_provider_live_pricing"
    )]
    provider_live_pricing: Option<crate::provider_catalog_live::ProviderLivePricingQuote>,
    #[serde(default)]
    billing_mode: RouteBillingMode,
    dispatched_at: DateTime<Utc>,
}

impl<'de> serde::Deserialize<'de> for EffectiveRouteEnvelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = <EffectiveRouteEnvelopeWire as serde::Deserialize>::deserialize(deserializer)?;
        let provider = codewhale_config::descriptors::kind_from_tui_wire_tag(
            &wire.provider,
            &wire.provider_identity,
        )
        .ok_or_else(|| serde::de::Error::custom("contradictory usage provider identity"))?;
        Ok(Self {
            provider,
            provider_identity: wire.provider_identity,
            model: wire.model,
            openrouter_vendor: wire.openrouter_vendor,
            billing_surface: wire.billing_surface,
            endpoint_fingerprint: wire.endpoint_fingerprint,
            provider_live_pricing: wire.provider_live_pricing,
            billing_mode: wire.billing_mode,
            dispatched_at: wire.dispatched_at,
        })
    }
}

impl serde::Serialize for EffectiveRouteEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct as _;

        let route = self.sanitized_for_persistence();
        let mut state = serializer.serialize_struct(
            "EffectiveRouteEnvelope",
            8 + usize::from(route.openrouter_vendor.is_some()),
        )?;
        let provider = codewhale_config::descriptors::tui_wire_tag_for_route(
            route.provider,
            &route.provider_identity,
        )
        .ok_or_else(|| serde::ser::Error::custom("contradictory usage provider identity"))?;
        state.serialize_field("provider", provider)?;
        state.serialize_field("provider_identity", &route.provider_identity)?;
        state.serialize_field("model", &route.model)?;
        if let Some(vendor) = &route.openrouter_vendor {
            state.serialize_field("openrouter_vendor", vendor)?;
        }
        state.serialize_field("billing_surface", &route.billing_surface)?;
        state.serialize_field("endpoint_fingerprint", &route.endpoint_fingerprint)?;
        state.serialize_field("provider_live_pricing", &route.provider_live_pricing)?;
        state.serialize_field("billing_mode", &route.billing_mode)?;
        state.serialize_field("dispatched_at", &route.dispatched_at)?;
        state.end()
    }
}

/// One provider usage payload paired with the immutable route that served it.
///
/// Runtime hosts persist these for model calls made below the parent turn
/// (sub-agents, review/verify/RLM tools, and compaction). Keeping route and
/// usage together makes the record independently auditable and prevents a
/// later provider/model selection from changing its price.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffectiveRouteUsage {
    pub route: EffectiveRouteEnvelope,
    pub usage: Usage,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteBillingMode {
    Metered,
    Subscription,
    Local,
    #[default]
    Unknown,
}

impl From<BillingPresentation> for RouteBillingMode {
    fn from(value: BillingPresentation) -> Self {
        match value {
            BillingPresentation::Metered => Self::Metered,
            BillingPresentation::Subscription(_) => Self::Subscription,
            BillingPresentation::Local => Self::Local,
            BillingPresentation::Unknown => Self::Unknown,
        }
    }
}

impl EffectiveRouteEnvelope {
    /// Provider-neutral observation. This never opens configuration or credentials.
    #[must_use]
    pub fn capture_observed(
        provider: ProviderKind,
        provider_identity: impl Into<String>,
        model: impl Into<String>,
        base_url: Option<&str>,
        dispatched_at: DateTime<Utc>,
    ) -> Self {
        let key = provider_identity.into();
        Self::from_facts(None, (provider, &key), model, base_url, dispatched_at)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn capture(
        config: Option<&codewhale_runtime::config::Config>,
        provider: ProviderKind,
        provider_identity: impl Into<String>,
        model: impl Into<String>,
        base_url: Option<&str>,
        dispatched_at: DateTime<Utc>,
    ) -> Self {
        let key = provider_identity.into();
        if let Some(config) = config
            && let Ok(identity) =
                config.resolve_persisted_provider_identity(Some(provider.as_str()), Some(&key))
        {
            Self::from_admitted(Some(config), &identity, model, base_url, dispatched_at)
        } else {
            Self::capture_observed(provider, key, model, base_url, dispatched_at)
        }
    }

    #[must_use]
    pub fn from_admitted(
        config: Option<&codewhale_runtime::config::Config>,
        identity: &codewhale_runtime::config::ProviderIdentity,
        model: impl Into<String>,
        base_url: Option<&str>,
        dispatched_at: DateTime<Utc>,
    ) -> Self {
        let admission = config
            .filter(|config| config.verify_provider_identity(identity).is_ok())
            .map(|config| (config, identity));
        Self::from_facts(
            admission,
            (identity.provider, identity.key.as_str()),
            model,
            base_url,
            dispatched_at,
        )
    }

    fn from_facts(
        admission: Option<(
            &codewhale_runtime::config::Config,
            &codewhale_runtime::config::ProviderIdentity,
        )>,
        facts: (ProviderKind, &str),
        model: impl Into<String>,
        base_url: Option<&str>,
        dispatched_at: DateTime<Utc>,
    ) -> Self {
        let (provider, provider_identity) = facts;
        let config = admission.map(|(config, _)| config);
        let model = model.into();
        let billing = admission.map_or_else(
            || crate::route_billing::for_endpoint_without_config(provider, base_url),
            |(config, identity)| {
                base_url.map_or_else(
                    || crate::route_billing::for_route(config, identity),
                    |endpoint| {
                        crate::route_billing::for_route_with_endpoint(config, identity, endpoint)
                    },
                )
            },
        );
        let endpoint_fingerprint = base_url.and_then(endpoint_fingerprint);
        let provider_live_pricing = base_url.and_then(|base_url| {
            u64::try_from(dispatched_at.timestamp())
                .ok()
                .and_then(|at| {
                    crate::provider_catalog_live::declared_or_catalog_quote(
                        config.and_then(|config| {
                            crate::provider_catalog_live::configured_dispatch_pricing_quote_at(
                                config.custom_models.as_deref().unwrap_or_default(),
                                provider,
                                provider_identity,
                                &model,
                                base_url,
                                at,
                            )
                        }),
                        || {
                            crate::provider_catalog_live::fresh_dispatch_pricing_quote_at(
                                provider,
                                provider_identity,
                                &model,
                                base_url,
                                at,
                            )
                        },
                    )
                })
        });
        Self {
            provider,
            provider_identity: sanitize_persisted_route_label(provider_identity),
            model: sanitize_persisted_route_label(&model),
            openrouter_vendor: admission
                .filter(|_| provider == ProviderKind::Openrouter)
                .and_then(|(config, identity)| config.provider_config_for(identity))
                .and_then(|entry| entry.vendor.as_deref())
                .map(str::trim)
                .filter(|vendor| !vendor.is_empty())
                .map(sanitize_persisted_route_label),
            billing_surface: admission
                .map_or_else(
                    || crate::pricing::billing_surface_for_route(provider, base_url),
                    |(config, identity)| {
                        crate::route_billing::billing_surface_for_dispatch(
                            Some(config),
                            identity,
                            base_url,
                        )
                    },
                )
                .map(str::to_string),
            endpoint_fingerprint,
            provider_live_pricing,
            billing_mode: billing.into(),
            dispatched_at,
        }
    }

    #[must_use]
    pub fn audit_frozen(&self, usage: &Usage) -> TurnCostAudit {
        let audit = self.audit(usage);
        if self.provider_live_pricing.is_none()
            && audit
                .unpriced_reason
                .is_none_or(|reason| reason.counts_toward_money_coverage())
        {
            return TurnCostAudit::unpriced(crate::pricing::UnpricedReason::UnverifiedLivePricing);
        }
        audit
    }

    pub fn audit(&self, usage: &Usage) -> TurnCostAudit {
        let reviewed_custom_metered = crate::pricing::reviewed_custom_route_is_metered(
            self.provider,
            self.endpoint_fingerprint.as_deref(),
        );
        let declared_quote = self.provider_live_pricing.as_ref().filter(|quote| {
            quote.provenance == codewhale_config::pricing::PricingProvenance::UserOverride
                && self
                    .endpoint_fingerprint
                    .as_deref()
                    .zip(u64::try_from(self.dispatched_at.timestamp()).ok())
                    .is_some_and(|(fingerprint, at)| {
                        quote
                            .pricing_for_route(
                                self.provider,
                                &self.provider_identity,
                                &self.model,
                                fingerprint,
                                at,
                            )
                            .is_some()
                    })
        });
        let declared_estimate = declared_quote.is_some();
        match self.billing_mode {
            RouteBillingMode::Subscription | RouteBillingMode::Local => {
                return TurnCostAudit::unpriced(crate::pricing::UnpricedReason::NotMoneyMetered);
            }
            RouteBillingMode::Unknown if !reviewed_custom_metered && !declared_estimate => {
                return TurnCostAudit::unpriced(
                    crate::pricing::UnpricedReason::UnknownBillingBasis,
                );
            }
            RouteBillingMode::Metered | RouteBillingMode::Unknown => {}
        }
        // The OpenRouter model catalog does not identify a pinned upstream's
        // price. An endpoint match alone must not promote that aggregate rate.
        // An operator's own declared rate for this exact route is not the
        // aggregate catalog, so it still prices the pinned turn.
        if self.provider == ProviderKind::Openrouter
            && self.openrouter_vendor.is_some()
            && !declared_quote.is_some_and(|quote| quote.carries_rates())
        {
            return TurnCostAudit::unpriced(crate::pricing::UnpricedReason::RoutingDependentPrice);
        }
        crate::pricing::audit_turn_cost_for_route_on_endpoint_for_identity_at(
            self.provider,
            Some(&self.provider_identity),
            &self.model,
            self.billing_surface.as_deref(),
            self.endpoint_fingerprint.as_deref(),
            self.provider_live_pricing.as_ref(),
            usage,
            self.dispatched_at,
        )
    }

    #[must_use]
    pub fn receipt(&self, audit: &TurnCostAudit) -> String {
        let route = self.sanitized_for_persistence();
        let mut receipt = route_receipt(
            route.provider,
            Some(&route.provider_identity),
            &route.model,
            route.billing_surface.as_deref(),
            route.endpoint_fingerprint.as_deref(),
            route.billing_mode,
            currency_tag(audit),
        );
        if let Some(vendor) = route.openrouter_vendor.as_deref() {
            receipt.push_str(" openrouter_vendor=");
            receipt.push_str(&safe_receipt_field(vendor));
        }
        receipt
    }

    /// Redact filesystem-like labels before a route crosses a persistence or
    /// metadata boundary. Ordinary provider model namespaces such as
    /// `anthropic/claude-*` remain intact; absolute/local path forms do not.
    #[must_use]
    pub fn sanitized_for_persistence(&self) -> Self {
        let mut route = self.clone();
        route.provider_identity = sanitize_persisted_route_label(&route.provider_identity);
        route.model = sanitize_persisted_route_label(&route.model);
        route.openrouter_vendor = route
            .openrouter_vendor
            .as_deref()
            .map(sanitize_persisted_route_label);
        route.billing_surface = route
            .billing_surface
            .as_deref()
            .map(sanitize_persisted_route_label);
        route.endpoint_fingerprint =
            route
                .endpoint_fingerprint
                .as_deref()
                .and_then(|fingerprint| {
                    let fingerprint = fingerprint.trim();
                    (fingerprint.len() == 64
                        && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    .then(|| fingerprint.to_ascii_lowercase())
                });
        let quote_is_valid = route
            .provider_live_pricing
            .as_ref()
            .zip(route.endpoint_fingerprint.as_deref())
            .and_then(|(quote, fingerprint)| {
                u64::try_from(route.dispatched_at.timestamp())
                    .ok()
                    .and_then(|dispatched_at_unix| {
                        quote.pricing_for_route(
                            route.provider,
                            &route.provider_identity,
                            &route.model,
                            fingerprint,
                            dispatched_at_unix,
                        )
                    })
            })
            .is_some();
        if !quote_is_valid {
            route.provider_live_pricing = None;
        }
        route
    }
}

fn receipt_with_usage_classes(mut receipt: String, usage: &Usage) -> String {
    let classes = crate::pricing::token_usage_for_pricing(usage);
    if classes.cache_write > 0 {
        receipt.push_str(" cache_write=yes");
    }
    if usage.reasoning_tokens.unwrap_or(0) > 0 {
        receipt.push_str(" reasoning=yes");
    }
    receipt
}

/// Canonical redacted route receipt for one exact usage payload.
#[must_use]
pub fn effective_route_usage_receipt(
    route: &EffectiveRouteEnvelope,
    audit: &TurnCostAudit,
    usage: &Usage,
) -> String {
    receipt_with_usage_classes(route.receipt(audit), usage)
}

/// Canonical `child_*` token and route metadata for tools that make their own
/// LLM calls (`review`, `verify`, and `rlm`). Keeping this next to the immutable
/// route envelope prevents the pure model types from depending on app config.
#[must_use]
pub fn child_usage_metadata_fields(
    route: &EffectiveRouteEnvelope,
    usage: &Usage,
) -> serde_json::Map<String, serde_json::Value> {
    let route = route.sanitized_for_persistence();
    let mut fields = serde_json::Map::new();
    fields.insert("child_provider".into(), serde_json::json!(route.provider));
    fields.insert(
        "child_provider_identity".into(),
        serde_json::json!(route.provider_identity),
    );
    fields.insert("child_model".into(), serde_json::json!(route.model));
    fields.insert(
        "child_openrouter_vendor".into(),
        serde_json::json!(route.openrouter_vendor),
    );
    fields.insert(
        "child_billing_surface".into(),
        serde_json::json!(route.billing_surface),
    );
    fields.insert(
        "child_endpoint_fingerprint".into(),
        serde_json::json!(route.endpoint_fingerprint),
    );
    fields.insert(
        "child_provider_live_pricing".into(),
        serde_json::json!(route.provider_live_pricing),
    );
    fields.insert(
        "child_billing_mode".into(),
        serde_json::json!(route.billing_mode),
    );
    fields.insert(
        "child_dispatched_at".into(),
        serde_json::json!(route.dispatched_at),
    );
    fields.insert(
        "child_input_tokens".into(),
        serde_json::json!(usage.input_tokens),
    );
    fields.insert(
        "child_output_tokens".into(),
        serde_json::json!(usage.output_tokens),
    );
    fields.insert(
        "child_prompt_cache_hit_tokens".into(),
        serde_json::json!(usage.prompt_cache_hit_tokens),
    );
    fields.insert(
        "child_prompt_cache_miss_tokens".into(),
        serde_json::json!(usage.prompt_cache_miss_tokens),
    );
    fields.insert(
        "child_prompt_cache_write_tokens".into(),
        serde_json::json!(usage.prompt_cache_write_tokens),
    );
    // Informational: reasoning tokens are already included in output tokens.
    fields.insert(
        "child_reasoning_tokens".into(),
        serde_json::json!(usage.reasoning_tokens),
    );
    fields.insert(
        "child_reasoning_replay_tokens".into(),
        serde_json::json!(usage.reasoning_replay_tokens),
    );
    fields.insert(
        "child_server_tool_use".into(),
        serde_json::json!(usage.server_tool_use),
    );
    fields.insert(
        "child_input_audio_tokens".into(),
        serde_json::json!(usage.input_audio_tokens),
    );
    fields.insert(
        "child_output_audio_tokens".into(),
        serde_json::json!(usage.output_audio_tokens),
    );
    fields.insert(
        "child_safety_tokens".into(),
        serde_json::json!(usage.safety_tokens),
    );
    let requests = if usage.request_usages.len() > 1024
        || usage
            .request_usages
            .iter()
            .any(|request| !request.request_usages.is_empty())
    {
        vec![Usage::default(); 1025]
    } else {
        usage.request_usages.clone()
    };
    fields.insert("child_request_usages".into(), serde_json::json!(requests));
    fields
}

/// Merge canonical child usage into a tool metadata object.
pub fn attach_child_usage_metadata(
    metadata: &mut serde_json::Value,
    route: &EffectiveRouteEnvelope,
    usage: &Usage,
) {
    if let Some(object) = metadata.as_object_mut() {
        object.extend(child_usage_metadata_fields(route, usage));
    }
}

/// Maximum number of distinct routed-usage segments accepted from one tool
/// result. RLM reserves against the same bound before dispatch, so a valid
/// producer never has to discard a provider receipt after doing the work.
pub const MAX_CHILD_USAGE_RECORDS: usize = 64;

const CHILD_USAGE_RECORDS_KEY: &str = "child_usage_records";
const CHILD_DECISION_RECEIPTS_KEY: &str = "child_decision_receipts";
const CHILD_USAGE_DROP_RECORDS_KEY: &str = "child_usage_drop_records";
const CHILD_USAGE_DROPPED_RECORDS_KEY: &str = "child_usage_dropped_records";

/// Attach a bounded batch of routed child usage to tool metadata.
///
/// The source identity is reduced to a one-way fingerprint before metadata
/// can enter a transcript. Routes pass through their persistence sanitizer,
/// so neither a raw response id nor an endpoint/credential can hitch a ride.
/// New consumers prefer this batch over the legacy single `child_*` fields.
/// Attach a bounded batch containing both exact usage receipts and exact
/// provider-success/missing-usage route receipts.
pub fn attach_child_usage_batch_metadata(
    metadata: &mut serde_json::Value,
    batch: &RuntimeUsageBatch,
) {
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object.insert(
        CHILD_DECISION_RECEIPTS_KEY.into(),
        serde_json::json!(
            batch
                .decisions
                .iter()
                .filter(|receipt| receipt.is_bounded())
                .take(MAX_CHILD_USAGE_RECORDS)
                .map(RuntimeDecisionReceipt::sanitized)
                .collect::<Vec<_>>()
        ),
    );
    let retained_records = batch
        .records
        .iter()
        .take(MAX_CHILD_USAGE_RECORDS)
        .map(|record| {
            serde_json::json!({
                "source_id": format!(
                    "routed:{}",
                    usage_source_fingerprint(&record.source_id)
                ),
                "route": record.usage.route.sanitized_for_persistence(),
                "usage": record.usage.usage,
                "settlement": record.settlement,
            })
        })
        .collect::<Vec<_>>();
    let remaining = MAX_CHILD_USAGE_RECORDS.saturating_sub(retained_records.len());
    let retained_drops = batch
        .drop_records
        .iter()
        .take(remaining)
        .map(|record| RuntimeUsageDropRecord {
            reason: record.reason,
            source_id: format!("routed:{}", usage_source_fingerprint(&record.source_id)),
            route: record.route.sanitized_for_persistence(),
        })
        .collect::<Vec<_>>();
    object.insert(
        CHILD_USAGE_RECORDS_KEY.into(),
        serde_json::json!(retained_records),
    );
    object.insert(
        CHILD_USAGE_DROP_RECORDS_KEY.into(),
        serde_json::json!(retained_drops),
    );
    let usage_overflow = batch.records.len().saturating_sub(MAX_CHILD_USAGE_RECORDS);
    let decision_overflow = batch
        .decisions
        .len()
        .saturating_sub(MAX_CHILD_USAGE_RECORDS)
        .saturating_add(
            batch
                .decisions
                .iter()
                .take(MAX_CHILD_USAGE_RECORDS)
                .filter(|r| !r.is_bounded())
                .count(),
        );
    let dropped_records = batch
        .dropped_records
        .max(u64::try_from(batch.drop_records.len()).unwrap_or(u64::MAX))
        .saturating_add(u64::try_from(usage_overflow).unwrap_or(u64::MAX))
        .saturating_add(u64::try_from(decision_overflow).unwrap_or(u64::MAX));
    if dropped_records > 0 {
        object.insert(
            CHILD_USAGE_DROPPED_RECORDS_KEY.into(),
            serde_json::json!(dropped_records),
        );
    } else {
        object.remove(CHILD_USAGE_DROPPED_RECORDS_KEY);
    }
}

/// Parse the preferred routed child-usage batch.
///
/// `None` means the batch key was absent and callers may use the legacy
/// single-record parser. Once the key is present, malformed/overflow entries
/// are represented by `dropped_records` instead of falling back and risking a
/// partial subtotal being presented as complete.
#[must_use]
pub fn child_usage_records_from_metadata(
    metadata: &serde_json::Value,
) -> Option<RuntimeUsageBatch> {
    let value = metadata.get(CHILD_USAGE_RECORDS_KEY)?;
    let drop_values = metadata
        .get(CHILD_USAGE_DROP_RECORDS_KEY)
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let declared_dropped = metadata
        .get(CHILD_USAGE_DROPPED_RECORDS_KEY)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let Some(values) = value.as_array() else {
        return Some(RuntimeUsageBatch {
            decisions: Vec::new(),
            records: Vec::new(),
            drop_records: Vec::new(),
            dropped_records: declared_dropped.saturating_add(1),
        });
    };

    let overflow = values.len().saturating_sub(MAX_CHILD_USAGE_RECORDS);
    let mut batch = RuntimeUsageBatch {
        decisions: Vec::new(),
        records: Vec::with_capacity(values.len().min(MAX_CHILD_USAGE_RECORDS)),
        drop_records: Vec::with_capacity(drop_values.len().min(MAX_CHILD_USAGE_RECORDS)),
        dropped_records: declared_dropped
            .max(u64::try_from(drop_values.len()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(overflow).unwrap_or(u64::MAX)),
    };
    for value in values.iter().take(MAX_CHILD_USAGE_RECORDS) {
        let parsed = (|| {
            let source_id = value.get("source_id")?.as_str()?;
            let route =
                serde_json::from_value::<EffectiveRouteEnvelope>(value.get("route")?.clone())
                    .ok()?
                    .sanitized_for_persistence();
            let usage = serde_json::from_value::<Usage>(value.get("usage")?.clone()).ok()?;
            let mut record = RuntimeUsageRecord::legacy(
                usage_source_fingerprint(source_id),
                EffectiveRouteUsage { route, usage },
            );
            record.settlement = value
                .get("settlement")
                .filter(|value| !value.is_null())
                .map(|value| serde_json::from_value::<UsageSourceRevision>(value.clone()))
                .transpose()
                .ok()?;
            record.validate().ok()?;
            Some(record)
        })();
        if let Some(record) = parsed {
            batch.records.push(record);
        } else {
            batch.dropped_records = batch.dropped_records.saturating_add(1);
        }
    }
    let remaining = MAX_CHILD_USAGE_RECORDS.saturating_sub(batch.records.len());
    for value in drop_values.iter().take(remaining) {
        let parsed = (|| {
            let source_id = value.get("source_id")?.as_str()?;
            let route =
                serde_json::from_value::<EffectiveRouteEnvelope>(value.get("route")?.clone())
                    .ok()?
                    .sanitized_for_persistence();
            Some(RuntimeUsageDropRecord {
                reason: value
                    .get("reason")
                    .map(|value| serde_json::from_value(value.clone()))
                    .transpose()
                    .ok()?
                    .unwrap_or_default(),
                source_id: usage_source_fingerprint(source_id),
                route,
            })
        })();
        if let Some(record) = parsed {
            batch.drop_records.push(record);
        }
        // Every declared drop slot already contributes to dropped_records,
        // including malformed entries; do not count the same gap twice.
    }
    if let Some(decisions) = metadata.get(CHILD_DECISION_RECEIPTS_KEY) {
        if let Some(values) = decisions.as_array() {
            for value in values.iter().take(MAX_CHILD_USAGE_RECORDS) {
                let bounded = serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= 8 * 1024);
                let parsed = bounded
                    .then(|| serde_json::from_value::<RuntimeDecisionReceipt>(value.clone()).ok())
                    .flatten();
                if let Some(receipt) = parsed.filter(RuntimeDecisionReceipt::is_bounded) {
                    batch.decisions.push(receipt.sanitized());
                } else {
                    batch.dropped_records = batch.dropped_records.saturating_add(1);
                }
            }
            batch.dropped_records = batch.dropped_records.saturating_add(
                u64::try_from(values.len().saturating_sub(MAX_CHILD_USAGE_RECORDS))
                    .unwrap_or(u64::MAX),
            );
        } else {
            batch.dropped_records = batch.dropped_records.saturating_add(1);
        }
    }
    Some(batch)
}

/// Rehydrate the immutable route envelope emitted with child usage. Legacy or
/// incomplete metadata becomes an explicitly unknown route and never borrows
/// mutable parent-session facts.
#[must_use]
pub fn child_route_envelope_from_metadata(
    metadata: &serde_json::Value,
) -> Option<EffectiveRouteEnvelope> {
    let model = metadata.get("child_model")?.as_str()?.to_string();
    let provider = metadata
        .get("child_provider")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    let provider_identity = metadata
        .get("child_provider_identity")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    let billing_mode = metadata
        .get("child_billing_mode")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    let dispatched_at = metadata
        .get("child_dispatched_at")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());

    let complete = provider.is_some()
        && provider_identity.is_some()
        && billing_mode.is_some()
        && dispatched_at.is_some();
    Some(
        EffectiveRouteEnvelope {
            provider: provider.unwrap_or(ProviderKind::Custom),
            provider_identity: provider_identity.unwrap_or_else(|| "legacy-unreported".to_string()),
            model,
            openrouter_vendor: metadata
                .get("child_openrouter_vendor")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            billing_surface: metadata
                .get("child_billing_surface")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            endpoint_fingerprint: metadata
                .get("child_endpoint_fingerprint")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            provider_live_pricing: metadata
                .get("child_provider_live_pricing")
                .cloned()
                .and_then(|value| serde_json::from_value(value).ok()),
            billing_mode: billing_mode
                .filter(|_| complete)
                .unwrap_or(RouteBillingMode::Unknown),
            dispatched_at: dispatched_at.unwrap_or_else(|| {
                DateTime::<Utc>::from_timestamp(0, 0).expect("Unix epoch is representable")
            }),
        }
        .sanitized_for_persistence(),
    )
}

/// Rehydrate the complete child usage payload emitted by
/// [`attach_child_usage_metadata`]. The presence of a canonical child token
/// field is significant even when every value is zero: a zero-usage provider
/// call still needs a route receipt and coverage classification.
#[must_use]
pub fn child_usage_from_metadata(metadata: &serde_json::Value) -> Option<Usage> {
    const TOKEN_FIELDS: &[&str] = &[
        "child_input_tokens",
        "child_output_tokens",
        "child_prompt_cache_hit_tokens",
        "child_prompt_cache_miss_tokens",
        "child_prompt_cache_write_tokens",
        "child_reasoning_tokens",
        "child_reasoning_replay_tokens",
        "child_input_audio_tokens",
        "child_output_audio_tokens",
        "child_request_usages",
    ];
    if !TOKEN_FIELDS
        .iter()
        .any(|field| metadata.get(field).is_some())
    {
        return None;
    }

    fn u32_field(metadata: &serde_json::Value, field: &str) -> Option<u32> {
        metadata
            .get(field)
            .and_then(serde_json::Value::as_u64)
            .map(|value| u32::try_from(value).unwrap_or(u32::MAX))
    }

    let invalid_requests = || vec![Usage::default(); 1025];
    let token_value = |value: &serde_json::Value| {
        value.is_null()
            || value
                .as_u64()
                .is_some_and(|value| u32::try_from(value).is_ok())
    };
    let flat_request = |request: &serde_json::Value| {
        request.as_object().is_some_and(|fields| {
            ["input_tokens", "output_tokens"].iter().all(|field| {
                fields
                    .get(*field)
                    .is_some_and(|value| value.as_u64().is_some())
            }) && fields.iter().all(|(key, value)| match key.as_str() {
                "input_tokens"
                | "output_tokens"
                | "prompt_cache_hit_tokens"
                | "prompt_cache_miss_tokens"
                | "prompt_cache_write_tokens"
                | "reasoning_tokens"
                | "reasoning_replay_tokens"
                | "input_audio_tokens"
                | "output_audio_tokens"
                | "safety_tokens" => token_value(value),
                "request_usages" => value.as_array().is_some_and(|nested| nested.is_empty()),
                "server_tool_use" => {
                    value.is_null()
                        || value.as_object().is_some_and(|tools| {
                            tools.iter().all(|(key, value)| {
                                matches!(
                                    key.as_str(),
                                    "code_execution_requests"
                                        | "tool_search_requests"
                                        | "web_search_requests"
                                        | "web_fetch_requests"
                                        | "tool_search"
                                        | "tool_fetch"
                                        | "tool_image_search"
                                ) && token_value(value)
                            })
                        })
                }
                _ => false,
            })
        })
    };
    let request_usages = match metadata.get("child_request_usages") {
        None => Vec::new(),
        Some(serde_json::Value::Array(requests))
            if requests.len() <= 1024 && requests.iter().all(flat_request) =>
        {
            serde_json::from_value(serde_json::Value::Array(requests.clone()))
                .unwrap_or_else(|_| invalid_requests())
        }
        Some(_) => invalid_requests(),
    };
    Some(Usage {
        input_tokens: u32_field(metadata, "child_input_tokens").unwrap_or(0),
        output_tokens: u32_field(metadata, "child_output_tokens").unwrap_or(0),
        prompt_cache_hit_tokens: u32_field(metadata, "child_prompt_cache_hit_tokens"),
        prompt_cache_miss_tokens: u32_field(metadata, "child_prompt_cache_miss_tokens"),
        prompt_cache_write_tokens: u32_field(metadata, "child_prompt_cache_write_tokens"),
        reasoning_tokens: u32_field(metadata, "child_reasoning_tokens"),
        reasoning_replay_tokens: u32_field(metadata, "child_reasoning_replay_tokens"),
        server_tool_use: metadata
            .get("child_server_tool_use")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),

        input_audio_tokens: u32_field(metadata, "child_input_audio_tokens"),
        output_audio_tokens: u32_field(metadata, "child_output_audio_tokens"),
        safety_tokens: u32_field(metadata, "child_safety_tokens"),
        request_usages,
    })
}

impl PendingBackgroundCost {
    /// Whether anything at all was accrued.
    ///
    /// Compared against `Default` rather than checking a subset of fields, so a
    /// field added later cannot be silently left out of the emptiness test.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Default)]
struct ScopedPendingBackgroundCost {
    generation: u64,
    pending: PendingBackgroundCost,
    seen_usage_source_fingerprints: HashSet<String>,
    retired_usage_sources: HashSet<String>,
    source_records: BTreeMap<String, RuntimeUsageRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostScopeToken(u64);

pub type RuntimeDecisionSink = Arc<dyn Fn(RuntimeDecisionReceipt) -> bool + Send + Sync>;
pub type RuntimeUsageSink = Arc<dyn Fn(RuntimeUsageRecord) -> bool + Send + Sync>;
pub type RuntimeDropSink = Arc<dyn Fn(RuntimeUsageDropRecord) -> bool + Send + Sync>;

#[derive(Clone)]
pub struct NativeUsageOrigin {
    scope: CostScopeToken,
    #[cfg(any(test, feature = "test-support"))]
    test_origin: std::thread::ThreadId,
    destination: NativeUsageDestination,
}

#[derive(Clone)]
enum NativeUsageDestination {
    Session {
        sessions_dir: std::path::PathBuf,
        session_id: String,
        operation_id: String,
    },
    Runtime {
        owner: String,
        sink: RuntimeUsageSink,
        drop_sink: Option<RuntimeDropSink>,
        decision_sink: Option<RuntimeDecisionSink>,
    },
}

impl std::fmt::Debug for NativeUsageOrigin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NativeUsageOrigin")
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

static WARNED_USAGE_PERSISTENCE_FAILURES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn should_warn_usage_persistence_failure(session_id: &str, error: &std::io::Error) -> bool {
    let mut warned = WARNED_USAGE_PERSISTENCE_FAILURES
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if warned.len() >= 256 {
        warned.clear();
    }
    warned.insert(format!("{session_id}:{:?}", error.kind()))
}

impl NativeUsageOrigin {
    pub(crate) fn scope(&self) -> CostScopeToken {
        self.scope
    }

    pub(crate) fn session(
        scope: CostScopeToken,
        sessions_dir: std::path::PathBuf,
        session_id: String,
    ) -> Self {
        Self {
            scope,
            #[cfg(any(test, feature = "test-support"))]
            test_origin: test_cost_scope_id(),
            destination: NativeUsageDestination::Session {
                sessions_dir,
                session_id,
                operation_id: uuid::Uuid::new_v4().to_string(),
            },
        }
    }

    pub(crate) fn captured_runtime(
        scope: CostScopeToken,
        owner: String,
        sink: RuntimeUsageSink,
    ) -> Self {
        Self::captured_runtime_with_sinks(scope, owner, sink, None, None)
    }

    pub(crate) fn captured_runtime_with_sinks(
        scope: CostScopeToken,
        owner: String,
        sink: RuntimeUsageSink,
        drop_sink: Option<RuntimeDropSink>,
        decision_sink: Option<RuntimeDecisionSink>,
    ) -> Self {
        Self {
            scope,
            #[cfg(any(test, feature = "test-support"))]
            test_origin: test_cost_scope_id(),
            destination: NativeUsageDestination::Runtime {
                owner,
                sink,
                drop_sink,
                decision_sink,
            },
        }
    }

    pub(crate) fn publish(&self, record: &RuntimeUsageRecord) {
        #[cfg(any(test, feature = "test-support"))]
        let _origin = bind_test_cost_scope(self.test_origin);
        match &self.destination {
            NativeUsageDestination::Session {
                sessions_dir,
                session_id,
                operation_id,
            } => {
                if let Err(error) = crate::session_manager::SessionManager::new(
                    sessions_dir.clone(),
                )
                .and_then(|manager| {
                    crate::usage_persistence::report_runtime_usage_record_for_origin_with_manager(
                        self.scope,
                        session_id,
                        operation_id,
                        record,
                        &manager,
                    )
                }) {
                    if should_warn_usage_persistence_failure(session_id, &error) {
                        tracing::warn!(%error, session_id = %session_id, "usage persistence failed; continuing with unknown cost");
                    } else {
                        tracing::debug!(%error, session_id = %session_id, "usage persistence failed again; continuing with unknown cost");
                    }
                }
            }
            NativeUsageDestination::Runtime { owner, sink, .. } => {
                if !sink(record.clone()) {
                    tracing::warn!(
                        owner = %owner,
                        "captured usage receipt was not persisted; continuing"
                    );
                }
            }
        }
    }

    pub(crate) fn publish_drop(&self, record: &RuntimeUsageDropRecord) {
        #[cfg(any(test, feature = "test-support"))]
        let _origin = bind_test_cost_scope(self.test_origin);
        match &self.destination {
            NativeUsageDestination::Session {
                sessions_dir,
                session_id,
                operation_id,
            } => {
                let ok = crate::session_manager::SessionManager::new(sessions_dir.clone())
                    .is_ok_and(|manager| {
                        crate::usage_persistence::report_missing_usage_for_interactive_origin_with_manager(
                            self.scope,
                            session_id,
                            operation_id,
                            &record.source_id,
                            &record.route,
                            record.reason,
                            &manager,
                        )
                    });
                if !ok {
                    tracing::warn!(session_id = %session_id, "missing-usage receipt was not persisted; continuing");
                }
            }
            NativeUsageDestination::Runtime {
                owner,
                sink,
                drop_sink,
                ..
            } => {
                let ok = if let Some(drop_sink) = drop_sink {
                    drop_sink(record.clone())
                } else {
                    sink(record.as_usage_record())
                };
                if !ok {
                    tracing::warn!(owner = %owner, "runtime drop receipt was not persisted; continuing");
                }
            }
        }
    }

    pub(crate) fn publish_decision(&self, receipt: &RuntimeDecisionReceipt) {
        #[cfg(any(test, feature = "test-support"))]
        let _origin = bind_test_cost_scope(self.test_origin);
        match &self.destination {
            NativeUsageDestination::Session {
                sessions_dir,
                session_id,
                operation_id,
            } => {
                if let Ok(manager) =
                    crate::session_manager::SessionManager::new(sessions_dir.clone())
                    && manager
                        .persist_late_decision_receipt(session_id, operation_id, receipt)
                        .unwrap_or(false)
                {
                    let _ = manager.with_live_session_origin(session_id, || {
                        report_interactive_decision(self.scope, receipt);
                        true
                    });
                }
            }
            NativeUsageDestination::Runtime {
                owner,
                decision_sink,
                ..
            } => {
                if let Some(decision_sink) = decision_sink
                    && !decision_sink(receipt.clone())
                {
                    tracing::warn!(owner = %owner, "runtime decision receipt was not persisted; continuing");
                }
            }
        }
    }
}

#[cfg(not(any(test, feature = "test-support")))]
static PENDING: OnceLock<Mutex<ScopedPendingBackgroundCost>> = OnceLock::new();

#[cfg(any(test, feature = "test-support"))]
static TEST_PENDING: OnceLock<
    Mutex<std::collections::HashMap<std::thread::ThreadId, ScopedPendingBackgroundCost>>,
> = OnceLock::new();

fn with_pending_state_mut<R>(f: impl FnOnce(&mut ScopedPendingBackgroundCost) -> R) -> R {
    #[cfg(not(any(test, feature = "test-support")))]
    {
        let mut pending = PENDING
            .get_or_init(|| Mutex::new(ScopedPendingBackgroundCost::default()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        f(&mut pending)
    }
    #[cfg(any(test, feature = "test-support"))]
    {
        let mut by_thread = TEST_PENDING
            .get_or_init(|| Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        f(by_thread.entry(test_cost_scope_id()).or_default())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuntimeUsageBatch {
    pub decisions: Vec<RuntimeDecisionReceipt>,
    pub records: Vec<RuntimeUsageRecord>,
    pub drop_records: Vec<RuntimeUsageDropRecord>,
    pub dropped_records: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuntimeUsageRecord {
    pub source_id: String,
    pub usage: EffectiveRouteUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<UsageSourceRevision>,
}

impl PartialEq for RuntimeUsageRecord {
    fn eq(&self, other: &Self) -> bool {
        usage_source_fingerprint(&self.source_id) == usage_source_fingerprint(&other.source_id)
            && self.usage == other.usage
            && self.settlement == other.settlement
    }
}
impl Eq for RuntimeUsageRecord {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSpendOwner {
    Primary,
    Auxiliary,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UsageSourceRevision {
    lineage: String,
    revision: u64,
    previous_sha256: Option<String>,
    pub owner: UsageSpendOwner,
    pub coverage: crate::llm_client::ProviderUsageCoverage,
    pub reason: RuntimeUsageMissingReason,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub physical_requests: Vec<crate::llm_client::OutboundTransmissionObservation>,
    #[serde(default)]
    pub physical_requests_dropped: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    reservation: bool,
}

impl UsageSourceRevision {
    pub fn lineage(&self) -> &str {
        &self.lineage
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn is_reservation(&self) -> bool {
        self.reservation
    }
}

#[derive(Debug, Clone)]
pub struct NativeUsageSource {
    source_id: String,
    lineage: String,
    owner: UsageSpendOwner,
    origin: Option<NativeUsageOrigin>,
    wire: crate::llm_client::OutboundWireObservations,
    last_record: Arc<Mutex<Option<RuntimeUsageRecord>>>,
}

impl NativeUsageSource {
    pub fn capture(source_id: impl Into<String>, owner: UsageSpendOwner) -> Self {
        Self::capture_with_origin(source_id, owner, None)
    }

    pub fn capture_with_origin(
        source_id: impl Into<String>,
        owner: UsageSpendOwner,
        origin: Option<NativeUsageOrigin>,
    ) -> Self {
        let source_id = source_id.into();
        let source_id = if source_id.trim().is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            source_id
        };
        let wire = crate::llm_client::OutboundWireObservations::default();
        Self {
            source_id,
            lineage: uuid::Uuid::new_v4().to_string(),
            owner,
            origin,
            wire,
            last_record: Arc::new(Mutex::new(None)),
        }
    }

    pub fn origin(&self) -> Option<&NativeUsageOrigin> {
        self.origin.as_ref()
    }

    pub fn observation(&self) -> crate::llm_client::RequestRetryObservation {
        let mut obs =
            crate::llm_client::RequestRetryObservation::for_auxiliary_request(self.source_id());
        obs.wire_observations = self.wire.clone();
        obs
    }

    pub(crate) fn observed_wire(&self) -> crate::llm_client::OutboundWireObservationBatch {
        self.wire.snapshot()
    }

    pub fn source_id(&self) -> String {
        self.source_id.clone()
    }

    pub fn prepare_record(
        &self,
        route: &EffectiveRouteEnvelope,
        usage: &Usage,
        coverage: crate::llm_client::ProviderUsageCoverage,
        reason: RuntimeUsageMissingReason,
        wire: &crate::llm_client::OutboundWireObservationBatch,
    ) -> RuntimeUsageRecord {
        let effective = EffectiveRouteUsage {
            route: route.sanitized_for_persistence(),
            usage: usage.clone(),
        };
        let dropped_extra =
            u64::try_from(wire.transmissions.len().saturating_sub(32)).unwrap_or(u64::MAX);
        let physical_requests: Vec<_> = wire
            .transmissions
            .iter()
            .take(32)
            .cloned()
            .map(|mut attempt| {
                attempt.source_id = usage_source_fingerprint(&attempt.source_id);
                attempt
            })
            .collect();
        let physical_requests_dropped = wire.transmissions_dropped.saturating_add(dropped_extra);
        let mut last = self
            .last_record
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (revision, previous_sha256, lineage) = match last.as_ref() {
            Some(prior) if prior.usage.route == effective.route => {
                if prior.usage == effective
                    && prior.settlement.as_ref().is_some_and(|r| {
                        r.coverage == coverage
                            && r.reason == reason
                            && r.physical_requests == physical_requests
                            && r.physical_requests_dropped == physical_requests_dropped
                    })
                {
                    return prior.clone();
                }
                let prev_rev = prior.settlement.as_ref().map_or(0, |r| r.revision);
                (
                    prev_rev.saturating_add(1),
                    prior.digest().ok(),
                    prior
                        .settlement
                        .as_ref()
                        .map_or_else(|| self.lineage.clone(), |r| r.lineage.clone()),
                )
            }
            _ => (1, None, uuid::Uuid::new_v4().to_string()),
        };
        let record = RuntimeUsageRecord {
            source_id: self.source_id.clone(),
            usage: effective,
            settlement: Some(UsageSourceRevision {
                lineage,
                revision,
                previous_sha256,
                owner: self.owner,
                coverage,
                reason,
                physical_requests,
                physical_requests_dropped,
                reservation: false,
            }),
        };
        *last = Some(record.clone());
        record
    }

    pub fn prepare(
        &self,
        route: &EffectiveRouteEnvelope,
        usage: &Usage,
        coverage: crate::llm_client::ProviderUsageCoverage,
        reason: RuntimeUsageMissingReason,
    ) -> RuntimeUsageRecord {
        self.prepare_record(
            route,
            usage,
            coverage,
            reason,
            &crate::llm_client::OutboundWireObservationBatch::default(),
        )
    }

    pub fn response_record(
        &self,
        route: &EffectiveRouteEnvelope,
        response: &anyhow::Result<codewhale_models::MessageResponse>,
        wire: &crate::llm_client::OutboundWireObservationBatch,
    ) -> RuntimeUsageRecord {
        let empty = Usage::default();
        let usage = response
            .as_ref()
            .map(|response| &response.usage)
            .unwrap_or(&empty);
        let reported = usage != &empty;
        let coverage = if wire.last_transmission.is_some() {
            wire.usage_coverage(reported, response.is_ok())
        } else {
            match response {
                Ok(_) if reported => crate::llm_client::ProviderUsageCoverage::Reported,
                Ok(_) => crate::llm_client::ProviderUsageCoverage::Missing,
                Err(error) => match error
                    .downcast_ref::<crate::llm_client::LlmError>()
                    .map(crate::llm_client::LlmError::transmission)
                {
                    Some(crate::llm_client::RequestTransmission::NotDispatched) => {
                        crate::llm_client::ProviderUsageCoverage::NotDispatched
                    }
                    Some(crate::llm_client::RequestTransmission::Rejected) => {
                        crate::llm_client::ProviderUsageCoverage::Rejected
                    }
                    _ => crate::llm_client::ProviderUsageCoverage::Missing,
                },
            }
        };
        self.prepare_record(
            route,
            usage,
            coverage,
            if response.is_ok() {
                RuntimeUsageMissingReason::SuccessWithoutUsage
            } else {
                RuntimeUsageMissingReason::RequestOutcomeUnknown
            },
            wire,
        )
    }

    pub fn reported(&self, route: &EffectiveRouteEnvelope, usage: &Usage) -> RuntimeUsageRecord {
        self.prepare(
            route,
            usage,
            if usage == &Usage::default() {
                crate::llm_client::ProviderUsageCoverage::Missing
            } else {
                crate::llm_client::ProviderUsageCoverage::Reported
            },
            RuntimeUsageMissingReason::SuccessWithoutUsage,
        )
    }
}

impl RuntimeUsageRecord {
    pub fn legacy(source_id: impl Into<String>, usage: EffectiveRouteUsage) -> Self {
        Self {
            source_id: source_id.into(),
            usage,
            settlement: None,
        }
    }

    pub fn persisted(&self) -> Self {
        let mut record = self.clone();
        record.source_id = usage_source_fingerprint(&record.source_id);
        record.usage.route = record.usage.route.sanitized_for_persistence();
        record
    }

    pub fn digest(&self) -> std::io::Result<String> {
        use sha2::Digest as _;
        let bytes = serde_json::to_vec(&self.persisted()).map_err(std::io::Error::other)?;
        Ok(codewhale_runtime::hashing::hex_bytes(sha2::Sha256::digest(
            bytes,
        )))
    }

    pub fn validate(&self) -> std::io::Result<()> {
        if self.source_id.trim().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "usage source identity is empty",
            ));
        }
        if let Some(revision) = self.settlement.as_ref() {
            if matches!(
                revision.coverage,
                crate::llm_client::ProviderUsageCoverage::NotDispatched
                    | crate::llm_client::ProviderUsageCoverage::Rejected
            ) && self.usage.usage != Usage::default()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "a refused request cannot claim a token receipt",
                ));
            }
            if revision.physical_requests.len() > 32
                || uuid::Uuid::parse_str(&revision.lineage).is_err()
                || revision.revision == 0
                || (revision.revision == 1) != revision.previous_sha256.is_none()
                || revision.previous_sha256.as_ref().is_some_and(|hash| {
                    hash.len() != 64
                        || !hash
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "invalid usage source revision",
                ));
            }
        }
        Ok(())
    }

    pub fn route_conflicts_with(&self, previous: Option<&Self>) -> bool {
        previous.is_some_and(|old| {
            usage_source_fingerprint(&old.source_id) == usage_source_fingerprint(&self.source_id)
                && old.usage.route != self.usage.route
        })
    }

    pub fn follows_inherited(&self, inherited: &Self) -> bool {
        usage_source_fingerprint(&self.source_id) == usage_source_fingerprint(&inherited.source_id)
            && !self.route_conflicts_with(Some(inherited))
            && self.settlement.as_ref().map(|r| r.owner)
                == inherited.settlement.as_ref().map(|r| r.owner)
            && self
                .settlement
                .as_ref()
                .map_or(0, UsageSourceRevision::revision)
                >= inherited
                    .settlement
                    .as_ref()
                    .map_or(0, UsageSourceRevision::revision)
    }

    pub fn supersedes(&self, previous: Option<&Self>) -> bool {
        if self.source_id.trim().is_empty() {
            return false;
        }
        let Some(old) = previous else {
            return true;
        };
        if usage_source_fingerprint(&old.source_id) != usage_source_fingerprint(&self.source_id)
            || old == self
            || old.usage.route != self.usage.route
        {
            return false;
        }
        let old_final = old.settlement.as_ref().is_some_and(|r| {
            matches!(
                r.coverage,
                crate::llm_client::ProviderUsageCoverage::Reported
                    | crate::llm_client::ProviderUsageCoverage::NotDispatched
                    | crate::llm_client::ProviderUsageCoverage::Rejected
            )
        }) || (old.settlement.is_none() && old.usage.usage != Usage::default());
        if old_final {
            return false;
        }
        if self.usage.usage != Usage::default() {
            return true;
        }
        let old_rev = old.settlement.as_ref().map_or(0, |r| r.revision);
        let new_rev = self.settlement.as_ref().map_or(0, |r| r.revision);
        new_rev > old_rev
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UsageSourceContribution {
    pub record: RuntimeUsageRecord,
    pub usd: f64,
    pub cny: f64,
    pub priced: u32,
    pub unpriced: u32,
    pub cny_priced: u32,
    pub cny_unpriced: u32,
}

impl UsageSourceContribution {
    pub fn for_record(record: &RuntimeUsageRecord) -> Self {
        let audit = record.usage.route.audit_frozen(&record.usage.usage);
        let non_billable = record.settlement.as_ref().is_some_and(|revision| {
            matches!(
                revision.coverage,
                crate::llm_client::ProviderUsageCoverage::NotDispatched
                    | crate::llm_client::ProviderUsageCoverage::Rejected
            )
        });
        let reported = record.settlement.as_ref().map_or_else(
            || record.usage.usage != Usage::default(),
            |revision| {
                matches!(
                    revision.coverage,
                    crate::llm_client::ProviderUsageCoverage::Reported
                        | crate::llm_client::ProviderUsageCoverage::Incomplete
                )
            },
        );
        let metered = audit
            .unpriced_reason
            .is_none_or(|reason| reason.counts_toward_money_coverage());
        let incomplete = !non_billable
            && record.settlement.as_ref().is_some_and(|revision| {
                matches!(
                    revision.coverage,
                    crate::llm_client::ProviderUsageCoverage::Missing
                        | crate::llm_client::ProviderUsageCoverage::Incomplete
                )
            });
        let estimate = audit
            .estimate
            .filter(|_| reported && !non_billable)
            .unwrap_or_default()
            .sanitized();
        Self {
            record: record.persisted(),
            usd: estimate.usd,
            cny: estimate.cny,
            priced: u32::from(reported && !non_billable && audit.usd_priced),
            unpriced: u32::from(
                !non_billable && metered && (!reported || !audit.usd_priced || incomplete),
            ),
            cny_priced: u32::from(reported && !non_billable && audit.cny_priced),
            cny_unpriced: u32::from(
                !non_billable && metered && (!reported || !audit.cny_priced || incomplete),
            ),
        }
    }

    pub fn validate(&self) -> std::io::Result<()> {
        self.record.validate()?;
        if !self.usd.is_finite()
            || !self.cny.is_finite()
            || self.usd < 0.0
            || self.cny < 0.0
            || *self != Self::for_record(&self.record)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "persisted usage contribution does not match its frozen receipt",
            ));
        }
        Ok(())
    }

    pub fn owner(&self) -> UsageSpendOwner {
        self.record
            .settlement
            .as_ref()
            .map_or(UsageSpendOwner::Auxiliary, |revision| revision.owner)
    }

    pub fn total_tokens(&self) -> u64 {
        u64::from(self.record.usage.usage.input_tokens)
            .saturating_add(u64::from(self.record.usage.usage.output_tokens))
    }
}

/// One provider-success response that omitted usage metadata.
///
/// The frozen route is required to distinguish money-metered calls from
/// subscription/local calls without consulting mutable completion-time config.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeUsageDropRecord {
    #[serde(default, skip_serializing_if = "RuntimeUsageMissingReason::is_success")]
    pub reason: RuntimeUsageMissingReason,
    pub source_id: String,
    pub route: EffectiveRouteEnvelope,
}

/// Why this dispatched request has no usable usage receipt. Legacy bytes
/// mean a successful response omitted usage, never an inferred failure charge.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeUsageMissingReason {
    #[default]
    SuccessWithoutUsage,
    RequestOutcomeUnknown,
}
impl RuntimeUsageMissingReason {
    pub fn is_success(&self) -> bool {
        *self == Self::SuccessWithoutUsage
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::SuccessWithoutUsage => "provider_success_missing_usage",
            Self::RequestOutcomeUnknown => "request_outcome_unknown",
        }
    }
}
impl RuntimeUsageDropRecord {
    pub fn as_usage_record(&self) -> RuntimeUsageRecord {
        RuntimeUsageRecord {
            source_id: self.source_id.clone(),
            usage: EffectiveRouteUsage {
                route: self.route.sanitized_for_persistence(),
                usage: Usage::default(),
            },
            settlement: Some(UsageSourceRevision {
                lineage: uuid::Uuid::new_v4().to_string(),
                revision: 1,
                previous_sha256: None,
                owner: UsageSpendOwner::Auxiliary,
                coverage: crate::llm_client::ProviderUsageCoverage::Missing,
                reason: self.reason,
                physical_requests: Vec::new(),
                physical_requests_dropped: 0,
                reservation: false,
            }),
        }
    }
}
/// Provider decision evidence attached to the existing origin-turn ledger.
/// It is diagnostic evidence, never an instruction to change the model route.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RuntimeDecisionReceipt {
    pub source_id: String,
    pub route: EffectiveRouteEnvelope,
    pub usage: Option<Usage>,
    #[serde(default)]
    pub usage_complete: bool,
    pub shadow: bool,
    pub valid_answers: bool,
    pub evidence: codewhale_models::auto_route_evidence::AutoRouteDecisionEvidence,
}

impl RuntimeDecisionReceipt {
    pub fn is_bounded(&self) -> bool {
        !self.source_id.trim().is_empty()
            && self.source_id.len() <= 128
            && self.evidence.choice.len() <= 128
            && self.evidence.probabilities_bp.len() <= 64
            && self
                .evidence
                .probabilities_bp
                .iter()
                .all(|(key, value)| key.len() <= 128 && *value <= 10_000)
            && self.evidence.confidence_bp <= 10_000
            && self.evidence.min_confidence_bp <= 10_000
            && self
                .evidence
                .thinking
                .as_ref()
                .is_none_or(|v| v.len() <= 128)
            && self
                .evidence
                .response_model
                .as_ref()
                .is_none_or(|v| v.len() <= 128)
            && self
                .evidence
                .provider_reported_cost_usd
                .as_ref()
                .is_none_or(|v| {
                    v.len() <= 128
                        && v.parse::<f64>()
                            .is_ok_and(|cost| cost.is_finite() && cost >= 0.0)
                })
            && serde_json::to_vec(self).is_ok_and(|bytes| bytes.len() <= 8 * 1024)
    }

    pub fn sanitized(&self) -> Self {
        let mut receipt = self.clone();
        receipt.source_id = usage_source_fingerprint(&receipt.source_id);
        receipt.route = receipt.route.sanitized_for_persistence();
        receipt.evidence.choice = sanitize_persisted_route_label(&receipt.evidence.choice);
        receipt.evidence.thinking = receipt
            .evidence
            .thinking
            .as_deref()
            .map(sanitize_persisted_route_label);
        receipt.evidence.response_model = receipt
            .evidence
            .response_model
            .as_deref()
            .map(sanitize_persisted_route_label);
        receipt.evidence.probabilities_bp = receipt
            .evidence
            .probabilities_bp
            .into_iter()
            .map(|(key, value)| (sanitize_persisted_route_label(&key), value))
            .collect();
        receipt
    }

    pub fn diagnostic_receipt(&self) -> String {
        // Reuse cost_status's persisted diagnostic receipt surface. No prompt,
        // response id, URL, auth header or unoffered choice enters this string.
        format!(
            "decision:{}",
            serde_json::to_string(&self.sanitized()).unwrap_or_default()
        )
    }
}

pub type RuntimeUsageDropSink = Arc<dyn Fn(RuntimeUsageDropRecord) -> bool + Send + Sync>;

#[cfg(any(test, feature = "test-support"))]
pub fn decision_receipt_fixture(source_id: &str) -> RuntimeDecisionReceipt {
    let mut route = EffectiveRouteEnvelope::capture(
        None,
        codewhale_config::ProviderKind::Custom,
        "typesafe",
        "jev-latest",
        Some("https://api.typesafe.ai/v1"),
        Utc::now(),
    );
    route.billing_mode = RouteBillingMode::Unknown;
    RuntimeDecisionReceipt {
        source_id: source_id.to_string(),
        route,
        usage: Some(Usage {
            input_tokens: 9,
            output_tokens: 4,
            ..Default::default()
        }),
        usage_complete: true,
        shadow: true,
        valid_answers: false,
        evidence: codewhale_models::auto_route_evidence::AutoRouteDecisionEvidence {
            choice: "invalid".to_string(),
            probabilities_bp: Default::default(),
            confidence_bp: 0,
            min_confidence_bp: 5_000,
            abstained_questions: Vec::new(),
            cost_saving_kept_fast: false,
            thinking: None,
            provider_reported_cost_usd: Some("0.000012054".to_string()),
            latency_ms: 12,
            response_model: Some("jev-latest".to_string()),
        },
    }
}

const MAX_RUNTIME_USAGE_RECORDS_PER_OWNER: usize = 256;

pub fn report_interactive_decision(scope: CostScopeToken, receipt: &RuntimeDecisionReceipt) {
    if !receipt.is_bounded() {
        return;
    }
    with_pending_state_mut(|state| {
        if state.generation == scope.0 {
            if state
                .pending
                .route_receipts
                .iter()
                .filter(|v| v.starts_with("decision:"))
                .count()
                < MAX_RUNTIME_USAGE_RECORDS_PER_OWNER
            {
                state
                    .pending
                    .route_receipts
                    .insert(receipt.diagnostic_receipt());
            } else {
                state
                    .pending
                    .route_receipts
                    .insert("decision:diagnostic_receipt_bound_reached".to_string());
            }
        }
    });
}

#[must_use]
pub fn usage_source_fingerprint(source_id: &str) -> String {
    let source_id = source_id.trim();
    let fingerprint = source_id.strip_prefix("routed:").unwrap_or(source_id);
    if fingerprint.len() == 64 && fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return fingerprint.to_ascii_lowercase();
    }
    codewhale_config::catalog::base_url_fingerprint(source_id)
}

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static TEST_COST_ORIGIN: std::cell::Cell<Option<std::thread::ThreadId>> = const { std::cell::Cell::new(None) };
}

#[cfg(any(test, feature = "test-support"))]
pub fn test_cost_scope_id() -> std::thread::ThreadId {
    TEST_COST_ORIGIN
        .with(|scope| scope.get())
        .unwrap_or_else(|| std::thread::current().id())
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestCostScopeBinding(Option<std::thread::ThreadId>);

#[cfg(any(test, feature = "test-support"))]
pub fn bind_test_cost_scope(origin: std::thread::ThreadId) -> TestCostScopeBinding {
    TestCostScopeBinding(TEST_COST_ORIGIN.with(|scope| scope.replace(Some(origin))))
}

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestCostScopeBinding {
    fn drop(&mut self) {
        TEST_COST_ORIGIN.with(|scope| scope.set(self.0));
    }
}

/// Capture the current session/run generation before starting a background
/// provider request. The same token must be supplied when its usage returns.
#[must_use]
pub fn scope_token() -> CostScopeToken {
    with_pending_state_mut(|state| CostScopeToken(state.generation))
}

pub fn close_current_scope() -> PendingBackgroundCost {
    with_pending_state_mut(|state| {
        let pending = std::mem::take(&mut state.pending);
        state.generation = state.generation.wrapping_add(1);
        state.seen_usage_source_fingerprints.clear();
        state.retired_usage_sources.clear();
        state.source_records.clear();
        pending
    })
}

pub fn restore_usage_source_fingerprints(fingerprints: impl IntoIterator<Item = String>) {
    with_pending_state_mut(|state| {
        state.seen_usage_source_fingerprints = fingerprints.into_iter().collect();
        state.retired_usage_sources.clear();
    })
}

pub fn restore_accounted_usage_sources(
    sources: &BTreeMap<String, UsageSourceContribution>,
    fingerprints: &BTreeSet<String>,
) {
    with_pending_state_mut(|state| {
        state.source_records = sources
            .iter()
            .map(|(key, value)| (key.clone(), value.record.persisted()))
            .collect();
        state.seen_usage_source_fingerprints = fingerprints.iter().cloned().collect();
        state.retired_usage_sources.clear();
    });
}

pub fn mark_retired_usage_source(scope: CostScopeToken, source_id: &str) {
    with_pending_state_mut(|state| {
        if state.generation == scope.0 {
            let fingerprint = usage_source_fingerprint(source_id);
            state
                .seen_usage_source_fingerprints
                .insert(fingerprint.clone());
            state.retired_usage_sources.insert(fingerprint);
        }
    });
}

#[must_use]
pub fn is_usage_source_retired(scope: CostScopeToken, source_id: &str) -> bool {
    let fingerprint = usage_source_fingerprint(source_id);
    with_pending_state_mut(|state| {
        state.generation == scope.0 && state.retired_usage_sources.contains(&fingerprint)
    })
}

#[must_use]
pub fn usage_source_seen(source_id: &str) -> bool {
    let fingerprint = usage_source_fingerprint(source_id);
    with_pending_state_mut(|state| {
        state.seen_usage_source_fingerprints.contains(&fingerprint)
            || state.source_records.contains_key(&fingerprint)
    })
}

/// The non-secret identity of a background LLM call's route.
///
/// Background helpers run off a bare client with no app `Config`, so they cannot
/// resolve credential-derived billing. They *can* report what they actually know
/// — which provider, which configured route, which wire model, which endpoint —
/// and this type carries exactly that, so the pricing decision is made from
/// evidence instead of from a provider name.
#[derive(Debug, Clone, Copy)]
#[cfg(any(test, feature = "test-support"))]
pub struct BackgroundRoute<'a> {
    /// Provider kind serving the call.
    pub provider: ProviderKind,
    /// Configured route identity (the `[providers.<name>]` key), when the
    /// caller has one. This is a user-chosen label, not a credential.
    pub provider_identity: Option<&'a str>,
    /// Wire model id as sent on the request.
    pub wire_model: &'a str,
    /// Concrete base URL the request went to, when the client exposes one.
    ///
    /// Only ever used to derive a billing-surface classification and a
    /// SHA-256 fingerprint; the URL itself never leaves this struct.
    pub base_url: Option<&'a str>,
}

#[cfg(any(test, feature = "test-support"))]
impl<'a> BackgroundRoute<'a> {
    /// A route with no endpoint information.
    #[must_use]
    pub fn new(provider: ProviderKind, wire_model: &'a str) -> Self {
        Self {
            provider,
            provider_identity: None,
            wire_model,
            base_url: None,
        }
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: Option<&'a str>) -> Self {
        self.base_url = base_url;
        self
    }

    /// Non-secret billing-surface classification for this endpoint.
    #[must_use]
    pub fn billing_surface(&self) -> Option<&'static str> {
        crate::pricing::billing_surface_for_route(self.provider, self.base_url)
    }

    /// SHA-256 fingerprint of the normalized base URL, or `None` when unknown.
    ///
    /// This is the same digest the catalog scopes live rows on, so a live
    /// pricing row can be proven to price *this* endpoint.
    #[must_use]
    pub fn endpoint_fingerprint(&self) -> Option<String> {
        self.base_url.and_then(endpoint_fingerprint)
    }

    /// Billing presentation derivable without app config.
    #[must_use]
    pub fn billing(&self) -> BillingPresentation {
        crate::route_billing::for_endpoint_without_config(self.provider, self.base_url)
    }

    /// A redacted, stable receipt describing this route.
    #[must_use]
    pub fn receipt(&self, currency: &str) -> String {
        route_receipt(
            self.provider,
            self.provider_identity,
            self.wire_model,
            self.billing_surface(),
            self.endpoint_fingerprint().as_deref(),
            self.billing().into(),
            currency,
        )
    }
}

/// Format one redacted route receipt.
///
/// Contains only: provider kind, configured route label, wire model,
/// billing-surface classification, endpoint fingerprint, billing mode, and the currency the
/// estimate is denominated in. It deliberately contains no URL, no credential,
/// and no filesystem path, so it is safe to persist into a saved session and to
/// log. This is the single formatter, so the foreground turn path and the
/// background pool cannot describe the same route two different ways.
#[must_use]
pub fn route_receipt(
    provider: ProviderKind,
    provider_identity: Option<&str>,
    wire_model: &str,
    billing_surface: Option<&str>,
    endpoint_fingerprint: Option<&str>,
    billing_mode: RouteBillingMode,
    currency: &str,
) -> String {
    format!(
        "provider={} identity={} model={} surface={} endpoint_fp={} billing_mode={} currency={currency}",
        provider.as_str(),
        safe_receipt_field(provider_identity.unwrap_or("-")),
        safe_receipt_field(wire_model),
        safe_receipt_field(billing_surface.unwrap_or("unreported")),
        safe_receipt_field(endpoint_fingerprint.unwrap_or("unreported")),
        match billing_mode {
            RouteBillingMode::Metered => "metered",
            RouteBillingMode::Subscription => "subscription",
            RouteBillingMode::Local => "local",
            RouteBillingMode::Unknown => "unknown",
        },
    )
}

/// Validate and canonicalize an endpoint before producing the cryptographic
/// fingerprint persisted in a receipt. Secret-bearing/malformed URLs receive
/// no fingerprint at all; userinfo, query strings, and fragments are never fed
/// to the hash function.
#[must_use]
pub fn endpoint_fingerprint(base_url: &str) -> Option<String> {
    let mut parsed = reqwest::Url::parse(base_url.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.host_str().is_none()
    {
        return None;
    }
    parsed.set_query(None);
    parsed.set_fragment(None);
    let canonical = parsed.as_str().trim_end_matches('/');
    Some(codewhale_config::catalog::base_url_fingerprint(canonical))
}

/// Currency tag for a receipt, derived from authoritative currency coverage —
/// not from a positive amount, because a zero-usage priced turn is still a
/// valid zero in its published currency.
#[must_use]
pub fn currency_tag(audit: &TurnCostAudit) -> &'static str {
    match (audit.usd_priced, audit.cny_priced) {
        (true, true) => "usd+cny",
        (true, false) => "usd",
        (false, true) => "cny",
        (false, false) => "unpriced",
    }
}

/// Background callers report their LLM usage here.
///
/// The route is priced through the same [`crate::pricing::audit_turn_cost_for_route_on_endpoint`]
/// the foreground turn path uses, so a background turn cannot be counted under
/// different rules than a parent turn. Adds no money when the route is exactly
/// non-metered (a local runtime, an OAuth broker, a named plan endpoint), and
/// counts the turn as *missing spend* whenever it is money-metered or of unknown
/// basis but could not be priced — an unknown basis is never waved through as a
/// subscription (#4318).
#[cfg(any(test, feature = "test-support"))]
pub fn report(scope: CostScopeToken, route: &BackgroundRoute<'_>, usage: &Usage) {
    let billing_surface = route.billing_surface();
    let fingerprint = route.endpoint_fingerprint();
    let audit = crate::pricing::audit_turn_cost_for_route_on_endpoint(
        route.provider,
        route.wire_model,
        billing_surface,
        fingerprint.as_deref(),
        usage,
        chrono::Utc::now(),
        route.billing(),
    );
    record(scope, route.receipt(currency_tag(&audit)), &audit, usage);
}

pub(crate) fn unknown_cost_record(
    source_id: impl Into<String>,
    route: &EffectiveRouteEnvelope,
    usage: Usage,
) -> RuntimeUsageRecord {
    let mut route = route.sanitized_for_persistence();
    if !matches!(
        route.billing_mode,
        RouteBillingMode::Subscription | RouteBillingMode::Local
    ) {
        route.billing_mode = RouteBillingMode::Unknown;
    }
    route.provider_live_pricing = None;
    let coverage = if usage == Usage::default() {
        crate::llm_client::ProviderUsageCoverage::Missing
    } else {
        crate::llm_client::ProviderUsageCoverage::Incomplete
    };
    RuntimeUsageRecord {
        source_id: source_id.into(),
        usage: EffectiveRouteUsage { route, usage },
        settlement: Some(UsageSourceRevision {
            lineage: uuid::Uuid::new_v4().to_string(),
            revision: 1,
            previous_sha256: None,
            owner: UsageSpendOwner::Auxiliary,
            coverage,
            reason: RuntimeUsageMissingReason::RequestOutcomeUnknown,
            physical_requests: Vec::new(),
            physical_requests_dropped: 0,
            reservation: false,
        }),
    }
}

pub fn record_runtime_usage(
    scope: CostScopeToken,
    origin: Option<&NativeUsageOrigin>,
    record: &RuntimeUsageRecord,
) {
    if let Some(origin) = origin {
        origin.publish(record);
    } else {
        let _ = record_interactive_runtime_usage(scope, record.clone());
    }
}

pub async fn record_runtime_usage_async(
    scope: CostScopeToken,
    origin: Option<&NativeUsageOrigin>,
    record: &RuntimeUsageRecord,
) {
    let origin = origin.cloned();
    let record = record.clone();
    #[cfg(any(test, feature = "test-support"))]
    let test_origin = test_cost_scope_id();
    let _ = codewhale_runtime::runtime_control::owner_work(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _binding = bind_test_cost_scope(test_origin);
        record_runtime_usage(scope, origin.as_ref(), &record);
        Ok(())
    })
    .await;
}

pub fn report_missing_runtime_usage(
    scope: CostScopeToken,
    origin: Option<&NativeUsageOrigin>,
    source_id: &str,
    route: &EffectiveRouteEnvelope,
    reason: RuntimeUsageMissingReason,
) {
    let drop = RuntimeUsageDropRecord {
        reason,
        source_id: source_id.to_string(),
        route: route.sanitized_for_persistence(),
    };
    if let Some(origin) = origin {
        origin.publish_drop(&drop);
    } else {
        let _ = record_interactive_runtime_usage_drop(scope, drop);
    }
}

pub fn record_runtime_usage_batch(
    scope: CostScopeToken,
    origin: Option<&NativeUsageOrigin>,
    batch: &RuntimeUsageBatch,
) {
    if let Some(origin) = origin {
        for receipt in &batch.decisions {
            origin.publish_decision(receipt);
        }
        for record in &batch.records {
            origin.publish(record);
        }
        for drop in &batch.drop_records {
            origin.publish_drop(drop);
        }
        return;
    }
    for receipt in &batch.decisions {
        report_interactive_decision(scope, receipt);
    }
    for record in &batch.records {
        let _ = record_interactive_runtime_usage(scope, record.clone());
    }
    for drop in &batch.drop_records {
        let _ = record_interactive_runtime_usage_drop(scope, drop.clone());
    }
    let residual = batch
        .dropped_records
        .saturating_sub(u64::try_from(batch.drop_records.len()).unwrap_or(u64::MAX));
    if residual == 0 {
        return;
    }
    let mut identities = batch
        .records
        .iter()
        .map(|record| usage_source_fingerprint(&record.source_id))
        .chain(
            batch
                .drop_records
                .iter()
                .map(|record| usage_source_fingerprint(&record.source_id)),
        )
        .take(MAX_RUNTIME_USAGE_RECORDS_PER_OWNER)
        .collect::<Vec<_>>();
    identities.sort_unstable();
    let residual_source = format!(
        "runtime-usage-batch-residual:{}",
        usage_source_fingerprint(&format!(
            "{}:{}:{}:{}",
            batch.records.len(),
            batch.drop_records.len(),
            batch.dropped_records,
            identities.join(":")
        ))
    );
    let _ = record_interactive_runtime_usage_drop_count(scope, &residual_source, residual);
}

#[must_use]
pub fn background_cost_for_runtime_usage(record: &RuntimeUsageRecord) -> PendingBackgroundCost {
    if record.settlement.is_some() {
        let mut pending = PendingBackgroundCost::default();
        pending
            .usage_source_fingerprints
            .insert(usage_source_fingerprint(&record.source_id));
        pending.source_records.push(record.clone());
        return pending;
    }
    if record.usage.usage == Usage::default() {
        return background_cost_for_runtime_drop(&RuntimeUsageDropRecord {
            reason: RuntimeUsageMissingReason::default(),
            source_id: record.source_id.clone(),
            route: record.usage.route.clone(),
        });
    }
    let mut pending = PendingBackgroundCost::default();
    let fingerprint = usage_source_fingerprint(&record.source_id);
    let audit = record.usage.route.audit_frozen(&record.usage.usage);
    let receipt = record.usage.route.receipt(&audit);
    pending.usage_source_fingerprints.insert(fingerprint);
    fold_audit_into_pending(&mut pending, receipt, &audit, &record.usage.usage);
    pending
}

#[must_use]
pub fn background_cost_for_runtime_drop(record: &RuntimeUsageDropRecord) -> PendingBackgroundCost {
    let mut pending = PendingBackgroundCost::default();
    pending
        .usage_source_fingerprints
        .insert(usage_source_fingerprint(&record.source_id));
    pending.source_records.push(record.as_usage_record());
    pending
}

pub fn prune_settled_missing_reasons(
    accounted: &BTreeMap<String, UsageSourceContribution>,
    unpriced: u32,
    cny_unpriced: u32,
    reasons: &mut BTreeSet<String>,
    cny_reasons: &mut BTreeSet<String>,
) {
    for reason in [
        RuntimeUsageMissingReason::RequestOutcomeUnknown,
        RuntimeUsageMissingReason::SuccessWithoutUsage,
    ] {
        let has_usd_reason = accounted.values().any(|entry| {
            entry.unpriced > 0
                && entry.record.settlement.as_ref().is_some_and(|revision| {
                    matches!(
                        revision.coverage,
                        crate::llm_client::ProviderUsageCoverage::Missing
                            | crate::llm_client::ProviderUsageCoverage::Incomplete
                    ) && revision.reason == reason
                })
        });
        if unpriced == 0 || !has_usd_reason {
            reasons.remove(reason.label());
        }
        let has_cny_reason = accounted.values().any(|entry| {
            entry.cny_unpriced > 0
                && entry.record.settlement.as_ref().is_some_and(|revision| {
                    matches!(
                        revision.coverage,
                        crate::llm_client::ProviderUsageCoverage::Missing
                            | crate::llm_client::ProviderUsageCoverage::Incomplete
                    ) && revision.reason == reason
                })
        });
        if cny_unpriced == 0 || !has_cny_reason {
            cny_reasons.remove(reason.label());
        }
    }
    reasons.remove(crate::pricing::UnpricedReason::RequestInProgress.label());
    cny_reasons.remove(crate::pricing::UnpricedReason::RequestInProgress.label());
}

#[cfg(any(test, feature = "test-support"))]
fn record(scope: CostScopeToken, route_receipt: String, audit: &TurnCostAudit, usage: &Usage) {
    with_pending_state_mut(|state| {
        if state.generation != scope.0 {
            return;
        }
        fold_audit_into_pending(&mut state.pending, route_receipt, audit, usage);
    });
}

pub fn record_interactive_runtime_usage(scope: CostScopeToken, record: RuntimeUsageRecord) -> bool {
    with_pending_state_mut(|state| {
        if state.generation != scope.0 {
            return false;
        }
        let key = usage_source_fingerprint(&record.source_id);
        let record_to_store = if let Some(previous) = state.source_records.get(&key) {
            if record.route_conflicts_with(Some(previous)) {
                tracing::warn!(
                    source_id = %key,
                    "interactive usage source changed route; recording unknown cost"
                );
                unknown_cost_record(
                    &record.source_id,
                    &record.usage.route,
                    record.usage.usage.clone(),
                )
            } else if !record.supersedes(Some(previous)) {
                return true;
            } else {
                record
            }
        } else if state.seen_usage_source_fingerprints.contains(&key) {
            return true;
        } else {
            record
        };
        state.pending.usage_source_fingerprints.insert(key.clone());
        state.seen_usage_source_fingerprints.insert(key.clone());
        let record_to_store = if record_to_store.settlement.is_none()
            && record_to_store.usage.usage == Usage::default()
        {
            RuntimeUsageDropRecord {
                reason: RuntimeUsageMissingReason::SuccessWithoutUsage,
                source_id: record_to_store.source_id,
                route: record_to_store.usage.route,
            }
            .as_usage_record()
        } else {
            record_to_store
        };
        if let Some(revision) = record_to_store.settlement.as_ref() {
            state
                .pending
                .source_records
                .retain(|old| usage_source_fingerprint(&old.source_id) != key);
            if matches!(
                revision.coverage,
                crate::llm_client::ProviderUsageCoverage::Missing
                    | crate::llm_client::ProviderUsageCoverage::Incomplete
            ) && record_to_store
                .usage
                .route
                .audit_frozen(&record_to_store.usage.usage)
                .counts_toward_money_coverage()
            {
                state
                    .pending
                    .unpriced_reasons
                    .insert(revision.reason.label());
                state
                    .pending
                    .cny_unpriced_reasons
                    .insert(revision.reason.label());
            }
            state
                .source_records
                .insert(key, record_to_store.persisted());
            state.pending.source_records.push(record_to_store);
        } else {
            state
                .source_records
                .insert(key, record_to_store.persisted());
            let audit = record_to_store
                .usage
                .route
                .audit_frozen(&record_to_store.usage.usage);
            let receipt = record_to_store.usage.route.receipt(&audit);
            fold_audit_into_pending(
                &mut state.pending,
                receipt,
                &audit,
                &record_to_store.usage.usage,
            );
        }
        true
    })
}

pub fn record_interactive_runtime_usage_drop(
    scope: CostScopeToken,
    record: RuntimeUsageDropRecord,
) -> bool {
    record_interactive_runtime_usage(scope, record.as_usage_record())
}

fn record_interactive_runtime_usage_drop_count(
    scope: CostScopeToken,
    source_id: &str,
    count: u64,
) -> bool {
    with_pending_state_mut(|state| {
        if state.generation != scope.0 {
            return false;
        }
        let fingerprint = usage_source_fingerprint(source_id);
        if !state
            .seen_usage_source_fingerprints
            .insert(fingerprint.clone())
        {
            return true;
        }
        state.pending.usage_source_fingerprints.insert(fingerprint);
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        state.pending.unpriced_turns = state.pending.unpriced_turns.saturating_add(count);
        state.pending.cny_unpriced_turns = state.pending.cny_unpriced_turns.saturating_add(count);
        state
            .pending
            .unpriced_reasons
            .insert("routed_usage_receipt_missing");
        state
            .pending
            .cny_unpriced_reasons
            .insert("routed_usage_receipt_missing");
        true
    })
}

fn fold_audit_into_pending(
    pending: &mut PendingBackgroundCost,
    route_receipt: String,
    audit: &TurnCostAudit,
    usage: &Usage,
) {
    if let Some(provenance) = audit.provenance.as_ref() {
        pending.pricing_provenances.insert(provenance.label());
    }
    if let Some(defect) = audit.live_pricing_defect.as_ref() {
        if audit.estimate.is_some() {
            pending.live_pricing_defects.insert(defect.label());
        } else {
            pending.live_pricing_unusable_defects.insert(defect.label());
        }
    }
    if let Some(cost) = audit.estimate {
        pending.estimate = pending.estimate.saturating_add(cost);
    }
    if usage.prompt_cache_hit_tokens.is_some()
        || usage.prompt_cache_miss_tokens.is_some()
        || usage.prompt_cache_write_tokens.is_some()
    {
        let classes = crate::pricing::token_usage_for_pricing(usage);
        let add = |slot: &mut Option<u64>, tokens: u64| {
            *slot = Some(slot.unwrap_or(0).saturating_add(tokens));
        };
        add(&mut pending.cache_hit_tokens, classes.cache_read);
        add(&mut pending.cache_miss_tokens, classes.input);
        add(&mut pending.cache_write_tokens, classes.cache_write);
    }

    if audit.counts_toward_money_coverage() {
        if audit.usd_priced {
            pending.priced_turns = pending.priced_turns.saturating_add(1);
        } else {
            pending.unpriced_turns = pending.unpriced_turns.saturating_add(1);
        }
        if audit.cny_priced {
            pending.cny_priced_turns = pending.cny_priced_turns.saturating_add(1);
        } else {
            pending.cny_unpriced_turns = pending.cny_unpriced_turns.saturating_add(1);
        }
        for class in &audit.unpriced_classes {
            pending.unpriced_classes.insert(class.label());
        }
        if !audit.usd_priced
            && let Some(reason) = audit.unpriced_reason
        {
            pending.unpriced_reasons.insert(reason.label());
        }
        if !audit.cny_priced {
            pending.cny_unpriced_reasons.insert(
                audit
                    .unpriced_reason
                    .map_or("currency_not_published", |reason| reason.label()),
            );
        }
    }

    pending
        .route_receipts
        .insert(receipt_with_usage_classes(route_receipt, usage));
}

#[must_use]
pub fn drain() -> PendingBackgroundCost {
    with_pending_state_mut(|state| std::mem::take(&mut state.pending))
}

#[cfg(any(test, feature = "test-support"))]
pub fn reset_for_tests() {
    with_pending_state_mut(|state| {
        state.pending = PendingBackgroundCost::default();
        state.seen_usage_source_fingerprints.clear();
        state.retired_usage_sources.clear();
        state.source_records.clear();
    });
}

#[cfg(any(test, feature = "test-support"))]
pub struct TestCostScope;

#[cfg(any(test, feature = "test-support"))]
impl Drop for TestCostScope {
    fn drop(&mut self) {
        reset_for_tests();
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn test_scope() -> TestCostScope {
    reset_for_tests();
    TestCostScope
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage_persistence::*;

    fn configured_fixture_receipt() -> (
        codewhale_runtime::config::Config,
        EffectiveRouteEnvelope,
        Usage,
    ) {
        let config = toml::from_str(include_str!(
            "../../config/tests/fixtures/custom_models.toml"
        ))
        .unwrap();
        let receipt = EffectiveRouteEnvelope::capture(
            Some(&config),
            ProviderKind::Deepseek,
            "deepseek",
            "deepseek-v4.1-flash-expires-on-0910",
            Some("https://models.example.test/v1"),
            Utc::now(),
        );
        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Usage::default()
        };
        (config, receipt, usage)
    }

    #[test]
    fn configured_model_estimate_is_frozen_and_exactly_bound() {
        let (mut config, receipt, usage) = configured_fixture_receipt();
        let audit = receipt.audit(&usage);
        assert_eq!(
            audit.provenance,
            Some(codewhale_config::pricing::PricingProvenance::UserOverride)
        );
        assert!((audit.estimate.unwrap().usd - 2.0).abs() < 1e-12);
        let frozen: EffectiveRouteEnvelope =
            serde_json::from_str(&serde_json::to_string(&receipt).unwrap()).unwrap();
        config.custom_models.as_mut().unwrap()[0]
            .cost
            .as_mut()
            .unwrap()
            .input = Some(9.0);
        assert!((frozen.audit(&usage).estimate.unwrap().usd - 2.0).abs() < 1e-12);
        for (field, value) in [
            ("model", "deepseek-v4.1-flash"),
            ("identity", "other-provider"),
            ("endpoint", "https://other.example.test/v1"),
        ] {
            let mut wrong = frozen.clone();
            match field {
                "model" => wrong.model = value.into(),
                "identity" => wrong.provider_identity = value.into(),
                _ => wrong.endpoint_fingerprint = endpoint_fingerprint(value),
            }
            assert!(wrong.audit(&usage).estimate.is_none(), "{field}");
        }
        for billing in [RouteBillingMode::Local, RouteBillingMode::Subscription] {
            let mut nonmoney = frozen.clone();
            nonmoney.billing_mode = billing;
            assert_eq!(
                nonmoney.audit(&usage).unpriced_reason,
                Some(crate::pricing::UnpricedReason::NotMoneyMetered)
            );
        }
        let mut cached = usage.clone();
        cached.prompt_cache_write_tokens = Some(500);
        assert!(frozen.audit(&cached).estimate.is_none());
    }

    #[test]
    fn configured_model_missing_prices_stay_unknown_and_vendor_pin_still_wins() {
        let (mut config, receipt, usage) = configured_fixture_receipt();
        config.custom_models.as_mut().unwrap()[0].cost = None;
        let unknown = EffectiveRouteEnvelope::capture(
            Some(&config),
            receipt.provider,
            receipt.provider_identity.clone(),
            receipt.model.clone(),
            Some("https://models.example.test/v1"),
            receipt.dispatched_at,
        );
        assert!(unknown.provider_live_pricing.is_some());
        assert!(unknown.audit(&usage).estimate.is_none());
        let mut pinned = receipt;
        pinned.provider = ProviderKind::Openrouter;
        pinned.openrouter_vendor = Some("exact-upstream".into());
        pinned.billing_mode = RouteBillingMode::Metered;
        assert_eq!(
            pinned.audit(&usage).unpriced_reason,
            Some(crate::pricing::UnpricedReason::RoutingDependentPrice)
        );
    }

    #[test]
    fn configured_model_client_keeps_its_metadata_snapshot_after_reload() {
        let (mut config, _, usage) = configured_fixture_receipt();
        config.set_legacy_root(Some("fixture-not-a-provider-credential".into()), None);
        let id = "deepseek-v4.1-flash-expires-on-0910";
        let route =
            crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Deepseek, Some(id))
                .unwrap();
        let client =
            crate::client::CodewhaleClient::from_candidate(&config, &route.candidate).unwrap();
        config.custom_models.as_mut().unwrap()[0]
            .cost
            .as_mut()
            .unwrap()
            .input = Some(9.0);
        let envelope = client.effective_route_envelope(id, Utc::now());
        assert!((envelope.audit(&usage).estimate.unwrap().usd - 2.0).abs() < 1e-12);
        assert_eq!(
            client
                .effective_route_envelope("other-model", Utc::now())
                .provider_live_pricing,
            None
        );
    }

    const DECLARED_OPENROUTER_MODEL: &str = "synthetic/declared-model";

    /// An OpenRouter config on the official endpoint with one
    /// `[[custom_models]]` row; `extra` adds fields such as `cost`.
    fn openrouter_declared_config(
        extra: &str,
        vendor: Option<&str>,
    ) -> codewhale_runtime::config::Config {
        let vendor = vendor.map_or_else(String::new, |vendor| format!("vendor = \"{vendor}\"\n"));
        toml::from_str(&format!(
            "provider = \"openrouter\"\ntelemetry = false\n\n\
             [[custom_models]]\nprovider = \"openrouter\"\n\
             base_url = \"{base}\"\nid = \"{DECLARED_OPENROUTER_MODEL}\"\n{extra}\n\n\
             [providers.openrouter]\nbase_url = \"{base}\"\n\
             api_key = \"fixture-not-a-provider-credential\"\n{vendor}",
            base = codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL,
        ))
        .expect("declared OpenRouter config")
    }

    fn declared_openrouter_client(
        config: &codewhale_runtime::config::Config,
    ) -> crate::client::CodewhaleClient {
        let route = crate::route_runtime::resolve_runtime_route(
            config,
            ProviderKind::Openrouter,
            Some(DECLARED_OPENROUTER_MODEL),
        )
        .expect("declared OpenRouter route");
        crate::client::CodewhaleClient::from_candidate(config, &route.candidate)
            .expect("declared OpenRouter client")
    }

    /// #6690: a `[[custom_models]]` row declared only to add a model (no
    /// rates) used to freeze a rate-less quote on the main turn and hide the
    /// endpoint's own catalog price. A declared rate still wins, and with no
    /// catalog row the rate-less declaration stays frozen so no same-named
    /// bundled price can fill it.
    #[test]
    fn main_turn_rateless_declaration_yields_to_the_endpoint_catalog_price() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("isolated catalog home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();

        let fingerprint = endpoint_fingerprint(
            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL,
        )
        .expect("official endpoint");
        let now = u64::try_from(Utc::now().timestamp()).expect("timestamp");
        let rateless = declared_openrouter_client(&openrouter_declared_config("", None));
        let declared = declared_openrouter_client(&openrouter_declared_config(
            "cost = { input = 0.4, output = 1.6 }",
            None,
        ));
        let quote_for = |client: &crate::client::CodewhaleClient| {
            crate::client::main_turn_pricing_quote_at(
                Some(client),
                ProviderKind::Openrouter,
                "openrouter",
                DECLARED_OPENROUTER_MODEL,
                &fingerprint,
                now,
            )
            .expect("a frozen main-turn quote")
        };

        // No catalog row yet: the rate-less declaration is frozen as-is.
        let frozen = quote_for(&rateless);
        assert_eq!(
            frozen.provenance,
            codewhale_config::pricing::PricingProvenance::UserOverride
        );
        assert!(!frozen.carries_rates());

        crate::provider_catalog_live::record_success(priced_provider_delta(
            "openrouter",
            DECLARED_OPENROUTER_MODEL,
            &fingerprint,
            now,
        ));
        let catalog = quote_for(&rateless);
        assert_eq!(
            catalog.provenance,
            codewhale_config::pricing::PricingProvenance::ProviderLive
        );
        assert_eq!(catalog.input_per_million.as_deref(), Some("1.25"));
        assert_eq!(catalog.output_per_million.as_deref(), Some("5"));
        assert_eq!(catalog.cache_read_per_million.as_deref(), Some("0.25"));

        let explicit = quote_for(&declared);
        assert_eq!(
            explicit.provenance,
            codewhale_config::pricing::PricingProvenance::UserOverride
        );
        assert_eq!(explicit.input_per_million.as_deref(), Some("0.4"));
        assert_eq!(explicit.output_per_million.as_deref(), Some("1.6"));
    }

    /// #6690 review: a pinned OpenRouter vendor blocks the aggregate catalog
    /// price, but an operator's own declared rate for the exact route is not
    /// that aggregate and must still price the turn.
    #[test]
    fn openrouter_vendor_pin_is_priced_by_an_explicit_declared_rate() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("isolated catalog home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();

        let usage = Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            ..Usage::default()
        };
        let capture = |extra: &str| {
            EffectiveRouteEnvelope::capture(
                Some(&openrouter_declared_config(extra, Some("cerebras"))),
                ProviderKind::Openrouter,
                "openrouter",
                DECLARED_OPENROUTER_MODEL,
                Some(codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL),
                Utc::now(),
            )
        };

        let declared = capture("cost = { input = 0.4, output = 1.6 }");
        assert_eq!(declared.openrouter_vendor.as_deref(), Some("cerebras"));
        let audit = declared.audit(&usage);
        assert_eq!(audit.unpriced_reason, None, "{audit:?}");
        assert_eq!(
            audit.provenance,
            Some(codewhale_config::pricing::PricingProvenance::UserOverride)
        );
        let usd = audit.estimate.expect("declared estimate").usd;
        assert!((usd - 2.0).abs() < 1e-12, "{usd}");

        // A rate-less declaration is no explicit rate: the pin still wins.
        let rateless = capture("");
        assert_eq!(
            rateless.audit(&usage).unpriced_reason,
            Some(crate::pricing::UnpricedReason::RoutingDependentPrice)
        );
    }

    struct ProviderCatalogTestReset;

    impl Drop for ProviderCatalogTestReset {
        fn drop(&mut self) {
            crate::provider_catalog_live::reset_cache_for_test();
            crate::provider_lake::clear_live_snapshot();
        }
    }

    fn priced_provider_delta(
        provider: &str,
        model: &str,
        fingerprint: &str,
        fetched_at: u64,
    ) -> codewhale_config::catalog::ProviderCatalogDelta {
        priced_provider_delta_with_rates(provider, model, fingerprint, fetched_at, 1.25, 5.0)
    }

    fn priced_provider_delta_with_rates(
        provider: &str,
        model: &str,
        fingerprint: &str,
        fetched_at: u64,
        input: f64,
        output: f64,
    ) -> codewhale_config::catalog::ProviderCatalogDelta {
        codewhale_config::catalog::ProviderCatalogDelta {
            provider: provider.to_string(),
            base_url_fingerprint: fingerprint.to_string(),
            fetched_at,
            offerings: vec![codewhale_config::catalog::CatalogOffering {
                provider: provider.to_string(),
                wire_model_id: model.to_string(),
                endpoint_key: "chat".to_string(),
                cost: Some(codewhale_config::models_dev::ModelsDevCost {
                    input: Some(input),
                    output: Some(output),
                    cache_read: Some(0.25),
                    cache_write: None,
                    ..Default::default()
                }),
                ..Default::default()
            }],
        }
    }

    fn custom_usage_envelope(
        identity: &str,
        model: &str,
        fingerprint: &str,
        billing_mode: RouteBillingMode,
        dispatched_at: DateTime<Utc>,
    ) -> EffectiveRouteEnvelope {
        provider_live_usage_envelope(
            ProviderKind::Custom,
            identity,
            model,
            fingerprint,
            Some(crate::pricing::UNCLASSIFIED_BILLING_SURFACE),
            billing_mode,
            dispatched_at,
        )
    }

    fn provider_live_usage_envelope(
        provider: ProviderKind,
        identity: &str,
        model: &str,
        fingerprint: &str,
        billing_surface: Option<&str>,
        billing_mode: RouteBillingMode,
        dispatched_at: DateTime<Utc>,
    ) -> EffectiveRouteEnvelope {
        let provider_live_pricing =
            u64::try_from(dispatched_at.timestamp())
                .ok()
                .and_then(|dispatched_at_unix| {
                    crate::provider_catalog_live::fresh_provider_live_pricing_quote_at(
                        provider,
                        identity,
                        model,
                        fingerprint,
                        dispatched_at_unix,
                    )
                });
        EffectiveRouteEnvelope {
            provider,
            provider_identity: identity.to_string(),
            model: model.to_string(),
            openrouter_vendor: None,
            billing_surface: billing_surface.map(str::to_string),
            endpoint_fingerprint: Some(fingerprint.to_string()),
            provider_live_pricing,
            billing_mode,
            dispatched_at,
        }
    }

    fn small_usage() -> Usage {
        Usage {
            input_tokens: 1_000,
            output_tokens: 500,
            ..Default::default()
        }
    }

    #[test]
    fn baseten_usage_prices_only_the_reviewed_identity_on_the_official_endpoint() {
        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("test home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let now = Utc::now();
        let fetched_at = u64::try_from(now.timestamp()).expect("nonnegative timestamp");
        let model = "synthetic-baseten-priced-model";
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(
            codewhale_config::catalog::BASETEN_BASE_URL,
        );
        crate::provider_catalog_live::record_success(priced_provider_delta(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &fingerprint,
            fetched_at,
        ));
        let usage = Usage {
            input_tokens: 1_000_000,
            ..Usage::default()
        };

        let exact = custom_usage_envelope(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &fingerprint,
            RouteBillingMode::Unknown,
            now,
        )
        .audit(&usage);
        assert!(exact.is_priced(), "{exact:?}");
        assert_eq!(
            exact.provenance,
            Some(codewhale_config::pricing::PricingProvenance::ProviderLive)
        );
        assert_eq!(exact.estimate.expect("priced").usd, 1.25);

        // A reviewed schema alias remains a distinct custom ownership scope.
        // It becomes billable only after that exact identity refreshed its own
        // catalog; it cannot borrow the canonical `baseten` partition above.
        let alias = "base-ten";
        crate::provider_catalog_live::record_success(priced_provider_delta(
            alias,
            model,
            &fingerprint,
            fetched_at,
        ));
        let alias_audit =
            custom_usage_envelope(alias, model, &fingerprint, RouteBillingMode::Unknown, now)
                .audit(&usage);
        assert!(alias_audit.is_priced(), "{alias_audit:?}");
        assert_eq!(
            alias_audit.provenance,
            Some(codewhale_config::pricing::PricingProvenance::ProviderLive)
        );
        assert_eq!(alias_audit.estimate.expect("priced").usd, 1.25);

        let generic = custom_usage_envelope(
            "custom-lab",
            model,
            &fingerprint,
            RouteBillingMode::Metered,
            now,
        )
        .audit(&usage);
        assert!(!generic.is_priced(), "{generic:?}");
        // The endpoint fingerprint establishes Baseten's billing contract no
        // matter the table name, so the failure is an unverified price for
        // this identity — not an unknown basis (#6289).
        assert_eq!(
            generic.unpriced_reason,
            Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
        );

        let wrong_fingerprint =
            codewhale_config::catalog::base_url_fingerprint("https://proxy.example/v1");
        let wrong_endpoint = custom_usage_envelope(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &wrong_fingerprint,
            RouteBillingMode::Metered,
            now,
        )
        .audit(&usage);
        assert!(!wrong_endpoint.is_priced(), "{wrong_endpoint:?}");
        assert_eq!(
            wrong_endpoint.unpriced_reason,
            Some(crate::pricing::UnpricedReason::UnknownBillingBasis)
        );
    }

    #[test]
    fn baseten_usage_rejects_unknown_stale_and_failed_live_catalogs() {
        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("test home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let now = Utc::now();
        let now_unix = u64::try_from(now.timestamp()).expect("nonnegative timestamp");
        let model = "synthetic-baseten-status-model";
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(
            codewhale_config::catalog::BASETEN_BASE_URL,
        );
        let unknown_route = custom_usage_envelope(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &fingerprint,
            RouteBillingMode::Unknown,
            now,
        );
        assert!(unknown_route.provider_live_pricing.is_none());
        let usage = Usage {
            input_tokens: 1_000_000,
            ..Usage::default()
        };

        // A same-model price owned by another custom partition cannot price a
        // Baseten receipt whose exact catalog was never refreshed.
        crate::provider_catalog_live::record_success(priced_provider_delta(
            "other-custom",
            model,
            &fingerprint,
            now_unix,
        ));
        let unknown = unknown_route.audit(&usage);
        assert!(!unknown.is_priced(), "{unknown:?}");
        assert_eq!(
            unknown.unpriced_reason,
            Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
        );

        let stale_at = now_unix
            .saturating_sub(crate::provider_catalog_live::DEFAULT_PROVIDER_CATALOG_TTL_SECS)
            .saturating_sub(1);
        crate::provider_catalog_live::record_success(priced_provider_delta(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &fingerprint,
            stale_at,
        ));
        let stale_route = custom_usage_envelope(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &fingerprint,
            RouteBillingMode::Unknown,
            now,
        );
        assert!(stale_route.provider_live_pricing.is_none());
        let stale = stale_route.audit(&usage);
        assert!(!stale.is_priced(), "{stale:?}");
        assert_eq!(
            stale.unpriced_reason,
            Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
        );

        crate::provider_catalog_live::record_success(priced_provider_delta(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &fingerprint,
            now_unix,
        ));
        crate::provider_catalog_live::record_failure(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            &fingerprint,
            codewhale_config::catalog::CatalogRefreshError::Network,
        );
        let failed_route = custom_usage_envelope(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &fingerprint,
            RouteBillingMode::Unknown,
            now,
        );
        assert!(failed_route.provider_live_pricing.is_none());
        let failed = failed_route.audit(&usage);
        assert!(!failed.is_priced(), "{failed:?}");
        assert_eq!(
            failed.unpriced_reason,
            Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
        );
    }

    #[test]
    fn reviewed_provider_live_quotes_survive_same_second_refresh_and_key_state_changes() {
        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("test home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let now = Utc::now();
        let fetched_at = u64::try_from(now.timestamp()).expect("nonnegative timestamp");
        let cases = [
            (
                ProviderKind::Openrouter,
                ProviderKind::Openrouter.as_str(),
                "synthetic-openrouter-frozen-price",
                codewhale_config::catalog::base_url_fingerprint(
                    codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL,
                ),
                crate::pricing::AGGREGATOR_BILLING_SURFACE,
                RouteBillingMode::Metered,
            ),
            (
                ProviderKind::Custom,
                codewhale_config::catalog::BASETEN_PROVIDER_ID,
                "synthetic-baseten-frozen-price",
                codewhale_config::catalog::base_url_fingerprint(
                    codewhale_config::catalog::BASETEN_BASE_URL,
                ),
                crate::pricing::UNCLASSIFIED_BILLING_SURFACE,
                RouteBillingMode::Unknown,
            ),
        ];
        let usage = Usage {
            input_tokens: 1_000_000,
            ..Usage::default()
        };

        for (provider, identity, model, fingerprint, surface, mode) in cases {
            crate::provider_catalog_live::record_success(priced_provider_delta_with_rates(
                identity,
                model,
                &fingerprint,
                fetched_at,
                1.25,
                5.0,
            ));
            let first = provider_live_usage_envelope(
                provider,
                identity,
                model,
                &fingerprint,
                Some(surface),
                mode,
                now,
            );
            let first_quote = first
                .provider_live_pricing
                .as_ref()
                .expect("fresh exact scope freezes a quote");

            // A second refresh in the same Unix second must still be a distinct
            // catalog revision and must not retroactively change `first`.
            crate::provider_catalog_live::record_success(priced_provider_delta_with_rates(
                identity,
                model,
                &fingerprint,
                fetched_at,
                9.5,
                19.0,
            ));
            let second = provider_live_usage_envelope(
                provider,
                identity,
                model,
                &fingerprint,
                Some(surface),
                mode,
                now,
            );
            let second_quote = second
                .provider_live_pricing
                .as_ref()
                .expect("replacement fresh scope freezes a quote");
            assert_ne!(
                first_quote.catalog_revision, second_quote.catalog_revision,
                "same-second price changes need distinct revisions"
            );

            crate::provider_catalog_live::record_failure(
                identity,
                &fingerprint,
                codewhale_config::catalog::CatalogRefreshError::Unauthorized,
            );
            if provider == ProviderKind::Custom {
                // Baseten's same URL can represent another account after a key
                // switch. Starting that refresh clears the mutable old scope.
                let _new_key_refresh = crate::provider_catalog_live::begin_refresh_for_identity(
                    provider,
                    identity,
                    codewhale_config::catalog::BASETEN_BASE_URL,
                );
            }

            let first_audit = first.audit(&usage);
            let second_audit = second.audit(&usage);
            assert_eq!(first_audit.estimate.expect("first quote priced").usd, 1.25);
            assert_eq!(second_audit.estimate.expect("second quote priced").usd, 9.5);

            let after_mutation = provider_live_usage_envelope(
                provider,
                identity,
                model,
                &fingerprint,
                Some(surface),
                mode,
                now,
            );
            assert!(
                after_mutation.provider_live_pricing.is_none(),
                "failed or cleared mutable state cannot mint a new quote"
            );
        }
    }

    #[test]
    fn legacy_no_quote_receipts_cannot_be_retro_priced_by_a_later_refresh() {
        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("test home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let now = Utc::now();
        let fetched_at = u64::try_from(now.timestamp()).expect("nonnegative timestamp");
        let routes = [
            provider_live_usage_envelope(
                ProviderKind::Openrouter,
                ProviderKind::Openrouter.as_str(),
                "synthetic-openrouter-legacy",
                &codewhale_config::catalog::base_url_fingerprint(
                    codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL,
                ),
                Some(crate::pricing::AGGREGATOR_BILLING_SURFACE),
                RouteBillingMode::Metered,
                now,
            ),
            custom_usage_envelope(
                codewhale_config::catalog::BASETEN_PROVIDER_ID,
                "synthetic-baseten-legacy",
                &codewhale_config::catalog::base_url_fingerprint(
                    codewhale_config::catalog::BASETEN_BASE_URL,
                ),
                RouteBillingMode::Unknown,
                now,
            ),
        ];
        assert!(
            routes
                .iter()
                .all(|route| route.provider_live_pricing.is_none())
        );

        for route in &routes {
            crate::provider_catalog_live::record_success(priced_provider_delta(
                &route.provider_identity,
                &route.model,
                route.endpoint_fingerprint.as_deref().expect("fingerprint"),
                fetched_at,
            ));
            let audit = route.audit(&Usage {
                input_tokens: 1_000_000,
                ..Usage::default()
            });
            assert_eq!(
                audit.unpriced_reason,
                Some(if route.provider == ProviderKind::Openrouter {
                    crate::pricing::UnpricedReason::NoPricingRow
                } else {
                    crate::pricing::UnpricedReason::UnverifiedLivePricing
                }),
                "a completion-time refresh must not price {route:?}"
            );
        }
    }

    #[test]
    fn openrouter_offline_bundled_price_is_immutable_after_dispatch() {
        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("test home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let dispatched_at = Utc::now();
        let fetched_at = u64::try_from(dispatched_at.timestamp()).expect("timestamp");
        let model = "qwen/qwen3.8-flash";
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(
            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL,
        );
        let route = provider_live_usage_envelope(
            ProviderKind::Openrouter,
            ProviderKind::Openrouter.as_str(),
            model,
            &fingerprint,
            Some(crate::pricing::AGGREGATOR_BILLING_SURFACE),
            RouteBillingMode::Metered,
            dispatched_at,
        );
        assert!(route.provider_live_pricing.is_none());

        let usage = Usage {
            input_tokens: 1_000_000,
            ..Usage::default()
        };
        let offline = route.audit(&usage);
        assert_eq!(
            offline.estimate.expect("bundled OpenRouter price").usd,
            0.15
        );
        assert_eq!(
            offline.provenance,
            Some(codewhale_config::pricing::PricingProvenance::ModelsDevBundled)
        );

        // A later mutable refresh cannot change a turn that had no quote at
        // the application-dispatch boundary.
        crate::provider_catalog_live::record_success(priced_provider_delta_with_rates(
            ProviderKind::Openrouter.as_str(),
            model,
            &fingerprint,
            fetched_at,
            19.0,
            29.0,
        ));
        let after_refresh = route.audit(&usage);
        assert_eq!(after_refresh, offline);

        // Admission without provider usage does not create a charge.
        let no_usage = route.audit(&Usage::default());
        let no_usage_estimate = no_usage.estimate.expect("known zero usage is priced");
        assert_eq!(no_usage_estimate.usd, 0.0);
        assert_eq!(no_usage_estimate.cny, 0.0);
    }

    #[test]
    fn provider_live_quotes_reject_future_prices_and_every_route_binding_mismatch() {
        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("test home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let dispatched_at = Utc::now();
        let dispatch_unix = u64::try_from(dispatched_at.timestamp()).expect("timestamp");
        let future_at = dispatched_at + chrono::Duration::seconds(1);
        let future_unix = dispatch_unix.saturating_add(1);
        let cases = [
            (
                ProviderKind::Openrouter,
                ProviderKind::Openrouter.as_str(),
                "synthetic-openrouter-future",
                codewhale_config::catalog::base_url_fingerprint(
                    codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL,
                ),
                crate::pricing::AGGREGATOR_BILLING_SURFACE,
                RouteBillingMode::Metered,
            ),
            (
                ProviderKind::Custom,
                codewhale_config::catalog::BASETEN_PROVIDER_ID,
                "synthetic-baseten-future",
                codewhale_config::catalog::base_url_fingerprint(
                    codewhale_config::catalog::BASETEN_BASE_URL,
                ),
                crate::pricing::UNCLASSIFIED_BILLING_SURFACE,
                RouteBillingMode::Unknown,
            ),
        ];
        let usage = Usage {
            input_tokens: 1_000_000,
            ..Usage::default()
        };

        for (provider, identity, model, fingerprint, surface, mode) in cases {
            crate::provider_catalog_live::record_success(priced_provider_delta(
                identity,
                model,
                &fingerprint,
                future_unix,
            ));
            let no_future_quote = provider_live_usage_envelope(
                provider,
                identity,
                model,
                &fingerprint,
                Some(surface),
                mode,
                dispatched_at,
            );
            assert!(no_future_quote.provider_live_pricing.is_none());
            assert_eq!(
                no_future_quote.audit(&usage).unpriced_reason,
                Some(if provider == ProviderKind::Openrouter {
                    crate::pricing::UnpricedReason::NoPricingRow
                } else {
                    crate::pricing::UnpricedReason::UnverifiedLivePricing
                })
            );

            let captured = provider_live_usage_envelope(
                provider,
                identity,
                model,
                &fingerprint,
                Some(surface),
                mode,
                future_at,
            );
            assert!(captured.provider_live_pricing.is_some());

            let mut future_relative_to_dispatch = captured.clone();
            future_relative_to_dispatch.dispatched_at = dispatched_at;
            assert_eq!(
                future_relative_to_dispatch.audit(&usage).unpriced_reason,
                Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
            );
            let persisted = serde_json::to_value(&future_relative_to_dispatch)
                .expect("invalid future quote serializes only as absent");
            assert!(persisted["provider_live_pricing"].is_null());

            let mut wrong_model = captured.clone();
            wrong_model.model.push_str("-other");
            assert_eq!(
                wrong_model.audit(&usage).unpriced_reason,
                Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
            );

            let mut wrong_identity = captured.clone();
            wrong_identity.provider_identity.push_str("-other");
            // The endpoint fingerprint still establishes the billing contract,
            // so a renamed identity fails quote verification (#6289).
            assert_eq!(
                wrong_identity.audit(&usage).unpriced_reason,
                Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
            );

            let mut wrong_endpoint = captured;
            wrong_endpoint.endpoint_fingerprint = Some(
                codewhale_config::catalog::base_url_fingerprint("https://proxy.example/v1"),
            );
            assert_eq!(
                wrong_endpoint.audit(&usage).unpriced_reason,
                Some(if provider == ProviderKind::Custom {
                    crate::pricing::UnpricedReason::UnknownBillingBasis
                } else {
                    crate::pricing::UnpricedReason::UnverifiedLivePricing
                })
            );
        }
    }

    #[test]
    fn provider_live_quote_serialization_is_secret_free_and_legacy_compatible() {
        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("test home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let now = Utc::now();
        let fetched_at = u64::try_from(now.timestamp()).expect("nonnegative timestamp");
        let model = "synthetic-baseten-serialized-quote";
        let fingerprint = codewhale_config::catalog::base_url_fingerprint(
            codewhale_config::catalog::BASETEN_BASE_URL,
        );
        crate::provider_catalog_live::record_success(priced_provider_delta(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &fingerprint,
            fetched_at,
        ));
        let route = custom_usage_envelope(
            codewhale_config::catalog::BASETEN_PROVIDER_ID,
            model,
            &fingerprint,
            RouteBillingMode::Unknown,
            now,
        );
        assert!(route.provider_live_pricing.is_some());

        let serialized = serde_json::to_string(&route).expect("serialize frozen route");
        assert!(serialized.contains("provider_live_pricing"));
        assert!(serialized.contains("catalog_revision"));
        assert!(serialized.contains("input_per_million"));
        for secret in [
            codewhale_config::catalog::BASETEN_BASE_URL,
            "api_key",
            "Bearer ",
        ] {
            // The assertion message must not itself become a logging sink for
            // the credential fragment it checks for — name the check, not the
            // secret.
            assert!(
                !serialized.contains(secret),
                "frozen route serialization leaked a credential fragment"
            );
        }

        let mut child = serde_json::json!({});
        attach_child_usage_metadata(&mut child, &route, &Usage::default());
        let child_route = child_route_envelope_from_metadata(&child).expect("child route");
        assert_eq!(child_route, route.sanitized_for_persistence());

        let mut legacy: serde_json::Value =
            serde_json::from_str(&serialized).expect("route JSON value");
        legacy
            .as_object_mut()
            .expect("route object")
            .remove("provider_live_pricing");
        let legacy: EffectiveRouteEnvelope =
            serde_json::from_value(legacy).expect("legacy route remains readable");
        assert!(legacy.provider_live_pricing.is_none());
        let audit = legacy.audit(&Usage {
            input_tokens: 1_000_000,
            ..Usage::default()
        });
        assert_eq!(
            audit.unpriced_reason,
            Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
        );

        let mut wrong_model = route.clone();
        wrong_model.model.push_str("-other");
        assert_eq!(
            wrong_model.audit(&Usage::default()).unpriced_reason,
            Some(crate::pricing::UnpricedReason::UnverifiedLivePricing)
        );
    }

    #[test]
    fn routed_child_batch_is_preferred_bounded_and_sanitized() {
        let route = deepseek_envelope();
        let records = vec![
            RuntimeUsageRecord::legacy(
                "raw-provider-response-id-one".to_string(),
                EffectiveRouteUsage {
                    route: route.clone(),
                    usage: Usage {
                        input_tokens: 11,
                        ..Usage::default()
                    },
                },
            ),
            RuntimeUsageRecord::legacy(
                "raw-provider-response-id-two".to_string(),
                EffectiveRouteUsage {
                    route: route.clone(),
                    usage: Usage {
                        output_tokens: 7,
                        ..Usage::default()
                    },
                },
            ),
        ];
        let mut metadata = serde_json::json!({});
        attach_child_usage_metadata(&mut metadata, &route, &Usage::default());
        attach_child_usage_batch_metadata(
            &mut metadata,
            &RuntimeUsageBatch {
                decisions: Vec::new(),
                records,
                drop_records: Vec::new(),
                dropped_records: 0,
            },
        );

        let serialized = serde_json::to_string(&metadata).expect("batch metadata");
        assert!(!serialized.contains("raw-provider-response-id"));
        let batch = child_usage_records_from_metadata(&metadata).expect("preferred batch");
        assert_eq!(batch.records.len(), 2);
        assert_eq!(batch.records[0].usage.usage.input_tokens, 11);
        assert_eq!(batch.records[1].usage.usage.output_tokens, 7);
        assert_eq!(batch.dropped_records, 0);

        metadata[CHILD_USAGE_RECORDS_KEY] = serde_json::json!([{"bad": true}]);
        let malformed = child_usage_records_from_metadata(&metadata).expect("batch key wins");
        assert!(malformed.records.is_empty());
        assert_eq!(malformed.dropped_records, 1);
    }

    #[test]
    fn decision_receipts_metadata_round_trip_is_bounded_and_legacy_compatible() {
        let receipt = decision_receipt_fixture("raw-child-decision-id");
        let mut metadata = serde_json::json!({});
        attach_child_usage_batch_metadata(
            &mut metadata,
            &RuntimeUsageBatch {
                decisions: vec![receipt.clone()],
                ..Default::default()
            },
        );
        assert!(!metadata.to_string().contains("raw-child-decision-id"));
        let decoded = child_usage_records_from_metadata(&metadata).expect("batch");
        assert_eq!(decoded.decisions, vec![receipt.sanitized()]);
        assert_eq!(decoded.dropped_records, 0);
        metadata[CHILD_DECISION_RECEIPTS_KEY][0]["evidence"]["response_model"] =
            serde_json::json!("x".repeat(129));
        let invalid = child_usage_records_from_metadata(&metadata).expect("batch");
        assert!(invalid.decisions.is_empty());
        assert_eq!(invalid.dropped_records, 1);
        metadata
            .as_object_mut()
            .expect("metadata")
            .remove(CHILD_DECISION_RECEIPTS_KEY);
        assert!(
            child_usage_records_from_metadata(&metadata)
                .expect("old batch")
                .decisions
                .is_empty()
        );
        let mut oversized = receipt;
        oversized.evidence.response_model = Some("x".repeat(129));
        attach_child_usage_batch_metadata(
            &mut metadata,
            &RuntimeUsageBatch {
                decisions: vec![oversized],
                ..Default::default()
            },
        );
        let invalid = child_usage_records_from_metadata(&metadata).expect("bounded batch");
        assert!(invalid.decisions.is_empty());
        assert_eq!(
            invalid.dropped_records, 1,
            "discarded evidence must leave an explicit coverage gap"
        );
    }

    fn deepseek() -> BackgroundRoute<'static> {
        BackgroundRoute::new(ProviderKind::Deepseek, "deepseek-v4-flash").with_base_url(Some(
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
        ))
    }

    fn deepseek_envelope() -> EffectiveRouteEnvelope {
        let config = codewhale_runtime::config::Config::default();
        let identity = config
            .active_provider_identity()
            .expect("captured DeepSeek identity");
        EffectiveRouteEnvelope::from_admitted(
            Some(&config),
            &identity,
            "deepseek-v4-flash",
            Some(codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL),
            Utc::now(),
        )
    }

    #[test]
    fn child_cache_classes_reach_the_background_pool_only_when_reported() {
        // #6565: sub-agent cache was missing from session totals.
        let reported = background_cost_for_runtime_usage(&RuntimeUsageRecord::legacy(
            "child-cache-reported",
            EffectiveRouteUsage {
                route: deepseek_envelope(),
                usage: Usage {
                    input_tokens: 1_000,
                    output_tokens: 50,
                    prompt_cache_hit_tokens: Some(700),
                    prompt_cache_miss_tokens: Some(300),
                    ..Usage::default()
                },
            },
        ));
        assert_eq!(reported.cache_hit_tokens, Some(700));
        assert_eq!(reported.cache_miss_tokens, Some(300));
        assert_eq!(reported.cache_write_tokens, Some(0));

        let silent = background_cost_for_runtime_usage(&RuntimeUsageRecord::legacy(
            "child-cache-silent",
            EffectiveRouteUsage {
                route: deepseek_envelope(),
                usage: Usage {
                    input_tokens: 1_000,
                    output_tokens: 50,
                    ..Usage::default()
                },
            },
        ));
        assert_eq!(silent.cache_hit_tokens, None, "no report is not 0%");
        assert_eq!(silent.cache_miss_tokens, None);
    }

    #[test]
    fn background_cache_write_only_telemetry_is_recorded() {
        for written in [0, 400] {
            let pending = background_cost_for_runtime_usage(&RuntimeUsageRecord::legacy(
                "child-cache-write-only",
                EffectiveRouteUsage {
                    route: deepseek_envelope(),
                    usage: Usage {
                        input_tokens: 1_000,
                        prompt_cache_write_tokens: Some(written),
                        ..Usage::default()
                    },
                },
            ));
            assert_eq!(pending.cache_hit_tokens, Some(0));
            assert_eq!(pending.cache_miss_tokens, Some(u64::from(1_000 - written)));
            assert_eq!(pending.cache_write_tokens, Some(u64::from(written)));
        }
    }

    #[test]
    fn default_usage_is_one_missing_receipt_across_canonical_replay_and_owners() {
        let _g = test_scope();
        let route = deepseek_envelope();
        let raw = "compaction:turn:response";
        let fingerprint = usage_source_fingerprint(raw);
        let encoded = format!("routed:{fingerprint}");
        for source in [raw, fingerprint.as_str(), encoded.as_str()] {
            record_runtime_usage(
                scope_token(),
                None,
                &RuntimeUsageRecord::legacy(
                    source,
                    EffectiveRouteUsage {
                        route: route.clone(),
                        usage: Usage::default(),
                    },
                ),
            );
        }
        let missing = drain();
        assert_eq!(missing.source_records.len(), 1);
        assert_eq!(
            missing.usage_source_fingerprints,
            BTreeSet::from([fingerprint.clone()])
        );
        let contrib = UsageSourceContribution::for_record(&missing.source_records[0]);
        assert_eq!(contrib.priced, 0);
        assert_eq!(contrib.unpriced, 1);
        assert_eq!(contrib.cny_unpriced, 1);

        record_runtime_usage(
            scope_token(),
            None,
            &RuntimeUsageRecord::legacy(
                &encoded,
                EffectiveRouteUsage {
                    route: route.clone(),
                    usage: Usage::default(),
                },
            ),
        );
        assert!(
            drain().is_empty(),
            "replayed metadata must stay consumed after drain"
        );

        let recorded_owner = Arc::new(Mutex::new(Vec::new()));
        let recorded_sink = Arc::clone(&recorded_owner);
        let origin = NativeUsageOrigin::captured_runtime(
            scope_token(),
            "runtime-default-usage-owner".to_string(),
            Arc::new(move |record| {
                recorded_sink.lock().unwrap().push(record);
                true
            }),
        );
        for source in [raw, fingerprint.as_str(), encoded.as_str()] {
            record_runtime_usage(
                scope_token(),
                Some(&origin),
                &RuntimeUsageRecord::legacy(
                    source,
                    EffectiveRouteUsage {
                        route: route.clone(),
                        usage: Usage::default(),
                    },
                ),
            );
        }
        assert_eq!(recorded_owner.lock().unwrap().len(), 3);
        assert!(drain().is_empty());

        for billing_mode in [RouteBillingMode::Subscription, RouteBillingMode::Local] {
            let mut nonmetered = route.clone();
            nonmetered.billing_mode = billing_mode;
            let cost = background_cost_for_runtime_usage(&RuntimeUsageRecord::legacy(
                raw,
                EffectiveRouteUsage {
                    route: nonmetered,
                    usage: Usage::default(),
                },
            ));
            let c = UsageSourceContribution::for_record(&cost.source_records[0]);
            assert_eq!(c.unpriced, 0);
            assert_eq!(c.cny_unpriced, 0);
            assert_eq!(cost.usage_source_fingerprints.len(), 1);
        }

        let tmp = tempfile::tempdir().unwrap();
        let manager =
            crate::session_manager::SessionManager::new(tmp.path().join("sessions")).unwrap();
        let session = crate::session_manager::create_saved_session_with_id_and_mode(
            "missing-origin".into(),
            &[],
            "deepseek-v4-flash",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        manager.save_session(&session).unwrap();
        let origin_scope = scope_token();
        assert!(close_current_scope().is_empty());
        assert!(report_effective_route_for_interactive_origin_with_manager(
            origin_scope,
            "missing-origin",
            "turn",
            raw,
            &route,
            &Usage::default(),
            &manager,
        ));
        for _ in 0..2 {
            let snapshot = manager.load_session_snapshot("missing-origin").unwrap();
            assert_eq!(snapshot.metadata.total_tokens, 0);
            assert_eq!(snapshot.metadata.cost.priced_turns, 0);
            assert_eq!(snapshot.metadata.cost.unpriced_turns, 1);
            assert_eq!(snapshot.metadata.cost.cny_unpriced_turns, 1);
            assert_eq!(snapshot.metadata.cost.usage_source_fingerprints.len(), 1);
            manager.save_session(&snapshot).unwrap();
        }
        assert!(drain().is_empty());
    }

    #[test]
    fn openrouter_vendor_pin_does_not_inherit_aggregate_catalog_price() {
        let _env = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("isolated catalog home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        let _live = crate::provider_lake::lock_live_snapshot();
        crate::provider_lake::clear_live_snapshot();
        let mut route = EffectiveRouteEnvelope::capture(
            None,
            ProviderKind::Openrouter,
            "openrouter",
            "qwen/qwen3.7-plus",
            Some(ProviderKind::Openrouter.provider().default_base_url()),
            Utc::now(),
        );
        let usage = small_usage();
        let aggregate = route.audit(&usage);
        assert!(
            aggregate.is_priced(),
            "aggregate fixture must be priced: {aggregate:?}"
        );

        route.openrouter_vendor = Some("cerebras".to_string());
        let audit = route.audit(&usage);
        assert_eq!(
            audit.unpriced_reason,
            Some(crate::pricing::UnpricedReason::RoutingDependentPrice)
        );
        assert!(audit.estimate.is_none());
        assert!(audit.counts_toward_money_coverage());
        assert!(route.receipt(&audit).contains("openrouter_vendor=cerebras"));

        // Even a valid, frozen aggregate quote has no upstream-vendor dimension.
        let fingerprint = route
            .endpoint_fingerprint
            .clone()
            .expect("official endpoint");
        let dispatched_at = u64::try_from(route.dispatched_at.timestamp()).expect("timestamp");
        crate::provider_catalog_live::record_success(priced_provider_delta(
            "openrouter",
            &route.model,
            &fingerprint,
            dispatched_at,
        ));
        route.provider_live_pricing =
            crate::provider_catalog_live::fresh_provider_live_pricing_quote_at(
                route.provider,
                &route.provider_identity,
                &route.model,
                &fingerprint,
                dispatched_at,
            );
        assert!(route.provider_live_pricing.is_some());
        let saved: EffectiveRouteEnvelope =
            serde_json::from_value(serde_json::to_value(&route).unwrap()).unwrap();
        let child = child_route_envelope_from_metadata(&serde_json::Value::Object(
            child_usage_metadata_fields(&saved, &usage),
        ))
        .expect("child envelope");
        for receipt in [&route, &saved, &child] {
            assert_eq!(
                receipt.audit(&usage).unpriced_reason,
                Some(crate::pricing::UnpricedReason::RoutingDependentPrice)
            );
        }

        for (billing_mode, reason) in [
            (
                RouteBillingMode::Subscription,
                crate::pricing::UnpricedReason::NotMoneyMetered,
            ),
            (
                RouteBillingMode::Local,
                crate::pricing::UnpricedReason::NotMoneyMetered,
            ),
            (
                RouteBillingMode::Unknown,
                crate::pricing::UnpricedReason::UnknownBillingBasis,
            ),
        ] {
            route.billing_mode = billing_mode;
            assert_eq!(route.audit(&usage).unpriced_reason, Some(reason));
        }
    }

    #[test]
    fn openrouter_vendor_pin_survives_envelope_and_child_metadata_persistence() {
        let mut config = {
            let __cw_config_init_0_0 = Some("openrouter".to_string());
            let mut __cw_config_init_0_value: codewhale_runtime::config::Config =
                Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value
        };
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openrouter))
            .unwrap()
            .vendor = Some("cerebras".to_string());
        let route = EffectiveRouteEnvelope::capture(
            Some(&config),
            ProviderKind::Openrouter,
            "openrouter",
            "qwen/qwen3.7-plus",
            Some(ProviderKind::Openrouter.provider().default_base_url()),
            Utc::now(),
        );
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Openrouter))
            .unwrap()
            .vendor = None;
        assert_eq!(route.openrouter_vendor.as_deref(), Some("cerebras"));

        let mut json = serde_json::to_value(&route).expect("serialize route");
        let restored: EffectiveRouteEnvelope =
            serde_json::from_value(json.clone()).expect("restore route");
        assert_eq!(restored, route);
        let metadata =
            serde_json::Value::Object(child_usage_metadata_fields(&route, &small_usage()));
        assert_eq!(child_route_envelope_from_metadata(&metadata), Some(route));

        json.as_object_mut()
            .expect("route object")
            .remove("openrouter_vendor");
        let legacy: EffectiveRouteEnvelope = serde_json::from_value(json).expect("legacy route");
        assert_eq!(legacy.openrouter_vendor, None);
    }

    #[test]
    fn child_metadata_round_trip_preserves_zero_and_reasoning_usage() {
        let route = deepseek_envelope();
        let mut usage = Usage {
            input_tokens: 0,
            output_tokens: 9,
            reasoning_tokens: Some(7),
            reasoning_replay_tokens: Some(3),
            input_audio_tokens: Some(0),
            output_audio_tokens: Some(1),
            ..Usage::default()
        };
        usage.request_usages.push(usage.clone());
        let mut metadata = serde_json::json!({"tool": "rlm_eval"});
        attach_child_usage_metadata(&mut metadata, &route, &usage);

        assert_eq!(child_route_envelope_from_metadata(&metadata), Some(route));
        assert_eq!(child_usage_from_metadata(&metadata), Some(usage.clone()));
        metadata
            .as_object_mut()
            .unwrap()
            .remove("child_request_usages");
        let mut legacy = usage.clone();
        legacy.request_usages.clear();
        assert_eq!(child_usage_from_metadata(&metadata), Some(legacy));
        for malformed in [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!([{}]),
            serde_json::json!([{ "input_tokens": -1, "output_tokens": 9 }]),
            serde_json::json!([{ "input_tokens": 0, "output_tokens": 9, "request_usages": [{}] }]),
            serde_json::json!(vec![Usage::default(); 1025]),
        ] {
            metadata["child_request_usages"] = malformed;
            let decoded = child_usage_from_metadata(&metadata).unwrap();
            assert!(matches!(
                codewhale_config::pricing::OfferingPricing::default()
                    .quote_cost(&crate::pricing::token_usage_for_pricing(&decoded)),
                Err(codewhale_config::pricing::PricingQuoteError::InvalidUsage)
            ));
        }
        let pricing = codewhale_config::pricing::OfferingPricing {
            input_per_million: Some(1.0),
            output_per_million: Some(0.0),
            context_tiers: vec![
                serde_json::from_value(serde_json::json!({
                    "tier": { "type": "context", "size": 200_000 },
                    "input": 2.0,
                    "output": 0.0,
                }))
                .unwrap(),
            ],
            ..Default::default()
        };
        let mut aggregate = Usage::default();
        for _ in 0..3 {
            crate::core::turn::add_usage_to(
                &mut aggregate,
                &Usage {
                    input_tokens: 100_000,
                    ..Default::default()
                },
            );
        }
        let fields = child_usage_metadata_fields(&deepseek_envelope(), &aggregate);
        let mut decoded = child_usage_from_metadata(&serde_json::Value::Object(fields)).unwrap();
        assert_eq!(
            pricing
                .quote_cost(&crate::pricing::token_usage_for_pricing(&decoded))
                .unwrap()
                .amount,
            "0.3"
        );
        decoded.request_usages.clear();
        assert_eq!(
            pricing
                .quote_cost(&crate::pricing::token_usage_for_pricing(&decoded))
                .unwrap()
                .amount,
            "0.6"
        );

        let mut zero_metadata = serde_json::json!({});
        let zero = Usage::default();
        attach_child_usage_metadata(&mut zero_metadata, &deepseek_envelope(), &zero);
        assert_eq!(child_usage_from_metadata(&zero_metadata), Some(zero));
    }

    #[test]
    fn runtime_owned_usage_is_isolated_from_tui_pool() {
        let _g = test_scope();
        let route = deepseek_envelope();
        let usage = small_usage();
        let records_a = Arc::new(Mutex::new(Vec::new()));
        let records_b = Arc::new(Mutex::new(Vec::new()));
        let sink_a = Arc::clone(&records_a);
        let sink_b = Arc::clone(&records_b);
        let origin_a = NativeUsageOrigin::captured_runtime(
            scope_token(),
            "turn-a".to_string(),
            Arc::new(move |r| {
                sink_a.lock().unwrap().push(r);
                true
            }),
        );
        let origin_b = NativeUsageOrigin::captured_runtime(
            scope_token(),
            "turn-b".to_string(),
            Arc::new(move |r| {
                sink_b.lock().unwrap().push(r);
                true
            }),
        );
        let record_a = NativeUsageSource::capture("response-a", UsageSpendOwner::Auxiliary)
            .reported(&route, &usage);
        let record_b = NativeUsageSource::capture("response-b", UsageSpendOwner::Auxiliary)
            .reported(&route, &usage);
        record_runtime_usage(scope_token(), Some(&origin_a), &record_a);
        record_runtime_usage(scope_token(), Some(&origin_b), &record_b);

        assert_eq!(records_a.lock().unwrap().len(), 1);
        assert_eq!(records_b.lock().unwrap().len(), 1);
        assert!(
            drain().is_empty(),
            "runtime-owned usage must not enter TUI cost"
        );

        let tui_record = NativeUsageSource::capture("response-tui", UsageSpendOwner::Auxiliary)
            .reported(&route, &usage);
        record_runtime_usage(scope_token(), None, &tui_record);
        assert_eq!(
            drain().source_records.len(),
            1,
            "ownerless usage belongs to TUI"
        );
    }

    #[test]
    fn route_labels_redact_local_paths_but_preserve_model_namespaces() {
        let route = EffectiveRouteEnvelope {
            openrouter_vendor: None,
            provider: ProviderKind::Openrouter,
            provider_identity: "/Users/alice/.config/provider-secret".to_string(),
            model: "/Volumes/private/checkpoints/model.gguf".to_string(),
            billing_surface: None,
            endpoint_fingerprint: None,
            provider_live_pricing: None,
            billing_mode: RouteBillingMode::Metered,
            dispatched_at: Utc::now(),
        };
        let sanitized = route.sanitized_for_persistence();
        assert_eq!(sanitized.provider_identity, "redacted-local-path");
        assert_eq!(sanitized.model, "redacted-local-path");
        let receipt = route.receipt(&TurnCostAudit::unpriced(
            crate::pricing::UnpricedReason::NoPricingRow,
        ));
        assert!(!receipt.contains("alice"));
        assert!(!receipt.contains("Volumes"));

        assert_eq!(
            sanitize_persisted_route_label("anthropic/claude-sonnet-5"),
            "anthropic/claude-sonnet-5"
        );
    }

    #[test]
    fn route_label_sanitizer_rejects_credentials_urls_and_relative_paths() {
        for credential in [
            "Bearer secret-token",
            "Authorization: Basic abc123",
            "OPENAI_API_KEY=sk-secret",
            "service_token: ghp_secret",
            "hf_secret-token",
            "glpat-secret-token",
            "db-password=hunter2",
            "sk-live-secret",
            "https://alice:password@example.test/v1?api_key=secret#fragment",
        ] {
            let sanitized = sanitize_persisted_route_label(credential);
            assert!(
                sanitized.starts_with("redacted-"),
                "credential was not redacted: {credential:?} -> {sanitized:?}"
            );
        }
        for path in [
            ".ssh/id_ed25519",
            "../secrets/provider.key",
            "workspace/.ssh/config",
            "relative/path/to/credential",
            r"relative\path\credential",
        ] {
            assert_eq!(
                sanitize_persisted_route_label(path),
                "redacted-local-path",
                "path was not redacted: {path:?}"
            );
        }
        assert_eq!(
            sanitize_persisted_route_label("moonshot/kimi-k3"),
            "moonshot/kimi-k3"
        );
    }

    #[test]
    fn serialized_route_envelopes_records_and_child_receipts_are_secret_free() {
        let route = EffectiveRouteEnvelope {
            openrouter_vendor: Some("Authorization: Bearer vendor-secret".to_string()),
            provider: ProviderKind::Custom,
            provider_identity: "Authorization: Bearer provider-secret".to_string(),
            model: "MODEL_API_KEY=sk-model-secret".to_string(),
            billing_surface: Some(
                "https://alice:password@example.test/v1?token=secret#fragment".to_string(),
            ),
            endpoint_fingerprint: Some("../.ssh/provider_key".to_string()),
            provider_live_pricing: None,
            billing_mode: RouteBillingMode::Metered,
            dispatched_at: Utc::now(),
        };
        let usage = Usage {
            input_tokens: 7,
            output_tokens: 3,
            ..Usage::default()
        };

        let envelope_json = serde_json::to_string(&route).expect("serialize envelope");
        let record_json = serde_json::to_string(&EffectiveRouteUsage {
            route: route.clone(),
            usage: usage.clone(),
        })
        .expect("serialize route usage");
        let child_json = serde_json::to_string(&child_usage_metadata_fields(&route, &usage))
            .expect("serialize child receipt");
        for serialized in [&envelope_json, &record_json, &child_json] {
            for secret in [
                "vendor-secret",
                "provider-secret",
                "sk-model-secret",
                "alice",
                "password",
                "token=secret",
                ".ssh",
            ] {
                assert!(
                    !serialized.contains(secret),
                    "serialized route leaked {secret:?}: {serialized}"
                );
            }
        }
    }

    #[test]
    fn report_adds_to_pool_and_drain_returns_then_resets() {
        let _g = test_scope();
        report(scope_token(), &deepseek(), &small_usage());
        let first = drain();
        assert!(
            first.estimate.usd > 0.0,
            "expected positive USD cost, got {first:?}"
        );
        assert!(
            first.estimate.cny > 0.0,
            "expected positive CNY cost, got {first:?}"
        );
        assert_eq!(first.priced_turns, 1);
        assert_eq!(first.unpriced_turns, 0);
        assert_eq!(first.cny_priced_turns, 1);
        assert_eq!(first.cny_unpriced_turns, 0);
        // The receipt names the route without leaking the endpoint URL.
        assert_eq!(first.route_receipts.len(), 1);
        let receipt = first.route_receipts.iter().next().expect("receipt");
        assert!(receipt.contains("provider=deepseek"), "{receipt}");
        assert!(receipt.contains("model=deepseek-v4-flash"), "{receipt}");
        assert!(receipt.contains("currency=usd+cny"), "{receipt}");
        assert!(!receipt.contains("http"), "{receipt}");

        let second = drain();
        assert!(second.is_empty(), "drain must zero the pool: {second:?}");
    }

    #[test]
    fn reports_from_a_closed_session_scope_are_discarded() {
        let _g = test_scope();
        let old_scope = scope_token();
        let settled = close_current_scope();
        assert!(settled.is_empty());

        report(old_scope, &deepseek(), &small_usage());
        assert!(drain().is_empty(), "old session usage crossed the boundary");

        report(scope_token(), &deepseek(), &small_usage());
        assert_eq!(drain().priced_turns, 1);
    }

    #[test]
    fn retired_origin_suppresses_replay_without_accrual_or_scope_leak() {
        let _g = test_scope();
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = crate::session_manager::SessionManager::new(tmp.path().join("sessions"))
            .expect("manager");
        let session = crate::session_manager::create_saved_session_with_id_and_mode(
            "retired-origin".to_string(),
            &[],
            "deepseek-v4-flash",
            tmp.path(),
            0,
            None,
            Some("agent"),
        );
        manager.save_session(&session).expect("save origin");
        let origin_scope = scope_token();
        manager
            .delete_session("retired-origin")
            .expect("delete origin");
        let route = deepseek_envelope();
        for _ in 0..2 {
            assert!(report_effective_route_for_interactive_origin_with_manager(
                origin_scope,
                "retired-origin",
                "origin-turn",
                "retired-usage",
                &route,
                &small_usage(),
                &manager,
            ));
            assert!(report_unreceipted_for_interactive_origin_with_manager(
                origin_scope,
                "retired-origin",
                "origin-turn",
                "retired-drop",
                &route,
                &manager,
            ));
        }
        assert!(usage_source_seen("retired-usage"));
        assert!(usage_source_seen("retired-drop"));
        assert!(
            drain().is_empty(),
            "retirement must not create any pending projection"
        );
        assert!(
            !manager
                .sessions_dir()
                .join(".late-usage/retired-origin.json")
                .exists()
        );

        assert!(close_current_scope().is_empty());
        assert!(!usage_source_seen("retired-usage"));
        assert!(report_effective_route_for_interactive_origin_with_manager(
            origin_scope,
            "retired-origin",
            "origin-turn",
            "after-scope-change",
            &route,
            &small_usage(),
            &manager,
        ));
        assert!(
            !usage_source_seen("after-scope-change"),
            "old retirement cannot poison a new scope"
        );
        assert!(drain().is_empty());
        record_runtime_usage(
            scope_token(),
            None,
            &RuntimeUsageRecord::legacy(
                "after-scope-change",
                EffectiveRouteUsage {
                    route,
                    usage: small_usage(),
                },
            ),
        );
        assert_eq!(
            drain().priced_turns,
            1,
            "the replacement scope still admits its own response"
        );
    }

    #[test]
    fn detached_advisor_and_translation_receipts_survive_new_exactly_once() {
        let _g = test_scope();
        let tmp = tempfile::tempdir().expect("tempdir");
        let manager = crate::session_manager::SessionManager::new(tmp.path().join("sessions"))
            .expect("session manager");
        let old_session_id = "origin-session";
        let new_session_id = "replacement-session";
        for session_id in [old_session_id, new_session_id] {
            let session = crate::session_manager::create_saved_session_with_id_and_mode(
                session_id.to_string(),
                &[],
                "deepseek-v4-flash",
                tmp.path(),
                0,
                None,
                Some("agent"),
            );
            manager.save_session(&session).expect("save session");
        }

        let origin_scope = scope_token();
        let origin = NativeUsageOrigin::session(
            origin_scope,
            manager.sessions_dir().to_path_buf(),
            old_session_id.to_string(),
        );

        assert!(close_current_scope().is_empty());
        let route = deepseek_envelope();
        let usage = Usage {
            input_tokens: 17,
            output_tokens: 5,
            ..Usage::default()
        };
        let advisor_source = NativeUsageSource::capture_with_origin(
            "advisor:origin-turn:response",
            UsageSpendOwner::Auxiliary,
            Some(origin.clone()),
        );
        let advisor_record = advisor_source.reported(&route, &usage);
        let advisor_missing_source = NativeUsageSource::capture_with_origin(
            "advisor:origin-turn:missing-usage",
            UsageSpendOwner::Auxiliary,
            Some(origin.clone()),
        );
        let advisor_missing_record = advisor_missing_source.prepare(
            &route,
            &Usage::default(),
            crate::llm_client::ProviderUsageCoverage::Missing,
            RuntimeUsageMissingReason::SuccessWithoutUsage,
        );
        for _ in 0..2 {
            record_runtime_usage(origin_scope, Some(&origin), &advisor_record);
            record_runtime_usage(origin_scope, Some(&origin), &advisor_missing_record);
            assert!(report_effective_route_for_interactive_origin_with_manager(
                origin_scope,
                old_session_id,
                "origin-turn",
                "translation:origin-turn:assistant",
                &route,
                &usage,
                &manager,
            ));
            assert!(report_unreceipted_for_interactive_origin_with_manager(
                origin_scope,
                old_session_id,
                "origin-turn",
                "translation:origin-turn:thinking-missing-usage",
                &route,
                &manager,
            ));
        }

        assert!(drain().is_empty(), "late receipts polluted the new scope");

        let old = manager
            .load_session_snapshot(old_session_id)
            .expect("load origin session");
        assert_eq!(old.metadata.total_tokens, 44);
        assert_eq!(old.metadata.cost.priced_turns, 2);
        assert_eq!(old.metadata.cost.unpriced_turns, 2);
        assert_eq!(old.metadata.cost.cny_unpriced_turns, 2);
        assert_eq!(old.metadata.cost.usage_source_fingerprints.len(), 4);

        let replay = manager
            .load_session_snapshot(old_session_id)
            .expect("replay origin session");
        assert_eq!(replay.metadata.total_tokens, 44);
        assert_eq!(replay.metadata.cost.usage_source_fingerprints.len(), 4);

        let replacement = manager
            .load_session_snapshot(new_session_id)
            .expect("load replacement session");
        assert_eq!(replacement.metadata.total_tokens, 0);
        assert_eq!(replacement.metadata.cost.priced_turns, 0);
        assert_eq!(replacement.metadata.cost.unpriced_turns, 0);
        assert!(
            replacement
                .metadata
                .cost
                .usage_source_fingerprints
                .is_empty()
        );
    }

    #[test]
    fn report_counts_unknown_models_as_missing_spend_not_as_free() {
        let _g = test_scope();
        // NIM-hosted models intentionally have no DeepSeek pricing, but the
        // route *is* money-metered — so the turn is missing spend, not absent.
        report(
            scope_token(),
            &BackgroundRoute::new(ProviderKind::NvidiaNim, "deepseek-ai/deepseek-v4-pro"),
            &small_usage(),
        );
        let drained = drain();
        assert_eq!(drained.estimate, CostEstimate::default());
        assert_eq!(drained.priced_turns, 0);
        assert_eq!(drained.unpriced_turns, 1);
        assert!(!drained.unpriced_reasons.is_empty());
    }

    #[test]
    fn report_skips_codex_oauth_pricing_without_calling_it_incomplete() {
        let _g = test_scope();
        report(
            scope_token(),
            &BackgroundRoute::new(ProviderKind::OpenaiCodex, "gpt-5.5")
                .with_base_url(Some("https://chatgpt.com/backend-api/codex")),
            &small_usage(),
        );
        let drained = drain();
        assert_eq!(drained.estimate, CostEstimate::default());
        // Exactly non-metered: not counted in either coverage bucket.
        assert_eq!(drained.priced_turns, 0);
        assert_eq!(drained.unpriced_turns, 0);
        assert!(drained.unpriced_reasons.is_empty());
        assert!(drained.cny_unpriced_reasons.is_empty());
    }

    #[test]
    fn report_skips_stepfun_without_billing_surface() {
        let _g = test_scope();
        report(
            scope_token(),
            &BackgroundRoute::new(ProviderKind::Stepfun, "step-3.7-flash"),
            &small_usage(),
        );
        report(
            scope_token(),
            &BackgroundRoute::new(ProviderKind::Openrouter, "step-3.7-flash"),
            &small_usage(),
        );
        let drained = drain();
        assert_eq!(drained.estimate, CostEstimate::default());
        // Both are metered-or-unknown routes that could not be priced, so both
        // are reported as missing rather than dropped.
        assert_eq!(drained.unpriced_turns, 2);
    }

    /// A local runtime and a plan endpoint must never be guessed into public
    /// per-token dollars just because the provider also sells a paid API.
    #[test]
    fn local_and_plan_endpoints_are_never_treated_as_public_payg() {
        let _g = test_scope();
        report(
            scope_token(),
            &BackgroundRoute::new(ProviderKind::Ollama, "llama3.2"),
            &small_usage(),
        );
        report(
            scope_token(),
            &BackgroundRoute::new(ProviderKind::Zai, "glm-5.2")
                .with_base_url(Some("https://api.z.ai/api/coding/paas/v4")),
            &small_usage(),
        );
        report(
            scope_token(),
            &BackgroundRoute::new(ProviderKind::Moonshot, "kimi-for-coding").with_base_url(Some(
                codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL,
            )),
            &small_usage(),
        );
        let drained = drain();
        assert_eq!(drained.estimate, CostEstimate::default());
        assert_eq!(drained.priced_turns, 0);
        assert_eq!(
            drained.unpriced_turns, 0,
            "exactly non-metered routes are not missing dollars: {drained:?}"
        );
        assert!(drained.unpriced_reasons.is_empty());
        assert!(drained.cny_unpriced_reasons.is_empty());
        assert!(
            drained
                .route_receipts
                .iter()
                .any(|receipt| receipt.contains("surface=zai-coding-plan")),
            "{drained:?}"
        );
        assert!(
            drained
                .route_receipts
                .iter()
                .any(|receipt| receipt.contains("surface=local-no-bill")),
            "{drained:?}"
        );
        assert!(
            drained
                .route_receipts
                .iter()
                .any(|receipt| receipt.contains("surface=moonshot-kimi-code")),
            "{drained:?}"
        );
    }

    /// The receipt carries an endpoint *fingerprint*, never the URL.
    #[test]
    fn route_receipts_fingerprint_the_endpoint_and_keep_secrets_out() {
        let _g = test_scope();
        let base_url = "https://api.deepseek.com/v1";
        report(
            scope_token(),
            &deepseek().with_base_url(Some(base_url)),
            &small_usage(),
        );
        let drained = drain();
        let receipt = drained.route_receipts.iter().next().expect("receipt");
        let expected_fp = endpoint_fingerprint(base_url).expect("valid endpoint fingerprint");
        assert!(
            receipt.contains(&format!("endpoint_fp={expected_fp}")),
            "{receipt}"
        );
        for needle in ["http", "api.deepseek.com", "sk-", "/Users/", "/home/"] {
            assert!(!receipt.contains(needle), "{needle} leaked into {receipt}");
        }
    }

    #[test]
    fn receipt_fields_are_bounded_and_secret_bearing_urls_are_not_hashed() {
        let hostile = format!("model\nAuthorization: bearer {}", "x".repeat(400));
        let receipt = route_receipt(
            ProviderKind::Deepseek,
            Some("identity\r\nforged=yes"),
            &hostile,
            Some(crate::pricing::FIRST_PARTY_PAYG_BILLING_SURFACE),
            None,
            RouteBillingMode::Metered,
            "usd+cny",
        );
        assert!(!receipt.contains('\n'), "{receipt}");
        assert!(!receipt.contains('\r'), "{receipt}");
        assert!(
            receipt.len() < 420,
            "receipt was not bounded: {}",
            receipt.len()
        );

        for secret_url in [
            "https://user:secret@api.example.com/v1",
            "https://api.example.com/v1?api_key=secret",
            "https://api.example.com/v1#secret",
        ] {
            assert_eq!(endpoint_fingerprint(secret_url), None, "{secret_url}");
        }
        assert_eq!(
            endpoint_fingerprint("https://API.Example.com/v1/")
                .expect("valid endpoint")
                .len(),
            64
        );
    }

    #[test]
    fn report_accumulates_across_multiple_calls() {
        let _g = test_scope();
        report(scope_token(), &deepseek(), &small_usage());
        report(scope_token(), &deepseek(), &small_usage());
        let total = drain();
        // Two equal reports — total must be 2× a single report.
        let single = crate::pricing::calculate_turn_cost_estimate_from_usage(
            "deepseek-v4-flash",
            &small_usage(),
        )
        .unwrap();
        assert!((total.estimate.usd - 2.0 * single.usd).abs() < 1e-12);
        assert!((total.estimate.cny - 2.0 * single.cny).abs() < 1e-12);
        assert_eq!(total.priced_turns, 2);
        // Identical routes collapse to one receipt rather than growing without
        // bound across a long session.
        assert_eq!(total.route_receipts.len(), 1);
    }

    /// A cache-write turn on a route with no published write rate must show up
    /// as missing spend naming the class, not as a discounted total.
    #[test]
    fn unpriced_cache_write_class_is_reported_not_absorbed() {
        let _g = test_scope();
        let write_heavy = Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            prompt_cache_hit_tokens: Some(200_000),
            prompt_cache_write_tokens: Some(100_000),
            ..Default::default()
        };
        report(
            scope_token(),
            &BackgroundRoute::new(ProviderKind::Moonshot, "kimi-k2.7-code")
                .with_base_url(Some("https://api.moonshot.ai/v1")),
            &write_heavy,
        );
        let drained = drain();
        assert_eq!(drained.estimate, CostEstimate::default());
        assert_eq!(drained.unpriced_turns, 1);
        assert!(drained.unpriced_reasons.contains("missing_class_price"));
        assert!(drained.unpriced_classes.contains("cache_write"));
        assert!(
            drained
                .route_receipts
                .iter()
                .any(|receipt| receipt.contains("cache_write=yes")),
            "{drained:?}"
        );
    }

    #[test]
    fn missing_usage_reason_preserves_legacy_bytes_and_metadata_identity() {
        let route = deepseek_envelope();
        let old = serde_json::json!({"source_id":"opaque-response", "route":route});
        let legacy: RuntimeUsageDropRecord = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(
            legacy.reason,
            RuntimeUsageMissingReason::SuccessWithoutUsage
        );
        assert_eq!(serde_json::to_value(&legacy).unwrap(), old);
        let unknown = RuntimeUsageDropRecord {
            reason: RuntimeUsageMissingReason::RequestOutcomeUnknown,
            ..legacy
        };
        let mut metadata = serde_json::json!({});
        attach_child_usage_batch_metadata(
            &mut metadata,
            &RuntimeUsageBatch {
                drop_records: vec![unknown],
                dropped_records: 1,
                ..Default::default()
            },
        );
        let recovered = child_usage_records_from_metadata(&metadata).unwrap();
        assert_eq!(
            recovered.drop_records[0].reason,
            RuntimeUsageMissingReason::RequestOutcomeUnknown
        );
        assert_eq!(
            recovered.drop_records[0].source_id,
            usage_source_fingerprint("opaque-response")
        );
        assert_eq!(recovered.dropped_records, 1);
        assert!(
            !serde_json::to_string(&metadata)
                .unwrap()
                .contains("opaque-response")
        );
    }

    #[test]
    fn runtime_unknown_batch_promotes_and_prunes_reason_via_supersedes() {
        let _scope = test_scope();
        let route = deepseek_envelope();
        let batch = RuntimeUsageBatch {
            drop_records: vec![RuntimeUsageDropRecord {
                reason: RuntimeUsageMissingReason::RequestOutcomeUnknown,
                source_id: "batch-attempt".to_string(),
                route: route.clone(),
            }],
            dropped_records: 1,
            ..Default::default()
        };
        let mut metadata = serde_json::json!({});
        attach_child_usage_batch_metadata(&mut metadata, &batch);
        let batch = child_usage_records_from_metadata(&metadata).unwrap();
        record_runtime_usage_batch(scope_token(), None, &batch);
        record_runtime_usage_batch(scope_token(), None, &batch);
        let missing = drain();
        assert_eq!(missing.source_records.len(), 1);
        let prior = &missing.source_records[0];
        let mut accounted = BTreeMap::from([(
            usage_source_fingerprint("batch-attempt"),
            UsageSourceContribution::for_record(prior),
        )]);
        let mut reasons = BTreeSet::from(["request_outcome_unknown".to_string()]);
        let mut cny_reasons = BTreeSet::from(["request_outcome_unknown".to_string()]);
        assert_eq!(accounted.values().next().unwrap().unpriced, 1);

        let settled = NativeUsageSource::capture("batch-attempt", UsageSpendOwner::Auxiliary)
            .reported(&route, &small_usage());
        assert!(settled.supersedes(Some(prior)));
        record_runtime_usage(scope_token(), None, &settled);
        let known = drain();
        assert_eq!(known.source_records.len(), 1);
        let next = UsageSourceContribution::for_record(&known.source_records[0]);
        assert_eq!(next.priced, 1);
        assert_eq!(next.unpriced, 0);
        accounted.insert(usage_source_fingerprint("batch-attempt"), next);
        prune_settled_missing_reasons(&accounted, 0, 0, &mut reasons, &mut cny_reasons);
        assert!(!reasons.contains("request_outcome_unknown"));
        assert!(!cny_reasons.contains("request_outcome_unknown"));
    }

    #[test]
    fn late_unknown_receipt_promotes_after_restart_without_charging_replacement_session() {
        let _scope = test_scope();
        let tmp = tempfile::tempdir().unwrap();
        let sessions = tmp.path().join("sessions");
        let manager = crate::session_manager::SessionManager::new(sessions.clone()).unwrap();
        for id in ["unknown-origin", "replacement"] {
            let session = crate::session_manager::create_saved_session_with_id_and_mode(
                id.into(),
                &[],
                "deepseek-v4-flash",
                tmp.path(),
                0,
                None,
                Some("agent"),
            );
            manager.save_session(&session).unwrap();
        }
        let origin = scope_token();
        assert!(close_current_scope().is_empty());
        let route = deepseek_envelope();
        assert!(report_missing_usage_for_interactive_origin_with_manager(
            origin,
            "unknown-origin",
            "origin-turn",
            "late-attempt",
            &route,
            RuntimeUsageMissingReason::RequestOutcomeUnknown,
            &manager
        ));
        let missing = manager.load_session_snapshot("unknown-origin").unwrap();
        assert_eq!(missing.metadata.cost.unpriced_turns, 1);
        assert!(
            missing
                .metadata
                .cost
                .unpriced_reasons
                .contains("request_outcome_unknown")
        );
        manager.save_session(&missing).unwrap();
        drop(manager);
        let manager = crate::session_manager::SessionManager::new(sessions).unwrap();
        for _ in 0..2 {
            assert!(report_effective_route_for_interactive_origin_with_manager(
                origin,
                "unknown-origin",
                "origin-turn",
                "late-attempt",
                &route,
                &small_usage(),
                &manager
            ));
            let known = manager.load_session_snapshot("unknown-origin").unwrap();
            assert_eq!(known.metadata.cost.priced_turns, 1);
            assert_eq!(known.metadata.cost.unpriced_turns, 0);
            assert!(
                !known
                    .metadata
                    .cost
                    .unpriced_reasons
                    .contains("request_outcome_unknown")
            );
            assert_eq!(
                known.metadata.total_tokens,
                u64::from(small_usage().input_tokens) + u64::from(small_usage().output_tokens)
            );
            manager.save_session(&known).unwrap();
        }
        assert!(!report_effective_route_for_interactive_origin_with_manager(
            origin,
            "unknown-origin",
            "different-turn",
            "late-attempt",
            &route,
            &small_usage(),
            &manager
        ));
        let replacement = manager.load_session_snapshot("replacement").unwrap();
        assert_eq!(replacement.metadata.total_tokens, 0);
        assert_eq!(
            (
                replacement.metadata.cost.priced_turns,
                replacement.metadata.cost.unpriced_turns
            ),
            (0, 0)
        );
        assert!(drain().is_empty());
    }

    #[test]
    fn frozen_dispatch_quotes_price_google_and_major_providers_while_unpriced_models_fail_closed() {
        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().expect("test home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        let _reset = ProviderCatalogTestReset;
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();

        let now = Utc::now();
        let now_unix = u64::try_from(now.timestamp()).expect("nonnegative timestamp");
        let google_base = ProviderKind::Google.provider().default_base_url();
        let google_fp = codewhale_config::catalog::base_url_fingerprint(google_base);

        let google_roster = codewhale_config::catalog::ProviderCatalogDelta {
            provider: "google".to_string(),
            base_url_fingerprint: google_fp.clone(),
            fetched_at: now_unix,
            offerings: [
                "models/gemini-4-carbon",
                "models/gemini-4-hydrogen",
                "models/gemini-4-argon",
                "models/gemini-4-neon",
            ]
            .into_iter()
            .map(|id| codewhale_config::catalog::CatalogOffering {
                provider: "google".to_string(),
                wire_model_id: id.to_string(),
                endpoint_key: "chat".to_string(),
                source: codewhale_config::catalog::CatalogSource::Live {
                    base_url_fingerprint: google_fp.clone(),
                    fetched_at: now_unix,
                },
                ..Default::default()
            })
            .collect(),
        };
        assert_eq!(
            crate::provider_catalog_live::record_success(google_roster),
            codewhale_config::catalog::CatalogStatus::Fresh
        );

        let google_usage = Usage {
            input_tokens: 136_287,
            output_tokens: 46,
            prompt_cache_hit_tokens: Some(131_324),
            prompt_cache_miss_tokens: Some(4_963),
            ..Usage::default()
        };
        let roster_probe = Usage {
            input_tokens: 1,
            output_tokens: 1,
            ..Usage::default()
        };

        for (model, expected_usd) in [
            (
                "gemini-4-carbon",
                4_963.0 * 2e-6 + 131_324.0 * 0.1e-6 + 46.0 * 10e-6,
            ),
            (
                "models/gemini-4-carbon",
                4_963.0 * 2e-6 + 131_324.0 * 0.1e-6 + 46.0 * 10e-6,
            ),
            (
                "gemini-4-hydrogen",
                4_963.0 * 1.5e-6 + 131_324.0 * 0.075e-6 + 46.0 * 7.5e-6,
            ),
            (
                "models/gemini-4-hydrogen",
                4_963.0 * 1.5e-6 + 131_324.0 * 0.075e-6 + 46.0 * 7.5e-6,
            ),
            (
                "gemini-4-neon",
                4_963.0 * 3e-6 + 131_324.0 * 0.15e-6 + 46.0 * 15e-6,
            ),
            (
                "models/gemini-4-neon",
                4_963.0 * 3e-6 + 131_324.0 * 0.15e-6 + 46.0 * 15e-6,
            ),
        ] {
            let route = EffectiveRouteEnvelope::capture_observed(
                ProviderKind::Google,
                "google",
                model,
                Some(google_base),
                now,
            );
            assert!(route.provider_live_pricing.is_some(), "{model}");
            let audit = route.audit_frozen(&google_usage);
            assert!(audit.usd_priced, "{model}: {audit:?}");
            assert_eq!(audit.unpriced_reason, None, "{model}: {audit:?}");
            assert!(
                (audit.estimate.expect("priced").usd - expected_usd).abs() < 1e-12,
                "{model}: {audit:?}"
            );
            let probe = route.audit(&roster_probe);
            assert!(
                probe.estimate.is_some_and(CostEstimate::is_positive),
                "{model} roster probe: {probe:?}"
            );
            assert_eq!(probe.unpriced_reason, None, "{model} roster probe");
        }

        let live_models_dev = codewhale_config::catalog::CatalogSnapshot {
            offerings: vec![
                codewhale_config::catalog::CatalogOffering {
                    provider: "google".to_string(),
                    wire_model_id: "gemini-4-neon".to_string(),
                    endpoint_key: "chat".to_string(),
                    cost: Some(codewhale_config::models_dev::ModelsDevCost {
                        input: Some(3.0),
                        output: Some(15.0),
                        cache_read: Some(0.15),
                        input_audio: Some(3.0),
                        ..Default::default()
                    }),
                    source: codewhale_config::catalog::CatalogSource::ModelsDevLive {
                        fetched_at: now_unix,
                    },
                    ..Default::default()
                },
                codewhale_config::catalog::CatalogOffering {
                    provider: "google".to_string(),
                    wire_model_id: "gemini-4-carbon".to_string(),
                    endpoint_key: "chat".to_string(),
                    cost: Some(codewhale_config::models_dev::ModelsDevCost {
                        input: Some(2.0),
                        output: Some(10.0),
                        cache_read: Some(0.1),
                        input_audio: Some(2.0),
                        ..Default::default()
                    }),
                    source: codewhale_config::catalog::CatalogSource::ModelsDevLive {
                        fetched_at: now_unix.saturating_sub(
                            crate::provider_catalog_live::DEFAULT_PROVIDER_CATALOG_TTL_SECS + 60,
                        ),
                    },
                    ..Default::default()
                },
            ],
        };
        crate::provider_lake::set_live_snapshot(
            live_models_dev,
            crate::provider_lake::LiveSource::ModelsDev,
        );

        let expected_neon = 4_963.0 * 3e-6 + 131_324.0 * 0.15e-6 + 46.0 * 15e-6;
        for model in ["gemini-4-neon", "models/gemini-4-neon"] {
            let route = EffectiveRouteEnvelope::capture_observed(
                ProviderKind::Google,
                "google",
                model,
                Some(google_base),
                now,
            );
            let audit = route.audit_frozen(&google_usage);
            assert!(audit.usd_priced, "{model}: {audit:?}");
            assert_eq!(
                audit.provenance,
                Some(codewhale_config::pricing::PricingProvenance::ModelsDevLive)
            );
            assert!(
                (audit.estimate.expect("priced").usd - expected_neon).abs() < 1e-12,
                "{model}: {audit:?}"
            );
        }

        let stale_degraded = EffectiveRouteEnvelope::capture_observed(
            ProviderKind::Google,
            "google",
            "gemini-4-carbon",
            Some(google_base),
            now,
        );
        let stale_audit = stale_degraded.audit_frozen(&google_usage);
        assert!(stale_audit.usd_priced, "{stale_audit:?}");
        assert_eq!(
            stale_audit.provenance,
            Some(codewhale_config::pricing::PricingProvenance::ModelsDevBundled)
        );

        for (provider, identity, model, base_url) in [
            (
                ProviderKind::Openai,
                "openai",
                "gpt-5.6",
                "https://api.openai.com/v1",
            ),
            (
                ProviderKind::Anthropic,
                "anthropic",
                "claude-fable-5-2",
                "https://api.anthropic.com",
            ),
            (
                ProviderKind::Deepseek,
                "deepseek",
                "deepseek-v4-flash",
                "https://api.deepseek.com/beta",
            ),
            (
                ProviderKind::Openrouter,
                "openrouter",
                "deepseek/deepseek-v4-pro",
                "https://openrouter.ai/api/v1",
            ),
            (
                ProviderKind::Zai,
                "zai",
                "glm-5.2",
                "https://api.z.ai/api/paas/v4",
            ),
            (
                ProviderKind::Stepfun,
                "stepfun",
                "step-3.7-flash",
                "https://api.stepfun.ai/v1",
            ),
        ] {
            let api_key_config = codewhale_runtime::config::Config::default();
            let route = if provider == ProviderKind::Anthropic {
                let observed = EffectiveRouteEnvelope::capture_observed(
                    provider,
                    identity,
                    model,
                    Some(base_url),
                    now,
                );
                assert!(
                    !observed.audit_frozen(&small_usage()).is_priced(),
                    "{model} {base_url}"
                );
                EffectiveRouteEnvelope::from_admitted(
                    Some(&api_key_config),
                    &api_key_config.test_identity_for_kind(provider),
                    model,
                    Some(base_url),
                    now,
                )
            } else {
                EffectiveRouteEnvelope::capture_observed(
                    provider,
                    identity,
                    model,
                    Some(base_url),
                    now,
                )
            };
            assert!(
                route.provider_live_pricing.is_some(),
                "{provider:?} {model} {base_url}"
            );
            let audit = route.audit_frozen(&small_usage());
            assert!(
                audit.usd_priced && audit.estimate.is_some_and(CostEstimate::is_positive),
                "{provider:?} {model} {base_url}: {audit:?}"
            );
        }

        let proxy_route = EffectiveRouteEnvelope::capture_observed(
            ProviderKind::Google,
            "google",
            "gemini-4-carbon",
            Some("https://proxy.example/v1"),
            now,
        );
        assert!(proxy_route.provider_live_pricing.is_none());
        assert!(!proxy_route.audit_frozen(&google_usage).is_priced());

        let custom_route = EffectiveRouteEnvelope::capture_observed(
            ProviderKind::Custom,
            "my-local-gateway",
            "totally-unknown-custom-model",
            Some("https://gateway.example/v1"),
            now,
        );
        assert!(custom_route.provider_live_pricing.is_none());
        assert!(!custom_route.audit_frozen(&small_usage()).is_priced());
    }
}

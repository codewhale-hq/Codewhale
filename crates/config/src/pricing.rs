//! Provider/offering-scoped pricing projection with provenance (#3085).
//!
//! Network-free. Maps Models.dev offering `cost` (and live / user-override
//! rows) into pricing rows that carry explicit **provenance**, **currency**, and
//! **effective-at** metadata, plus a pure cost estimator over normalized token
//! usage. UI display (`CostDisplay`) and provider usage-payload parsing live
//! above this layer and are out of scope here.
//!
//! Boundary with the route layer: this models *pricing* — offering-owned,
//! per-token unit prices. The coarse route-facing meter shape already exists as
//! [`crate::route::PricingSku`]
//! (`Token` / `SubscriptionQuota` / `AccountCredits` / `LocalOrNotApplicable` /
//! `UnknownOrStale`); [`OfferingPricing::to_route_sku`] and
//! [`route_pricing_sku`] bridge to it.
//!
//! Honesty rule (#2608 / #3085): pricing is never assumed. A route with no
//! sourced price yields `None` here and `UnknownOrStale` at the route layer —
//! never a fabricated token price, and never an implicit "free" for
//! local/custom/subscription routes.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

use crate::catalog::{CatalogOffering, CatalogSource};
use crate::models_dev::{ModelsDevCost, ModelsDevCostTier};
use crate::route::PricingSku;

/// Billing currency for a pricing row. Models.dev publishes USD per-million
/// costs; other currencies arrive via provider docs or user overrides.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Currency {
    #[default]
    Usd,
    Cny,
    /// An ISO-4217-style code CodeWhale does not special-case.
    Other(String),
}

/// Where a pricing row came from. Retained so the UI can show provenance and so
/// stale/unknown prices are never silently treated as authoritative.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum PricingProvenance {
    /// Published-price estimate from the offline Models.dev snapshot.
    ModelsDevBundled,
    ModelsDevLive,
    /// From a provider live `/models` (or pricing) refresh — a rate the
    /// provider itself published for one endpoint, not a catalog's report of it.
    ProviderLive,
    /// From provider documentation / a hand-sourced seed. Set only by callers
    /// constructing rows directly; `from_catalog_offering` never produces this
    /// (Models.dev-sourced rows map to `ModelsDevBundled`).
    ProviderDocs,
    /// User-supplied override (custom endpoint, enterprise terms, local route).
    UserOverride,
    /// No sourced price.
    #[default]
    Unknown,
    /// From the Codewhale-owned signed/bundled catalog.
    CodewhaleCatalog,
    /// Verified signed cloud-facts price block; expiry is checked at projection.
    CloudFacts,
}

impl PricingProvenance {
    /// Stable, non-localized identifier for logs, JSON, and scorecards.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::ModelsDevBundled => "models_dev_bundled",
            Self::ModelsDevLive => "models_dev_live",
            Self::ProviderLive => "provider_live",
            Self::ProviderDocs => "provider_docs",
            Self::UserOverride => "user_override",
            Self::Unknown => "unknown",
            Self::CodewhaleCatalog => "codewhale_catalog",
            Self::CloudFacts => "cloud_facts",
        }
    }

    /// Whether this provenance may be presented as an authoritative published
    /// price without further freshness checks.
    ///
    /// [`Self::ProviderLive`] is deliberately excluded: a live row is only
    /// authoritative while it is fresh *and* was fetched from the endpoint the
    /// turn was actually served on. Callers must clear it through
    /// [`OfferingPricing::live_pricing_defect`] first.
    #[must_use]
    pub fn is_authoritative_without_freshness_check(&self) -> bool {
        matches!(
            self,
            Self::ModelsDevBundled
                | Self::ProviderDocs
                | Self::UserOverride
                | Self::CodewhaleCatalog
        )
    }
}

/// Default freshness window for a `ProviderLive` pricing row, in seconds.
///
/// A provider `/models` refresh is a snapshot of a mutable price list. Past
/// this age CodeWhale stops calling the row authoritative rather than billing
/// against a rate the provider may have already changed.
pub const LIVE_PRICING_MAX_AGE_SECS: u64 = 24 * 60 * 60;

/// Why a `ProviderLive` pricing row cannot be treated as authoritative.
///
/// Each variant is a non-secret receipt: fingerprints are FNV digests of a
/// normalized base URL (see [`crate::catalog::base_url_fingerprint`]), never the
/// URL itself, so these can be logged and serialized freely.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "defect", rename_all = "snake_case")]
pub enum LivePricingDefect {
    /// The row is older than the caller's freshness window.
    Stale { age_secs: u64, max_age_secs: u64 },
    /// The row was fetched from a different endpoint than the turn was served
    /// on, so it prices a different billing surface.
    EndpointMismatch {
        row_fingerprint: String,
        route_fingerprint: String,
    },
    /// The row claims live provenance but carries no endpoint fingerprint, so
    /// it cannot be matched to the route that is being priced.
    MissingEndpointFingerprint,
    /// The row claims live provenance but carries no fetch timestamp, so its
    /// age cannot be established.
    MissingTimestamp,
    /// The caller could not establish which endpoint the turn was served on, so
    /// a live row cannot be confirmed to price that route.
    UnknownRouteEndpoint,
}

impl LivePricingDefect {
    /// Stable, non-localized identifier for logs, JSON, and scorecards.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stale { .. } => "live_pricing_stale",
            Self::EndpointMismatch { .. } => "live_pricing_endpoint_mismatch",
            Self::MissingEndpointFingerprint => "live_pricing_missing_endpoint_fingerprint",
            Self::MissingTimestamp => "live_pricing_missing_timestamp",
            Self::UnknownRouteEndpoint => "live_pricing_unknown_route_endpoint",
        }
    }
}

/// Normalized token usage for a single turn, in canonical billable classes.
///
/// Producing this from provider-specific usage payloads (Chat Completions,
/// Responses, Anthropic) is a separate concern; this layer only consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Non-cached input (prompt) tokens.
    pub input: u64,
    /// Total billable output (completion) tokens.
    ///
    /// Providers report reasoning tokens as a *subset* of the completion token
    /// count (OpenAI `output_tokens_details.reasoning_tokens` ⊆ `output_tokens`,
    /// Chat Completions `completion_tokens_details.reasoning_tokens` ⊆
    /// `completion_tokens`), so a normalizer must never add reasoning tokens on
    /// top of this field — that double-bills every reasoning turn.
    pub output: u64,
    /// Cache-read (cache-hit) input tokens, billed at the cache-read rate.
    pub cache_read: u64,
    /// Cache-write (cache-creation) tokens, billed at the cache-write rate.
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_audio: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_usages: Vec<TokenUsage>,
}

/// A canonical billable token class.
///
/// Used to report *which* class of a turn's usage lacked a published price, so
/// an unpriced turn can be audited instead of silently dropping out of a total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenClass {
    Input,
    Output,
    CacheRead,
    CacheWrite,
    Reasoning,
    InputAudio,
    OutputAudio,
}

impl TokenClass {
    /// Every class, in reporting order.
    pub const ALL: [Self; 7] = [
        Self::Input,
        Self::Output,
        Self::CacheRead,
        Self::CacheWrite,
        Self::Reasoning,
        Self::InputAudio,
        Self::OutputAudio,
    ];

    /// Stable, non-localized identifier for logs, JSON, and scorecards.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Output => "output",
            Self::CacheRead => "cache_read",
            Self::CacheWrite => "cache_write",
            Self::Reasoning => "reasoning",
            Self::InputAudio => "input_audio",
            Self::OutputAudio => "output_audio",
        }
    }

    /// This class's token count within `usage`.
    #[must_use]
    pub fn tokens(self, usage: &TokenUsage) -> u64 {
        match self {
            Self::Input => usage.input,
            Self::Output => usage.output,
            Self::CacheRead => usage.cache_read,
            Self::CacheWrite => usage.cache_write,
            Self::Reasoning => usage.reasoning.unwrap_or(0),
            Self::InputAudio => usage.input_audio.unwrap_or(0),
            Self::OutputAudio => usage.output_audio.unwrap_or(0),
        }
    }
}

/// A provider/offering-scoped pricing row.
///
/// Prices are per million tokens in [`Currency`]. Any field may be unknown
/// (`None`); [`OfferingPricing::estimate_cost`] refuses to invent a number for a
/// used class whose price is unknown.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct OfferingPricing {
    /// Provider id serving the offering.
    pub provider: String,
    /// Provider-owned wire id the price applies to.
    pub wire_model_id: String,
    /// Canonical model identity, when the offering carries one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_model: Option<String>,
    /// Billing currency.
    pub currency: Currency,
    /// Input price per million tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_per_million: Option<f64>,
    /// Output price per million tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_per_million: Option<f64>,
    /// Cache-read price per million tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_per_million: Option<f64>,
    /// Cache-write price per million tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_audio_per_million: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_tiers: Vec<ModelsDevCostTier>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub context_tiers_inclusive: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rate_withheld: bool,
    /// Where the price came from.
    pub provenance: PricingProvenance,
    /// Actual rate-effective time, when the source declares one. Old serialized
    /// provider rows used this as a fetch time; `fetched_at` takes precedence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_last_updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    /// Fingerprint of the base URL this price was fetched from, for
    /// [`PricingProvenance::ProviderLive`] rows.
    ///
    /// This is the same non-secret SHA-256 digest the catalog cache scopes on
    /// (see [`crate::catalog::base_url_fingerprint`]) — never the URL itself.
    /// It exists so a live row can be proven to price the endpoint a turn was
    /// actually served on; a row whose fingerprint does not match the route
    /// is a different billing surface, not a fresher price for this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotedCost {
    pub amount: String,
    pub currency: Currency,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PricingQuoteError {
    MissingRate(TokenClass),
    MissingUsage(TokenClass),
    InvalidUsage,
    UnsupportedTier,
    InvalidRate,
}

impl OfferingPricing {
    /// Derive a pricing row from a catalog offering's `cost`, when priced.
    ///
    /// Returns `None` when the offering carries no cost. An explicitly empty
    /// cost object stays withheld, so another source cannot refill it.
    ///
    /// Models.dev `cost` values are USD per million tokens, so the currency is
    /// [`Currency::Usd`]; provenance and `fetched_at` follow the offering's
    /// [`CatalogSource`]. The upstream update date stays separate.
    #[must_use]
    pub fn from_catalog_offering(offering: &CatalogOffering) -> Option<Self> {
        Self::from_catalog_offering_at(offering, crate::catalog::now_unix())
    }

    /// Project at the dispatch clock when freezing a quote. Cloud-facts source
    /// validity is signed and cannot be extended by a later transport refresh.
    #[must_use]
    pub fn from_catalog_offering_at(offering: &CatalogOffering, now: u64) -> Option<Self> {
        let source = offering.pricing_source();
        if let CatalogSource::CloudFacts {
            fetched_at,
            valid_until,
            ..
        } = source
            && (*fetched_at > now || valid_until.is_some_and(|expires| now > expires))
        {
            return None;
        }
        let cost = offering.cost.as_ref()?;
        // A provider/catalog price is untrusted numeric input.  Reject the
        // entire row when any published class is NaN, infinite, or negative;
        // accepting only the apparently valid fields would turn a malformed
        // row into a silently incomplete (or negative) bill.
        if !catalog_cost_is_valid(cost) {
            return None;
        }
        let rate_withheld = cost.input.is_none()
            && cost.output.is_none()
            && cost.cache_read.is_none()
            && cost.cache_write.is_none()
            && cost.reasoning.is_none()
            && cost.input_audio.is_none()
            && cost.output_audio.is_none()
            && cost.context_tiers().is_empty();
        Some(Self {
            provider: offering.provider.clone(),
            wire_model_id: offering.wire_model_id.clone(),
            canonical_model: offering.canonical_model.clone(),
            currency: Currency::Usd,
            input_per_million: cost.input,
            output_per_million: cost.output,
            cache_read_per_million: cost.cache_read,
            cache_write_per_million: cost.cache_write,
            reasoning_per_million: cost.reasoning,
            input_audio_per_million: cost.input_audio,
            output_audio_per_million: cost.output_audio,
            context_tiers: cost.context_tiers(),
            context_tiers_inclusive: offering.provider == "xai",
            rate_withheld,
            provenance: provenance_from_source(source),
            effective_at: None,
            fetched_at: fetched_at_from_source(source),
            expires_at: match source {
                CatalogSource::CloudFacts { valid_until, .. } => *valid_until,
                CatalogSource::Live { fetched_at, .. }
                | CatalogSource::ModelsDevLive { fetched_at } => {
                    fetched_at.checked_add(LIVE_PRICING_MAX_AGE_SECS)
                }
                _ => None,
            },
            upstream_last_updated: offering.last_updated.clone(),
            source_ref: matches!(
                source,
                CatalogSource::Bundled | CatalogSource::ModelsDevLive { .. }
            )
            .then(|| {
                format!(
                    "https://models.dev/catalog.json#{}:{}",
                    offering.provider, offering.wire_model_id
                )
            }),
            endpoint_fingerprint: endpoint_fingerprint_from_source(source),
        })
    }

    /// Whether any per-token price is known.
    #[must_use]
    pub fn has_any_price(&self) -> bool {
        self.input_per_million.is_some()
            || self.output_per_million.is_some()
            || self.cache_read_per_million.is_some()
            || self.cache_write_per_million.is_some()
            || self.reasoning_per_million.is_some()
            || self.input_audio_per_million.is_some()
            || self.output_audio_per_million.is_some()
            || !self.context_tiers.is_empty()
    }

    /// Whether this price is older than `max_age_secs` at `now_unix`.
    ///
    /// Live rows age against their fetch clock. Declared effective dates do
    /// not become fetch dates; explicit expiry applies to every source.
    #[must_use]
    pub fn is_stale(&self, now_unix: u64, max_age_secs: u64) -> bool {
        self.expires_at.is_some_and(|expires| now_unix >= expires)
            || (matches!(
                self.provenance,
                PricingProvenance::ModelsDevLive | PricingProvenance::ProviderLive
            ) && self
                .fetched_at
                .or(self.effective_at)
                .is_some_and(|time| time > now_unix || now_unix - time >= max_age_secs))
    }

    #[must_use]
    pub fn status_label_at(&self, now: u64) -> &'static str {
        if self.rate_withheld || !self.has_any_price() {
            return "unavailable";
        }
        if self.is_stale(now, LIVE_PRICING_MAX_AGE_SECS) {
            return "stale";
        }
        if matches!(
            self.provenance,
            PricingProvenance::ProviderLive | PricingProvenance::ModelsDevLive
        ) && self
            .live_pricing_defect(
                self.endpoint_fingerprint.as_deref(),
                Some(now),
                LIVE_PRICING_MAX_AGE_SECS,
            )
            .is_some()
        {
            return "unverified";
        }
        match self.provenance {
            PricingProvenance::Unknown => "unverified",
            PricingProvenance::ModelsDevBundled | PricingProvenance::CodewhaleCatalog => {
                "bundled_estimate"
            }
            PricingProvenance::ModelsDevLive | PricingProvenance::ProviderLive => "live_estimate",
            PricingProvenance::ProviderDocs => "documented_estimate",
            PricingProvenance::UserOverride => "operator_estimate",
            PricingProvenance::CloudFacts => "verified_cloud_estimate",
        }
    }

    /// Why this row cannot be trusted as an authoritative live price, if so.
    ///
    /// Returns `None` for rows that are not [`PricingProvenance::ProviderLive`]
    /// (a bundled snapshot, a documented hand price, or a user override carries
    /// no fetch clock to go stale against) and for live rows that are both
    /// fresh and fingerprint-matched to `route_endpoint_fingerprint`.
    ///
    /// A live row with any defect must not be labelled `provider_live` nor used
    /// as complete pricing: it is either older than `max_age_secs` or priced for
    /// a different endpoint. Callers fail closed and receipt the returned
    /// defect. `route_endpoint_fingerprint` of `None` means the caller could not
    /// determine the endpoint at all, which is itself a defect — a live row can
    /// never be *confirmed* to price an unknown route.
    #[must_use]
    pub fn live_pricing_defect(
        &self,
        route_endpoint_fingerprint: Option<&str>,
        now_unix: Option<u64>,
        max_age_secs: u64,
    ) -> Option<LivePricingDefect> {
        if !matches!(
            self.provenance,
            PricingProvenance::ProviderLive | PricingProvenance::ModelsDevLive
        ) {
            return None;
        }
        if self.provenance == PricingProvenance::ProviderLive {
            let Some(row_fingerprint) = self.endpoint_fingerprint.as_deref() else {
                return Some(LivePricingDefect::MissingEndpointFingerprint);
            };
            let Some(route_fingerprint) = route_endpoint_fingerprint else {
                return Some(LivePricingDefect::UnknownRouteEndpoint);
            };
            if row_fingerprint != route_fingerprint {
                return Some(LivePricingDefect::EndpointMismatch {
                    row_fingerprint: row_fingerprint.to_string(),
                    route_fingerprint: route_fingerprint.to_string(),
                });
            }
        }
        let Some(fetched_at) = self.fetched_at.or(self.effective_at) else {
            return Some(LivePricingDefect::MissingTimestamp);
        };
        // Without a clock the age is unknowable, so the row stays unproven
        // rather than being assumed fresh.
        let Some(now_unix) = now_unix else {
            return Some(LivePricingDefect::MissingTimestamp);
        };
        let age_secs = now_unix.saturating_sub(fetched_at);
        if fetched_at > now_unix
            || age_secs >= max_age_secs
            || self.expires_at.is_some_and(|expires| now_unix >= expires)
        {
            return Some(LivePricingDefect::Stale {
                age_secs,
                max_age_secs,
            });
        }
        None
    }

    /// Per-million price for one canonical class, when published.
    #[must_use]
    pub fn price_per_million(&self, class: TokenClass) -> Option<f64> {
        match class {
            TokenClass::Input => self.input_per_million,
            TokenClass::Output => self.output_per_million,
            TokenClass::CacheRead => self.cache_read_per_million,
            TokenClass::CacheWrite => self.cache_write_per_million,
            TokenClass::Reasoning => self.reasoning_per_million,
            TokenClass::InputAudio => self.input_audio_per_million,
            TokenClass::OutputAudio => self.output_audio_per_million,
        }
    }

    /// Classes this turn actually used that carry no published price.
    ///
    /// Non-empty means [`Self::estimate_cost`] fails closed for this usage; the
    /// returned classes are exactly the reason why, so callers can report the
    /// gap instead of presenting a silently under-counted total.
    #[must_use]
    pub fn unpriced_used_classes(&self, usage: &TokenUsage) -> Vec<TokenClass> {
        if !usage.request_usages.is_empty() {
            if usage.request_usages.len() > 1024
                || usage
                    .request_usages
                    .iter()
                    .any(|request| !request.request_usages.is_empty())
            {
                return Vec::new();
            }
            return usage
                .request_usages
                .iter()
                .flat_map(|request| self.unpriced_used_classes(request))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
        }
        let Ok((cost, billable)) = self.rates_and_usage(usage) else {
            return Vec::new();
        };
        TokenClass::ALL
            .into_iter()
            .filter(|class| class.tokens(&billable) > 0 && rate_for_class(&cost, *class).is_none())
            .collect()
    }

    fn rates_and_usage(
        &self,
        usage: &TokenUsage,
    ) -> Result<(ModelsDevCost, TokenUsage), PricingQuoteError> {
        let base = self.cost_structure();
        if !catalog_cost_is_valid(&base) {
            return Err(PricingQuoteError::InvalidRate);
        }
        if self
            .context_tiers
            .iter()
            .any(|tier| tier.tier.kind != "context" || !tier.cost.context_tiers().is_empty())
        {
            return Err(PricingQuoteError::UnsupportedTier);
        }
        let total_input = usage
            .input
            .checked_add(usage.cache_read)
            .and_then(|total| total.checked_add(usage.cache_write))
            .ok_or(PricingQuoteError::InvalidUsage)?;
        let cost = self
            .context_tiers
            .iter()
            .filter(|tier| {
                if self.context_tiers_inclusive {
                    total_input >= tier.tier.size
                } else {
                    total_input > tier.tier.size
                }
            })
            .max_by_key(|tier| tier.tier.size)
            .map_or(base, |tier| tier.cost.clone());
        let mut billable = usage.clone();
        let reasoning = usage.reasoning.unwrap_or(0);
        let audio_output = usage.output_audio.unwrap_or(0);
        let audio_input = usage.input_audio.unwrap_or(0);
        if reasoning
            .checked_add(audio_output)
            .is_none_or(|total| total > usage.output)
            || audio_input > usage.input
        {
            return Err(PricingQuoteError::InvalidUsage);
        }
        for (class, tokens, rate, total) in [
            (
                TokenClass::Reasoning,
                usage.reasoning,
                cost.reasoning,
                usage.output,
            ),
            (
                TokenClass::InputAudio,
                usage.input_audio,
                cost.input_audio,
                total_input,
            ),
            (
                TokenClass::OutputAudio,
                usage.output_audio,
                cost.output_audio,
                usage.output,
            ),
        ] {
            if rate.is_some() && tokens.is_none() && total > 0 {
                return Err(PricingQuoteError::MissingUsage(class));
            }
        }
        if audio_input > 0 && (usage.cache_read > 0 || usage.cache_write > 0) {
            return Err(PricingQuoteError::MissingUsage(TokenClass::InputAudio));
        }
        billable.input = usage.input - audio_input;
        billable.output = usage.output - audio_output;
        if cost.reasoning.is_some() {
            billable.output -= reasoning;
        } else {
            billable.reasoning = None;
        }
        Ok((cost, billable))
    }

    #[must_use]
    pub fn cost_structure(&self) -> ModelsDevCost {
        ModelsDevCost {
            input: self.input_per_million,
            output: self.output_per_million,
            cache_read: self.cache_read_per_million,
            cache_write: self.cache_write_per_million,
            reasoning: self.reasoning_per_million,
            input_audio: self.input_audio_per_million,
            output_audio: self.output_audio_per_million,
            tiers: (!self.context_tiers.is_empty()).then(|| self.context_tiers.clone()),
            context_over_200k: None,
        }
    }

    pub fn quote_cost(&self, usage: &TokenUsage) -> Result<QuotedCost, PricingQuoteError> {
        if self.rate_withheld {
            return Err(PricingQuoteError::MissingRate(TokenClass::Input));
        }
        if !usage.request_usages.is_empty() {
            if usage.request_usages.len() > 1024 {
                return Err(PricingQuoteError::InvalidUsage);
            }
            let mut aggregate = [0_u64; 4];
            let mut total = Decimal::ZERO;
            for request in &usage.request_usages {
                if !request.request_usages.is_empty() {
                    return Err(PricingQuoteError::InvalidUsage);
                }
                for (sum, count) in aggregate.iter_mut().zip([
                    request.input,
                    request.output,
                    request.cache_read,
                    request.cache_write,
                ]) {
                    *sum = sum
                        .checked_add(count)
                        .ok_or(PricingQuoteError::InvalidUsage)?;
                }
                let quote = self.quote_cost(request)?;
                let amount = Decimal::from_str_exact(&quote.amount)
                    .map_err(|_| PricingQuoteError::InvalidRate)?;
                total = add_decimal_exact(total, amount)?;
            }
            if aggregate
                != [
                    usage.input,
                    usage.output,
                    usage.cache_read,
                    usage.cache_write,
                ]
            {
                return Err(PricingQuoteError::InvalidUsage);
            }
            return Ok(QuotedCost {
                amount: total.normalize().to_string(),
                currency: self.currency.clone(),
            });
        }
        let (cost, billable) = self.rates_and_usage(usage)?;
        let mut total = Decimal::ZERO;
        for class in TokenClass::ALL {
            let tokens = class.tokens(&billable);
            if tokens == 0 {
                continue;
            }
            let rate = rate_for_class(&cost, class).ok_or(PricingQuoteError::MissingRate(class))?;
            let rate = Decimal::from_str_exact(&rate.to_string())
                .map_err(|_| PricingQuoteError::InvalidRate)?;
            let mut coefficient = rate
                .mantissa()
                .checked_mul(i128::from(tokens))
                .ok_or(PricingQuoteError::InvalidRate)?;
            let mut scale = rate.scale() + 6;
            while scale > 0 && coefficient % 10 == 0 {
                coefficient /= 10;
                scale -= 1;
            }
            let component = Decimal::try_from_i128_with_scale(coefficient, scale)
                .map_err(|_| PricingQuoteError::InvalidRate)?;
            total = add_decimal_exact(total, component)?;
        }
        Ok(QuotedCost {
            amount: total.normalize().to_string(),
            currency: self.currency.clone(),
        })
    }

    /// Estimate in major currency units; floating point is a display projection
    /// of the exact decimal quote, never a second calculation authority.
    #[must_use]
    pub fn estimate_cost(&self, usage: &TokenUsage) -> Option<f64> {
        self.quote_cost(usage).ok()?.amount.parse().ok()
    }

    /// Project to the coarse route-facing meter shape.
    ///
    /// Returns [`PricingSku::Token`] only when an input or output rate is known.
    /// The route-layer `Token` shape carries only input/output rates, so a row
    /// priced *only* on cache classes would become a `Token` with no visible
    /// rates — misleading at the route layer. Such rows degrade to
    /// [`PricingSku::UnknownOrStale`] here while their cache rates remain usable
    /// through [`OfferingPricing::estimate_cost`].
    #[must_use]
    pub fn to_route_sku(&self) -> PricingSku {
        if self.input_per_million.is_none() && self.output_per_million.is_none() {
            return PricingSku::UnknownOrStale;
        }
        PricingSku::Token {
            input_per_mtok: self.input_per_million,
            output_per_mtok: self.output_per_million,
        }
    }
}

/// The honest route-facing pricing meter for a catalog offering.
///
/// An offering with a usable input/output rate becomes [`PricingSku::Token`];
/// everything else — no cost, a cost object with no concrete price, or a
/// cache-only price — becomes [`PricingSku::UnknownOrStale`] rather than a
/// fabricated zero price. An explicit empty cost stays withheld;
/// `to_route_sku` keeps both it and a cache-only rate unknown.
#[must_use]
pub fn route_pricing_sku(offering: &CatalogOffering) -> PricingSku {
    OfferingPricing::from_catalog_offering(offering)
        .map_or(PricingSku::UnknownOrStale, |pricing| pricing.to_route_sku())
}

/// The honest route-facing pricing meter for a raw Models.dev `cost` block.
///
/// Same honesty rule as [`route_pricing_sku`], but for callers that hold a
/// [`ModelsDevCost`] directly (the route-offering builders in
/// [`crate::models_dev`]) rather than a full [`CatalogOffering`]. An absent or
/// concretely-empty cost, or a cache-only cost, yields
/// [`PricingSku::UnknownOrStale`]; only a usable input/output rate yields
/// [`PricingSku::Token`].
#[must_use]
pub(crate) fn route_pricing_sku_from_cost(cost: Option<&ModelsDevCost>) -> PricingSku {
    let Some(cost) = cost else {
        return PricingSku::UnknownOrStale;
    };
    if !catalog_cost_is_valid(cost) {
        return PricingSku::UnknownOrStale;
    }
    if cost.input.is_none() && cost.output.is_none() {
        // No input/output rate: a cache-only or empty cost would render as a
        // rate-less `Token` at the route layer, so it stays honestly unknown.
        return PricingSku::UnknownOrStale;
    }
    PricingSku::Token {
        input_per_mtok: cost.input,
        output_per_mtok: cost.output,
    }
}

/// Upper bound, per million tokens, on a price CodeWhale will treat as real.
///
/// Published frontier rates are in the single-to-triple digits per million.
/// A value four orders of magnitude above that is not an expensive model, it is
/// a unit error — a per-token price parsed as per-million, or a minor-unit
/// integer (cents, fen) read as a major unit. Both mistakes bill the user
/// 10^6 or 10^2 times over, so the row is rejected rather than believed.
///
/// The bound is deliberately generous: it exists to catch impossible
/// magnitudes, not to second-guess a provider's pricing.
pub const MAX_PLAUSIBLE_PRICE_PER_MILLION: f64 = 100_000.0;

/// Whether every numeric field in a catalog price is finite, non-negative, and
/// of a plausible magnitude.
///
/// Kept at the catalog boundary so every projection (routing SKU and runtime
/// cost audit) applies the same validation rule. Catalog prices are untrusted
/// numeric input: they arrive from a bundled snapshot, a live provider
/// `/models` response, or a user override file, and any of the three can carry
/// a malformed value. The whole row is rejected on a single bad field —
/// accepting the fields that happen to parse would turn a malformed row into a
/// silently under-counted bill, which is worse than no price at all.
#[must_use]
pub fn catalog_cost_is_valid(cost: &ModelsDevCost) -> bool {
    cost.validate().is_ok()
        && cost.tiers.as_ref().is_none_or(|tiers| tiers.len() <= 128)
        && [
            cost.input,
            cost.output,
            cost.cache_read,
            cost.cache_write,
            cost.reasoning,
            cost.input_audio,
            cost.output_audio,
        ]
        .into_iter()
        .flatten()
        .all(|price| price.is_finite() && (0.0..=MAX_PLAUSIBLE_PRICE_PER_MILLION).contains(&price))
        && cost
            .tiers
            .iter()
            .flatten()
            .all(|tier| catalog_cost_is_valid(&tier.cost))
        && cost
            .context_over_200k
            .as_ref()
            .is_none_or(|cost| catalog_cost_is_valid(cost))
}

fn add_decimal_exact(total: Decimal, component: Decimal) -> Result<Decimal, PricingQuoteError> {
    let next = total
        .checked_add(component)
        .ok_or(PricingQuoteError::InvalidRate)?;
    if next.checked_sub(total) != Some(component) {
        return Err(PricingQuoteError::InvalidRate);
    }
    Ok(next)
}

fn rate_for_class(cost: &ModelsDevCost, class: TokenClass) -> Option<f64> {
    match class {
        TokenClass::Input => cost.input,
        TokenClass::Output => cost.output,
        TokenClass::CacheRead => cost.cache_read,
        TokenClass::CacheWrite => cost.cache_write,
        TokenClass::Reasoning => cost.reasoning,
        TokenClass::InputAudio => cost.input_audio,
        TokenClass::OutputAudio => cost.output_audio,
    }
}

fn provenance_from_source(source: &CatalogSource) -> PricingProvenance {
    match source {
        CatalogSource::CloudFacts { .. } => PricingProvenance::CloudFacts,
        CatalogSource::Bundled => PricingProvenance::ModelsDevBundled,
        CatalogSource::ModelsDevLive { .. } => PricingProvenance::ModelsDevLive,
        CatalogSource::Live { .. } => PricingProvenance::ProviderLive,
        CatalogSource::ConfigOverride | CatalogSource::UserOverride => {
            PricingProvenance::UserOverride
        }
        CatalogSource::CodewhaleBundled { .. } => PricingProvenance::CodewhaleCatalog,
    }
}

fn fetched_at_from_source(source: &CatalogSource) -> Option<u64> {
    match source {
        CatalogSource::Live { fetched_at, .. }
        | CatalogSource::ModelsDevLive { fetched_at }
        | CatalogSource::CloudFacts { fetched_at, .. } => Some(*fetched_at),
        CatalogSource::Bundled
        | CatalogSource::ConfigOverride
        | CatalogSource::UserOverride
        | CatalogSource::CodewhaleBundled { .. } => None,
    }
}

fn endpoint_fingerprint_from_source(source: &CatalogSource) -> Option<String> {
    match source {
        CatalogSource::Live {
            base_url_fingerprint,
            ..
        } => Some(base_url_fingerprint.clone()),
        CatalogSource::Bundled
        | CatalogSource::ModelsDevLive { .. }
        | CatalogSource::ConfigOverride
        | CatalogSource::UserOverride
        | CatalogSource::CodewhaleBundled { .. }
        | CatalogSource::CloudFacts { .. } => None,
    }
}

/// Cost display currency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostCurrency {
    Usd,
    Cny,
}

impl CostCurrency {
    #[must_use]
    pub fn from_setting(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "usd" | "dollar" | "dollars" | "$" => Some(Self::Usd),
            "cny" | "rmb" | "yuan" | "¥" => Some(Self::Cny),
            _ => None,
        }
    }

    #[must_use]
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Usd => "$",
            Self::Cny => "¥",
        }
    }
}

#[cfg(test)]
mod tests;

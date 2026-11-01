//! Models.dev-backed provider catalog snapshots and a secret-free live cache
//! (#3385, feeding EPIC #2608 and #3383).
//!
//! This module is **network-free** by construction. Callers supply parsed
//! [`crate::models_dev::ModelsDevCatalog`] JSON (bundled snapshot or live
//! refresh) and live [`ProviderCatalogDelta`]s; the HTTP `/models` fetch layer
//! lives above this module. Nothing here performs I/O or reads credentials.
//!
//! Layering (lowest precedence first):
//!
//! ```text
//! bundled Models.dev snapshot         (legacy seed, not competing truth)
//!   < live Models.dev                 (public catalog, external enrichment)
//!   < Codewhale corrections           (bundled field patches, see [`corrections`])
//!   < signed cloud facts              (curated correction, off by default)
//!   < live provider `/v1/models`      (credential-scoped workspace list)
//!   < config.toml / user overrides
//! ```
//!
//! The two live layers are not the same kind of claim. A provider roster is a
//! fact about an endpoint the caller authenticated to; models.dev is a public
//! third-party catalog that is merely fresher than the bundled copy of itself.
//! Only the first outranks a signed correction — see
//! [`CatalogSource::ModelsDevLive`].
//!
//! After #4187, live Models.dev rows are preferred over the bundled seed. The
//! bundled asset remains so offline startup and failed refreshes still resolve
//! defaults.
//!
//! Invariants preserved from #2608 / #3497:
//! - A catalog row is **not** an executable route. Rows still compile through
//!   `RouteResolver` into a `ReadyRouteCandidate` before execution.
//! - `wire_model_id` is kept separate from `canonical_model`; a provider row may
//!   not expose a canonical `base_model` join, and a prefix never proves
//!   canonical ownership.
//! - Unknown / custom / local rows are supported with explicit provenance and a
//!   `None` canonical model.
//!
//! The on-disk cache format intentionally uses plain `String` identity fields
//! rather than the internal route newtypes, so the persisted shape is decoupled
//! from internal types and trivially auditable for "no secrets" (see
//! [`ProviderCatalogCache`] tests).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::models_dev::{
    ModelsDevCatalog, ModelsDevCost, ModelsDevInterleaved, ModelsDevLimit, ModelsDevModalities,
    ModelsDevModelTransport,
};
use crate::route::{ModelId, ProviderId, ProviderModelOffering, RouteLimits, WireModelId};

pub mod configured;
pub mod corrections;
pub mod reviewed;

/// Provenance of a catalog row. Drives layer precedence and UI provenance.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CatalogSource {
    /// Offline/stale bundled seed (Models.dev-shaped snapshot). Not competing
    /// truth — live Models.dev rows override this layer (#4188).
    #[default]
    Bundled,
    /// A provider live `/models` row, scoped to a base-URL fingerprint and the
    /// unix timestamp it was fetched at.
    ///
    /// This is a **provider fact**: the row exists because that endpoint, asked
    /// under the caller's own credential, said so. It therefore outranks the
    /// signed cloud layer and is never patched by it, and its fingerprint names
    /// the billing surface the row prices. A third-party catalog describing the
    /// same model is [`Self::ModelsDevLive`], whatever URL it was fetched from.
    Live {
        base_url_fingerprint: String,
        fetched_at: u64,
    },
    /// A user / custom override (custom endpoint, pinned model, explicit facts).
    UserOverride,
    /// Live models.dev refresh (layer 10). Distinct from provider `/v1/models`.
    ///
    /// **External enrichment, not a provider fact.** Models.dev is a public
    /// catalog nobody authenticates to, so these rows sit *below* the signed
    /// cloud layer and a fresh signed correction may replace their limits and
    /// prices. Carrying no endpoint fingerprint is deliberate: like the bundled
    /// seed this layer refreshes, the row describes a model, not an endpoint.
    ModelsDevLive { fetched_at: u64 },
    /// `config.toml` `[providers.*]` override (layer 30).
    ConfigOverride,
    /// A Codewhale correction ([`corrections`]) owns this price (set or
    /// withheld). Corrections rank above both Models.dev layers, bundled and
    /// live, and below signed cloud facts, which may still correct them. Used
    /// as a `cost_source`: a corrected row keeps its own `source`.
    CodewhaleBundled { revision: String },
    /// Signed field patch, below provider-owned rows and explicit overrides.
    ///
    /// This is the only online catalog authority in the client. A second one
    /// (`CodewhaleLive`, a layer-25 "signed CWC catalog" declared in #5783 and
    /// never given a fetcher) was removed once signed cloud facts shipped as
    /// the implemented signed layer: two signed catalogs on opposite sides of
    /// the provider roster is exactly the split this product exists not to be.
    CloudFacts {
        facts_version: u64,
        key_id: String,
        fetched_at: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        valid_until: Option<u64>,
    },
}

/// One catalog-layer offering row.
///
/// This carries the routing identity (provider + wire id + optional canonical
/// model + endpoint) plus the offering-owned Models.dev facts CodeWhale wants to
/// preserve (family, limits, cost, reasoning support/options). It is a superset
/// of [`ProviderModelOffering`]; use [`CatalogOffering::to_offering`] to project
/// the minimal routing identity the resolver consumes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CatalogOffering {
    /// Provider id serving this offering.
    pub provider: String,
    /// Provider-owned wire id sent on the request (verbatim).
    pub wire_model_id: String,
    /// Canonical model identity, only when an explicit join exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_model: Option<String>,
    /// Endpoint key the offering is served on (e.g. `chat`).
    pub endpoint_key: String,
    /// Whether this is the provider's default offering.
    #[serde(default)]
    pub default_for_provider: bool,
    /// Model family/series as exposed for this offering (e.g. `glm`, `deepseek`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "type")]
    pub model_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_weights: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interleaved: Option<ModelsDevInterleaved>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<ModelsDevModelTransport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_api: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_npm: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub experimental: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub benchmarks: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fact_sources: BTreeMap<String, CatalogSource>,
    /// Token limits for this offering, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<ModelsDevLimit>,
    /// Provider-scoped pricing, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelsDevCost>,
    /// Price authority stays separate when a layer changes only capabilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<CatalogSource>,
    /// Who stated [`Self::modalities`], when a higher layer re-sourced the row
    /// without restating them (a signed patch, a correction, or a provider
    /// roster enriched from the seed). `None` means [`Self::source`] did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities_source: Option<CatalogSource>,
    /// Input/output modalities for this offering, when known. Carried as the
    /// raw Models.dev shape so a factual `text` vs `multimodal` label can be
    /// derived without guessing; `None` means the layer did not state it (an
    /// unknown, not "text-only").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modalities: Option<ModelsDevModalities>,
    /// Whether this provider offering accepts attachments, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment: Option<bool>,
    /// Whether this offering supports reasoning, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<bool>,
    /// Whether tool calling is supported, when known (#4115).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<bool>,
    /// Whether structured output is supported, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<bool>,
    /// Provider-scoped reasoning controls / accepted effort metadata. Kept as
    /// raw JSON so the same model family served through different gateways can
    /// expose different effort vocabularies without lossy collapsing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasoning_options: Vec<Value>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reasoning_options_present: bool,
    /// Where this row came from.
    pub source: CatalogSource,
}

impl CatalogOffering {
    #[must_use]
    pub fn fact_source(&self, field: &str) -> &CatalogSource {
        match field {
            "cost" => self.pricing_source(),
            "modalities" => self.modalities_source(),
            _ => self.fact_sources.get(field).unwrap_or(&self.source),
        }
    }

    pub fn retain_fact_sources(&mut self) {
        macro_rules! retain {
            ($field:ident) => {
                if self.$field.is_some() {
                    self.fact_sources
                        .entry(stringify!($field).to_string())
                        .or_insert_with(|| self.source.clone());
                }
            };
        }
        retain!(canonical_model);
        retain!(family);
        retain!(name);
        retain!(description);
        retain!(model_type);
        retain!(knowledge);
        retain!(release_date);
        retain!(last_updated);
        retain!(temperature);
        retain!(open_weights);
        retain!(interleaved);
        retain!(transport);
        retain!(provider_api);
        retain!(provider_npm);
        retain!(status);
        retain!(experimental);
        retain!(license);
        retain!(links);
        retain!(weights);
        retain!(benchmarks);
        retain!(attachment);
        retain!(reasoning);
        retain!(tool_call);
        retain!(structured_output);
        if let Some(limit) = &self.limit {
            for (field, value) in [
                ("limit.context", limit.context),
                ("limit.input", limit.input),
                ("limit.output", limit.output),
            ] {
                if value.is_some() {
                    self.fact_sources
                        .entry(field.to_string())
                        .or_insert_with(|| self.source.clone());
                }
            }
        }
        if self.reasoning_options_present || !self.reasoning_options.is_empty() {
            self.fact_sources
                .entry("reasoning_options".to_string())
                .or_insert_with(|| self.source.clone());
        }
    }

    pub fn complete_from(&mut self, lower: &Self, base_url: Option<&str>) -> bool {
        let normalize = |id: &str| {
            crate::descriptors::compatibility_for_id(id)
                .or_else(|| crate::descriptors::compatibility_for_selector(id))
                .map(|provider| provider.catalog_id.to_string())
                .unwrap_or_else(|| id.to_string())
        };
        if normalize(&self.provider) != normalize(&lower.provider)
            || self.wire_model_id != lower.wire_model_id
            || self.endpoint_key != lower.endpoint_key
            || self
                .canonical_model
                .as_ref()
                .zip(lower.canonical_model.as_ref())
                .is_some_and(|(current, prior)| current != prior)
            || matches!(
                self.source,
                CatalogSource::Bundled | CatalogSource::ModelsDevLive { .. }
            )
        {
            return false;
        }
        let published_base = lower
            .transport
            .as_ref()
            .and_then(|transport| transport.api.as_deref())
            .or(lower.provider_api.as_deref())
            .or_else(|| {
                crate::ProviderKind::parse(&lower.provider)
                    .filter(|provider| *provider != crate::ProviderKind::Custom)
                    .map(|provider| provider.provider().default_base_url())
            });
        let endpoint_facts = match (&self.source, &lower.source) {
            (
                CatalogSource::Live {
                    base_url_fingerprint: current,
                    ..
                },
                CatalogSource::Live {
                    base_url_fingerprint: prior,
                    ..
                },
            ) if current == prior
                && base_url.is_none_or(|url| base_url_fingerprint(url) == *current) =>
            {
                true
            }
            (
                CatalogSource::Live {
                    base_url_fingerprint: current,
                    ..
                },
                CatalogSource::Bundled
                | CatalogSource::ModelsDevLive { .. }
                | CatalogSource::CloudFacts { .. }
                | CatalogSource::CodewhaleBundled { .. },
            ) if base_url.is_none_or(|url| base_url_fingerprint(url) == *current) => {
                published_base.is_some_and(|url| base_url_fingerprint(url) == *current)
            }
            (
                CatalogSource::ConfigOverride | CatalogSource::UserOverride,
                CatalogSource::Bundled
                | CatalogSource::ModelsDevLive { .. }
                | CatalogSource::CloudFacts { .. }
                | CatalogSource::CodewhaleBundled { .. },
            ) if base_url
                .zip(published_base)
                .is_some_and(|(current, prior)| {
                    base_url_fingerprint(current) == base_url_fingerprint(prior)
                }) =>
            {
                true
            }
            _ => return false,
        };
        self.complete_facts(lower, endpoint_facts)
    }

    pub(crate) fn complete_facts_from(&mut self, lower: &Self) -> bool {
        self.complete_facts(lower, true)
    }

    fn complete_facts(&mut self, lower: &Self, endpoint_facts: bool) -> bool {
        let mut changed = false;
        macro_rules! fill {
            ($field:ident) => {
                if self.$field.is_none() && lower.$field.is_some() {
                    self.$field.clone_from(&lower.$field);
                    self.fact_sources.insert(
                        stringify!($field).to_string(),
                        lower.fact_source(stringify!($field)).clone(),
                    );
                    changed = true;
                }
            };
        }
        fill!(canonical_model);
        fill!(family);
        fill!(name);
        fill!(description);
        fill!(model_type);
        fill!(knowledge);
        fill!(release_date);
        fill!(last_updated);
        fill!(temperature);
        fill!(open_weights);
        fill!(interleaved);
        if endpoint_facts {
            fill!(transport);
            fill!(provider_api);
            fill!(provider_npm);
            fill!(status);
        }
        fill!(experimental);
        fill!(license);
        fill!(links);
        fill!(weights);
        fill!(benchmarks);
        fill!(attachment);
        fill!(reasoning);
        fill!(tool_call);
        fill!(structured_output);
        if let Some(lower_limit) = &lower.limit {
            let mut limit = self.limit.clone().unwrap_or_default();
            for (field, target, value) in [
                ("limit.context", &mut limit.context, lower_limit.context),
                ("limit.input", &mut limit.input, lower_limit.input),
                ("limit.output", &mut limit.output, lower_limit.output),
            ] {
                if target.is_none() && value.is_some() {
                    *target = value;
                    self.fact_sources
                        .insert(field.to_string(), lower.fact_source(field).clone());
                    changed = true;
                }
            }
            if self.limit.is_some() || limit != ModelsDevLimit::default() {
                self.limit = Some(limit);
            }
        }
        if endpoint_facts
            && self.cost.is_none()
            && self.cost_source.is_none()
            && (lower.cost.is_some() || lower.cost_source.is_some())
        {
            self.cost.clone_from(&lower.cost);
            self.cost_source = Some(lower.pricing_source().clone());
            changed = true;
        }
        if self.modalities.is_none() && lower.modalities.is_some() {
            self.modalities.clone_from(&lower.modalities);
            self.modalities_source = Some(lower.modalities_source().clone());
            changed = true;
        }
        if !self.reasoning_options_present
            && self.reasoning_options.is_empty()
            && (lower.reasoning_options_present || !lower.reasoning_options.is_empty())
            && self.reasoning != Some(false)
        {
            self.reasoning_options.clone_from(&lower.reasoning_options);
            self.reasoning_options_present = true;
            self.fact_sources.insert(
                "reasoning_options".to_string(),
                lower.fact_source("reasoning_options").clone(),
            );
            changed = true;
        }
        changed
    }

    #[must_use]
    pub fn pricing_source(&self) -> &CatalogSource {
        self.cost_source.as_ref().unwrap_or(&self.source)
    }

    /// The layer that stated this row's modalities.
    #[must_use]
    pub fn modalities_source(&self) -> &CatalogSource {
        self.modalities_source.as_ref().unwrap_or(&self.source)
    }

    /// The provider id as a route newtype.
    #[must_use]
    pub fn provider_id(&self) -> ProviderId {
        ProviderId::from(self.provider.clone())
    }

    /// The wire model id as a route newtype.
    #[must_use]
    pub fn wire_id(&self) -> WireModelId {
        WireModelId::from(self.wire_model_id.clone())
    }

    /// Project the minimal routing identity the resolver consumes.
    ///
    /// The catalog deliberately carries richer facts than routing needs; this
    /// drops most of them so `RouteResolver::from_offerings` stays the single
    /// seam. The route-facing pricing meter is the exception: it is projected
    /// here (where the offering's sourced `cost` is in scope) via
    /// [`crate::pricing::route_pricing_sku`] so a resolved candidate can carry
    /// honest pricing without the route layer ever seeing raw cost (#3085).
    #[must_use]
    pub fn to_offering(&self) -> ProviderModelOffering {
        ProviderModelOffering {
            provider: self.provider_id(),
            canonical_model: self.canonical_model.clone().map(ModelId::from),
            wire_model_id: self.wire_id(),
            endpoint_key: self.endpoint_key.clone(),
            default_for_provider: self.default_for_provider,
            limits: self
                .limit
                .as_ref()
                .map(RouteLimits::from)
                .unwrap_or_default(),
            capabilities: crate::route::RouteCapabilities {
                attachments: crate::route::CapabilityState::from_optional_bool(self.attachment),
                // The offline seed is stale by nature, so it may say an image
                // is accepted but never that it is refused: a wrong refusal
                // strips the user's images before sending, while a wrong
                // `Unknown` costs one rejected request that the turn loop
                // recovers from and reports (#6396).
                image_input: crate::models_dev::image_input_support_for(
                    self.modalities.as_ref(),
                    matches!(self.modalities_source(), CatalogSource::Bundled),
                ),
                reasoning: crate::route::CapabilityState::from_optional_bool(self.reasoning),
                native_tool_calls: crate::route::CapabilityState::from_optional_bool(
                    self.tool_call,
                ),
                structured_output: crate::route::CapabilityState::from_optional_bool(
                    self.structured_output,
                ),
                server_side_web_search: crate::route::documented_server_side_web_search(
                    &self.provider,
                    &self.wire_model_id,
                ),
                ..crate::route::RouteCapabilities::default()
            },
            pricing: crate::pricing::route_pricing_sku(self),
        }
    }

    /// Stable identity key for de-duplication and layer merging.
    fn merge_key(&self) -> (String, String) {
        (self.provider.clone(), self.wire_model_id.clone())
    }
}

/// Committed offline/stale Models.dev-shaped catalog snapshot (#3385 / #4188).
///
/// This is **not** a competing curated source of truth. Preferred metadata comes
/// from the live Models.dev catalog (#4187). The bundled asset is a compact
/// network-free projection of the Models.dev rows Codewhale ships, generated
/// by `scripts/catalog_models_dev.py seed render` from a reviewed spec and a
/// pinned lock (#6396), so [`crate::route::RouteResolver::new`] and pickers
/// still work offline or after a failed refresh. Deliberate holds (withheld
/// prices, clamped limits) are not in the asset: [`corrections`] applies them
/// to it and to live rows alike.
pub const BUNDLED_MODELS_DEV_JSON: &str = include_str!("../assets/models_dev.bundled.json");

/// Parse-once cache for the committed bundled Models.dev snapshot.
///
/// The bundled asset is compile-time constant (`include_str!`), so its parsed
/// form is immutable and safe to share process-wide. Before this cache, every
/// call site parsed the full snapshot independently — the client route path,
/// pickers, provider lake, and fleet identity each paid a full serde parse of
/// ~50KB on their own first use (perf-attributed during the 0.9.x perf
/// gauntlet: `ModelsDevCost` serde frames in startup profiles).
static BUNDLED_MODELS_DEV_CATALOG: OnceLock<ModelsDevCatalog> = OnceLock::new();

/// Parse the committed bundled Models.dev snapshot.
///
/// The first call parses; later calls return the shared parsed catalog.
///
/// # Panics
/// Panics only if the committed asset is not valid Models.dev JSON. The
/// `tests::bundled_asset_parses` guard makes that a build-time failure, so this
/// never panics in shipped builds.
#[must_use]
pub fn bundled_models_dev_catalog() -> &'static ModelsDevCatalog {
    BUNDLED_MODELS_DEV_CATALOG.get_or_init(|| {
        let catalog = ModelsDevCatalog::parse_json(BUNDLED_MODELS_DEV_JSON)
            .expect("committed bundled Models.dev asset must be valid JSON");
        catalog
            .reviewed
            .validate()
            .expect("committed reviewed catalog must be valid");
        catalog
    })
}

/// Bundled-layer [`CatalogOffering`] rows from the offline snapshot (#4188).
///
/// Lowest-precedence catalog layer: every text-chat row from
/// [`BUNDLED_MODELS_DEV_JSON`], tagged [`CatalogSource::Bundled`], with
/// Codewhale's [`corrections`] applied. Live Models.dev rows override these on
/// `(provider, wire_model_id)` when available.
#[must_use]
pub fn bundled_catalog_offerings() -> Vec<CatalogOffering> {
    let mut rows = bundled_offerings_from_models_dev(bundled_models_dev_catalog());
    corrections::bundled_corrections().apply_to(&mut rows);
    rows
}

/// Hydrate bundled [`CatalogOffering`] rows from a parsed Models.dev catalog.
///
/// Only text-chat offerings are emitted (TTS/audio-only rows stay in the parsed
/// catalog but are excluded from route candidates, matching
/// [`ModelsDevCatalog::provider_offerings`]). Each row is tagged
/// [`CatalogSource::Bundled`]. Provider rows link canonical models only through
/// an explicit `canonical_model_id` or legacy `base_model`. Namespaced entries in the canonical `models` map
/// fill missing offerings, retaining their map key as the canonical identity.
///
/// Provider-row ids are kept verbatim from the Models.dev payload (the
/// committed bundled asset already uses CodeWhale ids). Namespaced canonical
/// keys (`xiaomi/mimo-v2.6-pro`) name an upstream vendor, so their namespace is
/// normalized onto the CodeWhale provider id here too (#6396). Live refresh
/// also normalizes provider-row aliases via [`live_offerings_from_models_dev`].
#[must_use]
pub fn bundled_offerings_from_models_dev(catalog: &ModelsDevCatalog) -> Vec<CatalogOffering> {
    offerings_from_models_dev(catalog, CatalogSource::Bundled, false)
}

/// Hydrate live [`CatalogOffering`] rows from a fetched Models.dev catalog (#4187).
///
/// Same text-chat filter as [`bundled_offerings_from_models_dev`], but each row
/// is tagged [`CatalogSource::ModelsDevLive`] with the fetch timestamp, so a
/// refresh lands on layer 10: above the bundled seed it supersedes, below the
/// signed cloud layer that may correct it, and far below a provider roster.
/// Codewhale's [`corrections`] are applied here as they are to the seed, so a
/// refresh cannot undo one.
/// Provider keys are normalized onto CodeWhale [`crate::ProviderKind`] ids when
/// an alias match exists (`moonshotai` → `moonshot`, `togetherai` → `together`,
/// `zhipuai` → `zai`, …); unknown Models.dev providers keep their upstream id so
/// they stay discoverable without becoming executable routes.
///
/// These rows deliberately carry no base-URL fingerprint. This function used to
/// stamp [`CatalogSource::Live`] with the models.dev URL's fingerprint, which
/// made every enriched row claim to be a provider-owned roster fetched from an
/// endpoint nobody bills against: signed patches were skipped as "from a higher
/// layer", the price was labelled `ProviderLive` and then failed its endpoint
/// check, and route lookup dropped the row for the same mismatch. Models.dev is
/// a public catalog scoped to a model, exactly like the layer-0 seed.
#[must_use]
pub fn live_offerings_from_models_dev(
    catalog: &ModelsDevCatalog,
    fetched_at: u64,
) -> Vec<CatalogOffering> {
    let mut rows =
        offerings_from_models_dev(catalog, CatalogSource::ModelsDevLive { fetched_at }, true);
    corrections::bundled_corrections().apply_to(&mut rows);
    rows
}

fn offerings_from_models_dev(
    catalog: &ModelsDevCatalog,
    source: CatalogSource,
    normalize_provider_ids: bool,
) -> Vec<CatalogOffering> {
    let mut out = Vec::new();
    let mut provider_rows = BTreeSet::new();
    // Unknown upstream ids remain discoverable catalog rows, not routes.
    let normalized = |raw_id: &str| {
        crate::ProviderKind::parse(raw_id)
            .map(|kind| kind.as_str().to_string())
            .unwrap_or_else(|| raw_id.to_string())
    };
    let provider_id = |raw_id: &str| {
        if normalize_provider_ids {
            normalized(raw_id)
        } else {
            raw_id.to_string()
        }
    };
    for (provider_key, provider) in &catalog.providers {
        let raw_id = if provider.id.trim().is_empty() {
            provider_key.trim()
        } else {
            provider.id.trim()
        };
        if raw_id.is_empty() {
            continue;
        }
        let provider_id = provider_id(raw_id);
        // Gap-filling compares on the normalized identity, so a verbatim
        // bundled `moonshotai` row still shadows `moonshotai/<model>`.
        let route_id = normalized(raw_id);
        for (model_key, model) in &provider.models {
            let wire_model_id = if model.id.trim().is_empty() {
                model_key.trim()
            } else {
                model.id.trim()
            };
            if wire_model_id.is_empty() {
                continue;
            }
            provider_rows.insert((route_id.clone(), wire_model_id.to_string()));
            if !model.supports_text_chat() {
                continue;
            }
            // OpenCode Zen is model-aware: its catalog names each model's AI
            // SDK package, which is the wire (#6705). Every other provider's
            // endpoint key stays the Chat placeholder its fixed policy ignores.
            // A deprecated Zen row stays visible but is not a route: Zen no
            // longer serves it, so sending it would be a guaranteed upstream
            // failure instead of a local refusal that names the reason.
            let endpoint_key = if route_id != crate::ProviderKind::OpencodeZen.as_str() {
                "chat"
            } else if model.is_deprecated() {
                crate::route::OPENCODE_ZEN_DEPRECATED_ENDPOINT_KEY
            } else {
                crate::route::opencode_zen_endpoint_key_for_npm(
                    model
                        .provider
                        .as_ref()
                        .and_then(|transport| transport.npm.as_deref())
                        .or(provider.npm.as_deref()),
                )
            };
            out.push(CatalogOffering {
                provider: provider_id.clone(),
                wire_model_id: wire_model_id.to_string(),
                canonical_model: model.canonical_id().map(str::to_string),
                endpoint_key: endpoint_key.to_string(),
                default_for_provider: model.default_for_provider,
                family: model.family.clone(),
                name: model.name.clone(),
                description: model.description.clone(),
                model_type: model.model_type.clone(),
                knowledge: model.knowledge.clone(),
                release_date: model.release_date.clone(),
                last_updated: model.last_updated.clone(),
                temperature: model.temperature,
                open_weights: model.open_weights,
                interleaved: model.interleaved.clone(),
                transport: model.provider.clone(),
                provider_api: provider.api.clone(),
                provider_npm: provider.npm.clone(),
                status: model.status.clone(),
                experimental: model.experimental.clone(),
                limit: model.limit.clone(),
                cost: model.cost.clone(),
                modalities: model.modalities.clone(),
                attachment: model.attachment,
                reasoning: model.reasoning,
                tool_call: model.tool_call,
                structured_output: model.structured_output,
                reasoning_options: model.reasoning_options.clone().unwrap_or_default(),
                reasoning_options_present: model.reasoning_options.is_some(),
                source: source.clone(),
                cost_source: None,
                modalities_source: None,
                ..Default::default()
            });
        }
    }

    // Namespaced model facts fill gaps without overriding provider-owned rows,
    // including their non-chat exclusions. The namespace is always the
    // upstream vendor id (`xiaomi`, `moonshotai`), never a CodeWhale provider
    // id, so it is normalized in both modes: that is what lets an
    // upstream-shaped offline seed land on the same route as live refresh
    // (#6396). Bare keys cannot name a provider and are skipped.
    for (canonical_id, model) in &catalog.models {
        let Some((provider_key, wire_model_id)) = canonical_id.trim().split_once('/') else {
            continue;
        };
        let provider_key = provider_key.trim();
        let wire_model_id = wire_model_id.trim();
        if provider_key.is_empty() || wire_model_id.is_empty() || !model.supports_text_chat() {
            continue;
        }
        let provider = normalized(provider_key);
        // A canonical fact names no transport, and Zen's wire is per model.
        if provider == crate::ProviderKind::OpencodeZen.as_str() {
            continue;
        }
        if !provider_rows.insert((provider.clone(), wire_model_id.to_string())) {
            continue;
        }
        out.push(CatalogOffering {
            provider,
            wire_model_id: wire_model_id.to_string(),
            canonical_model: Some(canonical_id.trim().to_string()),
            endpoint_key: "chat".to_string(),
            family: model.family.clone(),
            name: model.name.clone(),
            description: model.description.clone(),
            model_type: model.model_type.clone(),
            knowledge: model.knowledge.clone(),
            release_date: model.release_date.clone(),
            last_updated: model.last_updated.clone(),
            temperature: model.temperature,
            open_weights: model.open_weights,
            license: model.license.clone(),
            links: model.links.clone(),
            weights: model.weights.clone(),
            benchmarks: model.benchmarks.clone(),
            limit: model.limit.clone(),
            modalities: model.modalities.clone(),
            attachment: model.attachment,
            reasoning: model.reasoning,
            tool_call: model.tool_call,
            structured_output: model.structured_output,
            source: source.clone(),
            ..Default::default()
        });
    }
    out
}

/// A provider's live `/models` refresh result, scoped to a base-URL fingerprint.
///
/// Returned as a delta rather than mutating any global model state directly, per
/// the #3385 architecture contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCatalogDelta {
    /// Provider this delta belongs to.
    pub provider: String,
    /// Fingerprint of the base URL the rows were fetched from.
    pub base_url_fingerprint: String,
    /// Unix seconds the rows were fetched at.
    pub fetched_at: u64,
    /// Live offering rows. Sources are normalized to `Live` on ingest.
    pub offerings: Vec<CatalogOffering>,
}

/// Why a provider live catalog refresh did not produce usable rows.
///
/// Every variant must leave previously cached / bundled / configured rows
/// available; a refresh failure is never fatal to model selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogRefreshError {
    /// 401 — auth missing or invalid.
    Unauthorized,
    /// 403 — auth present but not permitted.
    Forbidden,
    /// 404 — provider does not expose `/models` at this base URL.
    NotFound,
    /// 429 — rate limited.
    RateLimited,
    /// Response was not parseable as a model listing.
    InvalidResponse,
    /// Provider returned an empty model list.
    EmptyList,
    /// Transport / network failure.
    Network,
}

impl CatalogRefreshError {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::NotFound => "not found",
            Self::RateLimited => "rate limited",
            Self::InvalidResponse => "invalid response",
            Self::EmptyList => "empty list",
            Self::Network => "network error",
        }
    }
}

/// Freshness / health of a provider's cached live catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CatalogStatus {
    /// Cached rows are within their TTL.
    Fresh,
    /// Cached rows exist but are past their TTL.
    Stale { age_secs: u64 },
    /// The last refresh failed; any rows present are from an earlier success.
    Failed { reason: CatalogRefreshError },
    /// No refresh has been attempted for this provider + base URL.
    Unknown,
}

/// A secret-free cached provider catalog for one provider + base-URL fingerprint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedProviderCatalog {
    /// Provider id.
    pub provider: String,
    /// Base-URL fingerprint the rows were fetched from.
    pub base_url_fingerprint: String,
    /// Unix seconds of the last successful fetch (unchanged on failure).
    pub fetched_at: u64,
    /// Time-to-live, in seconds, after which rows are considered stale.
    pub ttl_secs: u64,
    /// Cached live offering rows (possibly empty after a failure with no prior).
    pub offerings: Vec<CatalogOffering>,
    /// Last known status of this entry.
    pub status: CatalogStatus,
}

impl CachedProviderCatalog {
    /// Age in seconds relative to `now_unix`, saturating at zero for clock skew.
    #[must_use]
    pub fn age_secs(&self, now_unix: u64) -> u64 {
        now_unix.saturating_sub(self.fetched_at)
    }

    /// Whether the cached rows are past their TTL at `now_unix`.
    ///
    /// A `ttl_secs` of zero means "always stale" (never serve as fresh).
    #[must_use]
    pub fn is_stale(&self, now_unix: u64) -> bool {
        self.age_secs(now_unix) >= self.ttl_secs
    }

    /// Whether this entry may contribute live offerings at `now_unix`.
    ///
    /// An entry is fresh only when it is within its TTL **and** its last
    /// recorded refresh succeeded. A `Failed` entry is never fresh even inside
    /// its TTL window — its rows survive a failed refresh for explicit fallback
    /// display via [`ProviderCatalogCache::get`], but they are not served as
    /// current live data.
    #[must_use]
    pub fn is_fresh(&self, now_unix: u64) -> bool {
        !self.is_stale(now_unix) && !matches!(self.status, CatalogStatus::Failed { .. })
    }
}

/// A secret-free store of cached provider catalogs, keyed by provider + base-URL
/// fingerprint.
///
/// Scoping rule (#3385): the SAME provider on DIFFERENT base URLs must not share
/// rows, and DIFFERENT providers on the same base URL must not share rows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProviderCatalogCache {
    /// Entries keyed by [`ProviderCatalogCache::cache_key`].
    #[serde(default)]
    pub entries: BTreeMap<String, CachedProviderCatalog>,
}

impl ProviderCatalogCache {
    /// Construct an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Compute the composite cache key for a provider + base-URL fingerprint.
    #[must_use]
    pub fn cache_key(provider: &str, base_url_fingerprint: &str) -> String {
        // Unit separator avoids ambiguity between provider and fingerprint.
        format!("{}\u{1f}{}", provider.trim(), base_url_fingerprint.trim())
    }

    /// Look up a cached entry by provider + base-URL fingerprint.
    #[must_use]
    pub fn get(
        &self,
        provider: &str,
        base_url_fingerprint: &str,
    ) -> Option<&CachedProviderCatalog> {
        self.entries
            .get(&Self::cache_key(provider, base_url_fingerprint))
    }

    /// Record a successful refresh, replacing any prior entry for this scope.
    ///
    /// Offering sources are normalized to [`CatalogSource::Live`] with the
    /// delta's fingerprint and `fetched_at`, so cached rows always carry honest
    /// provenance regardless of how the delta was assembled.
    pub fn record_success(&mut self, delta: ProviderCatalogDelta, ttl_secs: u64) {
        let ProviderCatalogDelta {
            provider,
            base_url_fingerprint,
            fetched_at,
            offerings,
        } = delta;
        let offerings = offerings
            .into_iter()
            .map(|mut row| {
                row.source = CatalogSource::Live {
                    base_url_fingerprint: base_url_fingerprint.clone(),
                    fetched_at,
                };
                row
            })
            .collect();
        let key = Self::cache_key(&provider, &base_url_fingerprint);
        self.entries.insert(
            key,
            CachedProviderCatalog {
                provider,
                base_url_fingerprint,
                fetched_at,
                ttl_secs,
                offerings,
                status: CatalogStatus::Fresh,
            },
        );
    }

    /// Record a refresh failure.
    ///
    /// Previously cached rows for this scope are preserved (so the UI can still
    /// offer them with a visible "stale/failed" status); only the status is
    /// updated. When no prior entry exists, an empty `Failed` entry is created so
    /// the failure is observable.
    pub fn record_failure(
        &mut self,
        provider: &str,
        base_url_fingerprint: &str,
        reason: CatalogRefreshError,
    ) {
        let key = Self::cache_key(provider, base_url_fingerprint);
        match self.entries.get_mut(&key) {
            Some(entry) => entry.status = CatalogStatus::Failed { reason },
            None => {
                self.entries.insert(
                    key,
                    CachedProviderCatalog {
                        provider: provider.trim().to_string(),
                        base_url_fingerprint: base_url_fingerprint.trim().to_string(),
                        fetched_at: 0,
                        ttl_secs: 0,
                        offerings: Vec::new(),
                        status: CatalogStatus::Failed { reason },
                    },
                );
            }
        }
    }

    /// The resolved status of an entry at `now_unix`.
    ///
    /// A `Fresh`-recorded entry that has since aged past its TTL reports
    /// `Stale`; `Failed`/`Unknown` are returned as stored.
    #[must_use]
    pub fn status(
        &self,
        provider: &str,
        base_url_fingerprint: &str,
        now_unix: u64,
    ) -> CatalogStatus {
        match self.get(provider, base_url_fingerprint) {
            None => CatalogStatus::Unknown,
            Some(entry) => match &entry.status {
                CatalogStatus::Failed { reason } => CatalogStatus::Failed { reason: *reason },
                CatalogStatus::Unknown => CatalogStatus::Unknown,
                CatalogStatus::Fresh | CatalogStatus::Stale { .. } => {
                    if entry.is_stale(now_unix) {
                        CatalogStatus::Stale {
                            age_secs: entry.age_secs(now_unix),
                        }
                    } else {
                        CatalogStatus::Fresh
                    }
                }
            },
        }
    }

    /// Fresh (within-TTL) live offerings for one provider + base URL at
    /// `now_unix`. Stale or failed entries contribute nothing here; callers fall
    /// back to bundled/configured rows and surface the status separately.
    #[must_use]
    pub fn fresh_offerings(
        &self,
        provider: &str,
        base_url_fingerprint: &str,
        now_unix: u64,
    ) -> Vec<CatalogOffering> {
        match self.get(provider, base_url_fingerprint) {
            Some(entry) if entry.is_fresh(now_unix) => entry.offerings.clone(),
            _ => Vec::new(),
        }
    }

    /// All fresh live offerings across every cached provider + base URL.
    #[must_use]
    pub fn all_fresh_offerings(&self, now_unix: u64) -> Vec<CatalogOffering> {
        self.entries
            .values()
            .filter(|entry| entry.is_fresh(now_unix))
            .flat_map(|entry| entry.offerings.clone())
            .collect()
    }

    /// Live offerings that pickers may still show: fresh rows plus stale / prior
    /// rows that survived a failed refresh (#4139).
    ///
    /// Unlike [`Self::all_fresh_offerings`], this keeps past-TTL and
    /// `Failed`-status entries as long as they still hold offering rows. Empty
    /// entries contribute nothing; callers fall back to the bundled snapshot.
    /// `now_unix` is accepted for API symmetry with the fresh helper (age chips
    /// live above this layer).
    #[must_use]
    pub fn all_visible_offerings(&self, _now_unix: u64) -> Vec<CatalogOffering> {
        self.entries
            .values()
            .filter(|entry| !entry.offerings.is_empty())
            .flat_map(|entry| entry.offerings.clone())
            .collect()
    }
}

/// A compiled, layer-merged catalog snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    /// Merged offerings, de-duplicated by (provider, wire id), in stable order.
    pub offerings: Vec<CatalogOffering>,
}

impl CatalogSnapshot {
    /// Project routing offerings for `RouteResolver::from_offerings`.
    #[must_use]
    pub fn to_offerings(&self) -> Vec<ProviderModelOffering> {
        self.offerings
            .iter()
            .map(CatalogOffering::to_offering)
            .collect()
    }

    /// All offerings for one provider id.
    #[must_use]
    pub fn offerings_for_provider(&self, provider: &str) -> Vec<&CatalogOffering> {
        self.offerings
            .iter()
            .filter(|row| row.provider == provider)
            .collect()
    }
}

/// Builds a [`CatalogSnapshot`] by merging layers in precedence order.
///
/// Last writer wins per `(provider, wire id)` field. Policy DENY is applied
/// after every layer and is never overridden:
///
/// ```text
///  0 bundled              committed models.dev-shaped snapshot
/// 10 live models.dev      models.dev refresh
/// 12 codewhale            bundled corrections, applied as rows 0 and 10
///                         are hydrated (see [`corrections`])
/// 15 cloud facts          verified field patches (default off)
/// 20 provider             per-provider /v1/models refresh
/// 30 config               config.toml [providers.*] overrides
/// 40 user                 user approved set
///    policy DENY          last, never overridden
/// ```
///
/// Layer 25 in `docs/CATALOG_REFRESH.md` — the Codewhale account roster — is
/// deliberately absent here: an account-scoped roster is entitlement, not a
/// public catalog layer, so it is enforced where the credential is known
/// (`provider_lake`'s endpoint-authoritative path) and never compiled into a
/// shared snapshot.
///
/// [`Self::with_live`] remains the combined live bucket so existing callers
/// keep working; prefer [`Self::with_models_dev_live`] / [`Self::with_provider_live`]
/// for the split.
#[derive(Debug, Clone, Default)]
pub struct CatalogCompiler {
    bundled: Vec<CatalogOffering>,
    models_dev_live: Vec<CatalogOffering>,
    cloud_facts: Option<(crate::cloud_facts::ScopedFacts, u64)>,
    provider_live: Vec<CatalogOffering>,
    config: Vec<CatalogOffering>,
    overrides: Vec<CatalogOffering>,
    policy: crate::route::CatalogPolicy,
}

impl CatalogCompiler {
    /// Start an empty compiler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add bundled (lowest-precedence) rows.
    #[must_use]
    pub fn with_bundled(mut self, rows: Vec<CatalogOffering>) -> Self {
        self.bundled.extend(rows);
        self
    }

    /// Seed bundled rows from a parsed Models.dev catalog.
    #[must_use]
    pub fn with_models_dev(mut self, catalog: &ModelsDevCatalog) -> Self {
        self.bundled
            .extend(bundled_offerings_from_models_dev(catalog));
        self
    }

    /// Add live models.dev refresh rows (layer 10).
    #[must_use]
    pub fn with_models_dev_live(mut self, rows: Vec<CatalogOffering>) -> Self {
        self.models_dev_live.extend(rows);
        self
    }

    /// Add live (combined models.dev + provider) rows.
    ///
    /// Prefer [`Self::with_models_dev_live`] / [`Self::with_provider_live`].
    /// Source ownership places each row on the corresponding side of the
    /// signed cloud layer; a legacy provider row never becomes a lower layer.
    #[must_use]
    pub fn with_live(mut self, rows: Vec<CatalogOffering>) -> Self {
        for row in rows {
            if matches!(row.source, CatalogSource::ModelsDevLive { .. }) {
                self.models_dev_live.push(row);
            } else {
                self.provider_live.push(row);
            }
        }
        self
    }

    /// Apply signed facts between generic catalogs and provider-owned rows.
    #[must_use]
    pub fn with_cloud_facts(
        mut self,
        facts: &crate::cloud_facts::ScopedFacts,
        fetched_at: u64,
    ) -> Self {
        self.cloud_facts = Some((facts.clone(), fetched_at));
        self
    }

    /// Add per-provider `/v1/models` refresh rows (layer 20).
    #[must_use]
    pub fn with_provider_live(mut self, rows: Vec<CatalogOffering>) -> Self {
        self.provider_live.extend(rows);
        self
    }

    /// Add `config.toml` `[providers.*]` override rows (layer 30).
    #[must_use]
    pub fn with_config(mut self, rows: Vec<CatalogOffering>) -> Self {
        self.config.extend(rows);
        self
    }

    /// Add user/custom override (highest catalog-layer precedence) rows.
    #[must_use]
    pub fn with_overrides(mut self, rows: Vec<CatalogOffering>) -> Self {
        self.overrides.extend(rows);
        self
    }

    /// Attach policy evaluated after every layer. DENY is never overridden.
    #[must_use]
    pub fn with_policy(mut self, policy: crate::route::CatalogPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Merge all layers into a deterministic snapshot, then apply policy DENY.
    #[must_use]
    pub fn compile(self) -> CatalogSnapshot {
        let mut merged: BTreeMap<(String, String), CatalogOffering> = BTreeMap::new();
        for row in self.bundled.into_iter().chain(self.models_dev_live) {
            merged.insert(row.merge_key(), row);
        }
        if let Some((facts, fetched_at)) = self.cloud_facts {
            crate::cloud_facts::catalog_patch::apply_model_patches(&mut merged, &facts, fetched_at);
        }
        for mut row in self
            .provider_live
            .into_iter()
            .chain(self.config)
            .chain(self.overrides)
        {
            if let Some(lower) = merged.get(&row.merge_key()) {
                let base_url = row.provider_api.clone();
                row.complete_from(lower, base_url.as_deref());
            }
            merged.insert(row.merge_key(), row);
        }
        let offerings = merged
            .into_values()
            .filter(|row| self.policy.allows(&row.provider, &row.wire_model_id))
            .collect();
        CatalogSnapshot { offerings }
    }
}

/// Normalize a base URL and fingerprint it for cache scoping.
///
/// Normalization folds case in the scheme/host, trims trailing slashes, and
/// drops a default-port suffix, so cosmetically different spellings of the same
/// endpoint share a cache scope while genuinely different endpoints do not. The
/// fingerprint is a SHA-256 digest. Secret-bearing URLs are mapped to one
/// constant redacted input before hashing, so userinfo, query credentials, and
/// fragments never enter the digest function at all.
#[must_use]
pub fn base_url_fingerprint(base_url: &str) -> String {
    use sha2::Digest as _;

    let normalized = secret_free_fingerprint_input(base_url);
    let digest = sha2::Sha256::digest(normalized.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

/// The conventional provider-table id for the Baseten known-good host.
///
/// Baseten is an ordinary named `[providers.baseten]` row (#6289); this
/// string is the identity the live-catalog path serves, not a wire-fact
/// switch — every runtime behavior keys off [`endpoint_is_baseten`].
pub const BASETEN_PROVIDER_ID: &str = "baseten";

/// Baseten Model APIs endpoint: the one hosted Chat Completions host whose
/// wire facts differ from the generic shape (#6289).
///
/// Baseten's `/models` uses its own response schema and returns an
/// account-scoped roster, so response parsing, account-scoped cache
/// isolation, and the reviewed per-token billing contract all key off this
/// endpoint. Recognition is by endpoint fingerprint — never by what the user
/// named the `[providers.<name>]` table — so renames and aliases cannot
/// change wire handling.
pub const BASETEN_BASE_URL: &str = "https://inference.baseten.co/v1";

/// The documented default model for the Baseten known-good host
/// (`docs/PROVIDERS.md`). The live-catalog offering builder marks a
/// discovered row with this wire id as the provider default.
pub const BASETEN_DEFAULT_MODEL: &str = "deepseek-ai/DeepSeek-V4-Pro";

/// Whether `base_url` is Baseten's Model APIs endpoint.
///
/// Compares fingerprints, not spellings, so a trailing slash or case
/// difference in a user-configured URL still recognizes the host.
#[must_use]
pub fn endpoint_is_baseten(base_url: &str) -> bool {
    base_url_fingerprint(base_url) == base_url_fingerprint(BASETEN_BASE_URL)
}

fn secret_free_fingerprint_input(base_url: &str) -> String {
    const REDACTED: &str = "invalid-or-secret-bearing-url";
    let trimmed = base_url.trim();
    if let Some((scheme, rest)) = trimmed.split_once("://") {
        let scheme = scheme.to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https") {
            return REDACTED.to_string();
        }
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let authority_with_userinfo = &rest[..authority_end];
        if authority_with_userinfo.contains(['?', '#']) {
            return REDACTED.to_string();
        }
        let authority = authority_with_userinfo
            .rsplit_once('@')
            .map_or(authority_with_userinfo, |(_, host)| host);
        if authority.is_empty() {
            return REDACTED.to_string();
        }
        let path = rest[authority_end..]
            .split(['?', '#'])
            .next()
            .unwrap_or_default();
        return normalize_base_url(&format!("{scheme}://{authority}{path}"));
    }
    // Scheme-less input still has an authority, and it can still carry
    // `user:pass@` userinfo. Strip it exactly as the scheme branch does, so the
    // digest input never contains a credential.
    let without_query = trimmed.split(['?', '#']).next().unwrap_or_default();
    let authority_end = without_query.find('/').unwrap_or(without_query.len());
    let authority = &without_query[..authority_end];
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    if authority.is_empty() {
        return REDACTED.to_string();
    }
    normalize_base_url(&format!("{authority}{}", &without_query[authority_end..]))
}

fn normalize_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    // Lowercase only the scheme://host authority; leave the path case-sensitive.
    if let Some(idx) = trimmed.find("://") {
        let (scheme, rest) = trimmed.split_at(idx);
        let scheme = scheme.to_ascii_lowercase();
        let rest = &rest[3..];
        let (authority, path) = match rest.find('/') {
            Some(p) => (&rest[..p], &rest[p..]),
            None => (rest, ""),
        };
        let authority = authority.to_ascii_lowercase();
        // Strip only the scheme's own default port, so a non-default pairing
        // such as `http://host:443` stays distinct from `http://host`.
        let default_port = match scheme.as_str() {
            "https" => Some(":443"),
            "http" => Some(":80"),
            _ => None,
        };
        let authority = default_port
            .and_then(|port| authority.strip_suffix(port))
            .unwrap_or(&authority);
        format!("{scheme}://{authority}{path}")
    } else {
        trimmed.to_ascii_lowercase()
    }
}

/// Current unix time in seconds, for callers assembling deltas / cache entries.
///
/// Pure cache logic takes `now_unix` explicitly so it stays deterministic in
/// tests; this helper is the one place that reads the wall clock.
#[must_use]
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;

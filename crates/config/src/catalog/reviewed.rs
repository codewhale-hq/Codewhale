//! Pure reviewed model facts in the existing catalog owner.
//!
//! The seed renderer copies the committed `catalog_corrections.json.reviewed`
//! data into the bundled Models.dev asset. Live/config/account rows never enter
//! this projection. An intrinsic fact or alias is metadata, not route authority;
//! executable requests still pass through the existing scoped RouteResolver.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::models_dev::{ModelsDevCatalog, ModelsDevModel};
use crate::route::{
    CapabilityState, ModelId, ProviderId, ProviderModelOffering, RouteCapabilities, RouteLimits,
    WireModelId,
};

/// Coarse model family for presentation only; never selects a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelFamily {
    DeepSeek,
    Anthropic,
    OpenAi,
    OpenAiCodex,
    Moonshot,
    Zai,
    Minimax,
    Stepfun,
    Qwen,
    Arcee,
    Together,
    XiaomiMimo,
    Meta,
    Xai,
    Mistral,
    Google,
    Other,
}

/// Reviewed intrinsic facts. Missing fields remain unknown. The generation
/// default is separate from the route maximum (notably Kimi K3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntrinsicModel {
    #[serde(default)]
    pub canonical_id: Option<String>,
    #[serde(default)]
    pub context_window: Option<u32>,
    #[serde(default)]
    pub max_output: Option<u32>,
    #[serde(default)]
    pub generation_default: Option<u32>,
    #[serde(default)]
    pub reasoning: Option<bool>,
    #[serde(default)]
    pub family: Option<ModelFamily>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub modalities: Vec<String>,
    #[serde(default)]
    pub supported_parameters: Vec<String>,
    #[serde(default)]
    pub capabilities: RouteCapabilities,
    #[serde(default)]
    pub architecture: Option<serde_json::Value>,
    #[serde(default)]
    pub latency: Option<serde_json::Value>,
    pub source: String,
}

/// Exact provider-scoped selector aliases. Their order preserves the released
/// first-match contract. These rows do not assert live account availability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelSelection {
    pub provider: String,
    pub id: String,
    pub aliases: Vec<String>,
    pub supports_tools: bool,
    pub supports_reasoning: bool,
}

/// Pure transport facts required by model-aware providers, without a second
/// routing implementation or pricing authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedTransport {
    pub provider: String,
    pub id: String,
    pub endpoint_key: String,
    #[serde(default)]
    pub canonical_model: Option<String>,
    #[serde(default)]
    pub default_for_provider: bool,
    #[serde(default)]
    pub limits: RouteLimits,
    #[serde(default)]
    pub capabilities: RouteCapabilities,
    pub source: String,
}

impl ReviewedTransport {
    #[must_use]
    pub fn to_offering(&self) -> ProviderModelOffering {
        ProviderModelOffering {
            provider: ProviderId::from(self.provider.clone()),
            canonical_model: self.canonical_model.clone().map(ModelId::from),
            wire_model_id: WireModelId::from(self.id.clone()),
            endpoint_key: self.endpoint_key.clone(),
            default_for_provider: self.default_for_provider,
            limits: self.limits,
            capabilities: self.capabilities,
            pricing: crate::route::PricingSku::UnknownOrStale,
        }
    }
}

/// Reviewed reference observations used by metadata-only presentation. They
/// never substitute for scoped live prices or authenticated billing facts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferencePrice {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: Option<f64>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicModel {
    pub id: String,
    pub label: Option<String>,
    pub added_at: Option<String>,
    pub aliases: Vec<String>,
}

/// A retained compile-time contract name projected from an intrinsic field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NumericRef {
    pub model: String,
    pub field: String,
}

/// Supplement to Models.dev's intrinsic/offering schema. The original tables
/// are retired into this one reviewed data owner, not reloaded from a global
/// provider cache. Runtime catalogs retain their existing scope and precedence.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewedCatalog {
    #[serde(default)]
    pub revision: String,
    #[serde(default)]
    pub intrinsic: BTreeMap<String, IntrinsicModel>,
    #[serde(default)]
    pub official_deepseek_aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub openai_reasoning_ids: BTreeMap<String, String>,
    #[serde(default)]
    pub snapshot_prefixes: BTreeMap<String, String>,
    #[serde(default)]
    pub selections: Vec<ModelSelection>,
    #[serde(default)]
    pub public_models: Vec<PublicModel>,
    #[serde(default)]
    pub transports: Vec<ReviewedTransport>,
    #[serde(default)]
    pub constants: BTreeMap<String, String>,
    #[serde(default)]
    pub numeric_refs: BTreeMap<String, NumericRef>,
    #[serde(default)]
    pub groups: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub compatibility_aliases: BTreeMap<String, BTreeMap<String, String>>,
    #[serde(default)]
    pub completion_rosters: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub go_aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub search_models: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub route_model_sets: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    pub reference_prices: BTreeMap<String, ReferencePrice>,
    #[serde(default)]
    pub reference_price_policies: BTreeMap<String, String>,
    #[serde(default)]
    pub source_receipts: BTreeMap<String, String>,
}

impl ReviewedCatalog {
    /// Validate authored source data before it can become a bundled projection.
    /// Empty is allowed only for external Models.dev documents with no supplement.
    pub fn validate(&self) -> Result<(), String> {
        if self.revision.is_empty() {
            if self == &Self::default() {
                return Ok(());
            }
            return Err("reviewed catalog revision missing".into());
        }
        let identifier = |s: &str| {
            !s.is_empty() && s.len() <= 512 && s.trim() == s && !s.chars().any(char::is_control)
        };
        for (id, row) in &self.intrinsic {
            if !identifier(id)
                || row.source.trim().is_empty()
                || row.context_window == Some(0)
                || row.max_output == Some(0)
                || row.generation_default == Some(0)
            {
                return Err(format!("malformed intrinsic model {id}"));
            }
        }
        for row in &self.selections {
            // Authored rows retain exact legacy table identities; the picker
            // parser deliberately collapses those into vendor-primary names.
            let provider = crate::ProviderKind::parse_config_identity(&row.provider);
            if !identifier(&row.id)
                || provider.is_none_or(|p| {
                    p.as_str() != row.provider || p == crate::ProviderKind::Antigravity
                })
                || row.aliases.iter().any(|s| !identifier(s))
            {
                return Err(format!(
                    "malformed provider selector {}/{}",
                    row.provider, row.id
                ));
            }
        }
        let mut routes = BTreeSet::new();
        for row in &self.transports {
            if !identifier(&row.id)
                || row.source.trim().is_empty()
                || !matches!(row.endpoint_key.as_str(), "chat" | "messages" | "responses")
                || !routes.insert((&row.provider, &row.id))
                || row.limits.context_tokens == Some(0)
                || row.limits.output_tokens == Some(0)
                || crate::ProviderKind::parse_config_identity(&row.provider).is_none_or(|p| {
                    p.as_str() != row.provider || p == crate::ProviderKind::Antigravity
                })
            {
                return Err(format!("malformed transport {}/{}", row.provider, row.id));
            }
        }
        for (id, row) in &self.reference_prices {
            if !identifier(id)
                || row.source.trim().is_empty()
                || [
                    Some(row.input),
                    Some(row.output),
                    Some(row.cache_read),
                    row.cache_write,
                ]
                .into_iter()
                .flatten()
                .any(|v| !v.is_finite() || v < 0.0)
            {
                return Err(format!("malformed reference price {id}"));
            }
        }
        for (alias, target) in self
            .official_deepseek_aliases
            .iter()
            .chain(&self.go_aliases)
        {
            if !identifier(alias) || !identifier(target) {
                return Err("malformed exact alias".into());
            }
        }
        for (prefix, target) in &self.snapshot_prefixes {
            if !identifier(prefix) || !self.intrinsic.contains_key(target) {
                return Err("missing snapshot target".into());
            }
        }
        for reference in self.numeric_refs.values() {
            let row = self
                .intrinsic
                .get(&reference.model)
                .ok_or("missing numeric model")?;
            let value = match reference.field.as_str() {
                "context_window" => row.context_window,
                "max_output" => row.max_output,
                "generation_default" => row.generation_default,
                _ => return Err("unknown numeric model field".into()),
            };
            if value.is_none_or(|value| value == 0) {
                return Err("missing numeric model fact".into());
            }
        }
        for row in &self.public_models {
            if !identifier(&row.id)
                || !self.intrinsic.contains_key(&row.id.to_ascii_lowercase())
                || row.label.as_deref().is_some_and(|label| !identifier(label))
                || row.aliases.iter().any(|alias| !identifier(alias))
                || row.added_at.as_deref().is_some_and(|date| {
                    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err()
                })
            {
                return Err(format!("missing public model {}", row.id));
            }
        }
        for (group, aliases) in &self.compatibility_aliases {
            if !identifier(group)
                || aliases
                    .iter()
                    .any(|(alias, target)| !identifier(alias) || !identifier(target))
            {
                return Err("malformed compatibility selector".into());
            }
        }
        for (provider, entries) in &self.completion_rosters {
            if provider != crate::descriptors::LEGACY_DEEPSEEK_CN.id
                && crate::ProviderKind::parse_config_identity(provider)
                    .is_none_or(|kind| kind.as_str() != provider)
                || entries.iter().any(|entry| !identifier(entry))
                || matches!(provider.as_str(), "antigravity" | "custom") && !entries.is_empty()
            {
                return Err(format!("malformed completion roster {provider}"));
            }
        }
        Ok(())
    }
}

/// The same immutable parsed bundled catalog used by config's existing owner.
#[must_use]
pub fn bundled_reviewed() -> &'static ReviewedCatalog {
    &super::bundled_models_dev_catalog().reviewed
}

/// Exact intrinsic lookup. Canonical facts win over provider enrichment; a
/// provider-scoped row is eligible only through an explicit, conflict-free join.
/// Never examine the process's live/custom/account catalog here.
#[must_use]
pub fn intrinsic_model(model: &str) -> Option<IntrinsicModel> {
    intrinsic_model_in(super::bundled_models_dev_catalog(), model)
}

#[must_use]
pub fn intrinsic_model_in(catalog: &ModelsDevCatalog, model: &str) -> Option<IntrinsicModel> {
    let id = model.trim().to_ascii_lowercase();
    let reviewed = catalog.reviewed.intrinsic.get(&id);
    let canonical = catalog
        .models
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(&id))
        .map(|(_, row)| row);
    let mut result = reviewed.cloned().or_else(|| canonical.map(from_canonical));
    if let Some(canonical) = canonical {
        let canonical = from_canonical(canonical);
        if let Some(row) = &mut result {
            fill_missing(row, &canonical);
        }
    }
    // A shared wire spelling does not establish a canonical model. Only
    // explicit joins can supply missing intrinsic facts.
    let joined = catalog
        .providers
        .values()
        .flat_map(|provider| provider.models.iter())
        .filter(|(_, row)| {
            row.canonical_id()
                .is_some_and(|base| base.eq_ignore_ascii_case(&id))
        })
        .collect::<Vec<_>>();
    let joins = joined
        .iter()
        .filter_map(|(_, row)| row.canonical_id())
        .map(str::to_ascii_lowercase)
        .collect::<BTreeSet<_>>();
    if joins.len() == 1 {
        // Enrich the named canonical model only. A provider-owned wire id is
        // resolved through its scoped offering, never this model-only lookup.
        if let Some(base) = joins.first().and_then(|base| {
            catalog
                .models
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(base))
                .map(|(_, row)| row)
        }) {
            let canonical = from_canonical(base);
            if let Some(row) = &mut result {
                fill_missing(row, &canonical);
            } else {
                result = Some(canonical);
            }
        }
        let consensus = IntrinsicModel {
            context_window: consensus(joined.iter().filter_map(|(_, row)| {
                row.limit
                    .as_ref()
                    .and_then(|v| v.context)
                    .and_then(|v| u32::try_from(v).ok())
            })),
            max_output: consensus(joined.iter().filter_map(|(_, row)| {
                row.limit
                    .as_ref()
                    .and_then(|v| v.output)
                    .and_then(|v| u32::try_from(v).ok())
            })),
            reasoning: consensus(joined.iter().filter_map(|(_, row)| row.reasoning)),
            source: "conflict-free explicitly joined bundled offerings".into(),
            ..IntrinsicModel::default()
        };
        if let Some(row) = &mut result {
            fill_missing(row, &consensus);
        } else if consensus.context_window.is_some()
            || consensus.max_output.is_some()
            || consensus.reasoning.is_some()
        {
            result = Some(consensus);
        }
    }
    result
}

fn consensus<T: Copy + PartialEq>(values: impl IntoIterator<Item = T>) -> Option<T> {
    let mut values = values.into_iter();
    let first = values.next()?;
    values.all(|value| value == first).then_some(first)
}

fn from_canonical(row: &ModelsDevModel) -> IntrinsicModel {
    IntrinsicModel {
        canonical_id: (!row.id.is_empty()).then(|| row.id.clone()),
        context_window: row
            .limit
            .as_ref()
            .and_then(|v| v.context)
            .and_then(|v| u32::try_from(v).ok()),
        max_output: row
            .limit
            .as_ref()
            .and_then(|v| v.output)
            .and_then(|v| u32::try_from(v).ok()),
        reasoning: row.reasoning,
        display_name: row.name.clone(),
        modalities: row
            .modalities
            .as_ref()
            .map(|v| v.input.clone())
            .unwrap_or_default(),
        capabilities: RouteCapabilities {
            reasoning: CapabilityState::from_optional_bool(row.reasoning),
            native_tool_calls: CapabilityState::from_optional_bool(row.tool_call),
            structured_output: CapabilityState::from_optional_bool(row.structured_output),
            attachments: CapabilityState::from_optional_bool(row.attachment),
            image_input: crate::models_dev::image_input_support(row.modalities.as_ref()),
            ..RouteCapabilities::default()
        },
        source: "bundled canonical Models.dev row".into(),
        ..IntrinsicModel::default()
    }
}

fn fill_missing(row: &mut IntrinsicModel, other: &IntrinsicModel) {
    row.context_window = row.context_window.or(other.context_window);
    row.max_output = row.max_output.or(other.max_output);
    row.reasoning = row.reasoning.or(other.reasoning);
    row.family = row.family.or(other.family);
    if row.display_name.is_none() {
        row.display_name.clone_from(&other.display_name);
    }
}

/// Exact public bundled identifiers only. Live/private/configured model IDs
/// can never enlarge a telemetry/UI public-label allowlist.
#[must_use]
pub fn public_model_identifier(model: &str) -> bool {
    let catalog = super::bundled_models_dev_catalog();
    catalog.reviewed.intrinsic.contains_key(model)
        || catalog.models.contains_key(model)
        || catalog.providers.values().any(|provider| {
            provider
                .models
                .iter()
                .any(|(id, row)| id == model || row.id == model)
        })
}

#[must_use]
pub fn official_deepseek_model_id(model: &str) -> Option<&'static str> {
    bundled_reviewed()
        .official_deepseek_aliases
        .get(&model.trim().to_ascii_lowercase())
        .map(String::as_str)
}

/// Exact data-owned compatibility selector. Provider/endpoint policy remains
/// at the caller; this projection cannot choose or authorize a route.
#[must_use]
pub fn compatibility_alias(group: &str, model: &str) -> Option<&'static str> {
    constants::compatibility_alias(group, model)
}

#[must_use]
pub fn route_model_set_contains(set: &str, model: &str) -> bool {
    bundled_reviewed()
        .route_model_sets
        .get(set)
        .is_some_and(|ids| ids.iter().any(|id| id.eq_ignore_ascii_case(model.trim())))
}

#[must_use]
pub fn reviewed_transport(provider: &str, model: &str) -> Option<&'static ReviewedTransport> {
    bundled_reviewed()
        .transports
        .iter()
        .find(|row| row.provider == provider && row.id == model)
}

/// Compile-time compatibility names are generated from the same JSON owner.
pub mod constants {
    include!(concat!(env!("OUT_DIR"), "/catalog_constants.rs"));
}

/// Actual source fetch time of the existing generated seed, not an unrelated
/// former bundled asset date. Callers evaluate TTL against their current clock.
#[must_use]
pub fn bundled_source_fetched_at() -> Option<u64> {
    let source = super::bundled_models_dev_catalog()
        .meta
        .get("upstream")?
        .as_str()?;
    let (_, source) = source.split_once(" fetched ")?;
    let time = source.split_whitespace().next()?;
    u64::try_from(chrono::DateTime::parse_from_rfc3339(time).ok()?.timestamp()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(value: serde_json::Value) -> ModelsDevCatalog {
        ModelsDevCatalog::parse_json(&value.to_string()).expect("fixture")
    }

    #[test]
    fn bundled_catalog_resolves_the_bare_deepseek_v41_flash_spelling() {
        // A custom `openai-compatible` endpoint whose `model` omits the vendor
        // prefix (`model = "deepseek-v4.1-flash"`) is the shape that used to
        // fall through to the static 128K window. This pins the curated row so
        // the fallback cannot silently regress.
        let data = super::super::bundled_models_dev_catalog();
        let found = intrinsic_model_in(data, "deepseek-v4.1-flash")
            .expect("bare spelling resolves in the bundled catalogue");
        assert_eq!(found.context_window, Some(1_000_000));
        assert_eq!(found.max_output, Some(384_000));
        // The `deepseek/`-namespaced spelling resolves to the same window and
        // ceiling. This does not make every spelling agree: the `deepseek-ai/`
        // twin is curated at 1048576 with no output ceiling, and reconciling
        // those two rows is a separate data question.
        let namespaced = intrinsic_model_in(data, "deepseek/deepseek-v4.1-flash")
            .expect("namespaced spelling resolves");
        assert_eq!(namespaced.context_window, found.context_window);
        assert_eq!(namespaced.max_output, found.max_output);
    }

    #[test]
    fn identical_wire_names_do_not_merge_provider_private_facts() {
        let data = catalog(serde_json::json!({ "providers": {
            "one": { "models": { "private-name": { "id": "private-name", "reasoning": true, "limit": { "context": 900000 } } } },
            "two": { "models": { "private-name": { "id": "private-name", "reasoning": false, "limit": { "context": 32000 } } } }
        }}));
        assert!(intrinsic_model_in(&data, "private-name").is_none());
        assert_eq!(
            data.provider_offering("one", "private-name")
                .unwrap()
                .limits
                .context_tokens,
            Some(900000)
        );
        assert_eq!(
            data.provider_offering("two", "private-name")
                .unwrap()
                .limits
                .context_tokens,
            Some(32000)
        );
        assert!(!public_model_identifier("private-name"));
    }

    #[test]
    fn proven_join_preserves_canonical_facts_and_wire_lookup_stays_scoped() {
        let mut fixture = serde_json::json!({
            "models": { "canonical": { "id": "canonical", "reasoning": false, "limit": { "context": 128000 } } },
            "providers": { "one": { "models": { "wire": { "id": "wire", "base_model": "canonical", "reasoning": true, "limit": { "context": 900000 } } } } }
        });
        let data = catalog(fixture.clone());
        assert!(intrinsic_model_in(&data, "wire").is_none());
        assert_eq!(
            data.provider_offering("one", "wire")
                .expect("exact provider-owned offering")
                .limits
                .context_tokens,
            Some(900000)
        );
        let joined = intrinsic_model_in(&data, "canonical").unwrap();
        assert_eq!(joined.canonical_id.as_deref(), Some("canonical"));
        assert_eq!(joined.context_window, Some(128000));
        assert_eq!(joined.reasoning, Some(false));
        fixture["providers"]["two"] = serde_json::json!({ "models": { "wire": { "id": "wire", "base_model": "different", "reasoning": true } } });
        assert!(intrinsic_model_in(&catalog(fixture), "wire").is_none());
    }

    #[test]
    fn reviewed_selection_and_transport_preserve_separate_scopes() {
        let reviewed = bundled_reviewed();
        reviewed.validate().expect("committed data");
        assert_eq!(reviewed.selections.len(), 187);
        assert_eq!(reviewed.transports.len(), 122);
        let go = reviewed_transport("opencode-go", "gpt-5.6-luna").unwrap();
        assert_eq!(go.endpoint_key, "responses");
        assert_eq!(
            go.limits,
            RouteLimits::default(),
            "Go metadata is enriched by existing resolver, not borrowed from another account"
        );
        assert_eq!(
            reviewed_transport("opencode-zen", "claude-opus-5")
                .unwrap()
                .endpoint_key,
            "messages"
        );
        assert!(reviewed_transport("custom", "gpt-5.6").is_none());
        assert!(constants::completion_names("custom").is_empty());
        assert!(constants::completion_names("antigravity").is_empty());
        assert_eq!(
            compatibility_alias("canonical_zai_model_id", "glm-5.2"),
            Some("GLM-5.2")
        );
        assert_eq!(
            compatibility_alias("canonical_zai_model_id", "glm-5.3"),
            Some("GLM-5.3")
        );
    }

    #[test]
    fn generation_default_does_not_assert_route_maximum_or_unknown_capability() {
        let kimi = intrinsic_model("kimi-k3").unwrap();
        assert_eq!(kimi.generation_default, Some(131072));
        assert_ne!(kimi.generation_default, Some(1_048_576));
        assert_eq!(kimi.capabilities.streaming, CapabilityState::Unknown);
        assert!(intrinsic_model("kimi-k3-private").is_none());
    }

    #[test]
    fn exact_legacy_identities_validate_without_accepting_picker_aliases() {
        let reviewed = ReviewedCatalog {
            revision: "fixture".into(),
            selections: vec![ModelSelection {
                provider: "minimax-anthropic".into(),
                id: "MiniMax-M3".into(),
                aliases: vec![],
                supports_tools: true,
                supports_reasoning: true,
            }],
            transports: vec![ReviewedTransport {
                provider: "modelstudio-coding-plan-anthropic".into(),
                id: "qwen".into(),
                endpoint_key: "messages".into(),
                canonical_model: None,
                default_for_provider: false,
                limits: RouteLimits::default(),
                capabilities: RouteCapabilities::default(),
                source: "fixture".into(),
            }],
            ..ReviewedCatalog::default()
        };
        reviewed
            .validate()
            .expect("exact released config identities");
        for bad in ["mini-max-anthropic", "unknown-provider", "antigravity"] {
            let mut malformed = reviewed.clone();
            malformed.selections[0].provider = bad.into();
            assert!(malformed.validate().is_err(), "selector {bad}");
            let mut malformed = reviewed.clone();
            malformed.transports[0].provider = bad.into();
            assert!(malformed.validate().is_err(), "transport {bad}");
        }
    }

    #[test]
    fn malformed_authored_rows_are_refused_before_projection() {
        for bad in ["unknown-provider", "antigravity", "openai-compatible"] {
            let mut reviewed = bundled_reviewed().clone();
            reviewed.selections[0].provider = bad.into();
            assert!(reviewed.validate().is_err());
        }
        let mut reviewed = bundled_reviewed().clone();
        reviewed.transports[0].limits.output_tokens = Some(0);
        assert!(reviewed.validate().is_err());
        reviewed = bundled_reviewed().clone();
        reviewed
            .completion_rosters
            .insert("custom".into(), vec!["private-name".into()]);
        assert!(reviewed.validate().is_err());
    }
}

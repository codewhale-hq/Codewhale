//! Factual model reference database (#3205, #2300).
//!
//! A browsable, read-only projection of the compiled catalog into per-offering
//! "fact cards": the model id as-is, the serving provider and its kind, the
//! context window, the price, and the modality (text vs multimodal). It exists
//! to answer "what are this model's stated attributes?", nothing more.
//!
//! This layer is **labels only**. It performs no selection, routing, tiering,
//! or ranking — it never decides which model to use, and it carries no
//! `strong`/`balanced`/`fast` or role concept. It is a superset-free view over
//! [`crate::catalog::CatalogOffering`] rows.
//!
//! Honesty rule (shared with #2608 / #3085): an attribute the catalog layer did
//! not state is reported as **unknown**, never guessed. A local/custom endpoint
//! with no catalog facts yields `Unknown` modality, `None` context window, and
//! an unknown price — its model id is still preserved verbatim. Nothing here is
//! inferred from a model-id prefix.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::ProviderKind;
use crate::catalog::{CatalogOffering, CatalogSnapshot, CatalogSource, bundled_catalog_offerings};
use crate::models_dev::ModelsDevModalities;
use crate::pricing::{Currency, OfferingPricing};

/// Coarse, factual input/output modality label for a model.
///
/// `text` vs `multimodal` is derived from the union of stated input/output
/// modalities. Absent modality metadata is [`Modality::Unknown`], distinct from
/// a stated text-only model — "we were not told" is not "text only".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    /// Every stated modality is text.
    Text,
    /// At least one stated modality is non-text (image/audio/video/…).
    Multimodal,
    /// No modality metadata was stated for this row.
    #[default]
    Unknown,
}

impl Modality {
    /// Classify the modality from a Models.dev-shaped modality block.
    ///
    /// Returns [`Modality::Unknown`] for absent metadata or an empty list,
    /// [`Modality::Multimodal`] when any stated input/output modality is not
    /// `text`, and [`Modality::Text`] when the only stated modalities are text.
    #[must_use]
    pub fn from_modalities(modalities: Option<&ModelsDevModalities>) -> Self {
        let Some(modalities) = modalities else {
            return Self::Unknown;
        };
        let mut saw_any = false;
        for modality in modalities.input.iter().chain(modalities.output.iter()) {
            let trimmed = modality.trim();
            if trimmed.is_empty() {
                continue;
            }
            saw_any = true;
            if !trimmed.eq_ignore_ascii_case("text") {
                return Self::Multimodal;
            }
        }
        if saw_any { Self::Text } else { Self::Unknown }
    }

    /// Stable lowercase label.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Multimodal => "multimodal",
            Self::Unknown => "unknown",
        }
    }
}

/// A factual reference card for one provider offering.
///
/// Every field is either a stated fact or an explicit unknown. This is a
/// labels-only projection: it carries no routing, tier, or selection concept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelReferenceCard {
    /// Provider id serving this offering, exactly as the catalog row states it.
    pub provider: String,
    /// Resolved built-in provider kind, when the provider id maps to one.
    ///
    /// `None` for an unrecognized / user-named custom provider — an unknown
    /// kind, not a guess.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_kind: Option<ProviderKind>,
    /// The provider wire model id, verbatim. Never normalized or prefixed.
    pub model_id: String,
    /// Canonical model identity, only when the row carried an explicit join.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_model: Option<String>,
    /// Model family / series, when stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fact_sources: std::collections::BTreeMap<String, CatalogSource>,
    /// Context-window tokens, when stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// Max-output tokens, when stated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output: Option<u64>,
    /// Text vs multimodal, or unknown.
    pub modality: Modality,
    /// Per-token pricing facts, when priced. `None` is unknown, never free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pricing: Option<OfferingPricing>,
    /// Provenance of the underlying catalog row (bundled / live / override).
    pub source: CatalogSource,
}

impl ModelReferenceCard {
    /// Project a catalog offering into its factual reference card.
    #[must_use]
    pub fn from_offering(offering: &CatalogOffering) -> Self {
        Self {
            provider: offering.provider.clone(),
            provider_kind: ProviderKind::parse(&offering.provider),
            model_id: offering.wire_model_id.clone(),
            canonical_model: offering.canonical_model.clone(),
            family: offering.family.clone(),
            display_name: offering.name.clone(),
            description: offering.description.clone(),
            max_input: offering.limit.as_ref().and_then(|limit| limit.input),
            last_updated: offering.last_updated.clone(),
            fact_sources: offering.fact_sources.clone(),
            context_window: offering.limit.as_ref().and_then(|limit| limit.context),
            max_output: offering.limit.as_ref().and_then(|limit| limit.output),
            modality: Modality::from_modalities(offering.modalities.as_ref()),
            pricing: OfferingPricing::from_catalog_offering(offering),
            source: offering.source.clone(),
        }
    }

    /// Label for the resolved provider kind, or `"unknown"`.
    #[must_use]
    pub fn provider_kind_label(&self) -> &'static str {
        self.provider_kind.map_or("unknown", ProviderKind::as_str)
    }

    /// Human context-window label such as `"1M"`, `"131K"`, `"512"`, or
    /// `"unknown"`. The exact token count remains on [`Self::context_window`].
    #[must_use]
    pub fn context_window_label(&self) -> String {
        humanize_tokens(self.context_window)
    }

    /// Human max-output label, same shape as [`Self::context_window_label`].
    #[must_use]
    pub fn max_output_label(&self) -> String {
        humanize_tokens(self.max_output)
    }

    /// Short factual price label, e.g. `"$0.30 / $1.20 per Mtok"`, or
    /// `"unknown"` when no per-token input/output rate is sourced.
    ///
    /// A `?` in one slot means that single rate is unknown while the other is
    /// stated; a fully unknown price collapses to `"unknown"` rather than a
    /// fabricated zero.
    #[must_use]
    pub fn price_label(&self) -> String {
        let Some(pricing) = self.pricing.as_ref() else {
            return "unknown".to_string();
        };
        if matches!(
            pricing.status_label_at(crate::catalog::now_unix()),
            "stale" | "unverified" | "unavailable"
        ) || (pricing.input_per_million.is_none() && pricing.output_per_million.is_none())
        {
            return "unknown".to_string();
        }
        let symbol = currency_symbol(&pricing.currency);
        let render = |value: Option<f64>| match value {
            Some(rate) => {
                let rounded = format!("{rate:.2}");
                if rounded.parse::<f64>().ok() == Some(rate) {
                    format!("{symbol}{rounded}")
                } else {
                    format!("{symbol}{rate}")
                }
            }
            None => "?".to_string(),
        };
        let suffix = currency_suffix(&pricing.currency);
        format!(
            "{} / {} per Mtok{suffix}",
            render(pricing.input_per_million),
            render(pricing.output_per_million),
        )
    }
}

/// A browsable, read-only factual reference database of model offerings.
///
/// Cards are sorted by `(provider, model id)` and de-duplicated on that
/// identity, so the database is deterministic regardless of input order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelReferenceDatabase {
    cards: Vec<ModelReferenceCard>,
}

impl ModelReferenceDatabase {
    /// Build from raw catalog offerings.
    ///
    /// Rows are keyed by `(provider, model id)`; a later row with the same
    /// identity replaces an earlier one, matching catalog merge semantics.
    #[must_use]
    pub fn from_offerings(offerings: &[CatalogOffering]) -> Self {
        let mut by_identity: BTreeMap<(String, String), ModelReferenceCard> = BTreeMap::new();
        for offering in offerings {
            let card = ModelReferenceCard::from_offering(offering);
            by_identity.insert((card.provider.clone(), card.model_id.clone()), card);
        }
        Self {
            cards: by_identity.into_values().collect(),
        }
    }

    /// Build from a compiled catalog snapshot (bundled < live < overrides).
    #[must_use]
    pub fn from_snapshot(snapshot: &CatalogSnapshot) -> Self {
        Self::from_offerings(&snapshot.offerings)
    }

    /// Build from CodeWhale's offline/stale bundled catalog snapshot (#4188).
    ///
    /// Prefer a live/compiled [`CatalogSnapshot`] when available. The bundled
    /// set needs no credentials or network and remains the offline fallback
    /// every install carries.
    #[must_use]
    pub fn bundled() -> Self {
        Self::from_offerings(&bundled_catalog_offerings())
    }

    /// All cards, in stable `(provider, model id)` order.
    #[must_use]
    pub fn cards(&self) -> &[ModelReferenceCard] {
        &self.cards
    }

    /// Number of cards.
    #[must_use]
    pub fn len(&self) -> usize {
        self.cards.len()
    }

    /// Whether the database is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    /// Distinct provider ids present, sorted.
    #[must_use]
    pub fn providers(&self) -> Vec<&str> {
        self.cards
            .iter()
            .map(|card| card.provider.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    /// All cards served by one provider id.
    #[must_use]
    pub fn for_provider(&self, provider: &str) -> Vec<&ModelReferenceCard> {
        self.cards
            .iter()
            .filter(|card| card.provider == provider)
            .collect()
    }

    /// Find a card by `(provider, model id)`.
    #[must_use]
    pub fn find(&self, provider: &str, model_id: &str) -> Option<&ModelReferenceCard> {
        self.cards
            .iter()
            .find(|card| card.provider == provider && card.model_id == model_id)
    }
}

/// Round a token count to a short human label (`"1M"`, `"203K"`, `"512"`), or
/// `"unknown"` for an absent count. Used for display only; callers needing the
/// exact value read the `Option<u64>` field directly.
fn humanize_tokens(tokens: Option<u64>) -> String {
    let Some(tokens) = tokens else {
        return "unknown".to_string();
    };
    if tokens >= 1_000_000 {
        let millions = tokens as f64 / 1_000_000.0;
        let rendered = format!("{millions:.2}");
        let trimmed = rendered.trim_end_matches('0').trim_end_matches('.');
        format!("{trimmed}M")
    } else if tokens >= 1_000 {
        format!("{}K", (tokens as f64 / 1_000.0).round() as u64)
    } else {
        tokens.to_string()
    }
}

fn currency_symbol(currency: &Currency) -> &'static str {
    match currency {
        Currency::Usd => "$",
        Currency::Cny => "¥",
        Currency::Other(_) => "",
    }
}

fn currency_suffix(currency: &Currency) -> String {
    match currency {
        Currency::Usd | Currency::Cny => String::new(),
        Currency::Other(code) => format!(" {code}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models_dev::{ModelsDevCost, ModelsDevLimit};

    fn offering(provider: &str, wire: &str) -> CatalogOffering {
        CatalogOffering {
            provider: provider.to_string(),
            wire_model_id: wire.to_string(),
            endpoint_key: "chat".to_string(),
            source: CatalogSource::Bundled,
            ..Default::default()
        }
    }

    #[test]
    fn modality_text_multimodal_and_unknown() {
        assert_eq!(Modality::from_modalities(None), Modality::Unknown);
        assert_eq!(
            Modality::from_modalities(Some(&ModelsDevModalities::default())),
            Modality::Unknown,
            "an empty modality block is unknown, not text-only"
        );
        assert_eq!(
            Modality::from_modalities(Some(&ModelsDevModalities {
                input: vec!["text".to_string()],
                output: vec!["text".to_string()],
            })),
            Modality::Text
        );
        assert_eq!(
            Modality::from_modalities(Some(&ModelsDevModalities {
                input: vec!["text".to_string(), "image".to_string()],
                output: vec!["text".to_string()],
            })),
            Modality::Multimodal
        );
        // Case-insensitive and tolerant of an output-only non-text modality.
        assert_eq!(
            Modality::from_modalities(Some(&ModelsDevModalities {
                input: vec!["TEXT".to_string()],
                output: vec!["Audio".to_string()],
            })),
            Modality::Multimodal
        );
    }

    #[test]
    fn card_projects_stated_facts() {
        let row = CatalogOffering {
            family: Some("deepseek".to_string()),
            limit: Some(ModelsDevLimit {
                context: Some(1_000_000),
                input: None,
                output: Some(384_000),
            }),
            cost: Some(ModelsDevCost {
                input: Some(0.3),
                output: Some(1.2),
                cache_read: Some(0.06),
                cache_write: None,
                ..Default::default()
            }),
            modalities: Some(ModelsDevModalities {
                input: vec!["text".to_string()],
                output: vec!["text".to_string()],
            }),
            ..offering("deepseek", "deepseek-v4-pro")
        };
        let card = ModelReferenceCard::from_offering(&row);

        assert_eq!(card.provider, "deepseek");
        assert_eq!(card.provider_kind, Some(ProviderKind::Deepseek));
        assert_eq!(card.provider_kind_label(), "deepseek");
        assert_eq!(card.model_id, "deepseek-v4-pro");
        assert_eq!(card.family.as_deref(), Some("deepseek"));
        assert_eq!(card.context_window, Some(1_000_000));
        assert_eq!(card.context_window_label(), "1M");
        assert_eq!(card.max_output, Some(384_000));
        assert_eq!(card.max_output_label(), "384K");
        assert_eq!(card.modality, Modality::Text);
        assert_eq!(card.price_label(), "$0.30 / $1.20 per Mtok");
    }

    #[test]
    fn custom_local_row_is_all_unknown_but_keeps_model_id_verbatim() {
        // A user-named custom endpoint with no catalog facts: provider kind,
        // context window, modality, and price are all unknown — never guessed —
        // and the model id is preserved exactly.
        let row = CatalogOffering {
            source: CatalogSource::UserOverride,
            ..offering("my-local-llm", "Vendor/Custom-Model_v1")
        };
        let card = ModelReferenceCard::from_offering(&row);

        assert_eq!(card.provider_kind, None);
        assert_eq!(card.provider_kind_label(), "unknown");
        assert_eq!(card.model_id, "Vendor/Custom-Model_v1");
        assert_eq!(card.context_window, None);
        assert_eq!(card.context_window_label(), "unknown");
        assert_eq!(card.max_output_label(), "unknown");
        assert_eq!(card.modality, Modality::Unknown);
        assert_eq!(card.price_label(), "unknown");
    }

    #[test]
    fn unpriced_and_cache_only_rows_report_unknown_price_never_zero() {
        // No cost block at all.
        let unpriced = ModelReferenceCard::from_offering(&offering("deepseek", "deepseek-v4-pro"));
        assert_eq!(unpriced.price_label(), "unknown");
        assert!(unpriced.pricing.is_none());

        // A cost object priced only on cache classes is still unknown for the
        // headline input/output rate label.
        let cache_only = CatalogOffering {
            cost: Some(ModelsDevCost {
                input: None,
                output: None,
                cache_read: Some(0.05),
                cache_write: None,
                ..Default::default()
            }),
            ..offering("acme", "house-model")
        };
        assert_eq!(
            ModelReferenceCard::from_offering(&cache_only).price_label(),
            "unknown"
        );
    }

    #[test]
    fn partial_price_renders_known_rate_and_marks_the_other_unknown() {
        let row = CatalogOffering {
            cost: Some(ModelsDevCost {
                input: Some(5.0),
                output: None,
                cache_read: None,
                cache_write: None,
                ..Default::default()
            }),
            ..offering("openai", "gpt-5.5")
        };
        assert_eq!(
            ModelReferenceCard::from_offering(&row).price_label(),
            "$5.00 / ? per Mtok"
        );
    }

    #[test]
    fn database_is_sorted_deduped_and_queryable() {
        let rows = vec![
            CatalogOffering {
                limit: Some(ModelsDevLimit {
                    context: Some(1),
                    input: None,
                    output: None,
                }),
                ..offering("zai", "GLM-5.2")
            },
            offering("deepseek", "deepseek-v4-pro"),
            // Duplicate identity with a higher context wins (last-write).
            CatalogOffering {
                limit: Some(ModelsDevLimit {
                    context: Some(1_000_000),
                    input: None,
                    output: None,
                }),
                ..offering("zai", "GLM-5.2")
            },
        ];
        let db = ModelReferenceDatabase::from_offerings(&rows);

        assert_eq!(db.len(), 2, "duplicate (provider, model) collapses to one");
        // Sorted by (provider, model id): deepseek before zai.
        assert_eq!(db.cards()[0].provider, "deepseek");
        assert_eq!(db.cards()[1].provider, "zai");
        assert_eq!(db.providers(), vec!["deepseek", "zai"]);
        assert_eq!(db.for_provider("zai").len(), 1);
        assert_eq!(
            db.find("zai", "GLM-5.2")
                .and_then(|card| card.context_window),
            Some(1_000_000),
            "last-write-wins kept the richer row"
        );
        assert!(db.find("zai", "missing").is_none());
    }

    #[test]
    fn bundled_database_is_nonempty_and_honest() {
        let db = ModelReferenceDatabase::bundled();
        assert!(!db.is_empty());
        assert!(
            db.len() >= 20,
            "bundled offline snapshot should carry seed offerings, got {}",
            db.len()
        );

        // Every card preserves a non-empty model id and resolves a known kind
        // for the bundled (first-class) providers.
        for card in db.cards() {
            assert!(!card.model_id.is_empty());
            assert!(
                card.provider_kind.is_some(),
                "bundled provider {} should map to a known kind",
                card.provider
            );
        }

        // A DeepSeek-native row: context window known, price honestly unknown
        // (the bundled snapshot omits DeepSeek-native per-token pricing).
        let deepseek = db
            .find("deepseek", "deepseek-v4-pro")
            .expect("bundled deepseek row");
        assert_eq!(deepseek.context_window, Some(1_048_576));
        assert_eq!(deepseek.modality, Modality::Text);
        assert_eq!(deepseek.price_label(), "unknown");

        // A priced row surfaces its stated per-token rate.
        let minimax = db
            .find("minimax", "MiniMax-M2.7")
            .expect("bundled minimax row");
        assert_eq!(minimax.price_label(), "$0.30 / $1.20 per Mtok");

        // MiniMax-M3 carries its own base rate and a context tier.
        let minimax_m3 = db
            .find("minimax", "MiniMax-M3")
            .expect("bundled minimax m3 row");
        assert_eq!(minimax_m3.price_label(), "$0.30 / $1.20 per Mtok");
        assert_eq!(minimax_m3.pricing.as_ref().unwrap().context_tiers.len(), 1);
    }

    #[test]
    fn humanize_tokens_shapes() {
        assert_eq!(humanize_tokens(None), "unknown");
        assert_eq!(humanize_tokens(Some(512)), "512");
        assert_eq!(humanize_tokens(Some(131_072)), "131K");
        assert_eq!(humanize_tokens(Some(1_000_000)), "1M");
        assert_eq!(humanize_tokens(Some(1_050_000)), "1.05M");
    }
}

//! Codewhale's corrections to Models.dev rows (#6396).
//!
//! Some Models.dev facts are true but misleading for a Codewhale route: a flat
//! price for a plan that bills quota, a base rate that doubles past a prompt
//! size, an output limit the provider publishes differently. These used to be
//! hand edits to the offline seed, so they held only until the first live
//! refresh replaced the row. Here they are field patches applied to every
//! Models.dev row as it is hydrated, bundled seed and live refresh alike, so a
//! correction means the same thing on every install.
//!
//! Corrections rank above bundled and live Models.dev and below signed cloud
//! facts (which may correct a correction) and provider rosters. A corrected
//! row keeps its own source, so layer code that sorts rows by source still
//! places it correctly; a price the correction owns (set or withheld) is
//! attributed to [`CatalogSource::CodewhaleBundled`] through `cost_source`.
//! It reuses the signed layer's [`ModelFact`] shape and patch code, so there
//! is one way to correct a catalog row. Corrections never add a row and never
//! hide one.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use super::{CatalogOffering, CatalogSource};
use crate::cloud_facts::catalog_patch::apply_patches;
use crate::cloud_facts::{ModelFact, ModelOp};

/// The committed corrections asset.
pub const CATALOG_CORRECTIONS_JSON: &str = include_str!("../../assets/catalog_corrections.json");

/// Parsed corrections.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogCorrections {
    #[serde(default, rename = "_about")]
    pub about: String,
    /// Stamped as [`CatalogSource::CodewhaleBundled`] on prices it owns.
    pub revision: String,
    /// Pure reviewed model/transport data. This file is its only owner; the
    /// bundled seed carries no copy.
    #[serde(default)]
    pub reviewed: super::reviewed::ReviewedCatalog,
    /// Rules for every row a provider serves.
    #[serde(default)]
    pub providers: Vec<ProviderCorrection>,
    /// Rules for one `(provider, wire id)` row.
    #[serde(default)]
    pub models: Vec<ModelCorrection>,
}

/// A rule for every row of one provider.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCorrection {
    pub provider: String,
    /// Why no price is shown for any of this provider's rows.
    pub pricing_withheld: String,
}

/// A field patch for one row, with the reason it exists.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelCorrection {
    /// Why the patch exists. Required unless the patch only withholds pricing
    /// (whose text is its own reason).
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(flatten)]
    pub fact: ModelFact,
}

impl CatalogCorrections {
    /// Parse and validate a corrections document.
    ///
    /// # Errors
    /// Returns a message naming the first entry that is malformed or does more
    /// than a correction may (hide, deprecate, annotate, or add a row).
    pub fn parse(json: &str) -> Result<Self, String> {
        let corrections: Self = serde_json::from_str(json).map_err(|err| err.to_string())?;
        corrections.validate()?;
        Ok(corrections)
    }

    fn validate(&self) -> Result<(), String> {
        if self.revision.trim().is_empty() {
            return Err("revision must be set".into());
        }
        self.reviewed.validate()?;
        for rule in &self.providers {
            if rule.provider.trim().is_empty() || rule.pricing_withheld.trim().is_empty() {
                return Err("provider rules need a provider and a reason".into());
            }
        }
        for correction in &self.models {
            let fact = &correction.fact;
            let name = format!("{}/{}", fact.provider, fact.id);
            if fact.provider.trim().is_empty() || fact.id.trim().is_empty() {
                return Err(format!("{name}: provider and id are required"));
            }
            if fact.op != ModelOp::Upsert || fact.allow_unlisted {
                return Err(format!("{name}: a correction may only patch a listed row"));
            }
            if fact.display_name.is_some()
                || fact.note.is_some()
                || fact.deprecated_at.is_some()
                || fact.replacement.is_some()
                || fact.applies_to.is_some()
            {
                return Err(format!("{name}: a correction changes facts, not labels"));
            }
            if fact.pricing.is_some() && fact.pricing_withheld.is_some() {
                return Err(format!("{name}: sets a price and withholds it"));
            }
            let patches_more_than_price = fact.context_window.is_some()
                || fact.max_output.is_some()
                || fact.pricing.is_some()
                || fact.reasoning.is_some()
                || fact.reasoning_options.is_some();
            if !patches_more_than_price && fact.pricing_withheld.is_none() {
                return Err(format!("{name}: changes nothing"));
            }
            if patches_more_than_price
                && correction
                    .reason
                    .as_deref()
                    .is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(format!("{name}: needs a reason"));
            }
        }
        Ok(())
    }

    /// Validate committed referents against the seed, not a partial live refresh.
    fn validate_targets(&self, seed: &[CatalogOffering]) -> Result<(), String> {
        for rule in &self.providers {
            if !seed.iter().any(|row| row.provider == rule.provider) {
                return Err(format!(
                    "{}: correction provider missing from bundled seed",
                    rule.provider
                ));
            }
        }
        for correction in &self.models {
            let fact = &correction.fact;
            if !seed
                .iter()
                .any(|row| row.provider == fact.provider && row.wire_model_id == fact.id)
            {
                return Err(format!(
                    "{}/{}: correction model missing from bundled seed",
                    fact.provider, fact.id
                ));
            }
        }
        Ok(())
    }

    /// Apply every correction to the rows it names, in place.
    ///
    /// Provider rules run first, then per-row patches, so a row patch can add
    /// to a provider rule (a DeepSeek output limit on top of its withheld
    /// price). Rows no rule names are untouched. Live refreshes may omit
    /// correction targets; only the committed seed must contain every target.
    pub fn apply_to(&self, rows: &mut [CatalogOffering]) {
        let mut patches: BTreeMap<(String, String), Vec<ModelFact>> = BTreeMap::new();
        for rule in &self.providers {
            for row in rows.iter().filter(|row| row.provider == rule.provider) {
                patches
                    .entry((row.provider.clone(), row.wire_model_id.clone()))
                    .or_default()
                    .push(ModelFact {
                        provider: row.provider.clone(),
                        id: row.wire_model_id.clone(),
                        pricing_withheld: Some(rule.pricing_withheld.clone()),
                        ..ModelFact::default()
                    });
            }
        }
        for correction in &self.models {
            patches
                .entry((correction.fact.provider.clone(), correction.fact.id.clone()))
                .or_default()
                .push(correction.fact.clone());
        }
        if patches.is_empty() {
            return;
        }
        let source = CatalogSource::CodewhaleBundled {
            revision: self.revision.clone(),
        };
        for row in rows.iter_mut() {
            let key = (row.provider.clone(), row.wire_model_id.clone());
            let Some(row_patches) = patches.get(&key) else {
                continue;
            };
            // The row stays on its own layer: a hydrated row is classified by
            // its source (a live Models.dev row must stay below signed facts),
            // so only the price a correction owns is attributed to it.
            let origin = row.source.clone();
            let modalities_source = row.modalities_source.clone();
            let cost_source = row.cost_source.clone();
            let mut single = BTreeMap::from([(key.clone(), std::mem::take(row))]);
            apply_patches(&mut single, row_patches, &source, false);
            if let Some(mut patched) = single.remove(&key) {
                patched.source = origin;
                patched.modalities_source = modalities_source;
                if !row_patches.iter().any(owns_price) {
                    patched.cost_source = cost_source;
                }
                *row = patched;
            }
        }
    }
}

fn owns_price(fact: &ModelFact) -> bool {
    fact.pricing.is_some() || fact.pricing_withheld.is_some()
}

/// The committed corrections, parsed once.
///
/// # Panics
/// Panics if the committed asset is invalid or names a missing seed target.
#[must_use]
pub fn bundled_corrections() -> &'static CatalogCorrections {
    static CORRECTIONS: OnceLock<CatalogCorrections> = OnceLock::new();
    CORRECTIONS.get_or_init(|| {
        let corrections = CatalogCorrections::parse(CATALOG_CORRECTIONS_JSON)
            .expect("committed catalog corrections must be valid");
        let seed = super::bundled_offerings_from_models_dev(super::bundled_models_dev_catalog());
        corrections
            .validate_targets(&seed)
            .expect("committed catalog correction targets must exist");
        corrections
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_seed_targets_fail_validation_but_partial_live_refreshes_are_allowed() {
        let corrections = bundled_corrections();
        let seed = super::super::bundled_offerings_from_models_dev(
            super::super::bundled_models_dev_catalog(),
        );
        let provider = &corrections.providers[0].provider;
        let without_provider: Vec<_> = seed
            .iter()
            .filter(|row| &row.provider != provider)
            .cloned()
            .collect();
        let error = corrections.validate_targets(&without_provider).unwrap_err();
        assert!(error.contains(provider), "{error}");

        let target = &corrections.models[0].fact;
        let mut without_model = seed;
        without_model
            .retain(|row| row.provider != target.provider || row.wire_model_id != target.id);
        let error = corrections.validate_targets(&without_model).unwrap_err();
        assert!(
            error.contains(&format!("{}/{}", target.provider, target.id)),
            "{error}"
        );

        // Absence from a live refresh does not invalidate committed corrections
        // or invent a row to patch.
        corrections.apply_to(&mut without_model);
        assert!(
            !without_model
                .iter()
                .any(|row| row.provider == target.provider && row.wire_model_id == target.id)
        );
        corrections.apply_to(&mut []);
    }
}

//! Scout — the one visible fast exploratory role.
//!
//! Scout is exploration, triage, and quick research. There is exactly one
//! concept; the legacy `faster`/`model_strength` control is removed from the
//! user-facing surface (its parsing survives for compatibility and maps onto
//! the Scout policy).
//!
//! Route resolution, in order:
//!
//! 1. **An explicit Scout pin always wins** and survives operator changes.
//! 2. **No pin:** a suggested fast companion from explicit catalog metadata —
//!    the provider's documented cheap sibling via
//!    [`crate::model_routing::provider_router_candidates`] (DeepSeek
//!    pro/flash, Z.ai 5.2/5.3 → GLM-5-Turbo, Claude → Haiku, …). The
//!    suggestion is honored only when that model actually exists in the
//!    merged catalog for the provider — never invented.
//! 3. **No verified companion:** the Scout inherits the session route
//!    deliberately, and the resolution says so.
//! 4. **No session route at all:** `Unavailable` with a precise reason.

use crate::fleet::store::FleetMember;
use crate::model_routing::provider_router_candidates;
use crate::provider_lake::all_catalog_models_for_provider;
use codewhale_config::ProviderKind;

/// Where the resolved Scout route came from. Shown before a run, never
/// guessed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScoutSource {
    /// The Fleet pins this exact provider/model.
    Pinned,
    /// The provider's documented fast sibling, verified present in the
    /// catalog for this provider.
    CatalogSuggestion,
    /// No pin, no verified companion — the Scout inherits the session route.
    Inherited,
    /// No route could be resolved; the reason names why.
    Unavailable(String),
}

/// The resolved Scout route and its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScoutResolution {
    pub provider: String,
    pub model: String,
    pub source: ScoutSource,
}

impl ScoutResolution {
    /// A compact one-line receipt: `deepseek/deepseek-v4-flash (pinned)`.
    #[must_use]
    pub fn receipt_line(&self) -> String {
        let source = match &self.source {
            ScoutSource::Pinned => "pinned",
            ScoutSource::CatalogSuggestion => "catalog suggestion",
            ScoutSource::Inherited => "inherits session route",
            ScoutSource::Unavailable(_) => "unavailable",
        };
        format!("{}/{} ({source})", self.provider, self.model)
    }
}

/// The provider's documented fast sibling for the given session route,
/// VERIFIED against the merged catalog. `None` means "no verified fast
/// companion exists for this provider/route" — never a guess.
#[must_use]
pub fn verified_fast_companion(provider_id: &str, session_model: &str) -> Option<(String, String)> {
    let provider = ProviderKind::parse(provider_id)?;
    let candidates = provider_router_candidates(provider, session_model);
    let cheap = candidates.cheap?;
    // Verification: the suggested model must actually exist as an offering
    // for this provider in the merged catalog. A table row whose model was
    // removed (or was never shipped) yields no suggestion.
    let available = all_catalog_models_for_provider(provider);
    if available.iter().any(|m| m == &cheap) {
        Some((provider_id.to_string(), cheap))
    } else {
        None
    }
}

/// Resolve the Scout route for a run. Explicit pins win; otherwise a verified
/// catalog companion; otherwise deliberate inheritance; otherwise a precise
/// unavailable reason. Never invents a fallback model.
#[must_use]
pub fn resolve_scout_route(
    scout_member: Option<&FleetMember>,
    session_provider: &str,
    session_model: &str,
) -> ScoutResolution {
    if let Some(member) = scout_member
        && let (Some(provider), Some(model)) = (&member.provider, &member.model)
    {
        return ScoutResolution {
            provider: provider.clone(),
            model: model.clone(),
            source: ScoutSource::Pinned,
        };
    }
    if let Some((provider, model)) = verified_fast_companion(session_provider, session_model) {
        return ScoutResolution {
            provider,
            model,
            source: ScoutSource::CatalogSuggestion,
        };
    }
    let session_model = session_model.trim();
    if session_model.is_empty() || session_model.eq_ignore_ascii_case("auto") {
        return ScoutResolution {
            provider: session_provider.to_string(),
            model: session_model.to_string(),
            source: ScoutSource::Unavailable(
                "no verified fast companion exists for this provider and no concrete \
                 session route is set to inherit"
                    .to_string(),
            ),
        };
    }
    ScoutResolution {
        provider: session_provider.to_string(),
        model: session_model.to_string(),
        source: ScoutSource::Inherited,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::store::FleetMember;

    fn member(pin: Option<(&str, &str)>) -> Option<FleetMember> {
        pin.map(|(provider, model)| FleetMember {
            id: "scout".to_string(),
            display_name: None,
            shortlist: false,
            role: "scout".to_string(),
            provider: Some(provider.to_string()),
            model: Some(model.to_string()),
            reasoning: None,
            instructions: None,
            requires: Vec::new(),
        })
    }

    #[test]
    fn pinned_scout_wins_and_survives_operator_changes() {
        // The pin is a different route than the session — the pin must win.
        let resolution = resolve_scout_route(
            member(Some(("deepseek", "deepseek-v4-pro"))).as_ref(),
            "deepseek",
            "deepseek-v4-flash",
        );
        assert_eq!(resolution.provider, "deepseek");
        assert_eq!(resolution.model, "deepseek-v4-pro");
        assert_eq!(resolution.source, ScoutSource::Pinned);

        // Operator change: still the pin.
        let resolution = resolve_scout_route(
            member(Some(("deepseek", "deepseek-v4-pro"))).as_ref(),
            "openai",
            "gpt-5",
        );
        assert_eq!(resolution.model, "deepseek-v4-pro");
        assert_eq!(resolution.source, ScoutSource::Pinned);
    }

    #[test]
    fn unpinned_scout_gets_verified_catalog_companion_or_inherits() {
        // DeepSeek's documented cheap sibling is deepseek-flash and it is
        // in the bundled catalog — a verified suggestion.
        let resolution = resolve_scout_route(None, "deepseek", "deepseek-v4-pro");
        assert_eq!(resolution.provider, "deepseek");
        assert_eq!(resolution.model, "deepseek-flash");
        assert_eq!(resolution.source, ScoutSource::CatalogSuggestion);

        // Anthropic's Claude models have a documented cheap sibling
        // (claude-haiku-4-5) — a verified suggestion.
        let resolution = resolve_scout_route(None, "anthropic", "claude-sonnet-4-6");
        assert_eq!(resolution.model, "claude-haiku-4-5");
        assert_eq!(resolution.source, ScoutSource::CatalogSuggestion);

        // A provider outside the companion tables has no verified fast
        // sibling — deliberate inheritance, never an invented fallback.
        let resolution = resolve_scout_route(None, "sglang", "some-model");
        assert_eq!(resolution.model, "some-model");
        assert_eq!(resolution.source, ScoutSource::Inherited);
    }

    #[test]
    fn no_session_route_is_unavailable_with_a_reason() {
        // No companion for this provider AND no session route to inherit.
        let resolution = resolve_scout_route(None, "sglang", "");
        assert!(
            matches!(&resolution.source, ScoutSource::Unavailable(reason) if !reason.is_empty()),
            "{resolution:?}"
        );
    }

    #[test]
    fn verified_companion_requires_the_model_in_the_catalog() {
        // A provider whose router table lists a sibling that is NOT in the
        // merged catalog yields no suggestion (the runtime verification is
        // the honesty gate).
        let resolution = resolve_scout_route(None, "zai", "GLM-5.2");
        match resolution.source {
            ScoutSource::CatalogSuggestion => {
                // GLM-5-Turbo must actually be listed for zai in this build.
                let available = all_catalog_models_for_provider(ProviderKind::Zai);
                assert!(
                    available.iter().any(|m| m == "GLM-5-Turbo"),
                    "a CatalogSuggestion must be verifiable in the catalog"
                );
            }
            ScoutSource::Inherited => {
                // Honest: no verified companion; the scout stays on the
                // session route.
                assert_eq!(resolution.model, "GLM-5.2");
            }
            other => panic!("unexpected source: {other:?}"),
        }
    }

    #[test]
    fn receipt_line_names_the_source() {
        let resolution = resolve_scout_route(None, "deepseek", "deepseek-v4-pro");
        let line = resolution.receipt_line();
        assert!(line.contains("deepseek-flash"), "{line}");
        assert!(line.contains("catalog suggestion"), "{line}");
    }
}

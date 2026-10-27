//! Concise model-capability badges for Fleet configuration UI (#5038).
//!
//! One resolver answers "what can this model do" for both the Fleet setup
//! model-selection step and the roster detail pane. Facts come from the
//! existing owners — the merged Models.dev catalog (via
//! [`crate::provider_lake::catalog_offering_for_model`], provider-aware, with
//! bundled/live/override layers) first, then the seeded
//! reviewed intrinsic facts for ids no catalog row covers (custom
//! providers, local models). No second model catalog is introduced here.
//!
//! Honesty rules: unknown facts are omitted rather than guessed, an explicit
//! catalog "unsupported" renders as `no <badge>`, provenance is always named,
//! and a completely unknown model resolves to `None` so callers can skip the
//! line entirely instead of blocking selection or fabricating capabilities.

use codewhale_config::catalog::CatalogSource;
use codewhale_config::route::{CapabilityState, RouteCapabilities, RouteLimits};

use codewhale_config::ProviderKind;
use codewhale_config::catalog::reviewed::IntrinsicModel;
use codewhale_util::utils::format_context_window;

/// Resolved capability badges for one Fleet route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteCapabilityBadges {
    /// Ordered, concise badges, e.g. `["1M ctx", "384K out", "tools",
    /// "reasoning", "no vision"]`. Never empty when resolution succeeds.
    pub badges: Vec<String>,
    /// Where the facts came from: `bundled catalog`, `live catalog`,
    /// `override`, or `registry`.
    pub provenance: &'static str,
}

impl RouteCapabilityBadges {
    /// One-line summary with provenance, sized for narrow detail panes:
    /// `1M ctx · 384K out · tools · reasoning · no vision (bundled catalog)`.
    #[must_use]
    pub fn summary(&self) -> String {
        format!("{} ({})", self.badges.join(" · "), self.provenance)
    }
}

/// Resolve capability badges for one `(provider, model)` Fleet route.
///
/// `provider_id` is the exact configured route key when known (canonical
/// built-in id or named custom table key); pass `None` when the route has no
/// resolvable provider (e.g. a pinned model that inherits the provider). Only
/// exact built-in ids reach the provider-scoped catalog; every other id falls
/// back to provider-agnostic registry facts rather than guessing a route.
///
/// The session `auto -> model` display form is accepted and resolved against
/// the effective model. Returns `None` when nothing is known about the model,
/// so absence renders as absence.
#[must_use]
pub fn resolve_route_capability_badges(
    provider_id: Option<&str>,
    model: &str,
) -> Option<RouteCapabilityBadges> {
    let model = effective_model(model)?;
    if let Some(provider) = provider_id.and_then(exact_builtin_provider)
        && let Some(offering) = crate::provider_lake::catalog_offering_for_model(provider, model)
    {
        let route = offering.to_offering();
        let badges = badges_from_route(&route.limits, &route.capabilities);
        if !badges.is_empty() {
            return Some(RouteCapabilityBadges {
                badges,
                provenance: catalog_provenance(&offering.source),
            });
        }
    }
    let meta = codewhale_config::catalog::reviewed::intrinsic_model(model)?;
    let badges = badges_from_intrinsic(&meta);
    (!badges.is_empty()).then_some(RouteCapabilityBadges {
        badges,
        provenance: "registry",
    })
}

/// Strip the session `auto -> model` display form down to the effective model.
/// A bare `auto` (nothing resolved yet) or empty id yields `None`.
fn effective_model(model: &str) -> Option<&str> {
    let model = model
        .split_once("->")
        .map_or(model, |(_, effective)| effective)
        .trim();
    (!model.is_empty() && !model.eq_ignore_ascii_case("auto")).then_some(model)
}

/// Accept only exact canonical built-in provider ids. Display labels and named
/// custom table keys must not inherit a built-in catalog by similarity.
fn exact_builtin_provider(provider_id: &str) -> Option<ProviderKind> {
    ProviderKind::parse(provider_id).filter(|provider| provider.as_str() == provider_id)
}

const fn catalog_provenance(source: &CatalogSource) -> &'static str {
    match source {
        CatalogSource::Bundled | CatalogSource::CodewhaleBundled { .. } => "bundled catalog",
        CatalogSource::Live { .. } | CatalogSource::ModelsDevLive { .. } => "live catalog",
        CatalogSource::CloudFacts { .. } => "signed cloud facts",
        CatalogSource::ConfigOverride | CatalogSource::UserOverride => "override",
    }
}

/// Badges from exact provider-offering facts. Three-state facts keep their
/// explicit `Unsupported` (`no tools` / `no vision`); `Unknown` is omitted.
fn badges_from_route(limits: &RouteLimits, capabilities: &RouteCapabilities) -> Vec<String> {
    let mut badges = Vec::new();
    if let Some(context) = limits.context_tokens {
        badges.push(format!("{} ctx", format_context_window(context)));
    }
    if let Some(output) = limits.output_tokens {
        badges.push(format!("{} out", format_context_window(output)));
    }
    push_state_badge(&mut badges, capabilities.native_tool_calls, "tools");
    push_state_badge(&mut badges, capabilities.reasoning, "reasoning");
    push_state_badge(&mut badges, capabilities.image_input, "vision");
    badges
}

/// Badges from reviewed intrinsic facts. The registry has no tool/vision facts,
/// and its `supports_reasoning: false` is a heuristic default rather than a
/// sourced denial, so only a positive reasoning fact is shown.
fn badges_from_intrinsic(meta: &IntrinsicModel) -> Vec<String> {
    let mut badges = Vec::new();
    if let Some(context) = meta.context_window {
        badges.push(format!("{} ctx", format_context_window(u64::from(context))));
    }
    if let Some(output) = meta.generation_default.or(meta.max_output) {
        badges.push(format!("{} out", format_context_window(u64::from(output))));
    }
    if meta.reasoning == Some(true) {
        badges.push("reasoning".to_string());
    }
    badges
}

fn push_state_badge(badges: &mut Vec<String>, state: CapabilityState, name: &str) {
    match state {
        CapabilityState::Supported => badges.push(name.to_string()),
        CapabilityState::Unsupported => badges.push(format!("no {name}")),
        CapabilityState::Unknown => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_config::catalog::CatalogOffering;
    use codewhale_config::models_dev::{ModelsDevLimit, ModelsDevModalities};

    #[test]
    fn known_catalog_model_resolves_provider_aware_badges() {
        let badges = resolve_route_capability_badges(Some("deepseek"), "deepseek-v4-pro")
            .expect("bundled catalog knows deepseek-v4-pro");
        assert!(
            badges.badges.contains(&"1.05M ctx".to_string()),
            "missing context badge: {:?}",
            badges.badges
        );
        assert!(badges.badges.contains(&"384K out".to_string()));
        assert!(badges.badges.contains(&"tools".to_string()));
        assert!(badges.badges.contains(&"reasoning".to_string()));
        // The offline seed's text-only row is not a refusal (#6396): the seed
        // lags providers, so it may say "vision" but never "no vision".
        assert!(!badges.badges.contains(&"no vision".to_string()));
        assert!(
            badges.provenance.contains("catalog"),
            "catalog facts must carry catalog provenance, got {}",
            badges.provenance
        );
        assert!(badges.summary().contains(" · "));
    }

    #[test]
    fn unknown_model_resolves_to_graceful_absence() {
        assert_eq!(
            resolve_route_capability_badges(Some("deepseek"), "totally-made-up-model-xyz"),
            None
        );
        assert_eq!(resolve_route_capability_badges(None, ""), None);
        assert_eq!(resolve_route_capability_badges(None, "auto"), None);
    }

    #[test]
    fn registry_fallback_covers_routes_without_catalog_rows() {
        // A named custom route key never inherits a built-in catalog; the
        // provider-agnostic registry still answers for the model id.
        let badges = resolve_route_capability_badges(Some("my-custom-endpoint"), "claude-fable-5")
            .expect("registry knows claude-fable-5");
        assert_eq!(badges.provenance, "registry");
        assert!(badges.badges.contains(&"1M ctx".to_string()));
        assert!(badges.badges.contains(&"reasoning".to_string()));
        // Tool/vision facts are unknown here — omitted, never fabricated.
        assert!(
            !badges
                .badges
                .iter()
                .any(|badge| badge.contains("tools") || badge.contains("vision")),
            "unsourced facts must not appear: {:?}",
            badges.badges
        );
    }

    #[test]
    fn auto_display_route_resolves_the_effective_model() {
        let badges = resolve_route_capability_badges(None, "auto -> deepseek-v4-pro")
            .expect("effective model resolves");
        assert!(badges.badges.contains(&"1M ctx".to_string()));
    }

    #[test]
    fn explicit_unsupported_catalog_facts_render_as_no_badges() {
        let offering = CatalogOffering {
            provider: "deepseek".to_string(),
            wire_model_id: "fixture-model".to_string(),
            endpoint_key: "chat".to_string(),
            limit: Some(ModelsDevLimit {
                context: Some(131_072),
                input: None,
                output: None,
            }),
            reasoning: Some(false),
            tool_call: Some(false),
            modalities: Some(ModelsDevModalities {
                input: vec!["text".to_string(), "image".to_string()],
                output: vec!["text".to_string()],
            }),
            ..CatalogOffering::default()
        };
        let route = offering.to_offering();
        let badges = badges_from_route(&route.limits, &route.capabilities);
        assert_eq!(
            badges,
            vec![
                "131K ctx".to_string(),
                "no tools".to_string(),
                "no reasoning".to_string(),
                "vision".to_string(),
            ]
        );
    }

    #[test]
    fn token_labels_match_picker_vocabulary() {
        assert_eq!(format_context_window(1_000_000), "1M");
        assert_eq!(format_context_window(1_050_000), "1.05M");
        assert_eq!(format_context_window(262_144), "262K");
        assert_eq!(format_context_window(500), "500");
    }
}

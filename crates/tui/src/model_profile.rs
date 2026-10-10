//! Typed model and resolved-route capability descriptors (#3365).
//!
//! This module bridges the additive config catalog intrinsic facts and the
//! provider+model capability matrix in [`crate::config::provider_capability`].
//! It intentionally keeps intrinsic model facts separate from resolved route
//! facts so future route resolution can combine catalog offerings, user
//! overrides, live hints, and auth readiness without scattering provider/model
//! string checks through prompt, tool, and Fleet code.

use crate::config::{ProviderKind, RequestPayloadMode, provider_capability};
pub use codewhale_config::catalog::reviewed::ModelFamily as ModelProvider;
use codewhale_config::route::{RouteCapabilities, RouteLimits};

/// Compatibility name for the canonical config-layer three-state fact.
pub use codewhale_config::route::CapabilityState as SupportState;

/// Coarse tool-catalog budget for the selected route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSurfaceBudget {
    /// Keep only the most essential turn-one tool surface eager.
    Compact,
    /// Current default surface: core tools eager, long tail deferred.
    Standard,
    /// Large-window/full-capability routes can afford the standard full head.
    Full,
}

/// Fact provenance for diagnostics and route explanations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactProvenance {
    ResolvedRouteCandidate,
    SeededModelRegistry,
    ConservativeUnknownFallback,
    LegacyProviderFallback,
    UserOverride,
}

/// Provider-agnostic facts owned by the model identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntrinsicCapabilityProfile {
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    pub reasoning: SupportState,
    pub native_tool_calls: SupportState,
    pub parallel_tool_calls: SupportState,
    pub structured_output: SupportState,
    pub streaming: SupportState,
    pub prompt_caching: SupportState,
    pub image_input: SupportState,
    pub tool_surface_budget: ToolSurfaceBudget,
}

/// A model-owned profile. This does not imply a provider route is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelProfile {
    pub canonical_id: String,
    pub display_name: String,
    pub aliases: Vec<String>,
    pub family: Option<ModelProvider>,
    pub capabilities: IntrinsicCapabilityProfile,
    pub provenance: FactProvenance,
}

/// Optional capability overrides layered after provider capability facts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilityOverride {
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    pub reasoning: Option<SupportState>,
    pub native_tool_calls: Option<SupportState>,
    pub parallel_tool_calls: Option<SupportState>,
    pub structured_output: Option<SupportState>,
    pub streaming: Option<SupportState>,
    pub prompt_caching: Option<SupportState>,
    pub image_input: Option<SupportState>,
    pub tool_surface_budget: Option<ToolSurfaceBudget>,
}

/// Capabilities after provider route facts and user overrides are applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityProfile {
    pub provider: ProviderKind,
    pub canonical_model: Option<String>,
    pub wire_model_id: String,
    pub request_payload_mode: RequestPayloadMode,
    pub context_window: Option<u32>,
    pub max_output: Option<u32>,
    pub reasoning: SupportState,
    pub native_tool_calls: SupportState,
    pub parallel_tool_calls: SupportState,
    pub structured_output: SupportState,
    pub streaming: SupportState,
    pub prompt_caching: SupportState,
    pub image_input: SupportState,
    pub tool_surface_budget: ToolSurfaceBudget,
    pub provenance: Vec<FactProvenance>,
}

impl CapabilityProfile {
    #[must_use]
    pub fn supports_reasoning(&self) -> bool {
        self.reasoning.is_supported()
    }

    #[cfg(test)]
    #[must_use]
    pub fn supports_image_input(&self) -> bool {
        self.image_input.is_supported()
    }

    #[cfg(test)]
    #[must_use]
    pub fn has_large_context(&self) -> bool {
        self.context_window.is_some_and(|window| window >= 400_000)
    }

    #[cfg(test)]
    #[must_use]
    pub fn prefers_full_tool_surface(&self) -> bool {
        matches!(self.tool_surface_budget, ToolSurfaceBudget::Full)
    }

    #[cfg(test)]
    #[must_use]
    pub fn suitable_for_broad_fleet_worker(&self) -> bool {
        self.has_large_context()
            && !matches!(self.native_tool_calls, SupportState::Unsupported)
            && matches!(
                self.tool_surface_budget,
                ToolSurfaceBudget::Standard | ToolSurfaceBudget::Full
            )
    }
}

/// Build an intrinsic profile for any model string.
#[must_use]
pub fn model_profile(model: &str) -> ModelProfile {
    let trimmed = model.trim();
    let display_name = display_name(trimmed);
    match codewhale_config::catalog::reviewed::intrinsic_model(trimmed) {
        Some(meta) => {
            let canonical_id = meta
                .canonical_id
                .clone()
                .unwrap_or_else(|| trimmed.to_string());
            let provenance = FactProvenance::SeededModelRegistry;
            ModelProfile {
                canonical_id,
                display_name: meta.display_name.clone().unwrap_or(display_name),
                aliases: meta.aliases,
                family: meta.family,
                capabilities: IntrinsicCapabilityProfile {
                    context_window: meta.context_window,
                    max_output: meta.generation_default.or(meta.max_output),
                    reasoning: SupportState::from_optional_bool(meta.reasoning),
                    native_tool_calls: meta.capabilities.native_tool_calls,
                    parallel_tool_calls: meta.capabilities.parallel_tool_calls,
                    structured_output: meta.capabilities.structured_output,
                    streaming: meta.capabilities.streaming,
                    prompt_caching: meta.capabilities.prompt_caching,
                    image_input: meta.capabilities.image_input,
                    tool_surface_budget: tool_surface_for_window(meta.context_window),
                },
                provenance,
            }
        }
        None => ModelProfile {
            canonical_id: trimmed.to_string(),
            display_name,
            aliases: Vec::new(),
            family: None,
            capabilities: IntrinsicCapabilityProfile {
                context_window: None,
                max_output: None,
                reasoning: SupportState::Unknown,
                native_tool_calls: SupportState::Unknown,
                parallel_tool_calls: SupportState::Unknown,
                structured_output: SupportState::Unknown,
                streaming: SupportState::Unknown,
                prompt_caching: SupportState::Unknown,
                image_input: SupportState::Unknown,
                tool_surface_budget: ToolSurfaceBudget::Compact,
            },
            provenance: FactProvenance::ConservativeUnknownFallback,
        },
    }
}

/// Resolve a profile from legacy model/provider heuristics.
///
/// This remains a picker/startup fallback for call sites that do not yet hold
/// an executable route candidate. Runtime execution should use
/// [`resolved_capability_profile_for_route`], where exact offering facts win.
#[must_use]
pub fn resolved_capability_profile(
    provider: ProviderKind,
    wire_model_id: &str,
) -> CapabilityProfile {
    resolved_capability_profile_with_overrides(
        provider,
        wire_model_id,
        CapabilityOverride::default(),
    )
}

/// Resolve legacy fallback capabilities and apply explicit overrides last.
#[must_use]
pub fn resolved_capability_profile_with_overrides(
    provider: ProviderKind,
    wire_model_id: &str,
    overrides: CapabilityOverride,
) -> CapabilityProfile {
    let model = model_profile(wire_model_id);
    let provider_cap = provider_capability(provider, wire_model_id);
    let request_payload_mode = provider_cap.request_payload_mode;
    let context_window = Some(
        overrides
            .context_window
            .unwrap_or(provider_cap.context_window),
    );
    // An explicit override wins; otherwise carry the compatibility cap through
    // *including its unknown state*, so pickers and diagnostics render `?`
    // rather than a fabricated ceiling.
    let max_output = overrides.max_output.or(provider_cap.max_output);
    let reasoning = overrides
        .reasoning
        .unwrap_or_else(|| bool_state(provider_cap.thinking_supported));
    let prompt_caching = overrides
        .prompt_caching
        .unwrap_or_else(|| bool_state(provider_cap.cache_telemetry_supported));
    let image_input = overrides.image_input.unwrap_or(SupportState::Unknown);
    let native_tool_calls = overrides
        .native_tool_calls
        .unwrap_or_else(|| native_tool_support_for_payload(request_payload_mode));
    let structured_output = overrides
        .structured_output
        .unwrap_or(model.capabilities.structured_output);
    let streaming = overrides.streaming.unwrap_or(SupportState::Supported);
    let parallel_tool_calls = overrides
        .parallel_tool_calls
        .unwrap_or(model.capabilities.parallel_tool_calls);
    let tool_surface_budget = overrides
        .tool_surface_budget
        .unwrap_or_else(|| tool_surface_for_window(context_window));

    let mut provenance = vec![model.provenance, FactProvenance::LegacyProviderFallback];
    if overrides != CapabilityOverride::default() {
        provenance.push(FactProvenance::UserOverride);
    }

    CapabilityProfile {
        provider,
        canonical_model: Some(model.canonical_id),
        wire_model_id: wire_model_id.to_string(),
        request_payload_mode,
        context_window,
        max_output,
        reasoning,
        native_tool_calls,
        parallel_tool_calls,
        structured_output,
        streaming,
        prompt_caching,
        image_input,
        tool_surface_budget,
        provenance,
    }
}

/// Resolve capabilities for an exact route candidate.
///
/// Sourced route facts and limits win. Legacy provider/model behavior is used
/// only for fields the selected offering leaves `Unknown`, and that fallback
/// remains visible in provenance.
#[must_use]
pub fn resolved_capability_profile_for_route(
    provider: ProviderKind,
    wire_model_id: &str,
    route_capabilities: RouteCapabilities,
    route_limits: RouteLimits,
) -> CapabilityProfile {
    let mut profile = resolved_capability_profile(provider, wire_model_id);
    profile.context_window = route_limits
        .context_tokens
        .and_then(|tokens| u32::try_from(tokens).ok())
        .or(profile.context_window);
    profile.max_output = route_limits
        .output_tokens
        .and_then(|tokens| u32::try_from(tokens).ok())
        .or(profile.max_output);
    profile.reasoning = route_fact_or_fallback(route_capabilities.reasoning, profile.reasoning);
    profile.native_tool_calls = route_fact_or_fallback(
        route_capabilities.native_tool_calls,
        profile.native_tool_calls,
    );
    profile.parallel_tool_calls = route_fact_or_fallback(
        route_capabilities.parallel_tool_calls,
        profile.parallel_tool_calls,
    );
    profile.structured_output = route_fact_or_fallback(
        route_capabilities.structured_output,
        profile.structured_output,
    );
    profile.streaming = route_fact_or_fallback(route_capabilities.streaming, profile.streaming);
    profile.prompt_caching =
        route_fact_or_fallback(route_capabilities.prompt_caching, profile.prompt_caching);
    profile.image_input =
        route_fact_or_fallback(route_capabilities.image_input, profile.image_input);
    profile.tool_surface_budget = tool_surface_for_window(profile.context_window);
    profile
        .provenance
        .insert(0, FactProvenance::ResolvedRouteCandidate);
    profile
}

/// Resolve an exact route profile, then apply explicit user/config overrides.
#[must_use]
pub fn resolved_capability_profile_for_route_with_overrides(
    provider: ProviderKind,
    wire_model_id: &str,
    route_capabilities: RouteCapabilities,
    route_limits: RouteLimits,
    overrides: CapabilityOverride,
) -> CapabilityProfile {
    let mut profile = resolved_capability_profile_for_route(
        provider,
        wire_model_id,
        route_capabilities,
        route_limits,
    );
    if let Some(context_window) = overrides.context_window {
        profile.context_window = Some(context_window);
    }
    if let Some(max_output) = overrides.max_output {
        profile.max_output = Some(max_output);
    }
    if let Some(reasoning) = overrides.reasoning {
        profile.reasoning = reasoning;
    }
    if let Some(native_tool_calls) = overrides.native_tool_calls {
        profile.native_tool_calls = native_tool_calls;
    }
    if let Some(parallel_tool_calls) = overrides.parallel_tool_calls {
        profile.parallel_tool_calls = parallel_tool_calls;
    }
    if let Some(structured_output) = overrides.structured_output {
        profile.structured_output = structured_output;
    }
    if let Some(streaming) = overrides.streaming {
        profile.streaming = streaming;
    }
    if let Some(prompt_caching) = overrides.prompt_caching {
        profile.prompt_caching = prompt_caching;
    }
    if let Some(image_input) = overrides.image_input {
        profile.image_input = image_input;
    }
    profile.tool_surface_budget = overrides
        .tool_surface_budget
        .unwrap_or_else(|| tool_surface_for_window(profile.context_window));
    if overrides != CapabilityOverride::default() {
        profile.provenance.push(FactProvenance::UserOverride);
    }
    profile
}

const fn bool_state(value: bool) -> SupportState {
    if value {
        SupportState::Supported
    } else {
        SupportState::Unsupported
    }
}

const fn route_fact_or_fallback(route_fact: SupportState, fallback: SupportState) -> SupportState {
    match route_fact {
        SupportState::Unknown => fallback,
        sourced => sourced,
    }
}

#[must_use]
pub fn tool_surface_for_window(context_window: Option<u32>) -> ToolSurfaceBudget {
    match context_window {
        Some(window) if window >= 400_000 => ToolSurfaceBudget::Full,
        Some(window) if window >= 128_000 => ToolSurfaceBudget::Standard,
        _ => ToolSurfaceBudget::Compact,
    }
}

fn native_tool_support_for_payload(mode: RequestPayloadMode) -> SupportState {
    match mode {
        RequestPayloadMode::ChatCompletions
        | RequestPayloadMode::Responses
        | RequestPayloadMode::AnthropicMessages => SupportState::Supported,
    }
}

fn display_name(model: &str) -> String {
    model
        .rsplit(['/', ':'])
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(model)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_profile_known_lookup_uses_seeded_model_facts() {
        let profile = model_profile("deepseek-v4-pro");

        assert_eq!(profile.canonical_id, "deepseek-v4-pro");
        assert_eq!(profile.family, Some(ModelProvider::DeepSeek));
        assert_eq!(profile.capabilities.context_window, Some(1_000_000));
        assert_eq!(profile.capabilities.max_output, Some(384_000));
        assert_eq!(profile.capabilities.reasoning, SupportState::Supported);
        assert_eq!(
            profile.capabilities.tool_surface_budget,
            ToolSurfaceBudget::Full
        );
        assert_eq!(profile.provenance, FactProvenance::SeededModelRegistry);
    }

    #[test]
    fn model_profile_unknown_fallback_is_conservative() {
        let profile = model_profile("custom-local-model");

        assert_eq!(profile.canonical_id, "custom-local-model");
        assert_eq!(profile.family, None);
        assert_eq!(profile.capabilities.context_window, None);
        assert_eq!(profile.capabilities.reasoning, SupportState::Unknown);
        assert_eq!(
            profile.capabilities.native_tool_calls,
            SupportState::Unknown
        );
        assert_eq!(
            profile.capabilities.tool_surface_budget,
            ToolSurfaceBudget::Compact
        );
        assert_eq!(
            profile.provenance,
            FactProvenance::ConservativeUnknownFallback
        );
    }

    #[test]
    fn resolved_capability_profile_merges_provider_facts_and_overrides() {
        let profile = resolved_capability_profile_with_overrides(
            ProviderKind::OpenaiCodex,
            "gpt-5-codex",
            CapabilityOverride {
                context_window: Some(123_456),
                reasoning: Some(SupportState::Unsupported),
                tool_surface_budget: Some(ToolSurfaceBudget::Compact),
                ..CapabilityOverride::default()
            },
        );

        assert_eq!(profile.provider, ProviderKind::OpenaiCodex);
        assert_eq!(profile.request_payload_mode, RequestPayloadMode::Responses);
        assert_eq!(profile.context_window, Some(123_456));
        assert_eq!(profile.reasoning, SupportState::Unsupported);
        assert_eq!(profile.native_tool_calls, SupportState::Supported);
        assert_eq!(profile.tool_surface_budget, ToolSurfaceBudget::Compact);
        assert!(profile.provenance.contains(&FactProvenance::UserOverride));
    }

    #[test]
    fn capability_predicates_are_not_provider_string_checks() {
        let broad = resolved_capability_profile(ProviderKind::Deepseek, "deepseek-v4-pro");
        let compact = resolved_capability_profile_with_overrides(
            ProviderKind::Openrouter,
            "unknown-small-model",
            CapabilityOverride {
                context_window: Some(32_000),
                native_tool_calls: Some(SupportState::Unknown),
                tool_surface_budget: Some(ToolSurfaceBudget::Compact),
                ..CapabilityOverride::default()
            },
        );

        assert!(broad.has_large_context());
        assert!(broad.prefers_full_tool_surface());
        assert!(broad.suitable_for_broad_fleet_worker());
        assert!(!compact.has_large_context());
        assert!(!compact.prefers_full_tool_surface());
        assert!(!compact.suitable_for_broad_fleet_worker());
    }

    #[test]
    fn exact_route_facts_override_legacy_provider_heuristics() {
        let profile = resolved_capability_profile_for_route(
            ProviderKind::Openai,
            "gpt-5.4",
            RouteCapabilities {
                reasoning: SupportState::Unsupported,
                native_tool_calls: SupportState::Unsupported,
                structured_output: SupportState::Supported,
                ..RouteCapabilities::default()
            },
            RouteLimits {
                context_tokens: Some(42_000),
                input_tokens: None,
                output_tokens: Some(7_000),
            },
        );

        assert_eq!(profile.context_window, Some(42_000));
        assert_eq!(profile.max_output, Some(7_000));
        assert_eq!(profile.reasoning, SupportState::Unsupported);
        assert_eq!(profile.native_tool_calls, SupportState::Unsupported);
        assert_eq!(profile.image_input, SupportState::Unknown);
        assert_eq!(profile.structured_output, SupportState::Supported);
        assert!(
            profile
                .provenance
                .starts_with(&[FactProvenance::ResolvedRouteCandidate])
        );
        assert!(
            profile
                .provenance
                .contains(&FactProvenance::LegacyProviderFallback)
        );
    }

    #[test]
    fn explicit_override_wins_after_exact_route_fact() {
        let profile = resolved_capability_profile_for_route_with_overrides(
            ProviderKind::Openai,
            "gpt-5.4",
            RouteCapabilities {
                reasoning: SupportState::Unsupported,
                ..RouteCapabilities::default()
            },
            RouteLimits::default(),
            CapabilityOverride {
                reasoning: Some(SupportState::Supported),
                ..CapabilityOverride::default()
            },
        );

        assert_eq!(profile.reasoning, SupportState::Supported);
        assert_eq!(
            profile.provenance.last(),
            Some(&FactProvenance::UserOverride)
        );
    }

    #[test]
    fn image_input_route_fact_and_override_are_explicit() {
        let sourced = resolved_capability_profile_for_route(
            ProviderKind::Openai,
            "vision-fixture",
            RouteCapabilities {
                image_input: SupportState::Supported,
                ..RouteCapabilities::default()
            },
            RouteLimits::default(),
        );
        assert!(sourced.supports_image_input());

        let overridden = resolved_capability_profile_for_route_with_overrides(
            ProviderKind::Openai,
            "vision-fixture",
            RouteCapabilities::default(),
            RouteLimits::default(),
            CapabilityOverride {
                image_input: Some(SupportState::Supported),
                ..CapabilityOverride::default()
            },
        );
        assert!(overridden.supports_image_input());
    }
}

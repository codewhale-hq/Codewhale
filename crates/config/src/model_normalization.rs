use crate::base_url_resolution::provider_preserves_custom_base_url_model;
use crate::descriptors::provider_passes_model_through;
use crate::provider_kind::ProviderKind;

#[must_use]
pub fn legacy_deepseek_alias_reasoning_effort(model: &str) -> Option<&'static str> {
    match model.trim().to_ascii_lowercase().as_str() {
        "deepseek-chat" => Some("off"),
        "deepseek-reasoner" => Some("high"),
        _ => None,
    }
}

pub(crate) fn root_default_model_is_foreign_to_provider(
    provider: ProviderKind,
    model: &str,
    base_url: &str,
) -> bool {
    if model.trim().eq_ignore_ascii_case("auto")
        || crate::route::resolve_model(ProviderKind::Deepseek, model, None).is_err()
    {
        return false;
    }
    if matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) {
        return false;
    }
    if provider_preserves_custom_base_url_model(provider, base_url) {
        return false;
    }
    if matches!(
        provider,
        ProviderKind::Xai | ProviderKind::Openai | ProviderKind::Moonshot
    ) {
        return true;
    }
    if provider_passes_model_through(provider) {
        return false;
    }
    if matches!(
        provider,
        ProviderKind::NvidiaNim
            | ProviderKind::Openrouter
            | ProviderKind::Orcarouter
            | ProviderKind::Novita
            | ProviderKind::Fireworks
            | ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Deepinfra
            | ProviderKind::Together
            | ProviderKind::Sglang
            | ProviderKind::Vllm
            | ProviderKind::Volcengine
            | ProviderKind::Atlascloud
            | ProviderKind::OpencodeGo
            | ProviderKind::WanjieArk
            | ProviderKind::Huggingface
    ) {
        return false;
    }
    true
}

/// A provider owner that Codewhale can identify with high confidence when an
/// official route is handed a foreign model id.
///
/// This intentionally reuses the conservative stale-root-model guard instead
/// of treating the partial provider catalog as a closed-world allowlist.
/// Unknown ids, custom endpoints, local runtimes, and multi-model gateways
/// therefore remain provider-authoritative.
#[must_use]
pub fn known_foreign_model_owner(
    provider: ProviderKind,
    model: &str,
    base_url: &str,
) -> Option<ProviderKind> {
    root_default_model_is_foreign_to_provider(provider, model, base_url)
        .then_some(ProviderKind::Deepseek)
}

pub(crate) fn default_model_for_provider(provider: ProviderKind) -> String {
    crate::catalog::bundled_listed_default(provider.as_str())
        .unwrap_or_else(|| provider.provider().default_model().to_string())
}

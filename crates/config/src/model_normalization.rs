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

/// Whether a root `default_text_model` would be foreign to the active
/// provider's endpoint, i.e. honouring it would send an id the endpoint cannot
/// serve.
///
/// This is the narrow case the old `provider == Deepseek` gate was covering by
/// accident: a user switches `provider` and leaves a DeepSeek id behind in
/// `default_text_model`. Forwarding `deepseek-chat` to Z.ai fails every
/// request, so the root default is dropped and the provider default used
/// instead — matching the decision `Config::default_model()` makes via
/// `root_deepseek_model_is_foreign_to_direct_provider`
/// (`crates/tui/src/config.rs`), whose provider lists this mirrors.
pub(crate) fn root_default_model_is_foreign_to_provider(
    provider: ProviderKind,
    model: &str,
    base_url: &str,
) -> bool {
    // Not a DeepSeek id at all: nothing to protect against here. A model the
    // provider does not serve for some other reason is the provider's error to
    // report, not ours to silently rewrite.
    if model.trim().eq_ignore_ascii_case("auto")
        || crate::route::resolve_model(ProviderKind::Deepseek, model, None).is_err()
    {
        return false;
    }
    // DeepSeek's own endpoints serve DeepSeek ids.
    if matches!(
        provider,
        ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
    ) {
        return false;
    }
    // A custom base URL may be any OpenAI-compatible proxy, and a proxy may
    // legitimately serve DeepSeek ids (#1519). Full pass-through.
    if provider_preserves_custom_base_url_model(provider, base_url) {
        return false;
    }
    // Vendor-locked official endpoints. These pass model ids through, but
    // api.x.ai will never answer to `deepseek-v4-pro`, so pass-through does not
    // make the id servable — this is the #3227 contamination case.
    if matches!(
        provider,
        ProviderKind::Xai | ProviderKind::Openai | ProviderKind::Moonshot
    ) {
        return true;
    }
    // Remaining pass-through providers forward the id verbatim to a service
    // that is the authority on its own catalog.
    if provider_passes_model_through(provider) {
        return false;
    }
    // Aggregators, local runtimes, and multi-vendor clouds host DeepSeek
    // models under their own catalogs, so a DeepSeek id is valid there.
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
            | ProviderKind::XiaomiMimo
    ) {
        return false;
    }
    // Everything else is a vendor serving only its own family (Z.ai, Stepfun,
    // MiniMax, Anthropic, …): a DeepSeek id there is the stale-config case.
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

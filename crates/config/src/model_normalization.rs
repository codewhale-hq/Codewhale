use crate::base_url_resolution::provider_preserves_custom_base_url_model;
use crate::opencode_go::opencode_go_model_id;
use crate::provider_defaults::*;
use crate::provider_kind::ProviderKind;

/// Canonical id for a DeepSeek-family model name, or `None` for anything else.
///
/// Kept behaviourally identical to `normalize_model_name` in
/// `crates/tui/src/config.rs`, which is the definition the TUI's own model
/// chain uses. It exists here only so this crate can answer "is this root
/// default a DeepSeek id?" without depending on the TUI.
pub(crate) fn deepseek_family_model_id(model: &str) -> Option<String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.to_ascii_lowercase();
    if let Some(canonical) =
        crate::catalog::reviewed::compatibility_alias("canonical_model_name", &normalized)
    {
        return Some(canonical.to_string());
    }
    if !normalized.starts_with("deepseek") && !normalized.contains("/deepseek") {
        return None;
    }
    if trimmed
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/'))
    {
        return Some(trimmed.to_string());
    }
    None
}

/// Providers whose model id is forwarded verbatim, because the upstream
/// service — not this crate — is the authority on what ids it serves.
///
/// Mirrors `provider_passes_model_through` in `crates/tui/src/config.rs`.
pub(crate) fn provider_passes_model_through(provider: ProviderKind) -> bool {
    matches!(
        provider,
        ProviderKind::Openai
            | ProviderKind::Atlascloud
            | ProviderKind::WanjieArk
            | ProviderKind::Volcengine
            | ProviderKind::XiaomiMimo
            | ProviderKind::Moonshot
            | ProviderKind::Qianfan
            | ProviderKind::Openmodel
            | ProviderKind::Ollama
            | ProviderKind::OllamaCloud
            | ProviderKind::Huggingface
            | ProviderKind::Modelscope
            | ProviderKind::Meta
            | ProviderKind::Xai
            | ProviderKind::Telecomjs
            | ProviderKind::Edenai
            | ProviderKind::Zenmux
            | ProviderKind::Csdn
            | ProviderKind::Concentrate
            | ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic
            | ProviderKind::Custom
    )
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
    if deepseek_family_model_id(model).is_none() {
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

pub(crate) fn normalize_model_for_provider(provider: ProviderKind, model: &str) -> String {
    if matches!(provider, ProviderKind::OpencodeGo) {
        // Canonicalize documented model ids. Unknown ids
        // must never be rewritten to the provider default — substituting a
        // different model is worse than letting the route layer reject the
        // request by the name the user actually configured.
        return opencode_go_model_id(model)
            .map(str::to_string)
            .unwrap_or_else(|| model.trim().to_string());
    }
    if matches!(provider, ProviderKind::XiaomiMimo)
        && let Some(canonical) = canonical_xiaomi_mimo_model_id(model)
    {
        return canonical.to_string();
    }
    if matches!(
        provider,
        ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
    ) && let Some(canonical) = canonical_minimax_model_id(model)
    {
        return canonical.to_string();
    }
    if matches!(provider, ProviderKind::Zai)
        && let Some(canonical) = canonical_zai_model_id(model)
    {
        return canonical.to_string();
    }

    if matches!(
        provider,
        ProviderKind::Atlascloud
            | ProviderKind::WanjieArk
            | ProviderKind::Volcengine
            | ProviderKind::XiaomiMimo
            | ProviderKind::Zai
            | ProviderKind::Stepfun
            | ProviderKind::Minimax
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::Qianfan
            | ProviderKind::Ollama
            | ProviderKind::OllamaCloud
            | ProviderKind::Meta
            | ProviderKind::Xai
    ) {
        return model.to_string();
    }

    let normalized = model.trim().to_ascii_lowercase();
    if provider == ProviderKind::Openrouter
        && let Some(canonical) = canonical_openrouter_recent_model_id(&normalized)
    {
        return canonical.to_string();
    }
    if provider == ProviderKind::Orcarouter
        && let Some(canonical) = canonical_orcarouter_recent_model_id(&normalized)
    {
        return canonical.to_string();
    }
    match (provider, normalized.as_str()) {
        (ProviderKind::NvidiaNim, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_NVIDIA_NIM_MODEL.to_string()
        }
        (
            ProviderKind::NvidiaNim,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-reasoner" | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_NVIDIA_NIM_FLASH_MODEL.to_string(),
        (ProviderKind::Openrouter, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_OPENROUTER_MODEL.to_string()
        }
        (
            ProviderKind::Openrouter,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-reasoner" | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_OPENROUTER_FLASH_MODEL.to_string(),
        (ProviderKind::Orcarouter, "deepseek-v4-pro" | "deepseek-v4pro") => {
            "deepseek/deepseek-v4-pro".to_string()
        }
        (
            ProviderKind::Orcarouter,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-reasoner" | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_ORCAROUTER_FLASH_MODEL.to_string(),
        (ProviderKind::Novita, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_NOVITA_MODEL.to_string()
        }
        (
            ProviderKind::Novita,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-reasoner" | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_NOVITA_FLASH_MODEL.to_string(),
        (ProviderKind::Fireworks, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_FIREWORKS_MODEL.to_string()
        }
        (ProviderKind::Fireworks, "deepseek-flash") => "deepseek-v4-flash".to_string(),
        (
            ProviderKind::Siliconflow | ProviderKind::SiliconflowCN,
            "deepseek-v4-pro" | "deepseek-v4pro" | "deepseek-reasoner" | "deepseek-r1",
        ) => DEFAULT_SILICONFLOW_MODEL.to_string(),
        (
            ProviderKind::Siliconflow | ProviderKind::SiliconflowCN,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-v3",
        ) => DEFAULT_SILICONFLOW_FLASH_MODEL.to_string(),
        (
            ProviderKind::Arcee,
            "trinity" | "arcee-trinity" | "trinity-large-thinking" | "arcee-trinity-large-thinking",
        ) => DEFAULT_ARCEE_MODEL.to_string(),
        (ProviderKind::Arcee, "trinity-mini" | "arcee-trinity-mini") => {
            ARCEE_TRINITY_MINI_MODEL.to_string()
        }
        (ProviderKind::Arcee, "arcee-trinity-large-preview") => {
            ARCEE_TRINITY_LARGE_PREVIEW_MODEL.to_string()
        }
        (
            ProviderKind::Moonshot,
            "kimi"
            | "kimi-k2"
            | "kimi-k2.7"
            | "kimi-k2-7"
            | "kimi-k2.7-code"
            | "kimi-k2-7-code"
            | "kimi-code"
            | "moonshot-kimi-k2.7-code",
        ) => DEFAULT_MOONSHOT_MODEL.to_string(),
        (ProviderKind::Moonshot, "kimi-k2.6" | "kimi-k2-6" | "moonshot-kimi-k2.6") => {
            MOONSHOT_KIMI_K2_6_MODEL.to_string()
        }
        (ProviderKind::Sglang, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_SGLANG_MODEL.to_string()
        }
        (
            ProviderKind::Sglang,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-reasoner" | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_SGLANG_FLASH_MODEL.to_string(),
        (ProviderKind::Vllm, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_VLLM_MODEL.to_string()
        }
        (
            ProviderKind::Vllm,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-reasoner" | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_VLLM_FLASH_MODEL.to_string(),
        (ProviderKind::Huggingface, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_HUGGINGFACE_MODEL.to_string()
        }
        (
            ProviderKind::Huggingface,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-reasoner" | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_HUGGINGFACE_FLASH_MODEL.to_string(),
        (ProviderKind::Together, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_TOGETHER_MODEL.to_string()
        }
        (
            ProviderKind::Together,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-reasoner" | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_TOGETHER_FLASH_MODEL.to_string(),
        (ProviderKind::Deepinfra, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_DEEPINFRA_MODEL.to_string()
        }
        (
            ProviderKind::Deepinfra,
            "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat"
            | "deepseek-reasoner" | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_DEEPINFRA_FLASH_MODEL.to_string(),
        _ => model.to_string(),
    }
}

pub(crate) fn canonical_xiaomi_mimo_model_id(model: &str) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    let normalized = normalized.replace(['_', ' '], "-");
    match normalized.as_str() {
        "mimo"
        | DEFAULT_XIAOMI_MIMO_MODEL
        | "mimo-v2-6-pro"
        | "xiaomi-mimo-v2.6-pro"
        | "xiaomi-mimo-v2-6-pro"
        | "mimo-v2.5-pro"
        | "mimo-v2-5-pro"
        | "xiaomi-mimo-v2.5-pro"
        | "xiaomi-mimo-v2-5-pro" => Some(DEFAULT_XIAOMI_MIMO_MODEL),
        XIAOMI_MIMO_V2_5_PRO_ULTRASPEED_MODEL
        | "mimo-v2-5-pro-ultraspeed"
        | "xiaomi-mimo-v2.5-pro-ultraspeed"
        | "xiaomi-mimo-v2-5-pro-ultraspeed"
        | "ultraspeed"
        | "pro-ultraspeed" => Some(XIAOMI_MIMO_V2_5_PRO_ULTRASPEED_MODEL),
        XIAOMI_MIMO_V2_6_FLASH_MODEL
        | "mimo-v2-6-flash"
        | "xiaomi-mimo-v2.6-flash"
        | "xiaomi-mimo-v2-6-flash"
        | "omni"
        | "mimo-omni"
        | "v2.5-omni"
        | "v25-omni"
        | "mimo-v2.5"
        | "mimo-v25"
        | "mimo-v2-5"
        | "mimo-v2.5-omni"
        | "mimo-v25-omni"
        | "mimo-v2-5-omni"
        | "xiaomi-mimo-v2.5"
        | "xiaomi-mimo-v2-5"
        | "xiaomi-mimo-v2.5-omni"
        | "xiaomi-mimo-v2-5-omni" => Some(XIAOMI_MIMO_V2_6_FLASH_MODEL),
        "asr" | "mimo-asr" | "mimo-v2.5-asr" | "speech-to-text" | "transcribe" => {
            Some(XIAOMI_MIMO_ASR_MODEL)
        }
        "mimo-tts" | "mimo-v25-tts" | "mimo-v2.5-tts" | "tts" | "speech" => {
            Some(XIAOMI_MIMO_TTS_MODEL)
        }
        "mimo-tts-voicedesign"
        | "mimo-voice-design"
        | "mimo-v25-tts-voicedesign"
        | "mimo-v2.5-tts-voicedesign"
        | "voicedesign"
        | "voice-design" => Some(XIAOMI_MIMO_TTS_VOICE_DESIGN_MODEL),
        "mimo-tts-voiceclone"
        | "mimo-voice-clone"
        | "mimo-v25-tts-voiceclone"
        | "mimo-v2.5-tts-voiceclone"
        | "voiceclone"
        | "voice-clone" => Some(XIAOMI_MIMO_TTS_VOICE_CLONE_MODEL),
        "mimo-v2-tts" => Some(XIAOMI_MIMO_V2_TTS_MODEL),
        _ => None,
    }
}

pub(crate) fn canonical_minimax_model_id(model: &str) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    let normalized = normalized.replace(['_', ' '], "-");
    match normalized.as_str() {
        "minimax" | "minimax-m3" | "minimax-m-3" | "minimax-m-3-thinking" => {
            Some(DEFAULT_MINIMAX_MODEL)
        }
        "minimax-m2.7" | "minimax-m2-7" | "minimax-m-2.7" | "minimax-m-2-7" => {
            Some(MINIMAX_M2_7_MODEL)
        }
        "minimax-m2.7-highspeed"
        | "minimax-m2-7-highspeed"
        | "minimax-m-2.7-highspeed"
        | "minimax-m-2-7-highspeed" => Some(MINIMAX_M2_7_HIGHSPEED_MODEL),
        "minimax-m2.5" | "minimax-m2-5" | "minimax-m-2.5" | "minimax-m-2-5" => {
            Some(MINIMAX_M2_5_MODEL)
        }
        "minimax-m2.5-highspeed"
        | "minimax-m2-5-highspeed"
        | "minimax-m-2.5-highspeed"
        | "minimax-m-2-5-highspeed" => Some(MINIMAX_M2_5_HIGHSPEED_MODEL),
        "minimax-m2.1" | "minimax-m2-1" | "minimax-m-2.1" | "minimax-m-2-1" => {
            Some(MINIMAX_M2_1_MODEL)
        }
        "minimax-m2.1-highspeed"
        | "minimax-m2-1-highspeed"
        | "minimax-m-2.1-highspeed"
        | "minimax-m-2-1-highspeed" => Some(MINIMAX_M2_1_HIGHSPEED_MODEL),
        "minimax-m2" | "minimax-m-2" => Some(MINIMAX_M2_MODEL),
        _ => None,
    }
}

pub(crate) fn canonical_zai_model_id(model: &str) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    let normalized = normalized.replace(['_', ' '], "-");
    match normalized.as_str() {
        "glm-5.1" | "glm-5-1" | "zai-glm-5.1" | "zai-glm-5-1" => Some(ZAI_GLM_5_1_MODEL),
        // Every alias resolves to its own id, never through DEFAULT_ZAI_MODEL:
        // moving the default (now GLM-5.3) must not silently re-point an
        // explicit GLM-5.2 route.
        "glm-5.2" | "glm-5-2" | "zai-glm-5.2" | "zai-glm-5-2" => Some(ZAI_GLM_5_2_MODEL),
        "glm-5.3-flash" | "glm-5-3-flash" | "zai-glm-5.3-flash" | "zai-glm-5-3-flash" => {
            Some(ZAI_GLM_5_3_FLASH_MODEL)
        }
        "glm-5.3" | "glm-5-3" | "zai-glm-5.3" | "zai-glm-5-3" => Some(ZAI_GLM_5_3_MODEL),
        "glm-5-turbo" | "glm-5turbo" | "zai-glm-5-turbo" => Some(ZAI_GLM_5_TURBO_MODEL),
        _ => None,
    }
}

pub(crate) fn canonical_openrouter_recent_model_id(model: &str) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    let normalized = normalized.replace(['_', ' '], "-");
    match normalized.as_str() {
        OPENROUTER_ARCEE_TRINITY_LARGE_THINKING_MODEL
        | "trinity"
        | "trinity-large-thinking"
        | "arcee-trinity"
        | "arcee-trinity-large-thinking" => Some(OPENROUTER_ARCEE_TRINITY_LARGE_THINKING_MODEL),
        OPENROUTER_GEMMA_4_31B_MODEL | "gemma-4-31b" | "gemma-4-31b-it" => {
            Some(OPENROUTER_GEMMA_4_31B_MODEL)
        }
        OPENROUTER_GEMMA_4_26B_A4B_MODEL | "gemma-4-26b-a4b" | "gemma-4-26b-a4b-it" => {
            Some(OPENROUTER_GEMMA_4_26B_A4B_MODEL)
        }
        OPENROUTER_GLM_5_1_MODEL | "glm-5.1" | "glm-5-1" | "zai-glm-5.1" | "zai-glm-5-1" => {
            Some(OPENROUTER_GLM_5_1_MODEL)
        }
        OPENROUTER_GLM_5_2_MODEL | "glm-5.2" | "glm-5-2" | "zai-glm-5.2" | "zai-glm-5-2" => {
            Some(OPENROUTER_GLM_5_2_MODEL)
        }
        OPENROUTER_GLM_5_3_FLASH_MODEL
        | "glm-5.3-flash"
        | "glm-5-3-flash"
        | "zai-glm-5.3-flash"
        | "zai-glm-5-3-flash" => Some(OPENROUTER_GLM_5_3_FLASH_MODEL),
        OPENROUTER_GLM_5_3_MODEL | "glm-5.3" | "glm-5-3" | "zai-glm-5.3" | "zai-glm-5-3" => {
            Some(OPENROUTER_GLM_5_3_MODEL)
        }
        OPENROUTER_KIMI_K2_7_CODE_MODEL
        | "kimi"
        | "kimi-k2"
        | "kimi-k2.7"
        | "kimi-k2-7"
        | "kimi-k2.7-code"
        | "kimi-k2-7-code"
        | "kimi-code"
        | "moonshot-kimi-k2.7-code"
        | "openrouter-kimi-k2.7-code" => Some(OPENROUTER_KIMI_K2_7_CODE_MODEL),
        OPENROUTER_KIMI_K2_6_MODEL | "kimi-k2.6" | "kimi-k2-6" | "moonshot-kimi-k2.6" => {
            Some(OPENROUTER_KIMI_K2_6_MODEL)
        }
        OPENROUTER_MINIMAX_M3_MODEL | "minimax-m3" | "minimax-m-3" => {
            Some(OPENROUTER_MINIMAX_M3_MODEL)
        }
        OPENROUTER_MINIMAX_M2_7_MODEL
        | "minimax-2.7"
        | "minimax-2-7"
        | "minimax-m2.7"
        | "minimax-m2-7"
        | "minimax-m-2.7"
        | "minimax-m-2-7" => Some(OPENROUTER_MINIMAX_M2_7_MODEL),
        OPENROUTER_NEMOTRON_3_NANO_OMNI_MODEL
        | "nemotron-3-nano-omni"
        | "nemotron-3-nano-omni-reasoning" => Some(OPENROUTER_NEMOTRON_3_NANO_OMNI_MODEL),
        OPENROUTER_QWEN_3_6_35B_A3B_MODEL
        | "qwen3.6-35b-a3b"
        | "qwen-3.6-35b-a3b"
        | "qwen3-6-35b-a3b" => Some(OPENROUTER_QWEN_3_6_35B_A3B_MODEL),
        OPENROUTER_QWEN_3_6_FLASH_MODEL | "qwen3.6-flash" | "qwen-3.6-flash" => {
            Some(OPENROUTER_QWEN_3_6_FLASH_MODEL)
        }
        OPENROUTER_QWEN_3_6_MAX_PREVIEW_MODEL
        | "qwen3.6-max-preview"
        | "qwen-3.6-max-preview"
        | "qwen-max-preview" => Some(OPENROUTER_QWEN_3_6_MAX_PREVIEW_MODEL),
        OPENROUTER_QWEN_3_6_27B_MODEL | "qwen3.6-27b" | "qwen-3.6-27b" | "qwen3-6-27b" => {
            Some(OPENROUTER_QWEN_3_6_27B_MODEL)
        }
        OPENROUTER_QWEN_3_6_PLUS_MODEL | "qwen3.6-plus" | "qwen-3.6-plus" => {
            Some(OPENROUTER_QWEN_3_6_PLUS_MODEL)
        }
        OPENROUTER_QWEN_3_7_PLUS_MODEL | "qwen3.7-plus" | "qwen-3.7-plus" => {
            Some(OPENROUTER_QWEN_3_7_PLUS_MODEL)
        }
        OPENROUTER_QWEN_3_7_MAX_MODEL | "qwen3.7-max" | "qwen-3.7-max" => {
            Some(OPENROUTER_QWEN_3_7_MAX_MODEL)
        }
        OPENROUTER_QWEN_3_8_FLASH_MODEL | "qwen3.8-flash" | "qwen-3.8-flash" => {
            Some(OPENROUTER_QWEN_3_8_FLASH_MODEL)
        }
        OPENROUTER_TENCENT_HY3_PREVIEW_MODEL
        | "hy3-preview"
        | "tencent-hy3-preview"
        | "hy3"
        | "hunyuan"
        | "tencent-hunyuan"
        | "hunyuan-hy3" => Some(OPENROUTER_TENCENT_HY3_PREVIEW_MODEL),
        OPENROUTER_XIAOMI_MIMO_V2_5_PRO_MODEL
        | "mimo-v2.5-pro"
        | "mimo-v2-5-pro"
        | "xiaomi-mimo-v2.5-pro"
        | "xiaomi-mimo-v2-5-pro" => Some(OPENROUTER_XIAOMI_MIMO_V2_5_PRO_MODEL),
        OPENROUTER_XIAOMI_MIMO_V2_5_MODEL
        | "mimo-v2.5"
        | "mimo-v2-5"
        | "xiaomi-mimo-v2.5"
        | "xiaomi-mimo-v2-5" => Some(OPENROUTER_XIAOMI_MIMO_V2_5_MODEL),
        _ => None,
    }
}

/// Canonical id resolution for OrcaRouter's own auto-routing model.
///
/// OrcaRouter is an aggregator whose upstream catalog uses the same
/// namespaced ids as OpenRouter, so those ids pass through verbatim. The one
/// OrcaRouter-specific alias worth normalizing is its `orcarouter/auto`
/// router, which is not an upstream model and needs the bare `auto` spelling
/// (as users naturally type it) to resolve to the namespaced wire id.
pub(crate) fn canonical_orcarouter_recent_model_id(model: &str) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    let normalized = normalized.replace(['_', ' '], "-");
    match normalized.as_str() {
        ORCAROUTER_AUTO_MODEL | "auto" | "orcarouter-auto" | "orca-auto" => {
            Some(ORCAROUTER_AUTO_MODEL)
        }
        _ => None,
    }
}

pub(crate) fn default_model_for_provider(provider: ProviderKind) -> &'static str {
    provider.provider().default_model()
}

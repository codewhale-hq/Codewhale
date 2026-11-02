use crate::provider;
use crate::provider_defaults::*;
use crate::provider_kind::ProviderKind;
use crate::runtime_options::base_url_uses_local_host;
use codewhale_secrets::{SecretSource, Secrets};

pub(crate) fn default_base_url_for_provider(provider: ProviderKind) -> &'static str {
    provider.provider().default_base_url()
}

pub(crate) fn moonshot_base_url_uses_kimi_code(base_url: &str) -> bool {
    let normalized = base_url.trim_end_matches('/').to_ascii_lowercase();
    normalized == DEFAULT_KIMI_CODE_BASE_URL
        || normalized == "https://api.kimi.com/coding"
        || normalized.starts_with("https://api.kimi.com/coding/")
}

/// Dual-wire vendors: dialect is config (`wire`), not a separate ProviderKind.
pub(crate) fn wire_prefers_anthropic(kind: ProviderKind, wire: Option<&str>) -> bool {
    if matches!(
        kind,
        ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlanAnthropic
    ) {
        return true;
    }
    provider::wire_dialect_prefers_anthropic(wire)
}

pub(crate) fn modelstudio_mode_is_coding_plan(kind: ProviderKind, mode: Option<&str>) -> bool {
    if matches!(
        kind,
        ProviderKind::ModelstudioCodingPlan | ProviderKind::ModelstudioCodingPlanAnthropic
    ) {
        return true;
    }
    let Some(raw) = mode.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let normalized = raw.to_ascii_lowercase().replace(['_', ' '], "-");
    matches!(
        normalized.as_str(),
        "coding-plan" | "coding" | "codingplan" | "dashscope-coding" | "code"
    )
}

pub(crate) fn is_modelstudio_family(kind: ProviderKind) -> bool {
    matches!(
        kind,
        ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic
    )
}

pub(crate) fn resolve_modelstudio_base_url(
    configured: Option<String>,
    kind: ProviderKind,
    mode: Option<&str>,
    wire: Option<&str>,
) -> String {
    if let Some(url) = configured.filter(|value| !value.trim().is_empty()) {
        return url;
    }
    let coding = modelstudio_mode_is_coding_plan(kind, mode);
    let anthropic = wire_prefers_anthropic(kind, wire);
    match (coding, anthropic) {
        (true, true) => MODELSTUDIO_CODING_PLAN_ANTHROPIC_BASE_URL.to_string(),
        (true, false) => DEFAULT_MODELSTUDIO_CODING_PLAN_BASE_URL.to_string(),
        (false, true) => MODELSTUDIO_TOKEN_PLAN_ANTHROPIC_BASE_URL.to_string(),
        (false, false) => DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL.to_string(),
    }
}

pub(crate) fn resolve_minimax_base_url(
    configured: Option<String>,
    kind: ProviderKind,
    wire: Option<&str>,
) -> String {
    if let Some(url) = configured.filter(|value| !value.trim().is_empty()) {
        return url;
    }
    if wire_prefers_anthropic(kind, wire) {
        DEFAULT_MINIMAX_ANTHROPIC_BASE_URL.to_string()
    } else {
        DEFAULT_MINIMAX_BASE_URL.to_string()
    }
}

pub(crate) fn resolve_deepseek_base_url(
    configured: Option<String>,
    kind: ProviderKind,
    wire: Option<&str>,
) -> String {
    if let Some(url) = configured.filter(|value| !value.trim().is_empty()) {
        return url;
    }
    if wire_prefers_anthropic(kind, wire) {
        DEFAULT_DEEPSEEK_ANTHROPIC_BASE_URL.to_string()
    } else {
        DEFAULT_DEEPSEEK_BASE_URL.to_string()
    }
}

pub fn xiaomi_mimo_base_url_for_mode(mode: &str) -> Option<&'static str> {
    let normalized = mode.trim().to_ascii_lowercase().replace(['_', ' '], "-");
    if normalized.is_empty() || xiaomi_mimo_mode_uses_standard_endpoint(&normalized) {
        return None;
    }
    Some(match normalized.as_str() {
        "token-plan" | "tokenplan" | "subscription" | "subscribed" | "plan" => {
            DEFAULT_XIAOMI_MIMO_BASE_URL
        }
        "token-plan-cn"
        | "token-plan-china"
        | "token-plan-mainland"
        | "token-plan-mainland-china"
        | "cn"
        | "china" => XIAOMI_MIMO_TOKEN_PLAN_CN_BASE_URL,
        "token-plan-sgp"
        | "token-plan-sg"
        | "token-plan-singapore"
        | "sgp"
        | "sg"
        | "singapore" => XIAOMI_MIMO_TOKEN_PLAN_SGP_BASE_URL,
        "token-plan-ams"
        | "token-plan-eu"
        | "token-plan-europe"
        | "token-plan-amsterdam"
        | "ams"
        | "eu"
        | "europe"
        | "amsterdam" => XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL,
        _ => DEFAULT_XIAOMI_MIMO_BASE_URL,
    })
}

pub fn xiaomi_mimo_mode_uses_standard_endpoint(normalized_mode: &str) -> bool {
    matches!(
        normalized_mode,
        "standard" | "default" | "payg" | "paygo" | "pay-as-you-go" | "pay-as-go"
    )
}

pub(crate) fn xiaomi_mimo_base_url_uses_token_plan(base_url: &str) -> bool {
    let normalized = base_url.trim_end_matches('/').to_ascii_lowercase();
    normalized == XIAOMI_MIMO_TOKEN_PLAN_CN_BASE_URL
        || normalized == XIAOMI_MIMO_TOKEN_PLAN_SGP_BASE_URL
        || normalized == XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL
}

pub(crate) const XIAOMI_MIMO_TOKEN_PLAN_ENV_VARS: &[&str] =
    &["XIAOMI_MIMO_TOKEN_PLAN_API_KEY", "MIMO_TOKEN_PLAN_API_KEY"];

pub(crate) const XIAOMI_MIMO_STANDARD_ENV_VARS: &[&str] =
    &["XIAOMI_MIMO_API_KEY", "XIAOMI_API_KEY", "MIMO_API_KEY"];

pub(crate) fn xiaomi_mimo_env_api_key_for_runtime(
    mode: Option<&str>,
    base_url: Option<&str>,
) -> Option<String> {
    let env_value = |vars: &[&str]| codewhale_secrets::env_first(vars).map(|(_, value)| value);

    let normalized_mode =
        mode.map(|value| value.trim().to_ascii_lowercase().replace(['_', ' '], "-"));
    let standard_selected = normalized_mode
        .as_deref()
        .is_some_and(xiaomi_mimo_mode_uses_standard_endpoint)
        || base_url.is_some_and(xiaomi_mimo_base_url_is_pay_as_you_go);
    if standard_selected {
        return env_value(XIAOMI_MIMO_STANDARD_ENV_VARS);
    }

    let token_plan_selected = normalized_mode
        .as_deref()
        .and_then(xiaomi_mimo_base_url_for_mode)
        .is_some()
        || base_url.is_some_and(xiaomi_mimo_base_url_uses_token_plan);
    if token_plan_selected {
        return env_value(XIAOMI_MIMO_TOKEN_PLAN_ENV_VARS);
    }

    env_value(XIAOMI_MIMO_TOKEN_PLAN_ENV_VARS).or_else(|| env_value(XIAOMI_MIMO_STANDARD_ENV_VARS))
}

pub fn resolve_xiaomi_mimo_base_url(
    configured: Option<String>,
    api_key: Option<&str>,
    mode: Option<&str>,
) -> String {
    let normalized_mode =
        mode.map(|value| value.trim().to_ascii_lowercase().replace(['_', ' '], "-"));
    let uses_standard_mode = normalized_mode
        .as_deref()
        .is_some_and(xiaomi_mimo_mode_uses_standard_endpoint);
    let mode_base_url = normalized_mode
        .as_deref()
        .and_then(xiaomi_mimo_base_url_for_mode);
    let uses_token_plan = xiaomi_mimo_api_key_uses_token_plan(api_key);
    match configured {
        Some(base_url) if uses_standard_mode => base_url,
        Some(base_url) if uses_token_plan && xiaomi_mimo_base_url_is_pay_as_you_go(&base_url) => {
            mode_base_url
                .unwrap_or(DEFAULT_XIAOMI_MIMO_BASE_URL)
                .to_string()
        }
        Some(base_url) => base_url,
        None => {
            if let Some(base_url) = mode_base_url {
                base_url.to_string()
            } else if uses_standard_mode {
                XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL.to_string()
            } else if uses_token_plan || api_key.is_none() {
                DEFAULT_XIAOMI_MIMO_BASE_URL.to_string()
            } else {
                XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL.to_string()
            }
        }
    }
}

pub fn xiaomi_mimo_api_key_uses_token_plan(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|key| key.trim_start().starts_with("tp-"))
}

pub(crate) fn xiaomi_mimo_base_url_is_pay_as_you_go(base_url: &str) -> bool {
    matches!(
        base_url.trim_end_matches('/').to_ascii_lowercase().as_str(),
        "https://api.xiaomimimo.com" | "https://api.xiaomimimo.com/v1"
    )
}

/// Whether `base_url` belongs to the provider's official endpoint family.
///
/// Some providers publish multiple stable paths for the same credential and
/// model namespace. Keep that family definition centralized so route
/// canonicalization and credential scoping cannot disagree.
#[must_use]
pub fn provider_base_url_is_official(provider: ProviderKind, base_url: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/').to_ascii_lowercase();
    match provider {
        ProviderKind::Deepseek => matches!(
            normalized.as_str(),
            "https://api.deepseek.com"
                | "https://api.deepseek.com/v1"
                | "https://api.deepseek.com/beta"
        ),
        ProviderKind::DeepseekAnthropic => matches!(
            normalized.as_str(),
            "https://api.deepseek.com/anthropic" | "https://api.deepseek.com/anthropic/v1"
        ),
        ProviderKind::Siliconflow | ProviderKind::SiliconflowCN => matches!(
            normalized.as_str(),
            "https://api.siliconflow.com/v1" | "https://api.siliconflow.cn/v1"
        ),
        ProviderKind::Moonshot => {
            matches!(
                normalized.as_str(),
                DEFAULT_MOONSHOT_BASE_URL | MOONSHOT_CN_BASE_URL
            ) || moonshot_base_url_uses_kimi_code(base_url)
        }
        ProviderKind::Zai => matches!(
            normalized.as_str(),
            "https://api.z.ai/api/coding/paas/v4"
                | "https://api.z.ai/api/paas/v4"
                | "https://open.bigmodel.cn/api/paas/v4"
        ),
        ProviderKind::XiaomiMimo => {
            xiaomi_mimo_base_url_uses_token_plan(base_url)
                || xiaomi_mimo_base_url_is_pay_as_you_go(base_url)
        }
        // StepFun publishes one vendor over four hosts: global (.ai) and
        // China (.com), each with a pay-as-you-go `/v1` and a Step Plan
        // `/step_plan/v1` surface. All four are StepFun's own, documented in
        // its console. Recognising only the global PAYG default made a Step
        // Plan subscriber's route read as a custom endpoint, and
        // `catalog_models_for_route` withholds the catalog from a custom
        // endpoint — so the picker reported `0 bundled`, offered no model
        // list, and fell back to a guessed context window on a route whose
        // console lists `step-5-preview` at 1M context.
        //
        // Note the plan hosts are listed from StepFun's console, not from
        // models.dev, whose `stepfun-ai-step-plan` entry is missing
        // `step-5-preview` that the vendor itself advertises. An aggregator
        // is a secondary source for what a host serves; the vendor is not.
        ProviderKind::Stepfun => matches!(
            normalized.as_str(),
            "https://api.stepfun.ai/v1"
                | "https://api.stepfun.ai/step_plan/v1"
                | "https://api.stepfun.com/v1"
                | "https://api.stepfun.com/step_plan/v1"
        ),
        ProviderKind::Ollama => {
            normalized == DEFAULT_OLLAMA_BASE_URL
                || provider::is_exact_ollama_cloud_route(provider, base_url)
        }
        ProviderKind::OllamaCloud => provider::is_exact_ollama_cloud_route(provider, base_url),
        ProviderKind::Edenai => matches!(
            normalized.as_str(),
            "https://api.edenai.run/v3" | "https://api.eu.edenai.run/v3"
        ),
        ProviderKind::Zenmux => normalized == DEFAULT_ZENMUX_BASE_URL,
        ProviderKind::Csdn => normalized == DEFAULT_CSDN_BASE_URL,
        ProviderKind::Concentrate => normalized == DEFAULT_CONCENTRATE_BASE_URL,
        // The Codewhale API's official endpoint family is its default base
        // plus whatever the operator declared in `CODEWHALE_API_BASE` — the
        // route's own documented override, already validated as HTTPS or
        // loopback, and the exact shape `CODEWHALE_CLOUD_API_BASE` has for the
        // account surface. Treating that declared origin as "custom" is what
        // silently stripped the account bearer and dispatched unauthenticated.
        // A base URL from anywhere else stays custom and keyless.
        ProviderKind::Codewhale => {
            normalized == DEFAULT_CODEWHALE_BASE_URL
                || provider::codewhale_api_base_from_env().is_some_and(|declared| {
                    normalized == declared.trim_end_matches('/').to_ascii_lowercase()
                })
        }
        // Custom routes have no Codewhale-owned official endpoint. The
        // descriptor URL is a schema placeholder, never a credential scope.
        ProviderKind::Custom => false,
        _ => {
            normalized
                == default_base_url_for_provider(provider)
                    .trim()
                    .trim_end_matches('/')
                    .to_ascii_lowercase()
        }
    }
}

pub(crate) fn base_url_is_custom_for_provider(provider: ProviderKind, base_url: &str) -> bool {
    !provider_base_url_is_official(provider, base_url)
}

/// Whether `base_url` is outside the provider's official endpoint family and
/// therefore owns its model-id namespace.
///
/// Custom OpenAI-compatible endpoints must receive the exact model selector
/// the user supplied. Official endpoints may safely canonicalize known aliases
/// to their provider wire ids.
#[must_use]
pub fn provider_preserves_custom_base_url_model(provider: ProviderKind, base_url: &str) -> bool {
    base_url_is_custom_for_provider(provider, base_url)
}

pub(crate) fn should_skip_secret_store_for_provider(
    provider: ProviderKind,
    base_url: &str,
    auth_mode: Option<&str>,
) -> bool {
    if auth_mode_disables_api_key(auth_mode) {
        return true;
    }
    if base_url_is_custom_for_provider(provider, base_url) {
        return true;
    }
    if auth_mode_requires_api_key(auth_mode) {
        return false;
    }
    // The Codewhale API authenticates on every origin it is allowed to reach,
    // including the loopback test origin `CODEWHALE_API_BASE` may name. It is
    // never a keyless local runtime.
    if provider == ProviderKind::Codewhale {
        return false;
    }

    matches!(provider, ProviderKind::Sglang | ProviderKind::Vllm)
        || (provider == ProviderKind::Ollama
            && !provider::is_exact_ollama_cloud_route(provider, base_url))
        || base_url_uses_local_host(base_url)
}

/// Read the durable provider slot without allowing environment fallback to
/// jump ahead of the bounded legacy slot. The old `ollama` slot is consulted
/// only for the exact route tuple migrated above; selecting `ollama-cloud`
/// directly never consumes a local provider credential.
pub(crate) fn stored_api_key_for_provider(
    secrets: &Secrets,
    provider: ProviderKind,
    legacy_ollama_cloud: bool,
) -> Option<(String, SecretSource)> {
    let mut slots = vec![provider.secret_store_slot()];
    if provider == ProviderKind::OllamaCloud && legacy_ollama_cloud {
        slots.push(ProviderKind::Ollama.secret_store_slot());
    }
    slots.into_iter().find_map(|slot| {
        secrets
            .get(slot)
            .ok()
            .flatten()
            .filter(|value| !value.trim().is_empty())
            .map(|value| (value, SecretSource::Keyring))
    })
}

/// The provider's API key from its own environment variables, the single
/// list on its descriptor ([`provider::Provider::env_vars`]).
///
/// Xiaomi MiMo is the exception: its token-plan variables belong to the
/// token-plan endpoints, and `xiaomi_mimo_env_api_key_for_runtime` (which the
/// resolver tries first) is the only reader that knows the selected mode. This
/// fallback reads the standard variables only, so a token-plan key is never
/// sent to the pay-as-you-go endpoint.
pub(crate) fn env_api_key_for_provider(provider: ProviderKind) -> Option<String> {
    let env_vars = if provider == ProviderKind::XiaomiMimo {
        XIAOMI_MIMO_STANDARD_ENV_VARS
    } else {
        provider.provider().env_vars()
    };
    codewhale_secrets::env_first(env_vars).map(|(_, value)| value)
}

/// Whether an authentication mode requires API-key material.
#[must_use]
pub fn auth_mode_requires_api_key(auth_mode: Option<&str>) -> bool {
    matches!(
        auth_mode
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase()),
        Some(value)
            if matches!(
                value.as_str(),
                "api_key" | "api-key" | "apikey" | "bearer" | "bearer-token"
            )
    )
}

/// Whether an authentication mode explicitly disables upstream provider auth.
#[must_use]
pub fn auth_mode_disables_api_key(auth_mode: Option<&str>) -> bool {
    matches!(
        auth_mode
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase()),
        Some(value)
            if matches!(
                value.as_str(),
                "none" | "off" | "disabled" | "no_auth" | "no-auth" | "anonymous"
            )
    )
}

/// Whether an authentication mode selects Kimi's imported bearer token.
#[must_use]
pub fn auth_mode_uses_kimi_imported_token(auth_mode: &str) -> bool {
    matches!(
        auth_mode
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_")
            .as_str(),
        "kimi" | "kimi_oauth" | "kimi_cli" | "oauth"
    )
}

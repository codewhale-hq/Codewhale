use crate::base_url_resolution::provider_preserves_custom_base_url_model;
use crate::display_redaction::{parse_bool, parse_http_headers};
use crate::model_normalization::normalize_model_for_provider;
use crate::provider;
use crate::provider_kind::ProviderKind;
use crate::telemetry::{read_telemetry_env, telemetry_floor_in_force};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub(crate) struct EnvRuntimeOverrides {
    pub(crate) provider: Option<ProviderKind>,
    pub(crate) provider_source: Option<&'static str>,
    pub(crate) model: Option<String>,
    pub(crate) volcengine_model: Option<String>,
    pub(crate) wanjie_ark_model: Option<String>,
    pub(crate) openrouter_model: Option<String>,
    pub(crate) orcarouter_model: Option<String>,
    pub(crate) moonshot_model: Option<String>,
    pub(crate) xiaomi_mimo_model: Option<String>,
    pub(crate) xiaomi_mimo_mode: Option<String>,
    pub(crate) novita_model: Option<String>,
    pub(crate) fireworks_model: Option<String>,
    pub(crate) arcee_model: Option<String>,
    pub(crate) auth_mode: Option<String>,
    pub(crate) log_level: Option<String>,
    pub(crate) telemetry: Option<bool>,
    /// `CODEWHALE_TELEMETRY`/`DEEPSEEK_TELEMETRY` was set to something
    /// [`parse_bool`] could not read. A typo in a kill switch must never
    /// resolve to "on", so this forces telemetry off the same way an explicit
    /// `false` does.
    pub(crate) telemetry_env_invalid: bool,
    /// An environment-level kill switch is in force for this process.
    ///
    /// See [`telemetry_floor_in_force`] for what sets it and why the dispatcher
    /// has to state it rather than let the child infer it.
    pub(crate) telemetry_floor: bool,
    /// `CODEWHALE_TELEMETRY_ENDPOINT`/`DEEPSEEK_TELEMETRY_ENDPOINT`. Overrides
    /// the config file. A workspace `.env` cannot reach this — the dotenv
    /// allowlist admits only built-in provider credential names.
    pub(crate) telemetry_endpoint: Option<String>,
    pub(crate) approval_policy: Option<String>,
    pub(crate) sandbox_mode: Option<String>,
    pub(crate) yolo: Option<bool>,
    pub(crate) verbosity: Option<String>,
    pub(crate) http_headers: Option<BTreeMap<String, String>>,
    pub(crate) active_route_base_url: Option<String>,
    pub(crate) deepseek_anthropic_base_url: Option<String>,
    pub(crate) nvidia_base_url: Option<String>,
    pub(crate) openai_base_url: Option<String>,
    pub(crate) atlascloud_base_url: Option<String>,
    pub(crate) volcengine_base_url: Option<String>,
    pub(crate) wanjie_ark_base_url: Option<String>,
    pub(crate) openrouter_base_url: Option<String>,
    pub(crate) orcarouter_base_url: Option<String>,
    pub(crate) xiaomi_mimo_base_url: Option<String>,
    pub(crate) novita_base_url: Option<String>,
    pub(crate) fireworks_base_url: Option<String>,
    pub(crate) siliconflow_base_url: Option<String>,
    pub(crate) siliconflow_model: Option<String>,
    pub(crate) arcee_base_url: Option<String>,
    pub(crate) moonshot_base_url: Option<String>,
    pub(crate) sglang_base_url: Option<String>,
    pub(crate) vllm_base_url: Option<String>,
    pub(crate) ollama_base_url: Option<String>,
    pub(crate) ollama_cloud_base_url: Option<String>,
    pub(crate) ollama_cloud_model: Option<String>,
    pub(crate) huggingface_base_url: Option<String>,
    pub(crate) huggingface_model: Option<String>,
    pub(crate) modelscope_base_url: Option<String>,
    pub(crate) modelscope_model: Option<String>,
    pub(crate) together_base_url: Option<String>,
    pub(crate) together_model: Option<String>,
    pub(crate) qianfan_base_url: Option<String>,
    pub(crate) qianfan_model: Option<String>,
    pub(crate) openai_codex_base_url: Option<String>,
    pub(crate) openai_codex_model: Option<String>,
    pub(crate) anthropic_base_url: Option<String>,
    pub(crate) anthropic_model: Option<String>,
    pub(crate) openmodel_base_url: Option<String>,
    pub(crate) openmodel_model: Option<String>,
    pub(crate) zai_base_url: Option<String>,
    pub(crate) zai_model: Option<String>,
    pub(crate) stepfun_base_url: Option<String>,
    pub(crate) stepfun_model: Option<String>,
    pub(crate) minimax_base_url: Option<String>,
    pub(crate) minimax_anthropic_base_url: Option<String>,
    pub(crate) minimax_model: Option<String>,
    pub(crate) deepinfra_base_url: Option<String>,
    pub(crate) deepinfra_model: Option<String>,
    pub(crate) sakana_base_url: Option<String>,
    pub(crate) sakana_model: Option<String>,
    pub(crate) longcat_base_url: Option<String>,
    pub(crate) longcat_model: Option<String>,
    pub(crate) opencode_go_base_url: Option<String>,
    pub(crate) opencode_go_model: Option<String>,
    pub(crate) opencode_zen_base_url: Option<String>,
    pub(crate) opencode_zen_model: Option<String>,
    pub(crate) meta_base_url: Option<String>,
    pub(crate) meta_model: Option<String>,
    pub(crate) xai_base_url: Option<String>,
    pub(crate) xai_model: Option<String>,
    pub(crate) mistral_base_url: Option<String>,
    pub(crate) mistral_model: Option<String>,
    pub(crate) google_base_url: Option<String>,
    pub(crate) google_model: Option<String>,
    pub(crate) telecomjs_base_url: Option<String>,
    pub(crate) telecomjs_model: Option<String>,
    pub(crate) edenai_base_url: Option<String>,
    pub(crate) edenai_model: Option<String>,
    pub(crate) zenmux_base_url: Option<String>,
    pub(crate) zenmux_model: Option<String>,
    pub(crate) csdn_base_url: Option<String>,
    pub(crate) csdn_model: Option<String>,
    pub(crate) concentrate_base_url: Option<String>,
    pub(crate) concentrate_model: Option<String>,
    pub(crate) codewhale_base_url: Option<String>,
    pub(crate) codewhale_model: Option<String>,
    pub(crate) modelstudio_token_plan_base_url: Option<String>,
    pub(crate) modelstudio_token_plan_model: Option<String>,
    pub(crate) modelstudio_coding_plan_base_url: Option<String>,
    pub(crate) modelstudio_coding_plan_model: Option<String>,
}

/// The first of `names` that is set to a non-blank value. A variable exported
/// empty (compose `${VAR:-}`, CI templates) must not shadow the legacy
/// variable or the config file.
pub(crate) fn env_non_blank(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

impl EnvRuntimeOverrides {
    pub(crate) fn load() -> Self {
        let (provider, provider_source) = Self::load_provider();
        let (telemetry, telemetry_env_invalid) = Self::load_telemetry();
        let telemetry_floor = telemetry_floor_in_force();
        Self {
            provider,
            provider_source,
            model: std::env::var("CODEWHALE_MODEL")
                .or_else(|_| std::env::var("DEEPSEEK_MODEL"))
                .or_else(|_| std::env::var("DEEPSEEK_DEFAULT_TEXT_MODEL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            volcengine_model: std::env::var("VOLCENGINE_MODEL")
                .or_else(|_| std::env::var("VOLCENGINE_ARK_MODEL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            wanjie_ark_model: std::env::var("WANJIE_ARK_MODEL")
                .or_else(|_| std::env::var("WANJIE_MODEL"))
                .or_else(|_| std::env::var("WANJIE_MAAS_MODEL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            openrouter_model: std::env::var("OPENROUTER_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            orcarouter_model: std::env::var("ORCAROUTER_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            moonshot_model: std::env::var("MOONSHOT_MODEL")
                .or_else(|_| std::env::var("KIMI_MODEL_NAME"))
                .or_else(|_| std::env::var("KIMI_MODEL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            xiaomi_mimo_model: std::env::var("XIAOMI_MIMO_MODEL")
                .or_else(|_| std::env::var("MIMO_MODEL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            xiaomi_mimo_mode: std::env::var("XIAOMI_MIMO_MODE")
                .or_else(|_| std::env::var("MIMO_MODE"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            novita_model: std::env::var("NOVITA_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            fireworks_model: std::env::var("FIREWORKS_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            arcee_model: std::env::var("ARCEE_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            verbosity: env_non_blank(&["CODEWHALE_VERBOSITY", "DEEPSEEK_VERBOSITY"]),
            auth_mode: env_non_blank(&["CODEWHALE_AUTH_MODE", "DEEPSEEK_AUTH_MODE"]),
            log_level: env_non_blank(&["CODEWHALE_LOG_LEVEL", "DEEPSEEK_LOG_LEVEL"]),
            telemetry,
            telemetry_env_invalid,
            telemetry_floor,
            // Empty is kept, not discarded. Since the config file's *absent*
            // endpoint now resolves to `DEFAULT_TELEMETRY_ENDPOINT`, dropping
            // an explicitly emptied variable here would make
            // `CODEWHALE_TELEMETRY_ENDPOINT=` select the shipped endpoint —
            // the opposite of what anyone typing it means. Resolution reads an
            // empty override as "contact nobody, write the dry-run file".
            telemetry_endpoint: std::env::var("CODEWHALE_TELEMETRY_ENDPOINT")
                .or_else(|_| std::env::var("DEEPSEEK_TELEMETRY_ENDPOINT"))
                .ok(),
            approval_policy: env_non_blank(&[
                "CODEWHALE_APPROVAL_POLICY",
                "DEEPSEEK_APPROVAL_POLICY",
            ]),
            sandbox_mode: env_non_blank(&["CODEWHALE_SANDBOX_MODE", "DEEPSEEK_SANDBOX_MODE"]),
            // `DEEPSEEK_YOLO` is a read-only deprecated alias of
            // `CODEWHALE_YOLO` so existing scripts keep working; when both are
            // set `CODEWHALE_YOLO` wins. The alias is removed in 0.10 per
            // issue #5443 — do not write it anywhere.
            yolo: std::env::var("CODEWHALE_YOLO")
                .or_else(|_| std::env::var("DEEPSEEK_YOLO"))
                .ok()
                .and_then(|v| match parse_bool(&v) {
                    Ok(b) => Some(b),
                    Err(_) => {
                        tracing::warn!("Invalid CODEWHALE_YOLO value '{v}', expected true/false");
                        None
                    }
                }),
            http_headers: std::env::var("CODEWHALE_HTTP_HEADERS")
                .or_else(|_| std::env::var("DEEPSEEK_HTTP_HEADERS"))
                .ok()
                .and_then(|value| match parse_http_headers(&value) {
                    Ok(h) => Some(h),
                    Err(_) => {
                        tracing::warn!("Invalid CODEWHALE_HTTP_HEADERS/DEEPSEEK_HTTP_HEADERS value, expected format: header1=val1,header2=val2");
                        None
                    }
                })
                .filter(|headers| !headers.is_empty()),
            active_route_base_url: std::env::var("CODEWHALE_BASE_URL")
                .or_else(|_| std::env::var("DEEPSEEK_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            deepseek_anthropic_base_url: std::env::var("DEEPSEEK_ANTHROPIC_BASE_URL")
                .or_else(|_| std::env::var("DEEPSEEK_CLAUDE_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            nvidia_base_url: std::env::var("NVIDIA_NIM_BASE_URL")
                .or_else(|_| std::env::var("NIM_BASE_URL"))
                .or_else(|_| std::env::var("NVIDIA_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            openai_base_url: std::env::var("OPENAI_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            atlascloud_base_url: std::env::var("ATLASCLOUD_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            volcengine_base_url: std::env::var("VOLCENGINE_BASE_URL")
                .or_else(|_| std::env::var("VOLCENGINE_ARK_BASE_URL"))
                .or_else(|_| std::env::var("ARK_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            wanjie_ark_base_url: std::env::var("WANJIE_ARK_BASE_URL")
                .or_else(|_| std::env::var("WANJIE_BASE_URL"))
                .or_else(|_| std::env::var("WANJIE_MAAS_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            openrouter_base_url: std::env::var("OPENROUTER_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            orcarouter_base_url: std::env::var("ORCAROUTER_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            xiaomi_mimo_base_url: std::env::var("XIAOMI_MIMO_BASE_URL")
                .or_else(|_| std::env::var("MIMO_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            novita_base_url: std::env::var("NOVITA_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            fireworks_base_url: std::env::var("FIREWORKS_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            siliconflow_base_url: std::env::var("SILICONFLOW_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            siliconflow_model: std::env::var("SILICONFLOW_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            arcee_base_url: std::env::var("ARCEE_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            moonshot_base_url: std::env::var("MOONSHOT_BASE_URL")
                .or_else(|_| std::env::var("KIMI_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            sglang_base_url: std::env::var("SGLANG_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            vllm_base_url: std::env::var("VLLM_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            ollama_base_url: std::env::var("OLLAMA_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            ollama_cloud_base_url: std::env::var("OLLAMA_CLOUD_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            ollama_cloud_model: std::env::var("OLLAMA_CLOUD_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            huggingface_base_url: std::env::var("HUGGINGFACE_BASE_URL")
                .or_else(|_| std::env::var("HF_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            huggingface_model: std::env::var("HUGGINGFACE_MODEL")
                .or_else(|_| std::env::var("HF_MODEL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            modelscope_base_url: std::env::var("MODELSCOPE_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            modelscope_model: std::env::var("MODELSCOPE_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            together_base_url: std::env::var("TOGETHER_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            together_model: std::env::var("TOGETHER_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            qianfan_base_url: std::env::var("QIANFAN_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| {
                    std::env::var("BAIDU_QIANFAN_BASE_URL")
                        .ok()
                        .filter(|v| !v.trim().is_empty())
                }),
            qianfan_model: std::env::var("QIANFAN_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| {
                    std::env::var("BAIDU_QIANFAN_MODEL")
                        .ok()
                        .filter(|v| !v.trim().is_empty())
                }),
            openai_codex_base_url: std::env::var("OPENAI_CODEX_BASE_URL")
                .or_else(|_| std::env::var("CODEX_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            openai_codex_model: std::env::var("OPENAI_CODEX_MODEL")
                .or_else(|_| std::env::var("CODEX_MODEL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            anthropic_base_url: std::env::var("ANTHROPIC_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            anthropic_model: std::env::var("ANTHROPIC_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            openmodel_base_url: std::env::var("OPENMODEL_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            openmodel_model: std::env::var("OPENMODEL_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            zai_base_url: std::env::var("ZAI_BASE_URL")
                .or_else(|_| std::env::var("Z_AI_BASE_URL"))
                .or_else(|_| std::env::var("ZHIPU_BASE_URL"))
                .or_else(|_| std::env::var("ZHIPUAI_BASE_URL"))
                .or_else(|_| std::env::var("BIGMODEL_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            zai_model: std::env::var("ZAI_MODEL")
                .or_else(|_| std::env::var("Z_AI_MODEL"))
                .or_else(|_| std::env::var("ZHIPU_MODEL"))
                .or_else(|_| std::env::var("ZHIPUAI_MODEL"))
                .or_else(|_| std::env::var("BIGMODEL_MODEL"))
                .or_else(|_| std::env::var("GLM_MODEL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            stepfun_base_url: std::env::var("STEPFUN_BASE_URL")
                .or_else(|_| std::env::var("STEP_BASE_URL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            stepfun_model: std::env::var("STEPFUN_MODEL")
                .or_else(|_| std::env::var("STEP_MODEL"))
                .ok()
                .filter(|v| !v.trim().is_empty()),
            minimax_base_url: std::env::var("MINIMAX_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            minimax_anthropic_base_url: std::env::var("MINIMAX_ANTHROPIC_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            minimax_model: std::env::var("MINIMAX_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            deepinfra_base_url: std::env::var("DEEPINFRA_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            deepinfra_model: std::env::var("DEEPINFRA_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            sakana_base_url: std::env::var("SAKANA_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            sakana_model: std::env::var("SAKANA_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            longcat_base_url: std::env::var("LONGCAT_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            longcat_model: std::env::var("LONGCAT_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            opencode_go_base_url: std::env::var("OPENCODE_GO_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            opencode_go_model: std::env::var("OPENCODE_GO_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            opencode_zen_base_url: std::env::var("OPENCODE_ZEN_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            opencode_zen_model: std::env::var("OPENCODE_ZEN_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            meta_base_url: std::env::var("META_MODEL_API_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| {
                    std::env::var("MODEL_API_BASE_URL")
                        .ok()
                        .filter(|v| !v.trim().is_empty())
                }),
            meta_model: std::env::var("META_MODEL_API_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| {
                    std::env::var("MODEL_API_MODEL")
                        .ok()
                        .filter(|v| !v.trim().is_empty())
                }),
            xai_base_url: std::env::var("XAI_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            xai_model: std::env::var("XAI_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            google_base_url: std::env::var("GOOGLE_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| {
                    std::env::var("GEMINI_BASE_URL")
                        .ok()
                        .filter(|v| !v.trim().is_empty())
                }),
            google_model: std::env::var("GOOGLE_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty())
                .or_else(|| {
                    std::env::var("GEMINI_MODEL")
                        .ok()
                        .filter(|v| !v.trim().is_empty())
                }),
            mistral_base_url: std::env::var("MISTRAL_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            mistral_model: std::env::var("MISTRAL_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            telecomjs_base_url: std::env::var("TELECOMJS_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            telecomjs_model: std::env::var("TELECOMJS_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            edenai_base_url: std::env::var("EDENAI_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            edenai_model: std::env::var("EDENAI_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            zenmux_base_url: std::env::var("ZENMUX_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            zenmux_model: std::env::var("ZENMUX_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            csdn_base_url: std::env::var("CSDN_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            csdn_model: std::env::var("CSDN_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            concentrate_base_url: std::env::var("CONCENTRATE_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            concentrate_model: std::env::var("CONCENTRATE_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            // `CODEWHALE_API_BASE` is a trust boundary, not a plain string:
            // an origin this route would refuse to send a bearer to is
            // dropped here rather than resolved into a route.
            codewhale_base_url: provider::codewhale_api_base_from_env(),
            codewhale_model: std::env::var("CODEWHALE_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            modelstudio_token_plan_base_url: std::env::var("MODELSTUDIO_TOKEN_PLAN_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            modelstudio_token_plan_model: std::env::var("MODELSTUDIO_TOKEN_PLAN_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            modelstudio_coding_plan_base_url: std::env::var("MODELSTUDIO_CODING_PLAN_BASE_URL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            modelstudio_coding_plan_model: std::env::var("MODELSTUDIO_CODING_PLAN_MODEL")
                .ok()
                .filter(|v| !v.trim().is_empty()),
        }
    }

    fn load_provider() -> (Option<ProviderKind>, Option<&'static str>) {
        for name in ["CODEWHALE_PROVIDER", "DEEPSEEK_PROVIDER"] {
            let Some(value) = env_non_blank(&[name]) else {
                continue;
            };
            if let Some(parsed) = ProviderKind::parse_config_identity(&value) {
                return (Some(parsed), Some(name));
            }
            // An unrecognized value used to be dropped silently, leaving the
            // config provider in charge while the user believed the env var
            // had switched it. Say so, then let the legacy variable apply.
            tracing::warn!(
                "{name} does not name a built-in provider and is ignored; \
                 select a named custom provider with `provider` in config.toml"
            );
        }
        (None, None)
    }

    /// Read the telemetry kill switch, reporting an unreadable value instead of
    /// swallowing it. See [`read_telemetry_env`].
    fn load_telemetry() -> (Option<bool>, bool) {
        read_telemetry_env()
    }

    pub(crate) fn base_url_for(&self, provider: ProviderKind) -> Option<String> {
        // Defaults belong in the resolver's final fallback so config-file
        // values (`providers.<name>.base_url`) still win when env is unset.
        match provider {
            ProviderKind::Deepseek => self.active_route_base_url.clone(),
            ProviderKind::DeepseekAnthropic => self.deepseek_anthropic_base_url.clone(),
            ProviderKind::NvidiaNim => self.nvidia_base_url.clone(),
            ProviderKind::Openai => self.openai_base_url.clone(),
            ProviderKind::Atlascloud => self.atlascloud_base_url.clone(),
            ProviderKind::WanjieArk => self.wanjie_ark_base_url.clone(),
            ProviderKind::Volcengine => self.volcengine_base_url.clone(),
            ProviderKind::Openrouter => self.openrouter_base_url.clone(),
            ProviderKind::Orcarouter => self.orcarouter_base_url.clone(),
            ProviderKind::XiaomiMimo => self.xiaomi_mimo_base_url.clone(),
            ProviderKind::Novita => self.novita_base_url.clone(),
            ProviderKind::Fireworks => self.fireworks_base_url.clone(),
            ProviderKind::Siliconflow | ProviderKind::SiliconflowCN => {
                self.siliconflow_base_url.clone()
            }
            ProviderKind::Arcee => self.arcee_base_url.clone(),
            ProviderKind::Moonshot => self.moonshot_base_url.clone(),
            ProviderKind::Sglang => self.sglang_base_url.clone(),
            ProviderKind::Vllm => self.vllm_base_url.clone(),
            ProviderKind::Ollama => self.ollama_base_url.clone(),
            ProviderKind::OllamaCloud => self.ollama_cloud_base_url.clone(),
            ProviderKind::Huggingface => self.huggingface_base_url.clone(),
            ProviderKind::Modelscope => self.modelscope_base_url.clone(),
            ProviderKind::Together => self.together_base_url.clone(),
            ProviderKind::Qianfan => self.qianfan_base_url.clone(),
            ProviderKind::OpenaiCodex => self.openai_codex_base_url.clone(),
            ProviderKind::Anthropic => self.anthropic_base_url.clone(),
            ProviderKind::Openmodel => self.openmodel_base_url.clone(),
            ProviderKind::Zai => self.zai_base_url.clone(),
            ProviderKind::Stepfun => self.stepfun_base_url.clone(),
            ProviderKind::Minimax => self.minimax_base_url.clone(),
            ProviderKind::MinimaxAnthropic => self.minimax_anthropic_base_url.clone(),
            ProviderKind::Deepinfra => self.deepinfra_base_url.clone(),
            ProviderKind::Sakana => self.sakana_base_url.clone(),
            ProviderKind::LongCat => self.longcat_base_url.clone(),
            ProviderKind::OpencodeGo => self.opencode_go_base_url.clone(),
            ProviderKind::OpencodeZen => self.opencode_zen_base_url.clone(),
            ProviderKind::Meta => self.meta_base_url.clone(),
            ProviderKind::Xai => self.xai_base_url.clone(),
            ProviderKind::Mistral => self.mistral_base_url.clone(),
            ProviderKind::Google => self.google_base_url.clone(),
            ProviderKind::Antigravity => None,
            ProviderKind::Telecomjs => self.telecomjs_base_url.clone(),
            ProviderKind::Edenai => self.edenai_base_url.clone(),
            ProviderKind::Zenmux => self.zenmux_base_url.clone(),
            ProviderKind::Csdn => self.csdn_base_url.clone(),
            ProviderKind::Concentrate => self.concentrate_base_url.clone(),
            ProviderKind::Codewhale => self.codewhale_base_url.clone(),
            ProviderKind::ModelstudioTokenPlan | ProviderKind::ModelstudioTokenPlanAnthropic => {
                self.modelstudio_token_plan_base_url.clone()
            }
            ProviderKind::ModelstudioCodingPlan | ProviderKind::ModelstudioCodingPlanAnthropic => {
                self.modelstudio_coding_plan_base_url.clone()
            }
            // No dedicated CODEWHALE_CUSTOM_BASE_URL env override: a custom
            // provider's base URL comes from its `[providers.<name>]` table.
            ProviderKind::Custom => None,
        }
    }

    pub(crate) fn model_for(&self, provider: ProviderKind, base_url: &str) -> Option<String> {
        let model = match provider {
            ProviderKind::WanjieArk => self.wanjie_ark_model.clone(),
            ProviderKind::Volcengine => self.volcengine_model.clone(),
            ProviderKind::Openrouter => self.openrouter_model.clone(),
            ProviderKind::Orcarouter => self.orcarouter_model.clone(),
            ProviderKind::Siliconflow | ProviderKind::SiliconflowCN => {
                self.siliconflow_model.clone()
            }
            ProviderKind::Arcee => self.arcee_model.clone(),
            ProviderKind::Moonshot => self.moonshot_model.clone(),
            ProviderKind::XiaomiMimo => self.xiaomi_mimo_model.clone(),
            ProviderKind::Novita => self.novita_model.clone(),
            ProviderKind::Fireworks => self.fireworks_model.clone(),
            ProviderKind::Huggingface => self.huggingface_model.clone(),
            ProviderKind::Modelscope => self.modelscope_model.clone(),
            ProviderKind::Together => self.together_model.clone(),
            ProviderKind::Qianfan => self.qianfan_model.clone(),
            ProviderKind::OpenaiCodex => self.openai_codex_model.clone(),
            ProviderKind::Anthropic => self.anthropic_model.clone(),
            ProviderKind::Openmodel => self.openmodel_model.clone(),
            ProviderKind::Zai => self.zai_model.clone(),
            ProviderKind::Stepfun => self.stepfun_model.clone(),
            ProviderKind::Minimax | ProviderKind::MinimaxAnthropic => self.minimax_model.clone(),
            ProviderKind::Deepinfra => self.deepinfra_model.clone(),
            ProviderKind::Sakana => self.sakana_model.clone(),
            ProviderKind::LongCat => self.longcat_model.clone(),
            ProviderKind::OpencodeGo => self.opencode_go_model.clone(),
            ProviderKind::OpencodeZen => self.opencode_zen_model.clone(),
            ProviderKind::Meta => self.meta_model.clone(),
            ProviderKind::Xai => self.xai_model.clone(),
            ProviderKind::Mistral => self.mistral_model.clone(),
            ProviderKind::Google => self.google_model.clone(),
            ProviderKind::Antigravity => None,
            ProviderKind::Telecomjs => self.telecomjs_model.clone(),
            ProviderKind::Edenai => self.edenai_model.clone(),
            ProviderKind::Zenmux => self.zenmux_model.clone(),
            ProviderKind::Csdn => self.csdn_model.clone(),
            ProviderKind::Concentrate => self.concentrate_model.clone(),
            ProviderKind::Codewhale => self.codewhale_model.clone(),
            ProviderKind::ModelstudioTokenPlan | ProviderKind::ModelstudioTokenPlanAnthropic => {
                self.modelstudio_token_plan_model.clone()
            }
            ProviderKind::ModelstudioCodingPlan | ProviderKind::ModelstudioCodingPlanAnthropic => {
                self.modelstudio_coding_plan_model.clone()
            }
            ProviderKind::OllamaCloud => self.ollama_cloud_model.clone(),
            _ => None,
        }?;

        if provider_preserves_custom_base_url_model(provider, base_url) {
            Some(model.trim().to_string())
        } else {
            Some(normalize_model_for_provider(provider, &model))
        }
    }
}

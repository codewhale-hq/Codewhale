use super::*;
use crate::test_support::EnvVarGuard;
use std::env;
use std::ffi::OsString;

pub struct EnvGuard {
    // Seal path overrides through EnvVarGuard so default_config_path honors
    // this fixture instead of the isolated test root (#5355, #5359).
    _sealed_home: EnvVarGuard,
    _sealed_userprofile: EnvVarGuard,
    _sealed_codewhale_home: EnvVarGuard,
    _sealed_codewhale_config_path: EnvVarGuard,
    _sealed_deepseek_config_path: EnvVarGuard,
    home: Option<OsString>,
    userprofile: Option<OsString>,
    codewhale_home: Option<OsString>,
    codewhale_config_path: Option<OsString>,
    deepseek_config_path: Option<OsString>,
    codewhale_secret_backend: Option<OsString>,
    deepseek_secret_backend: Option<OsString>,
    deepseek_provider: Option<OsString>,
    active_route_api_key: Option<OsString>,
    active_route_base_url: Option<OsString>,
    deepseek_http_headers: Option<OsString>,
    deepseek_model: Option<OsString>,
    deepseek_default_text_model: Option<OsString>,
    codewhale_provider: Option<OsString>,
    codewhale_model: Option<OsString>,
    codewhale_base_url: Option<OsString>,
    nvidia_api_key: Option<OsString>,
    nvidia_nim_api_key: Option<OsString>,
    nim_base_url: Option<OsString>,
    nvidia_base_url: Option<OsString>,
    nvidia_nim_base_url: Option<OsString>,
    nvidia_nim_model: Option<OsString>,
    openai_api_key: Option<OsString>,
    openai_base_url: Option<OsString>,
    openai_model: Option<OsString>,
    atlascloud_api_key: Option<OsString>,
    atlascloud_base_url: Option<OsString>,
    atlascloud_model: Option<OsString>,
    wanjie_ark_api_key: Option<OsString>,
    wanjie_api_key: Option<OsString>,
    wanjie_maas_api_key: Option<OsString>,
    wanjie_ark_base_url: Option<OsString>,
    wanjie_base_url: Option<OsString>,
    wanjie_maas_base_url: Option<OsString>,
    wanjie_ark_model: Option<OsString>,
    wanjie_model: Option<OsString>,
    wanjie_maas_model: Option<OsString>,
    openrouter_api_key: Option<OsString>,
    openrouter_base_url: Option<OsString>,
    openrouter_model: Option<OsString>,
    volcengine_api_key: Option<OsString>,
    volcengine_ark_api_key: Option<OsString>,
    ark_api_key: Option<OsString>,
    volcengine_base_url: Option<OsString>,
    volcengine_ark_base_url: Option<OsString>,
    ark_base_url: Option<OsString>,
    volcengine_model: Option<OsString>,
    volcengine_ark_model: Option<OsString>,
    xiaomi_mimo_token_plan_api_key: Option<OsString>,
    mimo_token_plan_api_key: Option<OsString>,
    xiaomi_mimo_api_key: Option<OsString>,
    xiaomi_api_key: Option<OsString>,
    mimo_api_key: Option<OsString>,
    xiaomi_mimo_base_url: Option<OsString>,
    mimo_base_url: Option<OsString>,
    xiaomi_mimo_model: Option<OsString>,
    mimo_model: Option<OsString>,
    xiaomi_mimo_mode: Option<OsString>,
    mimo_mode: Option<OsString>,
    novita_api_key: Option<OsString>,
    novita_base_url: Option<OsString>,
    novita_model: Option<OsString>,
    fireworks_api_key: Option<OsString>,
    fireworks_base_url: Option<OsString>,
    fireworks_model: Option<OsString>,
    siliconflow_api_key: Option<OsString>,
    siliconflow_base_url: Option<OsString>,
    siliconflow_model: Option<OsString>,
    arcee_api_key: Option<OsString>,
    arcee_base_url: Option<OsString>,
    arcee_model: Option<OsString>,
    moonshot_api_key: Option<OsString>,
    moonshot_base_url: Option<OsString>,
    moonshot_model: Option<OsString>,
    kimi_api_key: Option<OsString>,
    kimi_base_url: Option<OsString>,
    kimi_model: Option<OsString>,
    kimi_model_name: Option<OsString>,
    kimi_code_home: Option<OsString>,
    kimi_share_dir: Option<OsString>,
    sglang_api_key: Option<OsString>,
    sglang_base_url: Option<OsString>,
    sglang_model: Option<OsString>,
    vllm_api_key: Option<OsString>,
    vllm_base_url: Option<OsString>,
    vllm_model: Option<OsString>,
    ollama_cloud_api_key: Option<OsString>,
    ollama_cloud_base_url: Option<OsString>,
    ollama_cloud_model: Option<OsString>,
    ollama_api_key: Option<OsString>,
    ollama_base_url: Option<OsString>,
    ollama_model: Option<OsString>,
    huggingface_api_key: Option<OsString>,
    huggingface_token: Option<OsString>,
    huggingface_base_url: Option<OsString>,
    hf_base_url: Option<OsString>,
    huggingface_model: Option<OsString>,
    hf_model: Option<OsString>,
}

impl EnvGuard {
    pub fn new(home: &Path) -> Self {
        let home_str = OsString::from(home.as_os_str());
        let config_path = home.join(".deepseek").join("config.toml");
        let config_str = OsString::from(config_path.as_os_str());
        let home_prev = env::var_os("HOME");
        let userprofile_prev = env::var_os("USERPROFILE");
        let codewhale_home_prev = env::var_os("CODEWHALE_HOME");
        let codewhale_config_prev = env::var_os("CODEWHALE_CONFIG_PATH");
        let deepseek_config_prev = env::var_os("DEEPSEEK_CONFIG_PATH");
        let codewhale_secret_backend_prev = env::var_os("CODEWHALE_SECRET_BACKEND");
        let deepseek_secret_backend_prev = env::var_os("DEEPSEEK_SECRET_BACKEND");
        let deepseek_provider_prev = env::var_os("DEEPSEEK_PROVIDER");
        let api_key_prev = env::var_os("DEEPSEEK_API_KEY");
        let base_url_prev = env::var_os("DEEPSEEK_BASE_URL");
        let http_headers_prev = env::var_os("DEEPSEEK_HTTP_HEADERS");
        let model_prev = env::var_os("DEEPSEEK_MODEL");
        let default_text_model_prev = env::var_os("DEEPSEEK_DEFAULT_TEXT_MODEL");
        let codewhale_provider_prev = env::var_os("CODEWHALE_PROVIDER");
        let codewhale_model_prev = env::var_os("CODEWHALE_MODEL");
        let codewhale_base_url_prev = env::var_os("CODEWHALE_BASE_URL");
        let nvidia_api_key_prev = env::var_os("NVIDIA_API_KEY");
        let nvidia_nim_api_key_prev = env::var_os("NVIDIA_NIM_API_KEY");
        let nim_base_url_prev = env::var_os("NIM_BASE_URL");
        let nvidia_base_url_prev = env::var_os("NVIDIA_BASE_URL");
        let nvidia_nim_base_url_prev = env::var_os("NVIDIA_NIM_BASE_URL");
        let nvidia_nim_model_prev = env::var_os("NVIDIA_NIM_MODEL");
        let openai_api_key_prev = env::var_os("OPENAI_API_KEY");
        let openai_base_url_prev = env::var_os("OPENAI_BASE_URL");
        let openai_model_prev = env::var_os("OPENAI_MODEL");
        let atlascloud_api_key_prev = env::var_os("ATLASCLOUD_API_KEY");
        let atlascloud_base_url_prev = env::var_os("ATLASCLOUD_BASE_URL");
        let atlascloud_model_prev = env::var_os("ATLASCLOUD_MODEL");
        let wanjie_ark_api_key_prev = env::var_os("WANJIE_ARK_API_KEY");
        let wanjie_api_key_prev = env::var_os("WANJIE_API_KEY");
        let wanjie_maas_api_key_prev = env::var_os("WANJIE_MAAS_API_KEY");
        let wanjie_ark_base_url_prev = env::var_os("WANJIE_ARK_BASE_URL");
        let wanjie_base_url_prev = env::var_os("WANJIE_BASE_URL");
        let wanjie_maas_base_url_prev = env::var_os("WANJIE_MAAS_BASE_URL");
        let wanjie_ark_model_prev = env::var_os("WANJIE_ARK_MODEL");
        let wanjie_model_prev = env::var_os("WANJIE_MODEL");
        let wanjie_maas_model_prev = env::var_os("WANJIE_MAAS_MODEL");
        let openrouter_api_key_prev = env::var_os("OPENROUTER_API_KEY");
        let openrouter_base_url_prev = env::var_os("OPENROUTER_BASE_URL");
        let openrouter_model_prev = env::var_os("OPENROUTER_MODEL");
        let volcengine_api_key_prev = env::var_os("VOLCENGINE_API_KEY");
        let volcengine_ark_api_key_prev = env::var_os("VOLCENGINE_ARK_API_KEY");
        let ark_api_key_prev = env::var_os("ARK_API_KEY");
        let volcengine_base_url_prev = env::var_os("VOLCENGINE_BASE_URL");
        let volcengine_ark_base_url_prev = env::var_os("VOLCENGINE_ARK_BASE_URL");
        let ark_base_url_prev = env::var_os("ARK_BASE_URL");
        let volcengine_model_prev = env::var_os("VOLCENGINE_MODEL");
        let volcengine_ark_model_prev = env::var_os("VOLCENGINE_ARK_MODEL");
        let xiaomi_mimo_token_plan_api_key_prev = env::var_os("XIAOMI_MIMO_TOKEN_PLAN_API_KEY");
        let mimo_token_plan_api_key_prev = env::var_os("MIMO_TOKEN_PLAN_API_KEY");
        let xiaomi_mimo_api_key_prev = env::var_os("XIAOMI_MIMO_API_KEY");
        let xiaomi_api_key_prev = env::var_os("XIAOMI_API_KEY");
        let mimo_api_key_prev = env::var_os("MIMO_API_KEY");
        let xiaomi_mimo_base_url_prev = env::var_os("XIAOMI_MIMO_BASE_URL");
        let mimo_base_url_prev = env::var_os("MIMO_BASE_URL");
        let xiaomi_mimo_model_prev = env::var_os("XIAOMI_MIMO_MODEL");
        let mimo_model_prev = env::var_os("MIMO_MODEL");
        let xiaomi_mimo_mode_prev = env::var_os("XIAOMI_MIMO_MODE");
        let mimo_mode_prev = env::var_os("MIMO_MODE");
        let novita_api_key_prev = env::var_os("NOVITA_API_KEY");
        let novita_base_url_prev = env::var_os("NOVITA_BASE_URL");
        let novita_model_prev = env::var_os("NOVITA_MODEL");
        let fireworks_api_key_prev = env::var_os("FIREWORKS_API_KEY");
        let fireworks_base_url_prev = env::var_os("FIREWORKS_BASE_URL");
        let fireworks_model_prev = env::var_os("FIREWORKS_MODEL");
        let siliconflow_api_key_prev = env::var_os("SILICONFLOW_API_KEY");
        let siliconflow_base_url_prev = env::var_os("SILICONFLOW_BASE_URL");
        let siliconflow_model_prev = env::var_os("SILICONFLOW_MODEL");
        let arcee_api_key_prev = env::var_os("ARCEE_API_KEY");
        let arcee_base_url_prev = env::var_os("ARCEE_BASE_URL");
        let arcee_model_prev = env::var_os("ARCEE_MODEL");
        let moonshot_api_key_prev = env::var_os("MOONSHOT_API_KEY");
        let moonshot_base_url_prev = env::var_os("MOONSHOT_BASE_URL");
        let moonshot_model_prev = env::var_os("MOONSHOT_MODEL");
        let kimi_api_key_prev = env::var_os("KIMI_API_KEY");
        let kimi_base_url_prev = env::var_os("KIMI_BASE_URL");
        let kimi_model_prev = env::var_os("KIMI_MODEL");
        let kimi_model_name_prev = env::var_os("KIMI_MODEL_NAME");
        let kimi_code_home_prev = env::var_os("KIMI_CODE_HOME");
        let kimi_share_dir_prev = env::var_os("KIMI_SHARE_DIR");
        let sglang_api_key_prev = env::var_os("SGLANG_API_KEY");
        let sglang_base_url_prev = env::var_os("SGLANG_BASE_URL");
        let sglang_model_prev = env::var_os("SGLANG_MODEL");
        let vllm_api_key_prev = env::var_os("VLLM_API_KEY");
        let vllm_base_url_prev = env::var_os("VLLM_BASE_URL");
        let vllm_model_prev = env::var_os("VLLM_MODEL");
        let ollama_cloud_api_key_prev = env::var_os("OLLAMA_CLOUD_API_KEY");
        let ollama_cloud_base_url_prev = env::var_os("OLLAMA_CLOUD_BASE_URL");
        let ollama_cloud_model_prev = env::var_os("OLLAMA_CLOUD_MODEL");
        let ollama_api_key_prev = env::var_os("OLLAMA_API_KEY");
        let ollama_base_url_prev = env::var_os("OLLAMA_BASE_URL");
        let ollama_model_prev = env::var_os("OLLAMA_MODEL");
        let huggingface_api_key_prev = env::var_os("HUGGINGFACE_API_KEY");
        let huggingface_token_prev = env::var_os("HF_TOKEN");
        let huggingface_base_url_prev = env::var_os("HUGGINGFACE_BASE_URL");
        let hf_base_url_prev = env::var_os("HF_BASE_URL");
        let huggingface_model_prev = env::var_os("HUGGINGFACE_MODEL");
        let hf_model_prev = env::var_os("HF_MODEL");
        let sealed_home = EnvVarGuard::set("HOME", &home_str);
        let sealed_userprofile = EnvVarGuard::set("USERPROFILE", &home_str);
        let sealed_codewhale_home = EnvVarGuard::remove("CODEWHALE_HOME");
        let sealed_codewhale_config_path = EnvVarGuard::remove("CODEWHALE_CONFIG_PATH");
        let sealed_deepseek_config_path = EnvVarGuard::set("DEEPSEEK_CONFIG_PATH", &config_str);
        // Safety: test-only environment mutation guarded by a global mutex.
        unsafe {
            env::remove_var("CODEWHALE_SECRET_BACKEND");
            env::remove_var("DEEPSEEK_SECRET_BACKEND");
            env::remove_var("DEEPSEEK_PROVIDER");
            env::remove_var("DEEPSEEK_API_KEY");
            env::remove_var("DEEPSEEK_BASE_URL");
            env::remove_var("DEEPSEEK_HTTP_HEADERS");
            env::remove_var("DEEPSEEK_MODEL");
            env::remove_var("DEEPSEEK_DEFAULT_TEXT_MODEL");
            env::remove_var("CODEWHALE_PROVIDER");
            env::remove_var("CODEWHALE_MODEL");
            env::remove_var("CODEWHALE_BASE_URL");
            env::remove_var("NVIDIA_API_KEY");
            env::remove_var("NVIDIA_NIM_API_KEY");
            env::remove_var("NIM_BASE_URL");
            env::remove_var("NVIDIA_BASE_URL");
            env::remove_var("NVIDIA_NIM_BASE_URL");
            env::remove_var("NVIDIA_NIM_MODEL");
            env::remove_var("OPENAI_API_KEY");
            env::remove_var("OPENAI_BASE_URL");
            env::remove_var("OPENAI_MODEL");
            env::remove_var("ATLASCLOUD_API_KEY");
            env::remove_var("ATLASCLOUD_BASE_URL");
            env::remove_var("ATLASCLOUD_MODEL");
            env::remove_var("WANJIE_ARK_API_KEY");
            env::remove_var("WANJIE_API_KEY");
            env::remove_var("WANJIE_MAAS_API_KEY");
            env::remove_var("WANJIE_ARK_BASE_URL");
            env::remove_var("WANJIE_BASE_URL");
            env::remove_var("WANJIE_MAAS_BASE_URL");
            env::remove_var("WANJIE_ARK_MODEL");
            env::remove_var("WANJIE_MODEL");
            env::remove_var("WANJIE_MAAS_MODEL");
            env::remove_var("OPENROUTER_API_KEY");
            env::remove_var("OPENROUTER_BASE_URL");
            env::remove_var("OPENROUTER_MODEL");
            env::remove_var("VOLCENGINE_API_KEY");
            env::remove_var("VOLCENGINE_ARK_API_KEY");
            env::remove_var("ARK_API_KEY");
            env::remove_var("VOLCENGINE_BASE_URL");
            env::remove_var("VOLCENGINE_ARK_BASE_URL");
            env::remove_var("ARK_BASE_URL");
            env::remove_var("VOLCENGINE_MODEL");
            env::remove_var("VOLCENGINE_ARK_MODEL");
            env::remove_var("XIAOMI_MIMO_TOKEN_PLAN_API_KEY");
            env::remove_var("MIMO_TOKEN_PLAN_API_KEY");
            env::remove_var("XIAOMI_MIMO_API_KEY");
            env::remove_var("XIAOMI_API_KEY");
            env::remove_var("MIMO_API_KEY");
            env::remove_var("XIAOMI_MIMO_BASE_URL");
            env::remove_var("MIMO_BASE_URL");
            env::remove_var("XIAOMI_MIMO_MODEL");
            env::remove_var("MIMO_MODEL");
            env::remove_var("XIAOMI_MIMO_MODE");
            env::remove_var("MIMO_MODE");
            env::remove_var("NOVITA_API_KEY");
            env::remove_var("NOVITA_BASE_URL");
            env::remove_var("NOVITA_MODEL");
            env::remove_var("FIREWORKS_API_KEY");
            env::remove_var("FIREWORKS_BASE_URL");
            env::remove_var("FIREWORKS_MODEL");
            env::remove_var("SILICONFLOW_API_KEY");
            env::remove_var("SILICONFLOW_BASE_URL");
            env::remove_var("SILICONFLOW_MODEL");
            env::remove_var("ARCEE_API_KEY");
            env::remove_var("ARCEE_BASE_URL");
            env::remove_var("ARCEE_MODEL");
            env::remove_var("MOONSHOT_API_KEY");
            env::remove_var("MOONSHOT_BASE_URL");
            env::remove_var("MOONSHOT_MODEL");
            env::remove_var("KIMI_API_KEY");
            env::remove_var("KIMI_BASE_URL");
            env::remove_var("KIMI_MODEL");
            env::remove_var("KIMI_MODEL_NAME");
            env::remove_var("KIMI_CODE_HOME");
            env::remove_var("KIMI_SHARE_DIR");
            env::remove_var("SGLANG_API_KEY");
            env::remove_var("SGLANG_BASE_URL");
            env::remove_var("SGLANG_MODEL");
            env::remove_var("VLLM_API_KEY");
            env::remove_var("VLLM_BASE_URL");
            env::remove_var("VLLM_MODEL");
            env::remove_var("OLLAMA_CLOUD_API_KEY");
            env::remove_var("OLLAMA_CLOUD_BASE_URL");
            env::remove_var("OLLAMA_CLOUD_MODEL");
            env::remove_var("OLLAMA_API_KEY");
            env::remove_var("OLLAMA_BASE_URL");
            env::remove_var("OLLAMA_MODEL");
            env::remove_var("HUGGINGFACE_API_KEY");
            env::remove_var("HF_TOKEN");
            env::remove_var("HUGGINGFACE_BASE_URL");
            env::remove_var("HF_BASE_URL");
            env::remove_var("HUGGINGFACE_MODEL");
            env::remove_var("HF_MODEL");
        }
        Self {
            _sealed_home: sealed_home,
            _sealed_userprofile: sealed_userprofile,
            _sealed_codewhale_home: sealed_codewhale_home,
            _sealed_codewhale_config_path: sealed_codewhale_config_path,
            _sealed_deepseek_config_path: sealed_deepseek_config_path,
            home: home_prev,
            userprofile: userprofile_prev,
            codewhale_home: codewhale_home_prev,
            codewhale_config_path: codewhale_config_prev,
            deepseek_config_path: deepseek_config_prev,
            codewhale_secret_backend: codewhale_secret_backend_prev,
            deepseek_secret_backend: deepseek_secret_backend_prev,
            deepseek_provider: deepseek_provider_prev,
            active_route_api_key: api_key_prev,
            active_route_base_url: base_url_prev,
            deepseek_http_headers: http_headers_prev,
            deepseek_model: model_prev,
            deepseek_default_text_model: default_text_model_prev,
            codewhale_provider: codewhale_provider_prev,
            codewhale_model: codewhale_model_prev,
            codewhale_base_url: codewhale_base_url_prev,
            nvidia_api_key: nvidia_api_key_prev,
            nvidia_nim_api_key: nvidia_nim_api_key_prev,
            nim_base_url: nim_base_url_prev,
            nvidia_base_url: nvidia_base_url_prev,
            nvidia_nim_base_url: nvidia_nim_base_url_prev,
            nvidia_nim_model: nvidia_nim_model_prev,
            openai_api_key: openai_api_key_prev,
            openai_base_url: openai_base_url_prev,
            openai_model: openai_model_prev,
            atlascloud_api_key: atlascloud_api_key_prev,
            atlascloud_base_url: atlascloud_base_url_prev,
            atlascloud_model: atlascloud_model_prev,
            wanjie_ark_api_key: wanjie_ark_api_key_prev,
            wanjie_api_key: wanjie_api_key_prev,
            wanjie_maas_api_key: wanjie_maas_api_key_prev,
            wanjie_ark_base_url: wanjie_ark_base_url_prev,
            wanjie_base_url: wanjie_base_url_prev,
            wanjie_maas_base_url: wanjie_maas_base_url_prev,
            wanjie_ark_model: wanjie_ark_model_prev,
            wanjie_model: wanjie_model_prev,
            wanjie_maas_model: wanjie_maas_model_prev,
            openrouter_api_key: openrouter_api_key_prev,
            openrouter_base_url: openrouter_base_url_prev,
            openrouter_model: openrouter_model_prev,
            volcengine_api_key: volcengine_api_key_prev,
            volcengine_ark_api_key: volcengine_ark_api_key_prev,
            ark_api_key: ark_api_key_prev,
            volcengine_base_url: volcengine_base_url_prev,
            volcengine_ark_base_url: volcengine_ark_base_url_prev,
            ark_base_url: ark_base_url_prev,
            volcengine_model: volcengine_model_prev,
            volcengine_ark_model: volcengine_ark_model_prev,
            xiaomi_mimo_token_plan_api_key: xiaomi_mimo_token_plan_api_key_prev,
            mimo_token_plan_api_key: mimo_token_plan_api_key_prev,
            xiaomi_mimo_api_key: xiaomi_mimo_api_key_prev,
            xiaomi_api_key: xiaomi_api_key_prev,
            mimo_api_key: mimo_api_key_prev,
            xiaomi_mimo_base_url: xiaomi_mimo_base_url_prev,
            mimo_base_url: mimo_base_url_prev,
            xiaomi_mimo_model: xiaomi_mimo_model_prev,
            mimo_model: mimo_model_prev,
            xiaomi_mimo_mode: xiaomi_mimo_mode_prev,
            mimo_mode: mimo_mode_prev,
            novita_api_key: novita_api_key_prev,
            novita_base_url: novita_base_url_prev,
            novita_model: novita_model_prev,
            fireworks_api_key: fireworks_api_key_prev,
            fireworks_base_url: fireworks_base_url_prev,
            fireworks_model: fireworks_model_prev,
            siliconflow_api_key: siliconflow_api_key_prev,
            siliconflow_base_url: siliconflow_base_url_prev,
            siliconflow_model: siliconflow_model_prev,
            arcee_api_key: arcee_api_key_prev,
            arcee_base_url: arcee_base_url_prev,
            arcee_model: arcee_model_prev,
            moonshot_api_key: moonshot_api_key_prev,
            moonshot_base_url: moonshot_base_url_prev,
            moonshot_model: moonshot_model_prev,
            kimi_api_key: kimi_api_key_prev,
            kimi_base_url: kimi_base_url_prev,
            kimi_model: kimi_model_prev,
            kimi_model_name: kimi_model_name_prev,
            kimi_code_home: kimi_code_home_prev,
            kimi_share_dir: kimi_share_dir_prev,
            sglang_api_key: sglang_api_key_prev,
            sglang_base_url: sglang_base_url_prev,
            sglang_model: sglang_model_prev,
            vllm_api_key: vllm_api_key_prev,
            vllm_base_url: vllm_base_url_prev,
            vllm_model: vllm_model_prev,
            ollama_cloud_api_key: ollama_cloud_api_key_prev,
            ollama_cloud_base_url: ollama_cloud_base_url_prev,
            ollama_cloud_model: ollama_cloud_model_prev,
            ollama_api_key: ollama_api_key_prev,
            ollama_base_url: ollama_base_url_prev,
            ollama_model: ollama_model_prev,
            huggingface_api_key: huggingface_api_key_prev,
            huggingface_token: huggingface_token_prev,
            huggingface_base_url: huggingface_base_url_prev,
            hf_base_url: hf_base_url_prev,
            huggingface_model: huggingface_model_prev,
            hf_model: hf_model_prev,
        }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // Safety: test-only environment mutation guarded by a global mutex.
        unsafe {
            Self::restore_var("HOME", self.home.take());
            Self::restore_var("USERPROFILE", self.userprofile.take());
            Self::restore_var("CODEWHALE_HOME", self.codewhale_home.take());
            Self::restore_var("CODEWHALE_CONFIG_PATH", self.codewhale_config_path.take());
            Self::restore_var("DEEPSEEK_CONFIG_PATH", self.deepseek_config_path.take());
            Self::restore_var(
                "CODEWHALE_SECRET_BACKEND",
                self.codewhale_secret_backend.take(),
            );
            Self::restore_var(
                "DEEPSEEK_SECRET_BACKEND",
                self.deepseek_secret_backend.take(),
            );
            Self::restore_var("DEEPSEEK_PROVIDER", self.deepseek_provider.take());
            Self::restore_var("DEEPSEEK_API_KEY", self.active_route_api_key.take());
            Self::restore_var("DEEPSEEK_BASE_URL", self.active_route_base_url.take());
            Self::restore_var("DEEPSEEK_HTTP_HEADERS", self.deepseek_http_headers.take());
            Self::restore_var("DEEPSEEK_MODEL", self.deepseek_model.take());
            Self::restore_var(
                "DEEPSEEK_DEFAULT_TEXT_MODEL",
                self.deepseek_default_text_model.take(),
            );
            Self::restore_var("CODEWHALE_PROVIDER", self.codewhale_provider.take());
            Self::restore_var("CODEWHALE_MODEL", self.codewhale_model.take());
            Self::restore_var("CODEWHALE_BASE_URL", self.codewhale_base_url.take());
            Self::restore_var("NVIDIA_API_KEY", self.nvidia_api_key.take());
            Self::restore_var("NVIDIA_NIM_API_KEY", self.nvidia_nim_api_key.take());
            Self::restore_var("NIM_BASE_URL", self.nim_base_url.take());
            Self::restore_var("NVIDIA_BASE_URL", self.nvidia_base_url.take());
            Self::restore_var("NVIDIA_NIM_BASE_URL", self.nvidia_nim_base_url.take());
            Self::restore_var("NVIDIA_NIM_MODEL", self.nvidia_nim_model.take());
            Self::restore_var("OPENAI_API_KEY", self.openai_api_key.take());
            Self::restore_var("OPENAI_BASE_URL", self.openai_base_url.take());
            Self::restore_var("OPENAI_MODEL", self.openai_model.take());
            Self::restore_var("ATLASCLOUD_API_KEY", self.atlascloud_api_key.take());
            Self::restore_var("ATLASCLOUD_BASE_URL", self.atlascloud_base_url.take());
            Self::restore_var("ATLASCLOUD_MODEL", self.atlascloud_model.take());
            Self::restore_var("WANJIE_ARK_API_KEY", self.wanjie_ark_api_key.take());
            Self::restore_var("WANJIE_API_KEY", self.wanjie_api_key.take());
            Self::restore_var("WANJIE_MAAS_API_KEY", self.wanjie_maas_api_key.take());
            Self::restore_var("WANJIE_ARK_BASE_URL", self.wanjie_ark_base_url.take());
            Self::restore_var("WANJIE_BASE_URL", self.wanjie_base_url.take());
            Self::restore_var("WANJIE_MAAS_BASE_URL", self.wanjie_maas_base_url.take());
            Self::restore_var("WANJIE_ARK_MODEL", self.wanjie_ark_model.take());
            Self::restore_var("WANJIE_MODEL", self.wanjie_model.take());
            Self::restore_var("WANJIE_MAAS_MODEL", self.wanjie_maas_model.take());
            Self::restore_var("OPENROUTER_API_KEY", self.openrouter_api_key.take());
            Self::restore_var("OPENROUTER_BASE_URL", self.openrouter_base_url.take());
            Self::restore_var("OPENROUTER_MODEL", self.openrouter_model.take());
            Self::restore_var("VOLCENGINE_API_KEY", self.volcengine_api_key.take());
            Self::restore_var("VOLCENGINE_ARK_API_KEY", self.volcengine_ark_api_key.take());
            Self::restore_var("ARK_API_KEY", self.ark_api_key.take());
            Self::restore_var("VOLCENGINE_BASE_URL", self.volcengine_base_url.take());
            Self::restore_var(
                "VOLCENGINE_ARK_BASE_URL",
                self.volcengine_ark_base_url.take(),
            );
            Self::restore_var("ARK_BASE_URL", self.ark_base_url.take());
            Self::restore_var("VOLCENGINE_MODEL", self.volcengine_model.take());
            Self::restore_var("VOLCENGINE_ARK_MODEL", self.volcengine_ark_model.take());
            Self::restore_var(
                "XIAOMI_MIMO_TOKEN_PLAN_API_KEY",
                self.xiaomi_mimo_token_plan_api_key.take(),
            );
            Self::restore_var(
                "MIMO_TOKEN_PLAN_API_KEY",
                self.mimo_token_plan_api_key.take(),
            );
            Self::restore_var("XIAOMI_MIMO_API_KEY", self.xiaomi_mimo_api_key.take());
            Self::restore_var("XIAOMI_API_KEY", self.xiaomi_api_key.take());
            Self::restore_var("MIMO_API_KEY", self.mimo_api_key.take());
            Self::restore_var("XIAOMI_MIMO_BASE_URL", self.xiaomi_mimo_base_url.take());
            Self::restore_var("MIMO_BASE_URL", self.mimo_base_url.take());
            Self::restore_var("XIAOMI_MIMO_MODEL", self.xiaomi_mimo_model.take());
            Self::restore_var("MIMO_MODEL", self.mimo_model.take());
            Self::restore_var("XIAOMI_MIMO_MODE", self.xiaomi_mimo_mode.take());
            Self::restore_var("MIMO_MODE", self.mimo_mode.take());
            Self::restore_var("NOVITA_API_KEY", self.novita_api_key.take());
            Self::restore_var("NOVITA_BASE_URL", self.novita_base_url.take());
            Self::restore_var("NOVITA_MODEL", self.novita_model.take());
            Self::restore_var("FIREWORKS_API_KEY", self.fireworks_api_key.take());
            Self::restore_var("FIREWORKS_BASE_URL", self.fireworks_base_url.take());
            Self::restore_var("FIREWORKS_MODEL", self.fireworks_model.take());
            Self::restore_var("SILICONFLOW_API_KEY", self.siliconflow_api_key.take());
            Self::restore_var("SILICONFLOW_BASE_URL", self.siliconflow_base_url.take());
            Self::restore_var("SILICONFLOW_MODEL", self.siliconflow_model.take());
            Self::restore_var("ARCEE_API_KEY", self.arcee_api_key.take());
            Self::restore_var("ARCEE_BASE_URL", self.arcee_base_url.take());
            Self::restore_var("ARCEE_MODEL", self.arcee_model.take());
            Self::restore_var("MOONSHOT_API_KEY", self.moonshot_api_key.take());
            Self::restore_var("MOONSHOT_BASE_URL", self.moonshot_base_url.take());
            Self::restore_var("MOONSHOT_MODEL", self.moonshot_model.take());
            Self::restore_var("KIMI_API_KEY", self.kimi_api_key.take());
            Self::restore_var("KIMI_BASE_URL", self.kimi_base_url.take());
            Self::restore_var("KIMI_MODEL", self.kimi_model.take());
            Self::restore_var("KIMI_MODEL_NAME", self.kimi_model_name.take());
            Self::restore_var("KIMI_CODE_HOME", self.kimi_code_home.take());
            Self::restore_var("KIMI_SHARE_DIR", self.kimi_share_dir.take());
            Self::restore_var("SGLANG_API_KEY", self.sglang_api_key.take());
            Self::restore_var("SGLANG_BASE_URL", self.sglang_base_url.take());
            Self::restore_var("SGLANG_MODEL", self.sglang_model.take());
            Self::restore_var("VLLM_API_KEY", self.vllm_api_key.take());
            Self::restore_var("VLLM_BASE_URL", self.vllm_base_url.take());
            Self::restore_var("VLLM_MODEL", self.vllm_model.take());
            Self::restore_var("OLLAMA_CLOUD_API_KEY", self.ollama_cloud_api_key.take());
            Self::restore_var("OLLAMA_CLOUD_BASE_URL", self.ollama_cloud_base_url.take());
            Self::restore_var("OLLAMA_CLOUD_MODEL", self.ollama_cloud_model.take());
            Self::restore_var("OLLAMA_API_KEY", self.ollama_api_key.take());
            Self::restore_var("OLLAMA_BASE_URL", self.ollama_base_url.take());
            Self::restore_var("OLLAMA_MODEL", self.ollama_model.take());
            Self::restore_var("HUGGINGFACE_API_KEY", self.huggingface_api_key.take());
            Self::restore_var("HF_TOKEN", self.huggingface_token.take());
            Self::restore_var("HUGGINGFACE_BASE_URL", self.huggingface_base_url.take());
            Self::restore_var("HF_BASE_URL", self.hf_base_url.take());
            Self::restore_var("HUGGINGFACE_MODEL", self.huggingface_model.take());
            Self::restore_var("HF_MODEL", self.hf_model.take());
        }
    }
}

impl EnvGuard {
    /// Restore an env var to its prior value (or remove it if it was unset).
    ///
    /// # Safety
    /// Must only be called from test code guarded by a global mutex.
    pub(super) unsafe fn restore_var(key: &str, prev: Option<OsString>) {
        if let Some(value) = prev {
            unsafe { env::set_var(key, value) };
        } else {
            unsafe { env::remove_var(key) };
        }
    }
}

pub fn session_custom_provider_config(name: &str, kind: &str, base_url: &str) -> Config {
    let mut custom = HashMap::new();
    custom.insert(
        name.to_string(),
        ProviderConfig {
            kind: Some(kind.to_string()),
            base_url: Some(base_url.to_string()),
            model: Some("local-model".to_string()),
            ..Default::default()
        },
    );
    Config {
        provider: Some(name.to_string()),
        providers: Some(ProvidersConfig {
            custom,
            ..Default::default()
        }),
        ..Config::default()
    }
}

// === Identity-owned endpoint resolution (provider-truth regressions) ===
//
// Every test here is offline and env-locked. No credential is invented and no
// provider is contacted: the assertions are about which host string a route
// resolves to, and about the classifications derived from it.

/// A managed-config guard pointing at a path that does not exist, so an
/// operator-installed managed file on the developer's machine cannot leak into
/// these route assertions.
pub fn no_managed_config(root: &std::path::Path) -> EnvVarGuard {
    EnvVarGuard::set(
        "DEEPSEEK_MANAGED_CONFIG_PATH",
        root.join("absent-managed.toml"),
    )
}

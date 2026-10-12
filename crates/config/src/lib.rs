pub mod app_mode;
pub mod auth_source;
pub mod auto_model;
pub mod catalog;
pub mod cloud_facts;
mod config_document;
pub mod credentials;
pub mod descriptors;
pub mod device_code;
pub mod external_credentials;
pub mod legacy_root;
pub mod model_reference;
pub mod models_dev;
pub mod notifications;
mod opencode_go;
pub use opencode_go::{opencode_go_endpoint_key, opencode_go_model_id, opencode_go_models};
pub mod persistence;
pub mod pricing;
pub mod private_directory;
pub mod provider;
mod provider_defaults;
mod provider_kind;
pub mod redaction;
pub mod resolve;
pub mod route;
pub mod settings_schema;
pub mod setup_state;
pub mod user_constitution;
#[cfg(windows)]
pub mod windows_identity;
mod xai_credentials;
pub use config_document::{
    ConfigDocumentUndo, create_config_document, migrate_legacy_root_config, mutate_config_document,
    mutate_config_document_undoable, mutate_config_document_undoable_with_migration,
    mutate_config_document_with_migration, preview_legacy_root_config,
    replace_config_document_if_unchanged, set_config_document_value, unset_config_document_value,
    with_config_write_lock,
};
pub use model_reference::{Modality, ModelReferenceCard, ModelReferenceDatabase};
pub(crate) use provider_defaults::*;
pub use provider_kind::ProviderKind;
pub use route::ProviderId;
pub use settings_schema::{
    SETTINGS_SCHEMA, SettingDef, SettingKind, SettingOption, SettingUi, schema_groups, schema_rows,
    schema_tabs, setting, setting_index,
};
pub use setup_state::{
    ConstitutionAuthoring, ConstitutionChoice, ConstitutionSource, ConstitutionValidity,
    InheritedConfigFacts, RuntimePostureSource, SetupState, SetupStep, StepEntry, StepStatus,
    TELEMETRY_NOTICE_VERSION,
};
pub use user_constitution::{
    APPROX_BYTES_PER_TOKEN, AutonomyPreference, CacheProjection, ClauseOrigin, ClauseStatus,
    ConstitutionClause, ConstitutionRecommendation, MigrationOutcome, MigrationReceipt,
    MigrationRejection, Ratification, RatificationError, RecommendationParse,
    USER_CONSTITUTION_SCHEMA_VERSION, USER_CONSTITUTION_SCHEMA_VERSION_V1, UntrustedDraftParse,
    UserConstitution, UserConstitutionLoad,
};
pub use xai_credentials::{
    CHATGPT_HOST_FILE_NAME, CHATGPT_OAUTH_GENERATION_PREFIX, CHATGPT_OAUTH_GENERATION_SUFFIX,
    CLAUDE_OAUTH_GENERATION_PREFIX, CLAUDE_OAUTH_GENERATION_SUFFIX, LEGACY_CHATGPT_OAUTH_FILE_NAME,
    LEGACY_CLAUDE_OAUTH_FILE_NAME, LEGACY_XAI_OAUTH_FILE_NAME, XAI_OAUTH_GENERATION_PREFIX,
    XAI_OAUTH_GENERATION_SUFFIX, XaiOAuthCredentialStore, XaiOAuthRevocation,
    chatgpt_oauth_generation_path, claude_oauth_generation_path,
    clear_all_chatgpt_oauth_credentials, clear_all_chatgpt_oauth_credentials_locked,
    clear_all_claude_oauth_credentials_locked, clear_all_xai_oauth_credentials,
    is_valid_chatgpt_oauth_generation, is_valid_claude_oauth_generation,
    is_valid_xai_oauth_generation, legacy_chatgpt_oauth_path, legacy_xai_oauth_path,
    remove_chatgpt_oauth_generation, remove_xai_oauth_generation,
    validate_chatgpt_oauth_generation, validate_claude_oauth_generation,
    validate_xai_oauth_generation, with_xai_oauth_lifecycle_lock,
    with_xai_oauth_revocation_transaction, xai_oauth_credentials_dir, xai_oauth_generation_path,
};

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
pub use app_mode::AppMode;
pub use auth_source::{AuthSourceKind, ProviderAuthSourceToml};
pub use codewhale_execpolicy::ToolAskRule;
use codewhale_execpolicy::{ExecPolicyEngine, PermissionAction, Ruleset};
use codewhale_secrets::SecretSource;
pub use codewhale_secrets::Secrets;
pub use external_credentials::{
    EXTERNAL_CREDENTIAL_CONSENT_VERSION, EXTERNAL_CREDENTIAL_READ_ONLY_SEMANTICS,
    ExternalCredentialAccess, ExternalCredentialConsentStatus, ExternalCredentialConsentToml,
    ExternalCredentialReadGrant, ExternalCredentialSource, default_dsh_credentials_path,
    external_credential_consent_status, quote_os_path, resolve_external_credential_path,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const PERMISSIONS_FILE_NAME: &str = "permissions.toml";
pub const LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE: &str = "Antigravity is a retired, non-runnable legacy provider. Clear Codewhale-owned legacy state with `codewhale auth clear --provider antigravity`; this does not alter Google or Antigravity sessions. For Gemini use provider `google` with `GEMINI_API_KEY`.";

/// Secret-store routing metadata; never credential material.
pub const API_KEYRING_SENTINEL: &str = "__KEYRING__";

/// Canonical structural classification for configured API-key values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigApiKeyValueKind {
    Empty,
    SecretStoreSentinel,
    Literal,
}

#[must_use]
pub fn classify_config_api_key_value(value: &str) -> ConfigApiKeyValueKind {
    match value.trim() {
        "" => ConfigApiKeyValueKind::Empty,
        API_KEYRING_SENTINEL => ConfigApiKeyValueKind::SecretStoreSentinel,
        _ => ConfigApiKeyValueKind::Literal,
    }
}

fn http_headers_are_effectively_empty(headers: &BTreeMap<String, String>) -> bool {
    !headers
        .iter()
        .any(|(name, value)| !name.trim().is_empty() && !value.trim().is_empty())
}

/// Whether an HTTP header can carry the model provider's primary credential.
///
/// Header names are case-insensitive. Keeping this classifier in shared config
/// prevents `auth_mode = "none"` from disabling a generated bearer token while
/// still leaking the same credential through a configured alternate dialect.
#[must_use]
pub fn is_upstream_auth_header(name: &str) -> bool {
    let name = name.trim();
    // Configured gateways use more credential dialects than the three headers
    // generated by Codewhale itself. `auth_mode = "none"` is an endpoint
    // contract, so suppress every credential-shaped request header instead of
    // allowing the same secret through Proxy-Authorization, X-Auth-Token,
    // X-Access-Token, X-Goog-Api-Key, or another *-token/*-api-key spelling.
    is_sensitive_config_key(name)
}

/// Preserve OpenRouter endpoint slugs verbatim; an empty value clears a pin.
/// The service owns the vendor catalog, so validation must not freeze one here.
pub fn validate_openrouter_vendor(value: &str) -> Result<Option<&str>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    if value
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        bail!(
            "providers.openrouter.vendor must be an OpenRouter slug without whitespace or control characters"
        );
    }
    Ok(Some(value))
}

/// Apply a validated pin to an OpenRouter request without dropping unrelated
/// caller policies such as data collection or zero-data-retention constraints.
pub fn apply_openrouter_vendor(body: &mut serde_json::Value, vendor: Option<&str>) {
    if let Some(vendor) = vendor {
        if !body["provider"].is_object() {
            body["provider"] = serde_json::json!({});
        }
        body["provider"]["order"] = serde_json::json!([vendor]);
        body["provider"]["allow_fallbacks"] = serde_json::json!(false);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfigToml {
    /// OpenRouter upstream slug, including an optional endpoint variant.
    /// Requests with a vendor pin disable OpenRouter's upstream fallbacks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "contextWindow",
        alias = "context_window_tokens",
        alias = "contextWindowTokens",
        alias = "context_length",
        alias = "contextLength"
    )]
    pub context_window: Option<u32>,
    /// Per-model context-window overrides keyed by exact wire model id
    /// (`[providers.<id>.model_context_windows]`, #6108). A matching entry
    /// wins over this provider's `context_window` for that model only, so one
    /// gateway can front models with heterogeneous windows.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        alias = "modelContextWindows"
    )]
    pub model_context_windows: BTreeMap<String, u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Wire dialect preference for dual-protocol vendors (DeepSeek, MiniMax,
    /// Model Studio): `openai` (Chat Completions, default) or `anthropic`
    /// (Messages). Not a separate catalog provider — a power-user toggle.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        alias = "api_style",
        alias = "protocol",
        alias = "wire_format",
        alias = "dialect"
    )]
    pub wire: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insecure_skip_tls_verify: Option<bool>,
    /// Explicit consent to a plain-HTTP `base_url` for this provider (a
    /// llama.cpp box on the LAN, an internal gateway). Loopback hosts are
    /// always allowed without it. Distinct from `insecure_skip_tls_verify`,
    /// which skips TLS certificate verification on HTTPS URLs and does not
    /// permit plain HTTP (#5991).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_insecure_http: Option<bool>,
    #[serde(default, skip_serializing_if = "http_headers_are_effectively_empty")]
    pub http_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_suffix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<ProviderAuthSourceToml>,
    /// Explicit consent for reading one exact credential file owned by
    /// another CLI. Absence means disabled and must not trigger discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_credentials: Option<ExternalCredentialConsentToml>,
    /// Codewhale-owned xAI OAuth generation selected by config. The value is a
    /// validated basename under `$CODEWHALE_HOME/credentials`, never an
    /// arbitrary path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_credential_generation: Option<String>,
    /// Preserve provider fields introduced by newer Codewhale versions and by
    /// custom provider adapters when an older typed writer saves this file.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

impl ProviderConfigToml {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let blank = |value: Option<&String>| value.is_none_or(|value| value.trim().is_empty());

        blank(self.api_key.as_ref())
            && self.vendor.is_none()
            && blank(self.base_url.as_ref())
            && blank(self.model.as_ref())
            && self.context_window.is_none()
            && self.model_context_windows.is_empty()
            && blank(self.mode.as_ref())
            && blank(self.wire.as_ref())
            && blank(self.auth_mode.as_ref())
            && self.insecure_skip_tls_verify.is_none()
            && self.allow_insecure_http.is_none()
            && http_headers_are_effectively_empty(&self.http_headers)
            && blank(self.path_suffix.as_ref())
            && self.auth.is_none()
            && self.external_credentials.is_none()
            && self.oauth_credential_generation.is_none()
            && self.extras.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProvidersToml {
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub deepseek: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "deepseek-anthropic",
        alias = "deepseekAnthropic",
        alias = "deepseek-claude",
        alias = "deepseek_claude"
    )]
    pub deepseek_anthropic: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        // The canonical provider id is the kebab `nvidia-nim` (see
        // `provider.rs`); without these aliases a `[providers.nvidia-nim]`
        // TOML section was silently dropped (2026-08-04 review).
        alias = "nvidia-nim",
        alias = "nvidia",
        alias = "nim"
    )]
    pub nvidia_nim: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub openai: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub atlascloud: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "wanjie-ark",
        alias = "wanjie",
        alias = "ark-wanjie",
        alias = "ark_wanjie"
    )]
    pub wanjie_ark: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub volcengine: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub openrouter: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub orcarouter: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "xiaomi-mimo",
        alias = "xiaomi",
        alias = "mimo",
        alias = "xiaomimimo"
    )]
    pub xiaomi_mimo: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub novita: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub fireworks: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub siliconflow: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "siliconflow-CN",
        alias = "siliconflow-cn"
    )]
    pub siliconflow_cn: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub arcee: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub moonshot: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub sglang: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub vllm: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub ollama: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "ollama-cloud"
    )]
    pub ollama_cloud: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub huggingface: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub modelscope: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub together: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "baidu-qianfan",
        alias = "baidu_qianfan",
        alias = "baidu"
    )]
    pub qianfan: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "openai-codex",
        alias = "openai_codex",
        alias = "codex",
        alias = "chatgpt",
        alias = "chatgpt-codex"
    )]
    pub openai_codex: ProviderConfigToml,
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub anthropic: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "open-model",
        alias = "open_model"
    )]
    pub openmodel: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "z-ai",
        alias = "z_ai",
        alias = "z.ai",
        alias = "zhipu",
        alias = "zhipuai",
        alias = "bigmodel",
        alias = "big-model"
    )]
    pub zai: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "step-fun",
        alias = "step_fun",
        alias = "stepfun",
        alias = "stepflash",
        alias = "step-flash",
        alias = "step_flash"
    )]
    pub stepfun: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "mini-max",
        alias = "mini_max",
        alias = "minimax"
    )]
    pub minimax: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "minimax-anthropic",
        alias = "minimaxAnthropic",
        alias = "mini-max-anthropic",
        alias = "mini_max_anthropic"
    )]
    pub minimax_anthropic: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "deep-infra",
        alias = "deep_infra"
    )]
    pub deepinfra: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "sakana-ai",
        alias = "sakana_ai",
        alias = "fugu"
    )]
    pub sakana: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "long-cat",
        alias = "meituan-longcat",
        alias = "meituan"
    )]
    pub longcat: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "opencode-go",
        alias = "opencodego"
    )]
    pub opencode_go: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "opencode-zen",
        alias = "opencodezen",
        alias = "zen",
        alias = "opencode"
    )]
    pub opencode_zen: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "meta-ai",
        alias = "meta_ai",
        alias = "meta-model-api",
        alias = "meta_model_api",
        alias = "muse",
        alias = "muse-spark"
    )]
    pub meta: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "x-ai",
        alias = "x_ai",
        alias = "grok"
    )]
    pub xai: ProviderConfigToml,
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "mistral-ai",
        alias = "mistral_ai",
        alias = "mistralai",
        alias = "la-plateforme",
        alias = "la_plateforme"
    )]
    pub mistral: ProviderConfigToml,
    /// Google Gemini — official OpenAI-compatible endpoint with thought
    /// signatures on tool calls.
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "google-gemini",
        alias = "google_gemini",
        alias = "gemini"
    )]
    pub google: ProviderConfigToml,
    /// Retired Antigravity configuration. This table exists only so old
    /// Codewhale-owned state can deserialize and be cleared safely.
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "agy"
    )]
    pub antigravity: ProviderConfigToml,
    /// Jiangsu Telecom TokenHub — OpenAI-compatible AI gateway.
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "telecom-js",
        alias = "telecom_js",
        alias = "telecomjs-cn",
        alias = "tokenhub"
    )]
    pub telecomjs: ProviderConfigToml,
    /// Eden AI — OpenAI-compatible AI gateway (aggregator).
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "eden-ai",
        alias = "eden_ai"
    )]
    pub edenai: ProviderConfigToml,
    /// ZenMux — OpenAI-compatible AI gateway (aggregator).
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "zen-mux",
        alias = "zen_mux"
    )]
    pub zenmux: ProviderConfigToml,
    /// CSDN 星图 — hosted OpenAI-compatible platform and Coding Plan.
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "csdn-ai",
        alias = "csdn_ai",
        alias = "csdn-coding-plan",
        alias = "csdn_coding_plan",
        alias = "starmap"
    )]
    pub csdn: ProviderConfigToml,
    /// Concentrate — OpenAI Responses-compatible AI gateway (aggregator).
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "concentrate-ai",
        alias = "concentrate_ai",
        alias = "concentrateai"
    )]
    pub concentrate: ProviderConfigToml,
    /// Codewhale API — account-backed model access over connected provider keys.
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "codewhale-api",
        alias = "codewhale_api",
        alias = "cw-api",
        alias = "codewhale-cloud"
    )]
    pub codewhale: ProviderConfigToml,
    /// Alibaba Cloud Model Studio — Token Plan (OpenAI-compatible endpoint).
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "modelstudio-token-plan",
        alias = "modelstudio_token_plan",
        alias = "alibaba-token-plan",
        alias = "dashscope-token-plan"
    )]
    pub modelstudio_token_plan: ProviderConfigToml,
    /// Alibaba Cloud Model Studio — Token Plan Anthropic-compatible endpoint.
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "modelstudio-token-plan-anthropic",
        alias = "modelstudio_token_plan_anthropic",
        alias = "alibaba-token-plan-anthropic"
    )]
    pub modelstudio_token_plan_anthropic: ProviderConfigToml,
    /// Alibaba Cloud Model Studio — Coding Plan (OpenAI-compatible endpoint).
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "modelstudio-coding-plan",
        alias = "modelstudio_coding_plan",
        alias = "alibaba-coding-plan",
        alias = "dashscope-coding-plan"
    )]
    pub modelstudio_coding_plan: ProviderConfigToml,
    /// Alibaba Cloud Model Studio — Coding Plan Anthropic-compatible endpoint.
    #[serde(
        default,
        skip_serializing_if = "ProviderConfigToml::is_empty",
        alias = "modelstudio-coding-plan-anthropic",
        alias = "modelstudio_coding_plan_anthropic",
        alias = "alibaba-coding-plan-anthropic"
    )]
    pub modelstudio_coding_plan_anthropic: ProviderConfigToml,
    /// Catch-all table for the dynamic OpenAI-compatible custom provider
    /// identity (#1519). Arbitrary `[providers.<name>]` tables are handled by
    /// the tui-side flatten map; this named slot keeps the canonical
    /// `ProviderKind::Custom` lookups total without leaking into another
    /// provider's config.
    #[serde(default, skip_serializing_if = "ProviderConfigToml::is_empty")]
    pub custom: ProviderConfigToml,
    /// Preserve dynamically named provider tables and providers added by a
    /// newer Codewhale version.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

/// Sibling `permissions.toml` schema.
///
/// Each rule is a typed condition that can deny, allow, or ask before a tool
/// invocation. The approval card persists ask rules and narrowly scoped,
/// exact allow grants; deny rules remain manually authored.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PermissionsToml {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<ToolAskRule>,
}

/// On-disk state of the active sibling `permissions.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionsFileState {
    /// No sibling permission file exists.
    Missing,
    /// The sibling permission file exists but contains no TOML content.
    Empty,
    /// The sibling permission file contains a parsed TOML document.
    Present,
}

/// A parsed, read-only view of the active sibling `permissions.toml`.
///
/// Removal tokens bind a displayed rule index to the exact file bytes that
/// produced this snapshot. A later editor must present the rule again when
/// another process changed the file instead of deleting whichever rule moved
/// into the old index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionsSnapshot {
    path: PathBuf,
    file_state: PermissionsFileState,
    permissions: PermissionsToml,
    removal_tokens: Vec<String>,
}

impl PermissionsSnapshot {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn file_exists(&self) -> bool {
        self.file_state != PermissionsFileState::Missing
    }

    #[must_use]
    pub fn file_state(&self) -> PermissionsFileState {
        self.file_state
    }

    #[must_use]
    pub fn permissions(&self) -> &PermissionsToml {
        &self.permissions
    }

    #[must_use]
    pub fn rules(&self) -> &[ToolAskRule] {
        &self.permissions.rules
    }

    /// Return the opaque confirmation token for a zero-based rule index.
    #[must_use]
    pub fn removal_token(&self, index: usize) -> Option<&str> {
        self.removal_tokens.get(index).map(String::as_str)
    }
}

impl PermissionsToml {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    #[must_use]
    pub fn ruleset(&self) -> Ruleset {
        let mut denied = Vec::new();
        let mut trusted = Vec::new();
        let mut ask_rules = Vec::new();

        for rule in &self.rules {
            match rule.action {
                PermissionAction::Deny => {
                    // Command-based deny rules are promoted to denied_prefixes
                    // so they are caught by execpolicy's deny-always-wins check.
                    if let Some(cmd) = &rule.command
                        && !rule.command_exact
                        && rule.workspace.is_none()
                    {
                        denied.push(cmd.clone());
                    }
                    // Always keep in ask_rules for path-based and tool-only matching.
                    ask_rules.push(rule.clone());
                }
                PermissionAction::Allow => {
                    // Command-based allow rules are promoted to trusted_prefixes
                    // for arity-aware matching.  Path-only allow rules are
                    // handled through ask_rules (they skip the approval prompt).
                    if let Some(cmd) = &rule.command
                        && !rule.command_exact
                        && rule.workspace.is_none()
                    {
                        trusted.push(cmd.clone());
                    }
                    // Keep in ask_rules so path-only allow rules also work.
                    ask_rules.push(rule.clone());
                }
                PermissionAction::Ask => {
                    ask_rules.push(rule.clone());
                }
            }
        }

        Ruleset::user(trusted, denied).with_ask_rules(ask_rules)
    }
}

impl ProvidersToml {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        // The full registry, not the selectable catalog: legacy dialect and
        // plan tables (`deepseek_anthropic`, `modelstudio_coding_plan`, ...)
        // are still fields here, and skipping the whole `[providers]` section
        // when only one of them is set would erase it on the next typed save.
        self.extras.is_empty()
            && provider::all_providers()
                .iter()
                .all(|provider| self.for_provider(provider.kind()).is_empty())
            && self.antigravity.is_empty()
    }

    #[must_use]
    pub fn for_provider(&self, provider: ProviderKind) -> &ProviderConfigToml {
        match provider {
            ProviderKind::Deepseek => &self.deepseek,
            ProviderKind::DeepseekAnthropic => &self.deepseek_anthropic,
            ProviderKind::NvidiaNim => &self.nvidia_nim,
            ProviderKind::Openai => &self.openai,
            ProviderKind::Atlascloud => &self.atlascloud,
            ProviderKind::WanjieArk => &self.wanjie_ark,
            ProviderKind::Volcengine => &self.volcengine,
            ProviderKind::Openrouter => &self.openrouter,
            ProviderKind::Orcarouter => &self.orcarouter,
            ProviderKind::XiaomiMimo => &self.xiaomi_mimo,
            ProviderKind::Novita => &self.novita,
            ProviderKind::Fireworks => &self.fireworks,
            ProviderKind::Siliconflow => &self.siliconflow,
            ProviderKind::SiliconflowCN => &self.siliconflow_cn,
            ProviderKind::Arcee => &self.arcee,
            ProviderKind::Moonshot => &self.moonshot,
            ProviderKind::Sglang => &self.sglang,
            ProviderKind::Vllm => &self.vllm,
            ProviderKind::Ollama => &self.ollama,
            ProviderKind::OllamaCloud => &self.ollama_cloud,
            ProviderKind::Huggingface => &self.huggingface,
            ProviderKind::Modelscope => &self.modelscope,
            ProviderKind::Together => &self.together,
            ProviderKind::Qianfan => &self.qianfan,
            ProviderKind::OpenaiCodex => &self.openai_codex,
            ProviderKind::Anthropic => &self.anthropic,
            ProviderKind::Openmodel => &self.openmodel,
            ProviderKind::Zai => &self.zai,
            ProviderKind::Stepfun => &self.stepfun,
            ProviderKind::Minimax => &self.minimax,
            ProviderKind::MinimaxAnthropic => &self.minimax_anthropic,
            ProviderKind::Deepinfra => &self.deepinfra,
            ProviderKind::Sakana => &self.sakana,
            ProviderKind::LongCat => &self.longcat,
            ProviderKind::OpencodeGo => &self.opencode_go,
            ProviderKind::OpencodeZen => &self.opencode_zen,
            ProviderKind::Meta => &self.meta,
            ProviderKind::Xai => &self.xai,
            ProviderKind::Mistral => &self.mistral,
            ProviderKind::Google => &self.google,
            ProviderKind::Antigravity => &self.antigravity,
            ProviderKind::Telecomjs => &self.telecomjs,
            ProviderKind::Edenai => &self.edenai,
            ProviderKind::Zenmux => &self.zenmux,
            ProviderKind::Csdn => &self.csdn,
            ProviderKind::Concentrate => &self.concentrate,
            ProviderKind::Codewhale => &self.codewhale,
            ProviderKind::ModelstudioTokenPlan => &self.modelstudio_token_plan,
            ProviderKind::ModelstudioTokenPlanAnthropic => &self.modelstudio_token_plan_anthropic,
            ProviderKind::ModelstudioCodingPlan => &self.modelstudio_coding_plan,
            ProviderKind::ModelstudioCodingPlanAnthropic => &self.modelstudio_coding_plan_anthropic,
            ProviderKind::Custom => &self.custom,
        }
    }

    pub fn for_provider_mut(&mut self, provider: ProviderKind) -> &mut ProviderConfigToml {
        match provider {
            ProviderKind::Deepseek => &mut self.deepseek,
            ProviderKind::DeepseekAnthropic => &mut self.deepseek_anthropic,
            ProviderKind::NvidiaNim => &mut self.nvidia_nim,
            ProviderKind::Openai => &mut self.openai,
            ProviderKind::Atlascloud => &mut self.atlascloud,
            ProviderKind::WanjieArk => &mut self.wanjie_ark,
            ProviderKind::Volcengine => &mut self.volcengine,
            ProviderKind::Openrouter => &mut self.openrouter,
            ProviderKind::Orcarouter => &mut self.orcarouter,
            ProviderKind::XiaomiMimo => &mut self.xiaomi_mimo,
            ProviderKind::Novita => &mut self.novita,
            ProviderKind::Fireworks => &mut self.fireworks,
            ProviderKind::Siliconflow => &mut self.siliconflow,
            ProviderKind::SiliconflowCN => &mut self.siliconflow_cn,
            ProviderKind::Arcee => &mut self.arcee,
            ProviderKind::Moonshot => &mut self.moonshot,
            ProviderKind::Sglang => &mut self.sglang,
            ProviderKind::Vllm => &mut self.vllm,
            ProviderKind::Ollama => &mut self.ollama,
            ProviderKind::OllamaCloud => &mut self.ollama_cloud,
            ProviderKind::Huggingface => &mut self.huggingface,
            ProviderKind::Modelscope => &mut self.modelscope,
            ProviderKind::Together => &mut self.together,
            ProviderKind::Qianfan => &mut self.qianfan,
            ProviderKind::OpenaiCodex => &mut self.openai_codex,
            ProviderKind::Anthropic => &mut self.anthropic,
            ProviderKind::Openmodel => &mut self.openmodel,
            ProviderKind::Zai => &mut self.zai,
            ProviderKind::Stepfun => &mut self.stepfun,
            ProviderKind::Minimax => &mut self.minimax,
            ProviderKind::MinimaxAnthropic => &mut self.minimax_anthropic,
            ProviderKind::Deepinfra => &mut self.deepinfra,
            ProviderKind::Sakana => &mut self.sakana,
            ProviderKind::LongCat => &mut self.longcat,
            ProviderKind::OpencodeGo => &mut self.opencode_go,
            ProviderKind::OpencodeZen => &mut self.opencode_zen,
            ProviderKind::Meta => &mut self.meta,
            ProviderKind::Xai => &mut self.xai,
            ProviderKind::Mistral => &mut self.mistral,
            ProviderKind::Google => &mut self.google,
            ProviderKind::Antigravity => &mut self.antigravity,
            ProviderKind::Telecomjs => &mut self.telecomjs,
            ProviderKind::Edenai => &mut self.edenai,
            ProviderKind::Zenmux => &mut self.zenmux,
            ProviderKind::Csdn => &mut self.csdn,
            ProviderKind::Concentrate => &mut self.concentrate,
            ProviderKind::Codewhale => &mut self.codewhale,
            ProviderKind::ModelstudioTokenPlan => &mut self.modelstudio_token_plan,
            ProviderKind::ModelstudioTokenPlanAnthropic => {
                &mut self.modelstudio_token_plan_anthropic
            }
            ProviderKind::ModelstudioCodingPlan => &mut self.modelstudio_coding_plan,
            ProviderKind::ModelstudioCodingPlanAnthropic => {
                &mut self.modelstudio_coding_plan_anthropic
            }
            ProviderKind::Custom => &mut self.custom,
        }
    }
}

fn deserialize_root_provider<'de, D>(deserializer: D) -> std::result::Result<ProviderKind, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    let strict = serde::de::value::StringDeserializer::<D::Error>::new(value);
    Ok(ProviderKind::deserialize(strict).unwrap_or(ProviderKind::Custom))
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConfigToml {
    // There is no top-level `api_key` or `base_url`: every parse moves the
    // legacy root keys into `[providers.<name>]` first (`legacy_root`, #6394).
    /// Optional extra HTTP headers forwarded to model API requests.
    #[serde(default, skip_serializing_if = "http_headers_are_effectively_empty")]
    pub http_headers: BTreeMap<String, String>,
    /// TUI-compatible default DeepSeek model.
    pub default_text_model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_root_provider")]
    pub provider: ProviderKind,
    /// Exact saved selector for a named custom provider or a built-in alias.
    ///
    /// This is runtime parse state rather than a second on-disk key. The
    /// serialized `provider` value is restored by [`ConfigStore`] so a typed
    /// dispatcher read/write cannot collapse a named route to `custom` or a
    /// regional selector to its catalog parent.
    #[doc(hidden)]
    #[serde(skip)]
    pub selected_provider_id: Option<String>,
    pub model: Option<String>,
    pub auth_mode: Option<String>,
    pub verbosity: Option<String>,
    pub log_level: Option<String>,
    pub telemetry: Option<bool>,
    /// Where telemetry batches are sent, when telemetry is enabled at all.
    ///
    /// Unset here means "take the shipped default",
    /// [`DEFAULT_TELEMETRY_ENDPOINT`] — not "send nowhere". Setting it to the
    /// empty string is the way to say send nowhere: that resolves to no
    /// endpoint, which appends batches to `dryrun.jsonl` and constructs no HTTP
    /// client. Either way a persistent or run-scoped opt-out still prevents any
    /// batch from being constructed.
    ///
    /// Kept as a scalar sibling of `telemetry` rather than folded into a
    /// `[telemetry]` table. `telemetry` is already a scalar and every section
    /// table is declared after it, so a table of that name would be a hard
    /// `toml::from_str` failure — and one whose cause `ConfigStore::load`
    /// deliberately hides, leaving the user with an unloadable config and no
    /// explanation. It would also be a `ValueAfterTable` serialization hazard
    /// against the scalars that follow.
    pub telemetry_endpoint: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
    /// Native tool catalog controls shared with `codewhale-tui`.
    #[serde(default)]
    pub tools: Option<ToolsToml>,
    #[serde(default, skip_serializing_if = "ProvidersToml::is_empty")]
    pub providers: ProvidersToml,
    /// Operator declarations for exact provider/endpoint/model tuples.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "catalog::configured::deserialize_configured_models"
    )]
    pub custom_models: Option<Vec<catalog::configured::ConfiguredModel>>,
    /// Provider fallback chain (#2574). TUI runtime code may advance through
    /// these providers after recoverable provider errors; config resolution
    /// itself still reports the selected primary provider.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_providers: Vec<ProviderKind>,
    /// Per-domain network policy (#135). When absent, network tools fall back
    /// to a permissive default that mirrors pre-v0.7.0 behavior.
    #[serde(default)]
    pub network: Option<NetworkPolicyToml>,
    /// Verifier-preview behavior (#2093). When absent, verifier tools keep the
    /// shipped defaults: disabled automatic preview and hunt verdict mapping.
    #[serde(default)]
    pub verifier: Option<VerifierConfigToml>,
    /// Community skill installer settings (#140). Mirrors
    /// [`SkillsToml`] from the TUI side; the dispatcher consults
    /// `registry_url` when running `deepseek skill install`.
    #[serde(default)]
    pub skills: Option<SkillsToml>,
    /// Workspace side-git snapshots (#137). The live TUI defaults this to
    /// enabled with 7-day retention when absent.
    #[serde(default)]
    pub snapshots: Option<SnapshotsToml>,
    /// Post-edit LSP diagnostics injection (#136). When absent, the engine
    /// applies the defaults documented in [`LspConfigToml`].
    #[serde(default)]
    pub lsp: Option<LspConfigToml>,
    /// Optional 1-8 hotbar slot bindings (#2064). When absent, the TUI falls
    /// back to the built-in default slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotbar: Option<Vec<HotbarBindingToml>>,
    /// App-server hook sink configuration. Kept separate from the TUI
    /// lifecycle `[hooks]` table so config rewrites preserve existing hooks.
    #[serde(default)]
    pub hook_sinks: Option<HookSinksToml>,
    /// Lifecycle event outbox (`[lifecycle_outbox]`). Opt-in: an unset or
    /// empty `path` disables the feature and leaves behavior unchanged.
    #[serde(default)]
    pub lifecycle_outbox: Option<LifecycleOutboxToml>,
    /// Per-session control socket (`[control_socket]`). Opt-in: an absent
    /// table or `enabled = false` (the default) leaves the feature off and
    /// behavior unchanged.
    #[serde(default)]
    pub control_socket: Option<ControlSocketToml>,
    /// Agent Fleet trust and security policy (#3165). When absent, fleet
    /// workers inherit conservative Sandbox defaults.
    #[serde(default)]
    pub fleet: Option<FleetConfigToml>,
    /// Workflow automatic-launch, approval, isolation, and activity
    /// persistence knobs (#4128 / Section 2.11). When absent, consumers use
    /// [`WorkflowConfigToml::default`].
    #[serde(default)]
    pub workflow: Option<WorkflowConfigToml>,
    /// Model-bound credential redaction policy (`[redaction]`). When absent,
    /// masking is enabled — the shipped security default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redaction: Option<crate::redaction::RedactionToml>,
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

impl ConfigToml {
    /// The requested model-bound masking mode, defaulting to enabled.
    ///
    /// The request only takes effect once the interactive TUI has recorded a
    /// confirmation on its startup gate; see
    /// [`crate::redaction::effective_masking`].
    #[must_use]
    pub fn redaction_model_bound_masking(&self) -> crate::redaction::ModelBoundMasking {
        self.redaction
            .as_ref()
            .map(crate::redaction::RedactionToml::model_bound_masking)
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderConfigField {
    Vendor,
    ApiKey,
    BaseUrl,
    Model,
    ContextWindow,
    Mode,
    Wire,
    AuthMode,
    InsecureSkipTlsVerify,
    AllowInsecureHttp,
    HttpHeaders,
    PathSuffix,
}

impl ProviderConfigField {
    fn parse(key: &str) -> Option<Self> {
        Some(match key {
            "vendor" => Self::Vendor,
            "api_key" => Self::ApiKey,
            "base_url" => Self::BaseUrl,
            "model" => Self::Model,
            "context_window" | "context_window_tokens" => Self::ContextWindow,
            "mode" => Self::Mode,
            "wire" | "api_style" | "protocol" | "wire_format" | "dialect" => Self::Wire,
            "auth_mode" => Self::AuthMode,
            "insecure_skip_tls_verify" => Self::InsecureSkipTlsVerify,
            "allow_insecure_http" => Self::AllowInsecureHttp,
            "http_headers" => Self::HttpHeaders,
            "path_suffix" => Self::PathSuffix,
            _ => return None,
        })
    }

    fn key(self) -> &'static str {
        match self {
            Self::Vendor => "vendor",
            Self::ApiKey => "api_key",
            Self::BaseUrl => "base_url",
            Self::Model => "model",
            Self::ContextWindow => "context_window",
            Self::Mode => "mode",
            Self::Wire => "wire",
            Self::AuthMode => "auth_mode",
            Self::InsecureSkipTlsVerify => "insecure_skip_tls_verify",
            Self::AllowInsecureHttp => "allow_insecure_http",
            Self::HttpHeaders => "http_headers",
            Self::PathSuffix => "path_suffix",
        }
    }
}

fn parse_provider_config_key(key: &str) -> Option<(ProviderKind, ProviderConfigField)> {
    let suffix = key.strip_prefix("providers.")?;
    let (provider_key, field_key) = suffix.split_once('.')?;
    let field = ProviderConfigField::parse(field_key)?;
    // Full registry, not ProviderKind::ALL: legacy dialect/plan kinds keep
    // their own [providers.*] tables even though they left the catalog.
    let provider = provider::all_providers()
        .iter()
        .map(|p| p.kind())
        .find(|kind| kind.provider().provider_config_key() == provider_key)?;
    Some((provider, field))
}

/// Split a `providers.<id>.<field>` key without resolving the provider. Used
/// for custom providers, whose ids live in `[providers.<id>]` tables inside
/// `ProvidersToml::extras` rather than in [`ProviderKind::ALL`].
fn parse_custom_provider_config_key(key: &str) -> Option<(&str, &str)> {
    let suffix = key.strip_prefix("providers.")?;
    let (provider_id, field_key) = suffix.split_once('.')?;
    (!provider_id.is_empty()).then_some((provider_id, field_key))
}

fn is_builtin_provider_config_id(provider_id: &str) -> bool {
    provider::all_providers()
        .iter()
        .any(|p| p.provider_config_key() == provider_id)
}

fn builtin_provider_kind_for_config_id(provider_id: &str) -> Option<ProviderKind> {
    provider::all_providers()
        .iter()
        .map(|p| p.kind())
        .find(|kind| kind.provider().provider_config_key() == provider_id)
}

/// Split `providers.<id>.model_context_windows.<model>` (#6108). The model leg
/// is the whole remainder, so dotted wire ids like `qwen3.5` stay intact.
fn parse_model_context_window_key(key: &str) -> Option<(&str, &str)> {
    let (provider_id, field_key) = parse_custom_provider_config_key(key)?;
    let model = field_key.strip_prefix("model_context_windows.")?;
    (!model.is_empty()).then_some((provider_id, model))
}

/// Field legs a `[providers.<id>]` custom table accepts through
/// `config set`, including the required `kind` marker.
const CUSTOM_PROVIDER_FIELD_HINT: &str = "api_key, base_url, model, context_window, mode, wire, auth_mode, \
     insecure_skip_tls_verify, allow_insecure_http, http_headers, path_suffix, kind";

fn provider_config_key(provider: ProviderKind, field: ProviderConfigField) -> String {
    format!(
        "providers.{}.{}",
        provider.provider().provider_config_key(),
        field.key()
    )
}

fn get_provider_config_value(
    config: &ProviderConfigToml,
    field: ProviderConfigField,
) -> Option<String> {
    match field {
        ProviderConfigField::Vendor => config.vendor.clone(),
        ProviderConfigField::ApiKey => config.api_key.clone(),
        ProviderConfigField::BaseUrl => config.base_url.clone(),
        ProviderConfigField::Model => config.model.clone(),
        ProviderConfigField::ContextWindow => config.context_window.map(|value| value.to_string()),
        ProviderConfigField::Mode => config.mode.clone(),
        ProviderConfigField::Wire => config.wire.clone(),
        ProviderConfigField::AuthMode => config.auth_mode.clone(),
        ProviderConfigField::InsecureSkipTlsVerify => config
            .insecure_skip_tls_verify
            .map(|value| value.to_string()),
        ProviderConfigField::AllowInsecureHttp => {
            config.allow_insecure_http.map(|value| value.to_string())
        }
        ProviderConfigField::HttpHeaders => serialize_http_headers(&config.http_headers),
        ProviderConfigField::PathSuffix => config.path_suffix.clone(),
    }
}

fn get_provider_config_display_value(
    config: &ProviderConfigToml,
    field: ProviderConfigField,
) -> Option<String> {
    match field {
        ProviderConfigField::ApiKey => config.api_key.as_deref().map(redact_secret),
        ProviderConfigField::HttpHeaders => {
            serialize_http_headers_for_display(&config.http_headers)
        }
        _ => get_provider_config_value(config, field),
    }
}

fn parse_context_window(value: &str) -> Result<u32> {
    let parsed = value.trim().parse::<u32>().with_context(|| {
        format!("invalid context_window '{value}': expected a positive token count")
    })?;
    if parsed == 0 {
        bail!("context_window must be greater than 0");
    }
    Ok(parsed)
}

fn set_provider_config_value(
    config: &mut ConfigToml,
    provider: ProviderKind,
    field: ProviderConfigField,
    value: &str,
) -> Result<()> {
    if provider == ProviderKind::Antigravity {
        bail!(LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE);
    }
    match field {
        ProviderConfigField::Vendor => {
            if provider != ProviderKind::Openrouter {
                bail!("vendor is only supported by providers.openrouter");
            }
            validate_openrouter_vendor(value)?;
            config.providers.for_provider_mut(provider).vendor = Some(value.to_string());
        }
        ProviderConfigField::ApiKey => {
            config.providers.for_provider_mut(provider).api_key = Some(value.to_string());
        }
        ProviderConfigField::BaseUrl => {
            config.providers.for_provider_mut(provider).base_url = Some(value.to_string());
        }
        // Provider-scoped values stay in their `[providers.<name>]` table.
        // The root `http_headers` / `default_text_model` apply to every
        // provider, so mirroring DeepSeek's values there sent them to other
        // providers after a switch.
        ProviderConfigField::Model => {
            config.providers.for_provider_mut(provider).model = Some(value.to_string());
        }
        ProviderConfigField::ContextWindow => {
            config.providers.for_provider_mut(provider).context_window =
                Some(parse_context_window(value)?);
        }
        ProviderConfigField::Mode => {
            config.providers.for_provider_mut(provider).mode = Some(value.to_string());
        }
        ProviderConfigField::Wire => {
            config.providers.for_provider_mut(provider).wire = Some(value.to_string());
        }
        ProviderConfigField::AuthMode => {
            config.providers.for_provider_mut(provider).auth_mode = Some(value.to_string());
        }
        ProviderConfigField::InsecureSkipTlsVerify => {
            config
                .providers
                .for_provider_mut(provider)
                .insecure_skip_tls_verify = Some(parse_bool(value)?);
        }
        ProviderConfigField::AllowInsecureHttp => {
            config
                .providers
                .for_provider_mut(provider)
                .allow_insecure_http = Some(parse_bool(value)?);
        }
        ProviderConfigField::HttpHeaders => {
            config.providers.for_provider_mut(provider).http_headers = parse_http_headers(value)?;
        }
        ProviderConfigField::PathSuffix => {
            config.providers.for_provider_mut(provider).path_suffix = Some(value.to_string());
        }
    }
    Ok(())
}

fn unset_provider_config_value(
    config: &mut ConfigToml,
    provider: ProviderKind,
    field: ProviderConfigField,
) {
    match field {
        ProviderConfigField::Vendor => {
            config.providers.for_provider_mut(provider).vendor = None;
        }
        ProviderConfigField::ApiKey => {
            config.providers.for_provider_mut(provider).api_key = None;
        }
        ProviderConfigField::BaseUrl => {
            config.providers.for_provider_mut(provider).base_url = None;
        }
        ProviderConfigField::Model => {
            let removed = config.providers.for_provider_mut(provider).model.take();
            // Earlier releases mirrored DeepSeek's model into the root key;
            // clear that copy too, but never a root value the user set apart.
            if provider == ProviderKind::Deepseek
                && removed.is_some()
                && config.default_text_model == removed
            {
                config.default_text_model = None;
            }
        }
        ProviderConfigField::ContextWindow => {
            config.providers.for_provider_mut(provider).context_window = None;
        }
        ProviderConfigField::Mode => {
            config.providers.for_provider_mut(provider).mode = None;
        }
        ProviderConfigField::Wire => {
            config.providers.for_provider_mut(provider).wire = None;
        }
        ProviderConfigField::AuthMode => {
            config.providers.for_provider_mut(provider).auth_mode = None;
        }
        ProviderConfigField::InsecureSkipTlsVerify => {
            config
                .providers
                .for_provider_mut(provider)
                .insecure_skip_tls_verify = None;
        }
        ProviderConfigField::AllowInsecureHttp => {
            config
                .providers
                .for_provider_mut(provider)
                .allow_insecure_http = None;
        }
        ProviderConfigField::HttpHeaders => {
            let removed =
                std::mem::take(&mut config.providers.for_provider_mut(provider).http_headers);
            // Earlier releases mirrored DeepSeek's headers into the root table;
            // clear that copy too, but never root headers the user set apart.
            if provider == ProviderKind::Deepseek
                && !removed.is_empty()
                && config.http_headers == removed
            {
                config.http_headers.clear();
            }
        }
        ProviderConfigField::PathSuffix => {
            config.providers.for_provider_mut(provider).path_suffix = None;
        }
    }
}

fn insert_provider_config_values(
    out: &mut BTreeMap<String, String>,
    provider: ProviderKind,
    config: &ProviderConfigToml,
) {
    if let Some(v) = config.vendor.as_ref() {
        out.insert(
            provider_config_key(provider, ProviderConfigField::Vendor),
            v.clone(),
        );
    }
    if let Some(v) = config.api_key.as_ref() {
        out.insert(
            provider_config_key(provider, ProviderConfigField::ApiKey),
            redact_secret(v),
        );
    }
    if let Some(v) = config.base_url.as_ref() {
        out.insert(
            provider_config_key(provider, ProviderConfigField::BaseUrl),
            v.clone(),
        );
    }
    if let Some(v) = config.model.as_ref() {
        out.insert(
            provider_config_key(provider, ProviderConfigField::Model),
            v.clone(),
        );
    }
    if let Some(v) = config.context_window {
        out.insert(
            provider_config_key(provider, ProviderConfigField::ContextWindow),
            v.to_string(),
        );
    }
    if let Some(v) = config.mode.as_ref() {
        out.insert(
            provider_config_key(provider, ProviderConfigField::Mode),
            v.clone(),
        );
    }
    if let Some(v) = config.auth_mode.as_ref() {
        out.insert(
            provider_config_key(provider, ProviderConfigField::AuthMode),
            v.clone(),
        );
    }
    if let Some(v) = config.insecure_skip_tls_verify {
        out.insert(
            provider_config_key(provider, ProviderConfigField::InsecureSkipTlsVerify),
            v.to_string(),
        );
    }
    if let Some(v) = config.allow_insecure_http {
        out.insert(
            provider_config_key(provider, ProviderConfigField::AllowInsecureHttp),
            v.to_string(),
        );
    }
    if let Some(v) = serialize_http_headers_for_display(&config.http_headers) {
        out.insert(
            provider_config_key(provider, ProviderConfigField::HttpHeaders),
            v,
        );
    }
    if let Some(v) = config.path_suffix.as_ref() {
        out.insert(
            provider_config_key(provider, ProviderConfigField::PathSuffix),
            v.clone(),
        );
    }
}

impl ConfigToml {
    /// Resolve durable hotbar config into normalized 1-8 slot bindings.
    ///
    /// `known_action_ids` is supplied by the TUI action registry in later
    /// slices. Unknown actions are preserved so the UI can render a disabled
    /// `?` cell instead of silently deleting user config.
    #[must_use]
    pub fn resolve_hotbar_bindings(&self, known_action_ids: &[&str]) -> HotbarConfigResolution {
        resolve_hotbar_bindings(self.hotbar.as_deref(), known_action_ids)
    }
}

/// Ordered primary-plus-fallback provider list for future provider routing.
///
/// The helper is intentionally dormant: constructing or parsing a chain does
/// not change [`ConfigToml::resolve_runtime_options`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderChain {
    providers: Vec<ProviderKind>,
    position: usize,
}

pub const HOTBAR_SLOT_COUNT: u8 = 8;

pub const DEFAULT_HOTBAR_ACTIONS: [&str; HOTBAR_SLOT_COUNT as usize] = [
    "slash.workflow",
    "slash.goal",
    "slash.auto",
    "mode.plan",
    "mode.agent",
    "mode.operate",
    "palette.open",
    "sidebar.toggle",
];

/// On-disk schema for one `[[hotbar]]` table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HotbarBindingToml {
    pub slot: u8,
    pub action: String,
    #[serde(default)]
    pub label: Option<String>,
}

/// Validated hotbar binding used by future render/dispatch layers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotbarBinding {
    pub slot: u8,
    pub action: String,
    pub label: Option<String>,
}

/// Non-fatal hotbar config issue. Invalid slots are skipped; duplicate slots
/// use the last binding; unknown actions are kept for UI feedback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotbarConfigWarning {
    SlotOutOfRange {
        slot: u8,
        action: String,
    },
    DuplicateSlot {
        slot: u8,
        previous_action: String,
        replacement_action: String,
    },
    UnknownAction {
        slot: u8,
        action: String,
    },
}

impl fmt::Display for HotbarConfigWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SlotOutOfRange { slot, action } => write!(
                f,
                "hotbar slot {slot} for action '{action}' is outside 1-{HOTBAR_SLOT_COUNT}; skipped"
            ),
            Self::DuplicateSlot {
                slot,
                previous_action,
                replacement_action,
            } => write!(
                f,
                "hotbar slot {slot} was bound to '{previous_action}' more than once; using '{replacement_action}'"
            ),
            Self::UnknownAction { slot, action } => write!(
                f,
                "hotbar slot {slot} references unknown action '{action}'; keeping binding"
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotbarConfigResolution {
    pub bindings: Vec<HotbarBinding>,
    pub warnings: Vec<HotbarConfigWarning>,
}

#[must_use]
pub fn default_hotbar_bindings() -> Vec<HotbarBinding> {
    DEFAULT_HOTBAR_ACTIONS
        .iter()
        .enumerate()
        .map(|(idx, action)| HotbarBinding {
            slot: u8::try_from(idx + 1).expect("default hotbar slot fits in u8"),
            action: (*action).to_string(),
            label: None,
        })
        .collect()
}

/// The default hotbar slots in on-disk (`[[hotbar]]`) form. Since #3807 an
/// absent `hotbar` key means "hidden", so `/hotbar on` persists these explicit
/// bindings rather than deleting the key. Kept in terms of
/// [`default_hotbar_bindings`] so `DEFAULT_HOTBAR_ACTIONS` stays the single
/// source of truth.
#[must_use]
pub fn default_hotbar_bindings_toml() -> Vec<HotbarBindingToml> {
    default_hotbar_bindings()
        .into_iter()
        .map(|binding| HotbarBindingToml {
            slot: binding.slot,
            action: binding.action,
            label: binding.label,
        })
        .collect()
}

#[must_use]
pub fn resolve_hotbar_bindings(
    configured: Option<&[HotbarBindingToml]>,
    known_action_ids: &[&str],
) -> HotbarConfigResolution {
    let known = known_action_ids.iter().copied().collect::<BTreeSet<&str>>();
    let mut warnings = Vec::new();

    let source = match configured {
        Some(bindings) => bindings
            .iter()
            .map(|binding| HotbarBinding {
                slot: binding.slot,
                action: binding.action.clone(),
                label: binding.label.clone(),
            })
            .collect::<Vec<_>>(),
        // #3807: an absent `hotbar` key means the Hotbar is hidden until the
        // user opts in (via the setup wizard or `/hotbar on`). Only an explicit
        // `[[hotbar]]` config produces bindings. `Some([])` stays "disabled".
        None => Vec::new(),
    };

    let mut by_slot: BTreeMap<u8, HotbarBinding> = BTreeMap::new();
    for binding in source {
        if !(1..=HOTBAR_SLOT_COUNT).contains(&binding.slot) {
            warnings.push(HotbarConfigWarning::SlotOutOfRange {
                slot: binding.slot,
                action: binding.action,
            });
            continue;
        }
        if !known.is_empty() && !known.contains(binding.action.as_str()) {
            warnings.push(HotbarConfigWarning::UnknownAction {
                slot: binding.slot,
                action: binding.action.clone(),
            });
        }
        if let Some(previous) = by_slot.insert(binding.slot, binding.clone()) {
            warnings.push(HotbarConfigWarning::DuplicateSlot {
                slot: binding.slot,
                previous_action: previous.action,
                replacement_action: binding.action,
            });
        }
    }

    HotbarConfigResolution {
        bindings: by_slot.into_values().collect(),
        warnings,
    }
}

impl ProviderChain {
    #[must_use]
    pub fn new(active: ProviderKind, fallbacks: &[ProviderKind]) -> Self {
        let mut providers = vec![active];
        for fallback in fallbacks {
            if *fallback != active && !providers.contains(fallback) {
                providers.push(*fallback);
            }
        }
        Self {
            providers,
            position: 0,
        }
    }

    #[must_use]
    pub fn providers(&self) -> &[ProviderKind] {
        &self.providers
    }

    #[must_use]
    pub fn position(&self) -> usize {
        self.position
    }

    #[must_use]
    pub fn current(&self) -> ProviderKind {
        self.providers
            .get(self.position)
            .copied()
            .or_else(|| self.providers.first().copied())
            .unwrap_or_default()
    }

    #[must_use]
    pub fn has_next(&self) -> bool {
        self.position + 1 < self.providers.len()
    }

    pub fn advance(&mut self) -> Option<ProviderKind> {
        if !self.has_next() {
            return None;
        }
        self.position += 1;
        Some(self.current())
    }

    pub fn reset(&mut self) {
        self.position = 0;
    }

    #[must_use]
    pub fn is_fallback_active(&self) -> bool {
        self.position > 0
    }

    /// Count the current provider plus untried chain entries.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.providers.len() - self.position
    }
}

#[cfg(test)]
mod provider_chain_tests {
    use super::*;

    #[test]
    fn current_on_empty_chain_returns_default_provider() {
        let chain = ProviderChain {
            providers: vec![],
            position: 0,
        };
        assert_eq!(chain.current(), ProviderKind::default());
    }
}

/// On-disk schema for the `[hook_sinks]` table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookSinksToml {
    /// Unix domain socket path used by the app-server event sink.
    ///
    /// Known limitation: only `codewhale app-server` reads this table; the
    /// TUI and the Runtime API do not dispatch hook events to it.
    ///
    /// When unset, no Unix socket sink is registered. There is deliberately no
    /// shared `/tmp` default because socket ownership should be explicit.
    #[serde(default)]
    pub unix_socket_path: Option<PathBuf>,
}

/// On-disk schema for the `[lifecycle_outbox]` table.
///
/// Opt-in lifecycle event outbox: every emitted event is appended as one
/// JSONL line to `path` in the `RuntimeEventEnvelope` shape
/// (`schema_version, seq, event, kind, thread_id, turn_id, item_id,
/// timestamp, payload`), and optionally POSTed to `webhook_url`. An unset or
/// empty `path` disables the feature entirely — behavior is unchanged from a
/// release without the table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LifecycleOutboxToml {
    /// Path to the JSONL outbox file. Parent directories are created lazily
    /// on the first event. Unset or empty = feature OFF.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Optional webhook URL. Events are POSTed as `{"at", "event"}` JSON
    /// only when this is set (in addition to, never instead of, `path`).
    /// Delivery is best-effort: failures are logged and dropped.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Optional bearer token sent as `Authorization: Bearer <token>` on
    /// webhook POSTs. Ignored when `webhook_url` is unset.
    #[serde(default)]
    pub webhook_token: Option<String>,
}

/// On-disk schema for the `[control_socket]` table.
///
/// Opt-in per-session control surface: when `enabled`, the interactive TUI
/// binds a unix domain socket at `<sessions-dir>/<session-id>/control.sock`
/// for the running session. The socket speaks newline-framed JSON-RPC with
/// the verbs `message`, `interrupt`, and `status`. An absent
/// table, or `enabled = false` (the default), disables the feature entirely —
/// behavior is unchanged from a release without the table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ControlSocketToml {
    /// Bind the per-session control socket. Default: false (OFF).
    #[serde(default)]
    pub enabled: bool,
}

/// On-disk schema for the `[skills]` table (#140). See `config.example.toml`
/// for documentation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsToml {
    /// Curated registry index URL. When unset, the TUI falls back to the
    /// bundled default (community-curated GitHub raw).
    #[serde(default)]
    pub registry_url: Option<String>,
    /// Per-skill maximum *uncompressed* size in bytes. When unset, the TUI
    /// uses 5 MiB.
    #[serde(default)]
    pub max_install_size_bytes: Option<u64>,
    /// Keys owned by the TUI runtime (for example `scan_codewhale_only`) or
    /// added by newer releases must survive dispatcher reads and typed saves.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

/// On-disk schema for the `[tools]` table (#2076).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsToml {
    /// Native tool names to keep loaded outside the default core catalog.
    #[serde(default)]
    pub always_load: Vec<String>,
    /// Runtime-owned tool settings must survive dispatcher reads and saves.
    /// Their validation belongs to the runtime's ToolsConfig, not this facade.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

/// On-disk schema for the `[snapshots]` table (#137). See
/// `config.example.toml` for documentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotsToml {
    #[serde(default = "default_snapshots_enabled")]
    pub enabled: bool,
    #[serde(default = "default_snapshot_max_age_days")]
    pub max_age_days: u64,
    /// Keys owned by the TUI runtime (for example `max_workspace_gb`) or
    /// added by newer releases must survive dispatcher reads and typed saves.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

fn default_snapshots_enabled() -> bool {
    true
}

fn default_snapshot_max_age_days() -> u64 {
    7
}

impl Default for SnapshotsToml {
    fn default() -> Self {
        Self {
            enabled: default_snapshots_enabled(),
            max_age_days: default_snapshot_max_age_days(),
            extras: BTreeMap::new(),
        }
    }
}

/// On-disk schema for the `[fleet]` table (#3165). See `config.example.toml`
/// and `docs/FLEET.md` for documentation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FleetConfigToml {
    /// User-defined and built-in role presets.
    ///
    /// Each role defines default tool profiles, capabilities, and execution
    /// requests that task specs can reference by name. Built-in roles
    /// (`smoke-runner`, `reviewer`, `builder`, `read-only`) are always
    /// available; user-defined roles in config override or extend them.
    #[serde(default)]
    pub roles: BTreeMap<String, FleetRolePreset>,
    /// Fleet profile vocabulary (#3167). Profiles group role semantics,
    /// loadout hints, route identity, and delegation bounds. Runtime authority
    /// is intentionally not a profile property.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub profiles: BTreeMap<String, FleetProfile>,
    /// Headless worker execution hardening (#3027).
    #[serde(default)]
    pub exec: FleetExecConfig,
}

/// Canonical recursion-depth policy for the headless worker runtime.
///
/// Single source of truth shared by BOTH standalone sub-agents and fleet
/// workers so the two cannot drift into "two moving targets":
/// - [`DEFAULT_SPAWN_DEPTH`] is the default recursion budget (the sub-agent
///   runtime's `DEFAULT_MAX_SPAWN_DEPTH` is defined as this value).
/// - [`MAX_SPAWN_DEPTH_CEILING`] is the opt-in safety cap; every configured
///   value (fleet `max_spawn_depth`, the `agent` tool's `max_depth`) clamps to it.
///
/// A worker runs at `spawn_depth = 0` and may spawn while
/// `spawn_depth + 1 <= max_spawn_depth`, so a depth of N affords N nested
/// delegation levels below the root worker. The default of 3 affords at least
/// three recursion levels out of the box; the root worker still runs at
/// depth 0 even when the budget is 0.
pub const DEFAULT_SPAWN_DEPTH: u32 = 3;
pub const DEFAULT_STREAM_CHUNK_TIMEOUT_SECS: u64 = 900;
pub const MIN_STREAM_CHUNK_TIMEOUT_SECS: u64 = 1;
pub const MAX_STREAM_CHUNK_TIMEOUT_SECS: u64 = 3600;

/// Hard ceiling on recursion depth for any worker/sub-agent. The default stays
/// conservative at [`DEFAULT_SPAWN_DEPTH`], while explicit config can opt into
/// deeper trees for direct-API providers that can tolerate the fanout.
/// Raising this single constant lifts the limit everywhere (the fleet clamp
/// and `agent` validation both read it).
pub const MAX_SPAWN_DEPTH_CEILING: u32 = 8;

/// Headless worker execution constraints (#3027).
///
/// These limits apply to all fleet workers and sub-agents spawned through
/// the headless worker runtime. Task specs can tighten but not loosen them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetExecConfig {
    /// Tools that are always allowed regardless of role or task spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    /// Tools that are always disallowed, overriding role and task spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    /// Optional hard ceiling on sub-agent steps (tool calls + model turns).
    /// Zero keeps the normal agent loop unbounded; a positive value terminates
    /// workers that exceed the explicit operator cap.
    #[serde(default = "default_fleet_max_turns")]
    pub max_turns: u32,
    /// Recursive child-agent budget for headless fleet workers.
    /// Defaults to [`DEFAULT_SPAWN_DEPTH`] (3) so a fleet worker has the SAME
    /// recursion budget as a standalone sub-agent — fleet and sub-agents are one
    /// substrate, not two. Set 0 to block child `agent` calls (the root worker
    /// still runs); the value is clamped to [`MAX_SPAWN_DEPTH_CEILING`].
    #[serde(default = "default_fleet_max_spawn_depth")]
    pub max_spawn_depth: u32,
    /// Extra system prompt text appended to every headless worker.
    /// Useful for injecting org-wide policy or behavior constraints.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub append_system_prompt: String,
    /// Output format for fleet worker results.
    /// `"text"` (default) or `"stream-json"` for newline-delimited JSON events.
    #[serde(default = "default_fleet_output_format")]
    pub output_format: String,
}

/// Fleet workers run until the model finishes unless an operator supplies a
/// positive `max_turns` value. Individual task budgets may still opt into a
/// narrower explicit model-turn cap through `budget.max_steps`; tool-call
/// admission is enforced independently through `budget.max_tool_calls`.
pub const FLEET_DEFAULT_MAX_TURNS: u32 = 0;

fn default_fleet_max_turns() -> u32 {
    FLEET_DEFAULT_MAX_TURNS
}

fn default_fleet_max_spawn_depth() -> u32 {
    DEFAULT_SPAWN_DEPTH
}

fn default_fleet_output_format() -> String {
    "text".to_string()
}

impl Default for FleetExecConfig {
    fn default() -> Self {
        Self {
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            max_turns: default_fleet_max_turns(),
            max_spawn_depth: default_fleet_max_spawn_depth(),
            append_system_prompt: String::new(),
            output_format: default_fleet_output_format(),
        }
    }
}

/// Fleet org-chart profile.
///
/// A profile is an additive config record for future fleet scheduling policy.
/// Loading one must not grant runtime permissions by itself: shell and trust
/// escalation default off, and approvals default on.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FleetProfile {
    /// Org-chart slot this profile describes.
    #[serde(default)]
    pub slot: FleetSlot,
    /// Semantic role name and optional instruction overlay.
    #[serde(default)]
    pub role: FleetRole,
    /// Model class / route-role hint. This is data only in this slice.
    #[serde(default)]
    pub loadout: FleetLoadout,
    /// Optional explicit model id for this profile on the active/resolved route.
    ///
    /// This is not an auth or endpoint selector. Provider-scoped routing still
    /// validates the executable provider/model/wire-model decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Optional explicit provider id for this profile's model (#4093).
    ///
    /// Present only when the profile was created against a specific,
    /// credential-checked provider (e.g. via the Fleet setup model picker),
    /// so a worker can be pinned to a route independent of the parent/current
    /// session provider. `None` means "no route pin" (inherit), matching
    /// `model: None`; a profile must never carry `provider` without `model`.
    ///
    /// EPIC #2608 explicit-config-only mandate: this field is the ONLY
    /// authority for the profile's provider. It is never inferred by sniffing
    /// a substring/prefix out of `model` — callers that need the provider for
    /// this profile must read this field, not guess from the model id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Optional explicit reasoning/thinking tier for this profile (#4137).
    ///
    /// This is a safe, non-secret route tuning value. `None` means inherit the
    /// operator/session reasoning tier. Concrete values are normalized by the
    /// TUI loader before they are used at runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Legacy ignored input retained for old Fleet-profile files. Runtime
    /// authority is derived after identity selection and is never persisted in
    /// this profile.
    #[doc(hidden)]
    #[serde(default, skip_serializing)]
    pub permissions: FleetProfilePermissions,
    /// Delegation hints for future manager policy.
    #[serde(default)]
    pub delegation: FleetDelegationHints,
}

/// Semantic role declaration for a fleet profile.
///
/// TOML may use either `role = "reviewer"` or a role table with `name` and
/// `instructions`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FleetRole {
    /// Stable role name, e.g. `scout`, `implementer`, or `verifier`.
    pub name: String,
    /// Optional short description for config UIs and docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional instruction overlay to apply when the role is later consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

impl Default for FleetRole {
    fn default() -> Self {
        Self {
            name: "general".to_string(),
            description: None,
            instructions: None,
        }
    }
}

impl<'de> Deserialize<'de> for FleetRole {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum FleetRoleWire {
            Name(String),
            Full {
                #[serde(default)]
                name: Option<String>,
                #[serde(default)]
                description: Option<String>,
                #[serde(default)]
                instructions: Option<String>,
            },
        }

        match FleetRoleWire::deserialize(deserializer)? {
            FleetRoleWire::Name(name) => Ok(Self {
                name,
                ..Self::default()
            }),
            FleetRoleWire::Full {
                name,
                description,
                instructions,
            } => Ok(Self {
                name: name.unwrap_or_else(|| Self::default().name),
                description,
                instructions,
            }),
        }
    }
}

/// Org-chart slot for grouping fleet profiles.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FleetSlot {
    Manager,
    Scout,
    Planner,
    Implementer,
    Reviewer,
    Verifier,
    Operator,
    Summarizer,
    #[default]
    General,
    Custom(String),
}

impl FleetSlot {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Manager => "manager",
            Self::Scout => "scout",
            Self::Planner => "planner",
            Self::Implementer => "implementer",
            Self::Reviewer => "reviewer",
            Self::Verifier => "verifier",
            Self::Operator => "operator",
            Self::Summarizer => "summarizer",
            Self::General => "general",
            Self::Custom(value) => value.as_str(),
        }
    }

    #[must_use]
    pub fn from_name(value: &str) -> Self {
        match value.trim() {
            "manager" | "coordinator" => Self::Manager,
            "scout" | "research" | "research-worker" => Self::Scout,
            "planner" | "plan" | "awaiter" => Self::Planner,
            "implementer" | "builder" => Self::Implementer,
            "reviewer" => Self::Reviewer,
            "verifier" | "tester" => Self::Verifier,
            "operator" | "incident" | "incident-worker" => Self::Operator,
            "summarizer" | "reducer" => Self::Summarizer,
            "general" | "" => Self::General,
            // Removed slots (e.g. the old "tool-heavy") and unknown names parse
            // as Custom. Note the runtime side no longer treats an undeclared
            // role as write-capable: it fails closed to the read-only `explore`
            // posture (#5575), so this is narrower than the removed variants.
            other => Self::Custom(other.to_string()),
        }
    }
}

impl Serialize for FleetSlot {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FleetSlot {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_name(&value))
    }
}

/// Model class or route-role hint for a profile.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FleetLoadout {
    /// Reuse the active session route (the operator's model). Default.
    #[default]
    Inherit,
    /// Route to the provider's faster/cheaper model class for wide fan-out.
    Fast,
    /// Unrecognized loadout names parse here (including the retired
    /// strong/balanced/deep-reasoning/code/review/tool-heavy tiers, which
    /// never routed differently). Treated as auto routing.
    Custom(String),
}

impl FleetLoadout {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Inherit => "inherit",
            Self::Fast => "fast",
            Self::Custom(value) => value.as_str(),
        }
    }

    #[must_use]
    pub fn from_name(value: &str) -> Self {
        match value.trim() {
            "inherit" | "default" | "auto" | "" => Self::Inherit,
            "fast" => Self::Fast,
            // Retired tiers (strong/balanced/deep-reasoning/code/review/
            // tool-heavy) and unknown names parse as Custom → auto routing,
            // exactly what those tiers resolved to before removal.
            other => Self::Custom(other.to_string()),
        }
    }
}

impl Serialize for FleetLoadout {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for FleetLoadout {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_name(&value))
    }
}

/// Legacy Fleet-profile permission payload retained only for source and input
/// compatibility. Runtime ignores it; Fleet identity cannot grant authority.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetProfilePermissions {
    #[doc(hidden)]
    #[serde(default, skip_serializing)]
    pub allow_shell: bool,
    #[doc(hidden)]
    #[serde(default, skip_serializing)]
    pub trust: bool,
    #[doc(hidden)]
    #[serde(default = "default_fleet_profile_approval_required", skip_serializing)]
    pub approval_required: bool,
}

fn default_fleet_profile_approval_required() -> bool {
    true
}

impl Default for FleetProfilePermissions {
    fn default() -> Self {
        Self {
            allow_shell: false,
            trust: false,
            approval_required: true,
        }
    }
}

/// Delegation hints for future fleet manager scheduling.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct FleetDelegationHints {
    /// Optional profile-level child spawn depth. `None` means inherit existing
    /// fleet/sub-agent config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spawn_depth: Option<u32>,
    /// Optional profile-level worker concurrency hint.
    #[serde(
        default,
        alias = "concurrency",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_concurrency: Option<usize>,
}

/// A named role preset that bundles common worker settings.
///
/// Task specs reference a role name (e.g. `"role": "reviewer"`), and the
/// fleet manager fills in any missing fields from the preset. User-defined
/// roles in `[fleet.roles]` override built-in defaults with the same name.
///
/// Token budgets and tool-call limits are task-level decisions — they don't
/// belong on role presets. Use `timeout_seconds` as the safety bound.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetRolePreset {
    /// Short description of what this role is for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Default tool profile (`"read-only"`, `"read-write"`, or `"custom"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_profile: Option<String>,
    /// Default set of tool names available to this role.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<String>,
    /// Default capability tags (e.g. `"rust"`, `"git"`, `"gh"`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Default timeout in seconds for tasks using this role.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl FleetConfigToml {
    /// Resolve a role preset by name. Checks user-defined roles first,
    /// then falls back to built-in role defaults.
    #[must_use]
    pub fn resolve_role(&self, name: &str) -> Option<FleetRolePreset> {
        self.roles
            .get(name)
            .cloned()
            .or_else(|| built_in_role_presets().get(name).cloned())
    }
}

/// On-disk schema for the `[workflow]` table (#4128 / Section 2.11).
///
/// Automatic Workflow launch, write/approval gates, child/isolation budgets,
/// and completed-activity persistence all read from this one model. When the
/// table is absent, consumers resolve [`WorkflowConfigToml::default`].
/// See `config.example.toml` for documentation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowConfigToml {
    /// Allow the parent agent to auto-launch Workflow for multi-agent work.
    /// Product default is on; set `false` to require explicit `/workflow`.
    #[serde(default = "default_workflow_automatic")]
    pub automatic: bool,
    /// When automatic launch is enabled, start read-only child plans without
    /// an approval card. Write/shell/network plans still consult
    /// [`Self::require_approval_for_writes`].
    #[serde(default = "default_workflow_auto_start_read_only")]
    pub auto_start_read_only: bool,
    /// Require an operator approval card before launching plans that write,
    /// elevate shell/network, or otherwise leave the read-only envelope.
    #[serde(default = "default_workflow_require_approval_for_writes")]
    pub require_approval_for_writes: bool,
    /// Hard ceiling on total children in one Workflow run (product: 1000).
    #[serde(default = "default_workflow_max_children")]
    pub max_children: u32,
    /// Maximum concurrently live agents inside one Workflow run (product: 16).
    #[serde(default = "default_workflow_max_concurrent")]
    pub max_concurrent: u32,
    /// Maximum structural nesting depth accepted for Workflow IR.
    ///
    /// This is independent of Runtime child delegation, whose default is 3
    /// and whose opt-in hard ceiling is 8.
    #[serde(default = "default_workflow_max_depth")]
    pub max_depth: u32,
    /// Default shared token budget for a Workflow run and its children.
    /// `0` applies no shared cap — the run is advisory-only like the parent
    /// turn loop — while any positive value is enforced across the run.
    #[serde(default = "default_workflow_default_token_budget")]
    pub default_token_budget: u64,
}

fn default_workflow_automatic() -> bool {
    true
}

fn default_workflow_auto_start_read_only() -> bool {
    true
}

fn default_workflow_require_approval_for_writes() -> bool {
    true
}

fn default_workflow_max_children() -> u32 {
    1000
}

fn default_workflow_max_concurrent() -> u32 {
    16
}

fn default_workflow_max_depth() -> u32 {
    5
}

fn default_workflow_default_token_budget() -> u64 {
    // Off by default: a cap the caller never asked for must not throttle a
    // run — a 120k default silently killed real fan-outs mid-task (#6189).
    // Spend discipline stays available as an explicit opt-in (tool
    // `token_budget`, spec `budget.max_tokens`, or a configured value here).
    0
}

impl Default for WorkflowConfigToml {
    fn default() -> Self {
        Self {
            automatic: default_workflow_automatic(),
            auto_start_read_only: default_workflow_auto_start_read_only(),
            require_approval_for_writes: default_workflow_require_approval_for_writes(),
            max_children: default_workflow_max_children(),
            max_concurrent: default_workflow_max_concurrent(),
            max_depth: default_workflow_max_depth(),
            default_token_budget: default_workflow_default_token_budget(),
        }
    }
}

/// Built-in role presets that are always available without config.
#[must_use]
pub fn built_in_role_presets() -> BTreeMap<String, FleetRolePreset> {
    [
        (
            "smoke-runner".to_string(),
            FleetRolePreset {
                description: Some("Lightweight read-only smoke check worker".to_string()),
                tool_profile: Some("read-only".to_string()),
                tools: vec![],
                capabilities: vec![],
                timeout_seconds: Some(300),
            },
        ),
        (
            "reviewer".to_string(),
            FleetRolePreset {
                description: Some("Read-only code and documentation review".to_string()),
                tool_profile: Some("read-only".to_string()),
                tools: vec![],
                capabilities: vec![],
                timeout_seconds: Some(600),
            },
        ),
        (
            "builder".to_string(),
            FleetRolePreset {
                description: Some(
                    "Read-write builder with compilation and test access".to_string(),
                ),
                tool_profile: Some("read-write".to_string()),
                tools: vec![],
                capabilities: vec![],
                timeout_seconds: Some(1800),
            },
        ),
        (
            "read-only".to_string(),
            FleetRolePreset {
                description: Some(
                    "Minimal read-only observer with no writes or secrets".to_string(),
                ),
                tool_profile: Some("read-only".to_string()),
                tools: vec![],
                capabilities: vec![],
                timeout_seconds: Some(300),
            },
        ),
    ]
    .into()
}

/// On-disk schema for `[verifier]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifierConfigToml {
    /// Enable automatic verifier preview when the runtime wires a
    /// claim-of-done trigger. Manual `run_verifiers` remains available
    /// regardless.
    #[serde(default)]
    pub enabled: bool,
}

/// On-disk schema for `[advisor]` (#3982).
///
/// Advisor mode is **off by default**. When enabled, the engine spawns a
/// short-lived background reviewer after each turn that contained tool calls.
/// The reviewer reads a bounded slice of recent tool calls, makes a concise
/// LLM advisory call, and emits the note as an `AdvisoryNote` event without
/// blocking the parent turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdvisorConfigToml {
    /// Master on/off switch. `false` by default — no background reviewer is
    /// spawned until the user opts in via `[advisor] enabled = true` or
    /// `/advisor on`.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum number of recent tool-call/result pairs to include in each
    /// advisory review. Keeps the reviewer's context window bounded regardless
    /// of turn length. Defaults to 10; clamped to 1–50.
    #[serde(default = "advisor_default_max_tool_calls")]
    pub max_tool_calls: u32,
    /// Minimum wall-clock seconds between two consecutive advisor emissions.
    /// Prevents noise on rapid multi-turn sequences. Defaults to 60 seconds;
    /// clamped to 5–3600.
    #[serde(default = "advisor_default_rate_limit_secs")]
    pub rate_limit_secs: u64,
    /// Deduplication window in seconds. An advisory note whose content hash
    /// matches the previous note within this window is silently dropped.
    /// Defaults to 300 seconds (5 minutes).
    #[serde(default = "advisor_default_dedup_window_secs")]
    pub dedup_window_secs: u64,
    /// Optional model override for the advisor LLM call. When absent, the
    /// advisor reuses the session's current model.
    #[serde(default)]
    pub model: Option<String>,
}

fn advisor_default_max_tool_calls() -> u32 {
    10
}
fn advisor_default_rate_limit_secs() -> u64 {
    60
}
fn advisor_default_dedup_window_secs() -> u64 {
    300
}

impl Default for AdvisorConfigToml {
    fn default() -> Self {
        Self {
            enabled: false,
            max_tool_calls: advisor_default_max_tool_calls(),
            rate_limit_secs: advisor_default_rate_limit_secs(),
            dedup_window_secs: advisor_default_dedup_window_secs(),
            model: None,
        }
    }
}

/// On-disk schema for the `[network]` table (#135). See `config.example.toml`
/// for documentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicyToml {
    /// Decision for hosts that are not in `allow` or `deny`. One of
    /// `"allow" | "deny" | "prompt"`. Defaults to `"prompt"`.
    #[serde(default = "default_network_decision")]
    pub default: String,
    /// Hosts that are always allowed. Subdomain rules: a leading dot
    /// (`.example.com`) matches subdomains but not the apex.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Hosts that are always denied. Deny entries win over allow entries.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Hostnames whose DNS may resolve to fake-IP/private proxy ranges in an
    /// explicitly trusted proxy setup. Literal IP URLs remain blocked.
    #[serde(default)]
    pub proxy: Vec<String>,
    /// Explicit fake-IP placeholder CIDRs for those proxy hosts. The runtime
    /// accepts only subnets contained by `198.18.0.0/15`.
    #[serde(default)]
    pub proxy_fake_ip_cidrs: Vec<String>,
    /// Whether to record one audit-log line per outbound network call.
    #[serde(default = "default_network_audit")]
    pub audit: bool,
    /// Keys owned by the TUI runtime or added by newer releases must survive
    /// dispatcher reads and typed saves.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

fn default_network_decision() -> String {
    "prompt".to_string()
}

fn default_network_audit() -> bool {
    true
}

impl Default for NetworkPolicyToml {
    fn default() -> Self {
        Self {
            default: default_network_decision(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: default_network_audit(),
            extras: BTreeMap::new(),
        }
    }
}

/// User-defined LSP server for one file extension (used inside
/// [`LspConfigToml::custom`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CustomLspDef {
    /// LSP `languageId` value used in `textDocument/didOpen`.
    pub language_id: String,
    /// Executable to spawn.
    pub command: String,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
}

/// On-disk schema for the `[lsp]` table (#136). See `config.example.toml`
/// for documentation. All fields are optional so the TUI runtime can fall
/// back to its own defaults when keys are absent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LspConfigToml {
    /// Master switch.
    pub enabled: Option<bool>,
    /// Maximum time to wait for diagnostics after an edit, in milliseconds.
    pub poll_after_edit_ms: Option<u64>,
    /// Cap on diagnostics surfaced per file.
    pub max_diagnostics_per_file: Option<usize>,
    /// When `true`, warnings (severity 2) are surfaced in addition to errors.
    pub include_warnings: Option<bool>,
    /// Optional override for the `language -> [cmd, ...args]` table.
    pub servers: Option<BTreeMap<String, Vec<String>>>,
    /// User-defined LSP servers for file extensions not in the built-in
    /// registry. Keyed by extension (e.g. `"php"`, `"rb"`).
    pub custom: Option<BTreeMap<String, CustomLspDef>>,
    /// Keys owned by the TUI runtime or added by newer releases must survive
    /// dispatcher reads and typed saves.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

impl ConfigToml {
    /// Exact configured provider id, including a dynamically named custom
    /// provider selected by the TUI.
    #[must_use]
    pub fn provider_id(&self) -> &str {
        self.selected_provider_id
            .as_deref()
            .filter(|id| {
                if self.provider == ProviderKind::Custom {
                    self.providers.extras.contains_key(*id)
                } else {
                    ProviderKind::parse_config_identity(id) == Some(self.provider)
                        && self.providers.extras.get(*id).is_none_or(|value| {
                            value
                                .as_table()
                                .is_some_and(|table| !table.contains_key("kind"))
                        })
                }
            })
            .unwrap_or_else(|| self.provider.as_str())
    }

    /// The real key behind a legacy top-level `api_key` / `base_url`.
    ///
    /// Those keys no longer exist at the top level (#6394); `config get|set|
    /// unset base_url` addresses the active provider's own table instead, so
    /// the value lands where the route reads it.
    #[must_use]
    pub fn root_alias_key(&self, key: &str) -> Option<String> {
        let field = match key {
            "api_key" | "apiKey" => "api_key",
            "base_url" | "baseUrl" => "base_url",
            _ => return None,
        };
        let table = self
            .named_custom_provider_id()
            .unwrap_or_else(|| self.provider.provider().provider_config_key());
        Some(format!("providers.{table}.{field}"))
    }

    /// Return the exact id only when the root selection names a dynamic custom
    /// provider rather than the legacy literal `custom` route.
    #[must_use]
    pub fn named_custom_provider_id(&self) -> Option<&str> {
        (self.provider == ProviderKind::Custom)
            .then_some(self.selected_provider_id.as_deref())
            .flatten()
            .filter(|id| self.providers.extras.contains_key(*id))
    }

    fn named_custom_provider_table(&self, provider_id: &str) -> Result<&toml::value::Table> {
        let table = self
            .providers
            .extras
            .get(provider_id)
            .and_then(toml::Value::as_table)
            .with_context(|| {
                format!(
                    "custom provider '{provider_id}' requires a matching [providers.{provider_id}] table"
                )
            })?;
        let compatible = table
            .get("kind")
            .and_then(toml::Value::as_str)
            .is_some_and(|kind| {
                kind.trim()
                    .to_ascii_lowercase()
                    .replace('_', "-")
                    .eq("openai-compatible")
            });
        if !compatible {
            bail!(
                "custom provider '{provider_id}' must set [providers.{provider_id}].kind = \"openai-compatible\""
            );
        }
        Ok(table)
    }

    /// The typed `[providers.<id>]` table of a named custom provider. A table
    /// that fails validation is an error, never a reason to read another one.
    fn named_custom_provider_config_for(&self, provider_id: &str) -> Result<ProviderConfigToml> {
        let table = self.named_custom_provider_table(provider_id)?;
        // The deserializer error can quote the offending value, which may be a
        // credential, so report only which table is wrong.
        toml::Value::Table(table.clone()).try_into().map_err(|_| {
            anyhow::anyhow!(
                "custom provider '{provider_id}' has an invalid [providers.{provider_id}] table: a field has the wrong type"
            )
        })
    }

    /// Mutable access to a custom provider's `[providers.<id>]` table,
    /// creating it on the first `config set providers.<id>.<field>`.
    fn custom_provider_table_mut(&mut self, provider_id: &str) -> Result<&mut toml::value::Table> {
        let entry = self
            .providers
            .extras
            .entry(provider_id.to_string())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        entry.as_table_mut().with_context(|| {
            format!("custom provider '{provider_id}' must be a [providers.{provider_id}] table")
        })
    }

    /// Write one leg of a custom provider table. Named custom providers are
    /// not in [`ProviderKind::ALL`], so without this path
    /// `config set providers.<custom>.<field>` fell through to a literal
    /// top-level extras key and silently never took effect (#5167).
    fn set_custom_provider_value(
        &mut self,
        provider_id: &str,
        field_key: &str,
        value: &str,
    ) -> Result<()> {
        if is_builtin_provider_config_id(provider_id) {
            bail!(
                "unknown field '{field_key}' for built-in provider '{provider_id}': \
                 expected one of api_key, base_url, model, context_window, mode, auth_mode, \
                 insecure_skip_tls_verify, http_headers, path_suffix"
            );
        }
        if field_key == "kind" {
            let compatible =
                value.trim().to_ascii_lowercase().replace('_', "-") == "openai-compatible";
            if !compatible {
                bail!(
                    "custom provider '{provider_id}' must set [providers.{provider_id}].kind = \"openai-compatible\""
                );
            }
            self.custom_provider_table_mut(provider_id)?.insert(
                "kind".to_string(),
                toml::Value::String(value.trim().to_string()),
            );
            return Ok(());
        }
        let Some(field) = ProviderConfigField::parse(field_key) else {
            bail!(
                "unknown field '{field_key}' for custom provider '{provider_id}': \
                 expected one of {CUSTOM_PROVIDER_FIELD_HINT}"
            );
        };
        let toml_value = match field {
            ProviderConfigField::Vendor => {
                bail!("vendor is only supported by providers.openrouter")
            }
            ProviderConfigField::ApiKey
            | ProviderConfigField::BaseUrl
            | ProviderConfigField::Model
            | ProviderConfigField::Mode
            | ProviderConfigField::Wire
            | ProviderConfigField::AuthMode
            | ProviderConfigField::PathSuffix => toml::Value::String(value.to_string()),
            ProviderConfigField::ContextWindow => {
                toml::Value::Integer(i64::from(parse_context_window(value)?))
            }
            ProviderConfigField::InsecureSkipTlsVerify | ProviderConfigField::AllowInsecureHttp => {
                toml::Value::Boolean(parse_bool(value)?)
            }
            ProviderConfigField::HttpHeaders => toml::Value::Table(
                parse_http_headers(value)?
                    .into_iter()
                    .map(|(name, header)| (name, toml::Value::String(header)))
                    .collect(),
            ),
        };
        self.custom_provider_table_mut(provider_id)?
            .insert(field.key().to_string(), toml_value);
        Ok(())
    }

    fn get_custom_provider_value_with(
        &self,
        provider_id: &str,
        field_key: &str,
        render: fn(&ProviderConfigToml, ProviderConfigField) -> Option<String>,
    ) -> Option<String> {
        let table = self.providers.extras.get(provider_id)?.as_table()?;
        if field_key == "kind" {
            return table.get("kind")?.as_str().map(str::to_string);
        }
        let field = ProviderConfigField::parse(field_key)?;
        let config: ProviderConfigToml = toml::Value::Table(table.clone()).try_into().ok()?;
        render(&config, field)
    }

    fn unset_custom_provider_value(&mut self, provider_id: &str, field_key: &str) {
        let Some(table) = self
            .providers
            .extras
            .get_mut(provider_id)
            .and_then(toml::Value::as_table_mut)
        else {
            return;
        };
        let leg = if field_key == "kind" {
            "kind"
        } else {
            ProviderConfigField::parse(field_key).map_or(field_key, |field| field.key())
        };
        table.remove(leg);
    }

    /// Write one `[providers.<id>.model_context_windows]` entry (#6108),
    /// whether `<id>` is a built-in provider key or a named custom table.
    fn set_model_context_window(
        &mut self,
        provider_id: &str,
        model: &str,
        value: &str,
    ) -> Result<()> {
        if model.eq_ignore_ascii_case("auto")
            || model.chars().any(|c| c.is_whitespace() || c.is_control())
        {
            bail!("invalid model id for `model_context_windows`");
        }
        let window = parse_context_window(value)?;
        if let Some(kind) = builtin_provider_kind_for_config_id(provider_id) {
            self.providers
                .for_provider_mut(kind)
                .model_context_windows
                .insert(model.to_string(), window);
            return Ok(());
        }
        let table = self.custom_provider_table_mut(provider_id)?;
        let windows = table
            .entry("model_context_windows".to_string())
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()));
        windows
            .as_table_mut()
            .with_context(|| {
                format!("providers.{provider_id}.model_context_windows must be a table")
            })?
            .insert(model.to_string(), toml::Value::Integer(i64::from(window)));
        Ok(())
    }

    fn model_context_window_value(&self, provider_id: &str, model: &str) -> Option<String> {
        if let Some(kind) = builtin_provider_kind_for_config_id(provider_id) {
            return self
                .providers
                .for_provider(kind)
                .model_context_windows
                .get(model)
                .map(u32::to_string);
        }
        self.providers
            .extras
            .get(provider_id)?
            .as_table()?
            .get("model_context_windows")?
            .as_table()?
            .get(model)?
            .as_integer()
            .map(|value| value.to_string())
    }

    fn unset_model_context_window(&mut self, provider_id: &str, model: &str) {
        if let Some(kind) = builtin_provider_kind_for_config_id(provider_id) {
            self.providers
                .for_provider_mut(kind)
                .model_context_windows
                .remove(model);
            return;
        }
        if let Some(table) = self
            .providers
            .extras
            .get_mut(provider_id)
            .and_then(toml::Value::as_table_mut)
            && let Some(windows) = table
                .get_mut("model_context_windows")
                .and_then(toml::Value::as_table_mut)
        {
            windows.remove(model);
            if windows.is_empty() {
                table.remove("model_context_windows");
            }
        }
    }

    /// Bind the raw selector after deserializing a document. Exact custom
    /// tables take precedence over built-in aliases, and regional spellings
    /// survive later typed saves. This does not apply environment overrides.
    pub fn bind_persisted_provider_id(&mut self, provider_id: &str) -> Result<()> {
        let provider_id = provider_id.trim();
        // Earlier typed saves wrote the serde kebab-case spelling
        // `siliconflow-c-n` instead of the canonical `siliconflow-CN`. Read it
        // back as the built-in so those files load and the next save repairs
        // them, unless the user really declared a table by that name.
        let provider_id = if provider_id.eq_ignore_ascii_case("siliconflow-c-n")
            && !self.providers.extras.contains_key(provider_id)
        {
            ProviderKind::SiliconflowCN.as_str()
        } else {
            provider_id
        };
        let parsed = ProviderKind::parse_config_identity(provider_id);
        // Kindless tables mirroring a built-in alias remain inert. An explicit
        // custom declaration must validate; never fall back to a different
        // credential/endpoint authority when its kind is invalid.
        let custom_table = self.providers.extras.get(provider_id);
        let kindless_alias = parsed.is_some()
            && custom_table
                .and_then(toml::Value::as_table)
                .is_some_and(|table| !table.contains_key("kind"));
        let provider = if parsed != Some(ProviderKind::Antigravity)
            && custom_table.is_some()
            && !kindless_alias
        {
            self.named_custom_provider_config_for(provider_id)?;
            ProviderKind::Custom
        } else if let Some(provider) = parsed {
            provider
        } else {
            self.named_custom_provider_config_for(provider_id)?;
            ProviderKind::Custom
        };
        self.provider = provider;
        self.selected_provider_id = (provider_id != provider.as_str()
            && (provider != ProviderKind::Custom
                || self.providers.extras.contains_key(provider_id)))
        .then(|| provider_id.to_string());
        Ok(())
    }

    #[must_use]
    pub fn get_value(&self, key: &str) -> Option<String> {
        if let Some(alias) = self.root_alias_key(key) {
            return self.get_value(&alias);
        }
        if notifications::in_namespace(key) {
            let setting = notifications::NotificationSetting::parse(key)?;
            return Some(
                notifications::from_extras(&self.extras)
                    .ok()?
                    .display(setting),
            );
        }
        if let Some((provider_id, model)) = parse_model_context_window_key(key) {
            return self.model_context_window_value(provider_id, model);
        }
        if let Some((provider, field)) = parse_provider_config_key(key) {
            return get_provider_config_value(self.providers.for_provider(provider), field);
        }
        if let Some((provider_id, field_key)) = parse_custom_provider_config_key(key) {
            return self.get_custom_provider_value_with(
                provider_id,
                field_key,
                get_provider_config_value,
            );
        }

        match key {
            "provider" => Some(self.provider_id().to_string()),
            "stream_chunk_timeout_secs" | "tui.stream_chunk_timeout_secs" => {
                Some(self.stream_chunk_timeout_secs().to_string())
            }
            "http_headers" => serialize_http_headers(&self.http_headers),
            "default_text_model" => self.default_text_model.clone(),
            "model" => self.model.clone(),
            "auth.mode" => self.auth_mode.clone(),
            "verbosity" => self.verbosity.clone(),
            "log_level" => self.log_level.clone(),
            "telemetry" => self.telemetry.map(|v| v.to_string()),
            "telemetry_endpoint" => self.telemetry_endpoint.clone(),
            "approval_policy" => self.approval_policy.clone(),
            "sandbox_mode" => self.sandbox_mode.clone(),
            "tools.always_load" => self.tools.as_ref().map(|tools| tools.always_load.join(",")),
            "hook_sinks.unix_socket_path" => self
                .hook_sinks
                .as_ref()
                .and_then(|sinks| sinks.unix_socket_path.as_ref())
                .map(|path| path.display().to_string()),
            _ => self
                .extras
                .get(key)
                .map(toml::Value::to_string)
                .or_else(|| {
                    let document = toml::Value::try_from(self).ok()?;
                    config_value_at_path(&document, key).map(toml::Value::to_string)
                }),
        }
    }

    /// The unquoted contents of an extras key that holds a TOML string.
    ///
    /// [`ConfigToml::get_value`] renders extras through `toml::Value::to_string`,
    /// which re-applies TOML quoting — and switches to a single-quoted literal
    /// string whenever the payload contains a `"`. A JSON blob written with
    /// [`ConfigToml::set_value`] therefore comes back as `'[{"a":1}]'` and no
    /// longer parses as JSON (#4727). Callers that stored structured text want
    /// the payload, not its TOML rendering.
    #[must_use]
    pub fn get_raw_string(&self, key: &str) -> Option<&str> {
        self.extras.get(key).and_then(toml::Value::as_str)
    }

    #[must_use]
    pub fn get_display_value(&self, key: &str) -> Option<String> {
        if let Some(alias) = self.root_alias_key(key) {
            return self.get_display_value(&alias);
        }
        if notifications::in_namespace(key) {
            return self.get_value(key);
        }
        if let Some((provider_id, model)) = parse_model_context_window_key(key) {
            return self.model_context_window_value(provider_id, model);
        }
        if let Some((provider, field)) = parse_provider_config_key(key) {
            return get_provider_config_display_value(self.providers.for_provider(provider), field);
        }
        if let Some((provider_id, field_key)) = parse_custom_provider_config_key(key) {
            return self.get_custom_provider_value_with(
                provider_id,
                field_key,
                get_provider_config_display_value,
            );
        }

        if key == "telemetry" {
            // Report the resolved configuration preference even when the key is
            // absent. The telemetry owner also preserves recorded opt-outs
            // and fails closed when existing privacy state is unreadable.
            let (on, source) = resolved_telemetry_consent(self.telemetry);
            return Some(format!(
                "{} ({})",
                if on { "on" } else { "off" },
                source.as_str()
            ));
        }

        if key == "http_headers" {
            return serialize_http_headers_for_display(&self.http_headers);
        }

        if let Some(value) = self.extras.get(key) {
            return Some(redact_toml_value_for_display(key, value));
        }

        // Table and nested lookups use the same recursively redacted tree as
        // `config dump`; a parent such as `credentials` must keep its children
        // secret even when the requested leaf itself has an innocuous name.
        let document = self.redacted_toml_value();
        if let Some(value) = config_value_at_path(&document, key)
            && (value.is_table() || value.is_array() || key.contains('.'))
            && !matches!(key, "tui.stream_chunk_timeout_secs")
        {
            return Some(
                value
                    .as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| value.to_string()),
            );
        }

        self.get_value(key).map(|value| {
            if is_sensitive_config_key(key) {
                redact_secret(&value)
            } else {
                value
            }
        })
    }

    #[must_use]
    pub fn stream_chunk_timeout_secs(&self) -> u64 {
        let raw = self
            .extras
            .get("tui")
            .and_then(toml::Value::as_table)
            .and_then(|table| table.get("stream_chunk_timeout_secs"))
            .and_then(toml_value_as_u64)
            .or_else(|| {
                self.extras
                    .get("tui.stream_chunk_timeout_secs")
                    .and_then(toml_value_as_u64)
            })
            .or_else(|| {
                self.extras
                    .get("stream_chunk_timeout_secs")
                    .and_then(toml_value_as_u64)
            })
            .unwrap_or(DEFAULT_STREAM_CHUNK_TIMEOUT_SECS);
        if raw == 0 {
            DEFAULT_STREAM_CHUNK_TIMEOUT_SECS
        } else {
            raw.clamp(MIN_STREAM_CHUNK_TIMEOUT_SECS, MAX_STREAM_CHUNK_TIMEOUT_SECS)
        }
    }

    pub fn set_value(&mut self, key: &str, value: &str) -> Result<()> {
        if let Some(alias) = self.root_alias_key(key) {
            return self.set_value(&alias, value);
        }
        check_config_toml_choice(key, value)?;
        if let Some(field) = key.strip_prefix("stream.") {
            let def = setting(key).with_context(|| format!("unknown stream setting `{key}`"))?;
            let stored = schema_toml_value(key, def, value)?;
            if let Some(number) = stored.as_integer() {
                // Retry counts are u32 in the runtime reader; other values
                // are u64. Reject invalid types before touching the document.
                let max = if matches!(
                    field,
                    "max_resumes" | "max_transparent_retries" | "max_stream_errors"
                ) {
                    i64::from(u32::MAX)
                } else {
                    i64::MAX
                };
                anyhow::ensure!(
                    (0..=max).contains(&number),
                    "invalid unsigned value for `{key}`"
                );
            }
            self.extras
                .entry("stream".to_string())
                .or_insert_with(|| toml::Value::Table(toml::Table::new()))
                .as_table_mut()
                .context("stream must be a TOML table")?
                .insert(field.to_string(), stored);
            return Ok(());
        }
        if notifications::in_namespace(key) {
            let setting = notifications::NotificationSetting::required(key)?;
            let update = notifications::NotificationConfigUpdate::parse(setting, value)?;
            return notifications::edit_extras(&mut self.extras, setting, update.value()?);
        }
        if parse_custom_provider_config_key(key).is_some_and(|(provider_id, _)| {
            ProviderKind::parse_config_identity(provider_id) == Some(ProviderKind::Antigravity)
        }) {
            bail!(LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE);
        }
        if let Some((provider_id, model)) = parse_model_context_window_key(key) {
            return self.set_model_context_window(provider_id, model, value);
        }
        if let Some((provider, field)) = parse_provider_config_key(key) {
            return set_provider_config_value(self, provider, field, value);
        }
        if let Some((provider_id, field_key)) = parse_custom_provider_config_key(key) {
            return self.set_custom_provider_value(provider_id, field_key, value);
        }

        match key {
            "provider" => {
                if ProviderKind::parse_config_identity(value) == Some(ProviderKind::Antigravity) {
                    bail!(LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE);
                }
                self.bind_persisted_provider_id(value).with_context(|| {
                    format!(
                        "unknown provider '{value}': expected {} or a configured custom provider",
                        ProviderKind::names_hint()
                    )
                })?;
            }
            "http_headers" => self.http_headers = parse_http_headers(value)?,
            "default_text_model" => self.default_text_model = Some(value.to_string()),
            "model" => self.model = Some(value.to_string()),
            "auth.mode" => self.auth_mode = Some(value.to_string()),
            "verbosity" => self.verbosity = Some(value.to_string()),
            "log_level" => self.log_level = Some(value.to_string()),
            "telemetry" => {
                self.telemetry = Some(parse_bool(value)?);
            }
            // Scheme rules (HTTPS, or loopback HTTP) are enforced where a
            // batch would actually be sent, not here: a user must be able to
            // stage a value before the machinery that reads it exists.
            "telemetry_endpoint" => self.telemetry_endpoint = Some(value.to_string()),
            "approval_policy" => self.approval_policy = Some(value.to_string()),
            "sandbox_mode" => self.sandbox_mode = Some(value.to_string()),
            // The TUI reader (`ReasoningEffort::parse_strict`) owns this
            // vocabulary and accepts aliases the schema's option list does
            // not name (`none`, `mid`, `maximum`, ...), so the schema check
            // below would refuse values that take effect. `codewhale config
            // set` validates against that reader before calling here.
            "reasoning_effort" => {
                self.extras
                    .insert(key.to_string(), toml::Value::String(value.to_string()));
            }
            "hook_sinks.unix_socket_path" => {
                self.hook_sinks
                    .get_or_insert_with(HookSinksToml::default)
                    .unix_socket_path = Some(PathBuf::from(value));
            }
            // The MCP stdio dispatcher persists this established literal key
            // as JSON text; it is not a nested TOML setting.
            _ if key.contains('.') && key != "mcp.server_definitions" => {
                let (table, field) = key.rsplit_once('.').expect("dotted key");
                bail!(
                    "`config set` does not support nested key `{key}`; edit `{field}` in the [{table}] table of config.toml instead (use a TOML value of the documented type). No value was changed."
                );
            }
            _ => {
                // A declared setting keeps its declared type (#6563): refuse a
                // value the schema cannot hold, and store booleans and numbers
                // as TOML values so typed readers deserialize them. Whether an
                // undeclared key is read by anything is the caller's question;
                // `codewhale config set` refuses those before reaching here.
                let stored = match setting(key) {
                    Some(def) => schema_toml_value(key, def, value)?,
                    None => toml::Value::String(value.to_string()),
                };
                self.extras.insert(key.to_string(), stored);
            }
        }
        Ok(())
    }

    pub fn unset_value(&mut self, key: &str) -> Result<()> {
        if let Some(alias) = self.root_alias_key(key) {
            return self.unset_value(&alias);
        }
        if let Some(field) = key.strip_prefix("stream.") {
            anyhow::ensure!(setting(key).is_some(), "unknown stream setting `{key}`");
            if let Some(stream) = self.extras.get_mut("stream") {
                stream
                    .as_table_mut()
                    .context("stream must be a TOML table")?
                    .remove(field);
            }
            return Ok(());
        }
        if notifications::in_namespace(key) {
            let setting = notifications::NotificationSetting::required(key)?;
            return notifications::edit_extras(&mut self.extras, setting, None);
        }
        if let Some((provider_id, model)) = parse_model_context_window_key(key) {
            self.unset_model_context_window(provider_id, model);
            return Ok(());
        }
        if let Some((provider, field)) = parse_provider_config_key(key) {
            unset_provider_config_value(self, provider, field);
            return Ok(());
        }
        if let Some((provider_id, field_key)) = parse_custom_provider_config_key(key) {
            self.unset_custom_provider_value(provider_id, field_key);
            return Ok(());
        }

        match key {
            "provider" => {
                self.provider = ProviderKind::Deepseek;
                self.selected_provider_id = None;
            }
            "http_headers" => self.http_headers.clear(),
            "default_text_model" => self.default_text_model = None,
            "model" => self.model = None,
            "auth.mode" => self.auth_mode = None,
            "verbosity" => self.verbosity = None,
            "log_level" => self.log_level = None,
            "telemetry" => self.telemetry = None,
            "telemetry_endpoint" => self.telemetry_endpoint = None,
            "approval_policy" => self.approval_policy = None,
            "sandbox_mode" => self.sandbox_mode = None,
            "hook_sinks.unix_socket_path" => {
                if let Some(sinks) = self.hook_sinks.as_mut() {
                    sinks.unix_socket_path = None;
                }
            }
            _ => {
                self.extras.remove(key);
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn list_values(&self) -> BTreeMap<String, String> {
        let mut out = BTreeMap::new();
        out.insert("provider".to_string(), self.provider_id().to_string());

        if let Some(v) = serialize_http_headers_for_display(&self.http_headers) {
            out.insert("http_headers".to_string(), v);
        }
        if let Some(v) = self.default_text_model.as_ref() {
            out.insert("default_text_model".to_string(), v.clone());
        }
        if let Some(v) = self.model.as_ref() {
            out.insert("model".to_string(), v.clone());
        }
        if let Some(v) = self.auth_mode.as_ref() {
            out.insert("auth.mode".to_string(), v.clone());
        }
        if let Some(v) = self.verbosity.as_ref() {
            out.insert("verbosity".to_string(), v.clone());
        }
        if let Some(v) = self.log_level.as_ref() {
            out.insert("log_level".to_string(), v.clone());
        }
        if let Some(v) = self.telemetry {
            out.insert("telemetry".to_string(), v.to_string());
        }
        if let Some(v) = self.telemetry_endpoint.as_ref() {
            out.insert("telemetry_endpoint".to_string(), v.clone());
        }
        if let Some(v) = self.approval_policy.as_ref() {
            out.insert("approval_policy".to_string(), v.clone());
        }
        if let Some(v) = self.sandbox_mode.as_ref() {
            out.insert("sandbox_mode".to_string(), v.clone());
        }
        if let Some(v) = self
            .hook_sinks
            .as_ref()
            .and_then(|sinks| sinks.unix_socket_path.as_ref())
        {
            out.insert(
                "hook_sinks.unix_socket_path".to_string(),
                v.display().to_string(),
            );
        }

        for provider in provider::all_providers().iter().map(|p| p.kind()) {
            insert_provider_config_values(
                &mut out,
                provider,
                self.providers.for_provider(provider),
            );
        }

        for (k, v) in &self.extras {
            if k == "notifications" {
                if let Ok(config) = notifications::from_extras(&self.extras) {
                    for setting in notifications::NotificationSetting::ALL {
                        out.insert(
                            format!("notifications.{}", setting.key()),
                            config.display(setting),
                        );
                    }
                } else {
                    out.insert(k.clone(), "<invalid notification configuration>".into());
                }
            } else if notifications::in_namespace(k)
                && notifications::NotificationSetting::parse(k).is_some()
            {
                if let Ok(config) = notifications::from_extras(&self.extras) {
                    let setting = notifications::NotificationSetting::parse(k)
                        .expect("known notification key");
                    out.insert(
                        format!("notifications.{}", setting.key()),
                        config.display(setting),
                    );
                }
            } else {
                out.insert(k.clone(), redact_toml_value_for_display(k, v));
            }
        }
        out
    }

    /// Resolve runtime options without touching platform credential stores.
    ///
    /// This method keeps library callers prompt-free: CLI flag → config file
    /// → environment. Call `resolve_runtime_options_with_secrets` when a
    /// user-facing dispatcher should recover credentials from the configured
    /// secret store.
    #[must_use]
    pub fn resolve_runtime_options(&self, cli: &CliRuntimeOverrides) -> ResolvedRuntimeOptions {
        let no_keyring = Secrets::new(std::sync::Arc::new(
            codewhale_secrets::InMemoryKeyringStore::new(),
        ));
        self.resolve_runtime_options_with_secrets(cli, &no_keyring)
    }

    /// Resolve runtime options using an explicit secrets façade.
    ///
    /// API-key precedence is **CLI flag → config-file → secret store → environment**.
    #[must_use]
    pub fn resolve_runtime_options_with_secrets(
        &self,
        cli: &CliRuntimeOverrides,
        secrets: &Secrets,
    ) -> ResolvedRuntimeOptions {
        let env = EnvRuntimeOverrides::load();
        let (provider, provider_source) = if let Some(provider) = cli.provider {
            (provider, ProviderSource::Cli)
        } else if let Some(provider) = env.provider {
            (
                provider,
                ProviderSource::Env(env.provider_source.unwrap_or("CODEWHALE_PROVIDER")),
            )
        } else {
            (self.provider, ProviderSource::Config)
        };

        let named_custom_provider = (provider == ProviderKind::Custom
            && matches!(provider_source, ProviderSource::Config))
        .then(|| self.named_custom_provider_id())
        .flatten();
        let mut provider_cfg = if let Some(provider_id) = named_custom_provider {
            // A named route whose table became invalid after binding resolves
            // to the fail-closed loopback placeholder, never to the legacy
            // `[providers.custom]` endpoint, model and key.
            self.named_custom_provider_config_for(provider_id)
                .unwrap_or_else(|err| {
                    tracing::warn!("{err:#}");
                    ProviderConfigToml::default()
                })
        } else {
            self.providers.for_provider(provider).clone()
        };
        if provider == ProviderKind::SiliconflowCN {
            let fb = &self.providers.siliconflow;
            if provider_cfg.api_key.is_none() {
                provider_cfg.api_key = fb.api_key.clone();
            }
            if provider_cfg.base_url.is_none() {
                provider_cfg.base_url = fb.base_url.clone();
            }
            if provider_cfg.model.is_none() {
                provider_cfg.model = fb.model.clone();
            }
        }
        let auth_mode = cli
            .auth_mode
            .clone()
            .or_else(|| env.auth_mode.clone())
            .or_else(|| provider_cfg.auth_mode.clone())
            .or_else(|| self.auth_mode.clone());
        // The legacy top-level key and endpoint were moved into their owning
        // `[providers.<name>]` table when the file was parsed (#6394).
        let from_file = provider_cfg.api_key.clone();
        let cli_base_url = cli.base_url.clone();
        let env_base_url = env.base_url_for(provider);
        let file_base_url = provider_cfg.base_url.clone();
        let base_url_from_file =
            cli_base_url.is_none() && env_base_url.is_none() && file_base_url.is_some();
        let configured_base_url = cli_base_url.or(env_base_url).or(file_base_url);
        let xiaomi_mimo_mode = if provider == ProviderKind::XiaomiMimo {
            env.xiaomi_mimo_mode
                .clone()
                .or_else(|| provider_cfg.mode.clone())
        } else {
            None
        };
        let xiaomi_mimo_env_api_key = if provider == ProviderKind::XiaomiMimo {
            xiaomi_mimo_env_api_key_for_runtime(
                xiaomi_mimo_mode.as_deref(),
                configured_base_url.as_deref(),
            )
        } else {
            None
        };
        let explicit_api_key_for_endpoint = cli
            .api_key
            .as_deref()
            .or(from_file.as_deref().filter(|value| {
                classify_config_api_key_value(value) == ConfigApiKeyValueKind::Literal
            }))
            .or(xiaomi_mimo_env_api_key.as_deref());
        let provider_wire = provider_cfg.wire.as_deref();
        let base_url = if provider == ProviderKind::XiaomiMimo {
            resolve_xiaomi_mimo_base_url(
                configured_base_url,
                explicit_api_key_for_endpoint,
                xiaomi_mimo_mode.as_deref(),
            )
        } else if is_modelstudio_family(provider) {
            resolve_modelstudio_base_url(
                configured_base_url,
                provider,
                provider_cfg.mode.as_deref(),
                provider_wire,
            )
        } else if matches!(
            provider,
            ProviderKind::Minimax | ProviderKind::MinimaxAnthropic
        ) {
            resolve_minimax_base_url(configured_base_url, provider, provider_wire)
        } else if matches!(
            provider,
            ProviderKind::Deepseek | ProviderKind::DeepseekAnthropic
        ) {
            resolve_deepseek_base_url(configured_base_url, provider, provider_wire)
        } else {
            configured_base_url
                .unwrap_or_else(|| descriptor_fallback_base_url(provider, auth_mode.as_deref()))
        };
        // Released builds represented Ollama Cloud as the local `ollama`
        // identity plus one exact hosted base URL. Upgrade only that tuple in
        // memory: the parsed config and secret store are never rewritten, and
        // neighboring/custom routes retain the local/custom identity.
        let legacy_ollama_cloud = provider::migrates_legacy_ollama_cloud_route(provider, &base_url);
        let provider = if legacy_ollama_cloud {
            ProviderKind::OllamaCloud
        } else {
            provider
        };
        // `auth_mode = "none"` is an endpoint contract, so it suppresses every
        // credential source (including explicit CLI/config values). Otherwise
        // CLI and route-local config win outright. Ambient provider credentials
        // are allowed only on the provider's official endpoint family: a saved
        // OpenRouter key must never follow `provider = "openrouter"` to an
        // unrelated custom gateway merely because the provider id stayed the
        // same.
        let uses_kimi_imported_token = provider == ProviderKind::Moonshot
            && auth_mode
                .as_deref()
                .is_some_and(auth_mode_uses_kimi_imported_token);
        let auth_disabled = auth_mode_disables_api_key(auth_mode.as_deref());
        let custom_endpoint = provider_preserves_custom_base_url_model(provider, &base_url);
        let (api_key, api_key_source) = if auth_disabled {
            (None, None)
        } else if let Some(value) = cli.api_key.clone() {
            (Some(value), Some(RuntimeApiKeySource::Cli))
        } else if uses_kimi_imported_token && !custom_endpoint {
            (None, None)
        } else if (!custom_endpoint || base_url_from_file)
            && let Some(value) = from_file.clone().filter(|value| {
                classify_config_api_key_value(value) == ConfigApiKeyValueKind::Literal
            })
        {
            (Some(value), Some(RuntimeApiKeySource::ConfigFile))
        } else if !custom_endpoint
            && let Some(value) = xiaomi_mimo_env_api_key.filter(|v| !v.trim().is_empty())
        {
            (Some(value), Some(RuntimeApiKeySource::Env))
        } else if custom_endpoint {
            (None, None)
        } else if should_skip_secret_store_for_provider(provider, &base_url, auth_mode.as_deref()) {
            match env_api_key_for_provider(provider) {
                Some(value) => (Some(value), Some(RuntimeApiKeySource::Env)),
                None => (None, None),
            }
        } else {
            match stored_api_key_for_provider(secrets, provider, legacy_ollama_cloud) {
                Some((value, source)) => {
                    let source = match source {
                        SecretSource::Keyring => RuntimeApiKeySource::Keyring,
                        SecretSource::Env => RuntimeApiKeySource::Env,
                    };
                    (Some(value), Some(source))
                }
                None => match env_api_key_for_provider(provider) {
                    Some(value) => (Some(value), Some(RuntimeApiKeySource::Env)),
                    None => (None, None),
                },
            }
        };

        let env_provider_model = env.model_for(provider, &base_url);
        // Root `default_text_model` is the key `codewhale model set` writes and
        // the setup wizard writes, for every provider. It used to enter this
        // chain only when `provider == Deepseek`, which made this resolver
        // disagree with `Config::default_model()` in the TUI — the chain that
        // actually builds the request — for every non-DeepSeek provider
        // (#4832, #4838). The user's model still shipped; only this resolver,
        // and therefore `codewhale model resolve`, reported a provider default.
        //
        // It is honoured for any provider now, minus the one case the DeepSeek
        // gate was accidentally covering: a stale DeepSeek id left behind by a
        // provider switch must not be forwarded to an endpoint that cannot
        // serve it.
        let root_default_model = self
            .default_text_model
            .clone()
            .filter(|model| !root_default_model_is_foreign_to_provider(provider, model, &base_url));
        // Derived from the same chain as `model` below so the reported
        // provenance cannot drift from the id that is actually used.
        let model_source = if cli.model.is_some() {
            ModelSource::Cli
        } else if env.model.is_some() || env_provider_model.is_some() {
            ModelSource::Env
        } else if provider_cfg.model.is_some() {
            ModelSource::ProviderConfig
        } else if root_default_model.is_some() {
            ModelSource::RootDefaultTextModel
        } else if self.model.is_some() {
            ModelSource::RootModel
        } else {
            ModelSource::ProviderDefault
        };
        let explicit_model = model_source.is_explicit();
        let model = cli
            .model
            .clone()
            .or_else(|| env.model.clone())
            .or(env_provider_model)
            .or_else(|| provider_cfg.model.clone())
            .or(root_default_model)
            .or_else(|| self.model.clone())
            .unwrap_or_else(|| {
                if provider == ProviderKind::Moonshot
                    && (auth_mode
                        .as_deref()
                        .is_some_and(auth_mode_uses_kimi_imported_token)
                        || moonshot_base_url_uses_kimi_code(&base_url))
                {
                    DEFAULT_KIMI_CODE_MODEL.to_string()
                } else {
                    cloud_facts::cloud_default_model_for_route(provider, &base_url)
                        .map(|(model, _)| model)
                        .unwrap_or_else(|| default_model_for_provider(provider).to_string())
                }
            });
        let model = if provider == ProviderKind::OpencodeGo {
            // OpenCode Go's `/models` response also contains models that only
            // speak Anthropic Messages. This provider is deliberately bound to
            // Chat Completions, so even custom endpoint/env overrides cannot
            // promote an incompatible id onto `/chat/completions`.
            normalize_model_for_provider(provider, &model)
        } else if explicit_model && provider_preserves_custom_base_url_model(provider, &base_url) {
            model.trim().to_string()
        } else {
            normalize_model_for_provider(provider, &model)
        };

        // RouteResolver is the runtime path: the executable wire model,
        // protocol, and endpoint come from a ReadyRouteCandidate. Auth/key
        // resolution above is unchanged. A resolver error keeps the existing
        // model string so this method stays total, and keeps the error itself
        // so no caller can present the rejected model as a resolved route.
        let route = crate::route::RouteResolver::new().resolve(&crate::route::RouteRequest {
            explicit_provider: Some(provider),
            model_selector: Some(crate::route::LogicalModelRef::from(model.as_str())),
            saved_provider_model: None,
            base_url_override: Some(base_url.clone()),
            limit_overrides: Vec::new(),
        });

        let mut http_headers = self.http_headers.clone();
        http_headers.extend(provider_cfg.http_headers.clone());
        if let Some(env_headers) = env.http_headers {
            http_headers.extend(env_headers);
        }
        http_headers.retain(|name, value| !name.trim().is_empty() && !value.trim().is_empty());
        if auth_disabled {
            http_headers.retain(|name, _| !is_upstream_auth_header(name));
        }

        let log_level = cli
            .log_level
            .clone()
            .or_else(|| env.log_level.clone())
            .or_else(|| self.log_level.clone());
        // The telemetry preference resolves once in the shared core behind
        // [`resolved_telemetry_consent`]. The telemetry owner also checks old
        // durable declines and unreadable privacy state. The
        // comments that matter live there: the
        // environment/file/default chain, and why every kill switch is a
        // floor (`telemetry = false` persisted in the file is the *persistent*
        // off switch; an explicit env "off", an unreadable env value, or a
        // dispatcher-declared floor forces off regardless of CLI flag or
        // config file).
        let (telemetry_env_file, telemetry_source_env_file) = telemetry_consent_from_env(
            env.telemetry,
            env.telemetry_env_invalid,
            env.telemetry_floor,
            self.telemetry,
        );
        // The CLI flag is a run-scoped term on top: `--telemetry false` stops
        // this run; `--telemetry true` can never climb over a kill switch.
        // The source names the CLI only when the CLI term actually decided
        // the outcome — a flag that lost to a kill switch is not the provenance.
        let telemetry = telemetry_env_file && cli.telemetry != Some(false);
        let telemetry_source = if cli.telemetry == Some(false)
            || (cli.telemetry == Some(true) && telemetry_env_file)
        {
            TelemetrySource::Cli
        } else {
            telemetry_source_env_file
        };
        let telemetry_persisted_off = self.telemetry == Some(false);
        // Only a *persisted* off is an answer. `--telemetry false` and
        // `CODEWHALE_TELEMETRY=0` are run-scoped kill switches: they must stop
        // this run without deleting the identity and buffered events of a user
        // who never revoked consent — the dispatcher forwards a resolved
        // `false` on every ordinary run, so treating an environment "off" as an
        // answer would also make the default state indistinguishable from a
        // revocation.
        let telemetry_explicit_off = telemetry_persisted_off;
        // The shipped default is [`DEFAULT_TELEMETRY_ENDPOINT`], and it is a
        // default rather than a floor: an explicit value in the environment or
        // the config file wins outright. An explicit *empty* value is not a
        // missing value — it is the local dry-run sink, and it stays reachable
        // by resolving to `None` instead of falling through to the default.
        //
        // None of this changes the user's opt-out. A session only reaches an
        // endpoint after `telemetry` above resolved true; every persistent and
        // run-scoped kill switch is upstream of this line.
        let telemetry_endpoint = match env
            .telemetry_endpoint
            .clone()
            .or_else(|| self.telemetry_endpoint.clone())
        {
            Some(configured) if configured.trim().is_empty() => None,
            Some(configured) => Some(configured),
            None => Some(DEFAULT_TELEMETRY_ENDPOINT.to_string()),
        };
        let approval_policy = cli
            .approval_policy
            .clone()
            .or_else(|| env.approval_policy.clone())
            .or_else(|| self.approval_policy.clone());
        let sandbox_mode = cli
            .sandbox_mode
            .clone()
            .or_else(|| env.sandbox_mode.clone())
            .or_else(|| self.sandbox_mode.clone());
        let yolo = cli.yolo.or(env.yolo);
        let verbosity = cli
            .verbosity
            .clone()
            .or_else(|| env.verbosity.clone())
            .or_else(|| self.verbosity.clone());

        ResolvedRuntimeOptions {
            provider,
            provider_source,
            model,
            model_source,
            api_key,
            api_key_source,
            base_url,
            auth_mode,
            insecure_skip_tls_verify: provider_cfg.insecure_skip_tls_verify.unwrap_or(false),
            log_level,
            telemetry,
            telemetry_source,
            telemetry_explicit_off,
            telemetry_endpoint,
            approval_policy,
            sandbox_mode,
            yolo,
            verbosity,
            http_headers,
            route,
        }
    }
}

/// Default base URL from the route descriptor, plus the one Moonshot/Kimi
/// imported-token exception that is an auth-mode fact rather than a kind.
fn descriptor_fallback_base_url(provider: ProviderKind, auth_mode: Option<&str>) -> String {
    if provider == ProviderKind::Moonshot
        && auth_mode.is_some_and(auth_mode_uses_kimi_imported_token)
    {
        return DEFAULT_KIMI_CODE_BASE_URL.to_string();
    }
    crate::route::ProviderDescriptor::for_kind(provider)
        .default_base_url()
        .to_string()
}

/// Where an enabled session's batches go when nobody has said otherwise.
///
/// The first-party ingest service — a Cloudflare Worker that appends to Workers
/// Analytics Engine, with an optional disclosed PostHog sink. See
/// `docs/TELEMETRY.md` for what a batch contains and `telemetry-ingest/` for the handler.
///
/// This is a *default*, not a floor, and it changes nothing about permission:
/// `CODEWHALE_TELEMETRY=0`, `telemetry = false`, and a recorded decline all
/// stop the session long before an endpoint is read.
///
/// An explicit value — `CODEWHALE_TELEMETRY_ENDPOINT` or `telemetry_endpoint` in
/// the config file — wins outright, and an explicit *empty* value resolves to no
/// endpoint at all, which is the local dry-run sink: batches are serialized
/// exactly as a server would see them and appended to
/// `$CODEWHALE_HOME/telemetry/dryrun.jsonl`, and no HTTP client is constructed.
pub const DEFAULT_TELEMETRY_ENDPOINT: &str = "https://telemetry.codewhale.net/v1/telemetry";

/// Provider-neutral credential value forwarded from the CLI dispatcher to the
/// in-process TUI when `--api-key` must survive profile-late route selection.
pub const CLI_API_KEY_ENV: &str = "CODEWHALE_CLI_API_KEY";

/// Source marker paired with [`CLI_API_KEY_ENV`] on the CLI-to-TUI boundary.
pub const CLI_API_KEY_SOURCE_ENV: &str = "CODEWHALE_CLI_API_KEY_SOURCE";

/// Read-only compatibility alias used by dispatchers before v0.9.12.
pub const LEGACY_CLI_API_KEY_SOURCE_ENV: &str = "DEEPSEEK_API_KEY_SOURCE";

/// The dispatcher's statement to the TUI child about *why* telemetry is off.
///
/// Private to the `codewhale` → `codewhale-tui` hop, in the same spirit as
/// [`CLI_API_KEY_SOURCE_ENV`]. Set to `1`/`0` on every delegated run.
pub const TELEMETRY_FLOOR_ENV: &str = "CODEWHALE_TELEMETRY_FLOOR";

/// Whether an environment-level kill switch forces telemetry off here.
///
/// A floor is *not* the same as "telemetry resolved to false": off is the
/// default, and the dispatcher forwards a resolved `CODEWHALE_TELEMETRY=false`
/// on every ordinary run, so a child reading only that value cannot tell an
/// operator's declared kill switch from the shipped default. That distinction
/// matters exactly once — the first-run notice must not ask a question whose
/// answer this environment overrides — so the dispatcher states it outright in
/// [`TELEMETRY_FLOOR_ENV`] and the child believes the statement.
///
/// With no statement (a directly launched `codewhale-tui`) the raw environment
/// is read instead, where an explicit "off" or an unreadable value is a floor.
#[must_use]
pub fn telemetry_floor_in_force() -> bool {
    if let Ok(raw) = std::env::var(TELEMETRY_FLOOR_ENV)
        && let Ok(declared) = parse_bool(&raw)
    {
        return declared;
    }
    let Ok(raw) =
        std::env::var("CODEWHALE_TELEMETRY").or_else(|_| std::env::var("DEEPSEEK_TELEMETRY"))
    else {
        return false;
    };
    !matches!(parse_bool(&raw), Ok(true))
}

/// Where resolved telemetry consent came from (#5441).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetrySource {
    /// `--telemetry` on this run's command line.
    Cli,
    /// `CODEWHALE_TELEMETRY`/`DEEPSEEK_TELEMETRY`, including the dispatcher's
    /// floor statement and every environment kill switch.
    Env,
    /// `telemetry = …` written to the config file.
    Config,
    /// Nobody said anything; the shipped preference is on.
    Default,
}

impl TelemetrySource {
    /// Stable label for the doctor row and config display.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Env => "env",
            Self::Config => "config",
            Self::Default => "default",
        }
    }
}

/// Read the telemetry environment override, reporting an unreadable value
/// instead of swallowing it.
///
/// Returns `(value, invalid)`. `invalid` is `true` only when the variable
/// was set to something [`parse_bool`] rejected; an unset variable is
/// simply `(None, false)`. Shared by the runtime resolver and the
/// provenance surfaces so they cannot drift.
fn read_telemetry_env() -> (Option<bool>, bool) {
    let Some(raw) = std::env::var("CODEWHALE_TELEMETRY")
        .or_else(|_| std::env::var("DEEPSEEK_TELEMETRY"))
        .ok()
    else {
        return (None, false);
    };
    match parse_bool(&raw) {
        Ok(value) => (Some(value), false),
        Err(_) => {
            tracing::warn!(
                "Invalid CODEWHALE_TELEMETRY/DEEPSEEK_TELEMETRY value; expected one of \
                 1/0, true/false, yes/no, on/off, enabled/disabled. Telemetry is forced off."
            );
            (None, true)
        }
    }
}

/// Resolved telemetry consent with its source, for surfaces that hold the
/// config-file value but not the full CLI/env resolution chain — the doctor
/// runtime-posture row and the `config get telemetry` display (#5441).
///
/// This is the same resolution [`ConfigToml::resolve_runtime_options`]
/// applies without its CLI term: environment first (an explicit value, an
/// unreadable one, or a dispatcher floor), then the file, then the shipped
/// default of `on`; a persisted `telemetry = false` is a floor no later
/// term can climb over. The runtime resolver calls this directly, so the
/// preference surfaces agree. The telemetry owner also checks historical
/// durable declines and fails closed on unreadable privacy state.
#[must_use]
pub fn resolved_telemetry_consent(file_telemetry: Option<bool>) -> (bool, TelemetrySource) {
    let (env_telemetry, env_invalid) = read_telemetry_env();
    telemetry_consent_from_env(
        env_telemetry,
        env_invalid,
        telemetry_floor_in_force(),
        file_telemetry,
    )
}

/// The decision core shared by [`resolved_telemetry_consent`] and the runtime
/// resolver, which already holds a snapshot of the same environment facts.
#[must_use]
fn telemetry_consent_from_env(
    env_telemetry: Option<bool>,
    env_invalid: bool,
    floor: bool,
    file_telemetry: Option<bool>,
) -> (bool, TelemetrySource) {
    let persisted_off = file_telemetry == Some(false);
    let allowed = env_telemetry.or(file_telemetry).unwrap_or(true);
    let on = allowed && env_telemetry != Some(false) && !env_invalid && !floor && !persisted_off;
    let source = if !on && (env_telemetry == Some(false) || env_invalid || floor) {
        // An environment kill switch decided the outcome.
        TelemetrySource::Env
    } else if !on && persisted_off {
        // The persistent opt-out outranked everything else in play.
        TelemetrySource::Config
    } else if env_telemetry.is_some() {
        TelemetrySource::Env
    } else if file_telemetry.is_some() {
        TelemetrySource::Config
    } else {
        TelemetrySource::Default
    };
    (on, source)
}

/// Values config.toml's root `approval_policy` accepts, compared trimmed and
/// case-insensitively. settings.toml's `approval_policy` is a different
/// vocabulary (`use-tui-default`, `ask`, `auto-review`, `full-access`).
pub const CONFIG_TOML_APPROVAL_POLICIES: &[&str] =
    &["on-request", "untrusted", "never", "auto", "suggest"];
/// Values config.toml's root `sandbox_mode` accepts.
pub const CONFIG_TOML_SANDBOX_MODES: &[&str] = &[
    "read-only",
    "workspace-write",
    "danger-full-access",
    "external-sandbox",
];
/// Values config.toml's root `verbosity` accepts.
pub const CONFIG_TOML_VERBOSITIES: &[&str] = &["normal", "concise"];

/// The closed vocabulary of a config.toml root key, if it has one.
#[must_use]
pub fn config_toml_choices(key: &str) -> Option<&'static [&'static str]> {
    match key {
        "approval_policy" => Some(CONFIG_TOML_APPROVAL_POLICIES),
        "sandbox_mode" => Some(CONFIG_TOML_SANDBOX_MODES),
        "verbosity" => Some(CONFIG_TOML_VERBOSITIES),
        _ => None,
    }
}

/// Refuse a value the config.toml loader would reject for a closed-vocabulary
/// root key, so shared setters cannot write a file the TUI then fails to load.
pub fn check_config_toml_choice(key: &str, value: &str) -> Result<()> {
    let Some(choices) = config_toml_choices(key) else {
        return Ok(());
    };
    if choices.contains(&value.trim().to_ascii_lowercase().as_str()) {
        return Ok(());
    }
    let settings_note = if key == "approval_policy" {
        " (`use-tui-default`, `ask`, `auto-review` and `full-access` are settings.toml \
         values for the /settings editor; config.toml does not read them.)"
    } else {
        ""
    };
    let value = codewhale_secrets::redact::redact_secrets(value);
    bail!(
        "invalid value '{value}' for '{key}': config.toml accepts {}.{settings_note} \
         No value was changed.\nfix: codewhale config set {key} {}",
        choices.join(", "),
        choices[0]
    )
}

#[must_use]
pub fn project_approval_policy_is_allowed(current: Option<&str>, project: &str) -> bool {
    let Some(project_rank) = approval_policy_rank(project) else {
        return false;
    };
    match current.and_then(approval_policy_rank) {
        Some(current_rank) => project_rank >= current_rank,
        None => project_rank >= 2,
    }
}

#[must_use]
pub fn project_sandbox_mode_is_allowed(current: Option<&str>, project: &str) -> bool {
    let normalized_project = project.trim().to_ascii_lowercase();
    if normalized_project == "external-sandbox" {
        return current
            .map(|value| value.trim().eq_ignore_ascii_case("external-sandbox"))
            .unwrap_or(false);
    }

    let Some(project_rank) = sandbox_mode_rank(project) else {
        return false;
    };
    match current.and_then(sandbox_mode_rank) {
        Some(current_rank) => project_rank >= current_rank,
        None => project_rank >= 2,
    }
}

fn approval_policy_rank(value: &str) -> Option<u8> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(0),
        "suggest" | "suggested" | "on-request" | "untrusted" => Some(1),
        "never" | "deny" | "denied" => Some(2),
        _ => None,
    }
}

fn sandbox_mode_rank(value: &str) -> Option<u8> {
    match value.trim().to_ascii_lowercase().as_str() {
        "danger-full-access" => Some(0),
        "external-sandbox" => Some(0),
        "workspace-write" => Some(1),
        "read-only" => Some(2),
        _ => None,
    }
}

/// What [`load_project_config_outcome`] found in the workspace.
///
/// The distinction between "no project config" and "a project config that is
/// broken" is security-relevant, so it is in the type rather than in a log
/// line. A project config can only *tighten* `approval_policy` /
/// `sandbox_mode` beyond the user's baseline; if a typo makes it unparseable
/// and that is reported as absence, the project silently loses its
/// restrictions and falls back to the user's more permissive baseline.
#[derive(Debug, Clone)]
pub enum ProjectConfigOutcome {
    /// No project config file exists in this workspace.
    Missing,
    /// A project config was found and parsed.
    Loaded(Box<ConfigToml>),
    /// A project config file exists but could not be used. Its contents are
    /// deliberately not included — a config file holds credentials.
    Invalid {
        /// The offending file.
        path: PathBuf,
        /// Why it could not be used, safe to display.
        reason: String,
    },
}

impl ProjectConfigOutcome {
    /// The parsed config, discarding the reason a broken one was rejected.
    #[must_use]
    pub fn into_config(self) -> Option<ConfigToml> {
        match self {
            Self::Loaded(config) => Some(*config),
            Self::Missing | Self::Invalid { .. } => None,
        }
    }

    /// The path and reason when a project config exists but is unusable.
    #[must_use]
    pub fn invalid(&self) -> Option<(&Path, &str)> {
        match self {
            Self::Invalid { path, reason } => Some((path.as_path(), reason.as_str())),
            Self::Missing | Self::Loaded(_) => None,
        }
    }
}

/// Load a project-level config from the workspace, reporting why a file that
/// exists could not be used.
///
/// Checks `$WORKSPACE/.codewhale/config.toml` first, falling back to
/// `$WORKSPACE/.deepseek/config.toml` for backward compatibility.
pub fn load_project_config_outcome(workspace: &Path) -> ProjectConfigOutcome {
    for dir in [CODEWHALE_APP_DIR, LEGACY_APP_DIR] {
        let path = workspace.join(dir).join(CONFIG_FILE_NAME);
        if !project_config_candidate_exists(&path) {
            continue;
        }
        let raw = match read_checked_config_file(&path) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!("Failed to read project config {}: {e:#}", path.display());
                return ProjectConfigOutcome::Invalid {
                    path,
                    reason: format!("could not be read: {e}"),
                };
            }
        };
        match parse_config_toml_str(&raw) {
            Ok(config) => {
                let raw_provider = toml::from_str::<toml::Value>(&raw)
                    .ok()
                    .and_then(|document| document.get("provider").cloned())
                    .and_then(|provider| provider.as_str().map(str::to_string));
                if config.provider == ProviderKind::Custom
                    && raw_provider.as_deref() != Some(ProviderKind::Custom.as_str())
                {
                    // An unrecognized provider name deserializes to `Custom`
                    // rather than failing, so a typo would otherwise be
                    // accepted as a deliberate custom-provider selection.
                    tracing::warn!(
                        "Failed to parse project config {}; file contents were omitted",
                        quote_os_path(&path)
                    );
                    return ProjectConfigOutcome::Invalid {
                        path,
                        reason: match raw_provider {
                            // A key pasted into `provider =` must not be
                            // echoed; an ordinary typo stays readable.
                            Some(name) => format!(
                                "unknown provider '{}'",
                                codewhale_secrets::redact::redact_secrets(&name)
                            ),
                            None => "unknown provider".to_string(),
                        },
                    };
                }
                return ProjectConfigOutcome::Loaded(Box::new(config));
            }
            Err(err) => {
                tracing::warn!(
                    "Failed to parse project config {}; file contents were omitted",
                    quote_os_path(&path)
                );
                return ProjectConfigOutcome::Invalid {
                    path,
                    // Position only: `toml`'s message and snippet can quote
                    // the offending value, which may be a credential.
                    reason: format!("invalid TOML at {}", config_toml_error_location(&raw, &err)),
                };
            }
        }
    }
    ProjectConfigOutcome::Missing
}

/// Load a project-level config from the workspace.
///
/// Returns `None` both when no project config exists and when one exists but
/// is unusable. Callers that act on the *absence* of project restrictions —
/// anything deciding whether a project tightens `approval_policy` or
/// `sandbox_mode` — should use [`load_project_config_outcome`] instead, so a
/// broken file is not read as "this project asked for nothing."
pub fn load_project_config(workspace: &Path) -> Option<ConfigToml> {
    load_project_config_outcome(workspace).into_config()
}

fn project_config_candidate_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| {
        let file_type = metadata.file_type();
        file_type.is_file() || file_type.is_symlink()
    })
}

/// Canonical id for a DeepSeek-family model name, or `None` for anything else.
///
/// Kept behaviourally identical to `normalize_model_name` in
/// `crates/tui/src/config.rs`, which is the definition the TUI's own model
/// chain uses. It exists here only so this crate can answer "is this root
/// default a DeepSeek id?" without depending on the TUI.
fn deepseek_family_model_id(model: &str) -> Option<String> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.to_ascii_lowercase().as_str() {
        "pro" | "deepseek-v4pro" => return Some("deepseek-v4-pro".to_string()),
        "flash" | "deepseek-v4flash" => return Some("deepseek-v4-flash".to_string()),
        "flash-vision" | "deepseek-v4flashvisionexp" => {
            return Some("deepseek-v4-flash-vision-exp".to_string());
        }
        _ => {}
    }

    let normalized = trimmed.to_ascii_lowercase();
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
fn provider_passes_model_through(provider: ProviderKind) -> bool {
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
fn root_default_model_is_foreign_to_provider(
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

fn normalize_model_for_provider(provider: ProviderKind, model: &str) -> String {
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
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner"
            | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_NVIDIA_NIM_FLASH_MODEL.to_string(),
        (ProviderKind::Openrouter, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_OPENROUTER_MODEL.to_string()
        }
        (
            ProviderKind::Openrouter,
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner"
            | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_OPENROUTER_FLASH_MODEL.to_string(),
        (ProviderKind::Orcarouter, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_ORCAROUTER_MODEL.to_string()
        }
        (
            ProviderKind::Orcarouter,
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner"
            | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_ORCAROUTER_FLASH_MODEL.to_string(),
        (ProviderKind::Novita, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_NOVITA_MODEL.to_string()
        }
        (
            ProviderKind::Novita,
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner"
            | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_NOVITA_FLASH_MODEL.to_string(),
        (ProviderKind::Fireworks, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_FIREWORKS_MODEL.to_string()
        }
        (
            ProviderKind::Siliconflow | ProviderKind::SiliconflowCN,
            "deepseek-v4-pro" | "deepseek-v4pro" | "deepseek-reasoner" | "deepseek-r1",
        ) => DEFAULT_SILICONFLOW_MODEL.to_string(),
        (
            ProviderKind::Siliconflow | ProviderKind::SiliconflowCN,
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-v3",
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
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner"
            | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_SGLANG_FLASH_MODEL.to_string(),
        (ProviderKind::Vllm, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_VLLM_MODEL.to_string()
        }
        (
            ProviderKind::Vllm,
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner"
            | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_VLLM_FLASH_MODEL.to_string(),
        (ProviderKind::Huggingface, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_HUGGINGFACE_MODEL.to_string()
        }
        (
            ProviderKind::Huggingface,
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner"
            | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_HUGGINGFACE_FLASH_MODEL.to_string(),
        (ProviderKind::Together, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_TOGETHER_MODEL.to_string()
        }
        (
            ProviderKind::Together,
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner"
            | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_TOGETHER_FLASH_MODEL.to_string(),
        (ProviderKind::Deepinfra, "deepseek-v4-pro" | "deepseek-v4pro") => {
            DEFAULT_DEEPINFRA_MODEL.to_string()
        }
        (
            ProviderKind::Deepinfra,
            "deepseek-v4-flash" | "deepseek-v4flash" | "deepseek-chat" | "deepseek-reasoner"
            | "deepseek-r1" | "deepseek-v3" | "deepseek-v3.2",
        ) => DEFAULT_DEEPINFRA_FLASH_MODEL.to_string(),
        _ => model.to_string(),
    }
}

fn canonical_xiaomi_mimo_model_id(model: &str) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    let normalized = normalized.replace(['_', ' '], "-");
    match normalized.as_str() {
        "mimo"
        | DEFAULT_XIAOMI_MIMO_MODEL
        | "mimo-v2-5-pro"
        | "xiaomi-mimo-v2.5-pro"
        | "xiaomi-mimo-v2-5-pro" => Some(DEFAULT_XIAOMI_MIMO_MODEL),
        XIAOMI_MIMO_V2_5_PRO_ULTRASPEED_MODEL
        | "mimo-v2-5-pro-ultraspeed"
        | "xiaomi-mimo-v2.5-pro-ultraspeed"
        | "xiaomi-mimo-v2-5-pro-ultraspeed"
        | "ultraspeed"
        | "pro-ultraspeed" => Some(XIAOMI_MIMO_V2_5_PRO_ULTRASPEED_MODEL),
        "omni"
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
        | "xiaomi-mimo-v2-5-omni" => Some(XIAOMI_MIMO_V2_5_OMNI_MODEL),
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

fn canonical_minimax_model_id(model: &str) -> Option<&'static str> {
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

fn canonical_zai_model_id(model: &str) -> Option<&'static str> {
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

fn canonical_openrouter_recent_model_id(model: &str) -> Option<&'static str> {
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
fn canonical_orcarouter_recent_model_id(model: &str) -> Option<&'static str> {
    let normalized = model.trim().to_ascii_lowercase();
    let normalized = normalized.replace(['_', ' '], "-");
    match normalized.as_str() {
        ORCAROUTER_AUTO_MODEL | "auto" | "orcarouter-auto" | "orca-auto" => {
            Some(ORCAROUTER_AUTO_MODEL)
        }
        _ => None,
    }
}

fn default_model_for_provider(provider: ProviderKind) -> &'static str {
    provider.provider().default_model()
}

fn default_base_url_for_provider(provider: ProviderKind) -> &'static str {
    provider.provider().default_base_url()
}

fn moonshot_base_url_uses_kimi_code(base_url: &str) -> bool {
    let normalized = base_url.trim_end_matches('/').to_ascii_lowercase();
    normalized == DEFAULT_KIMI_CODE_BASE_URL
        || normalized == "https://api.kimi.com/coding"
        || normalized.starts_with("https://api.kimi.com/coding/")
}

/// Dual-wire vendors: dialect is config (`wire`), not a separate ProviderKind.
fn wire_prefers_anthropic(kind: ProviderKind, wire: Option<&str>) -> bool {
    if matches!(
        kind,
        ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlanAnthropic
    ) {
        return true;
    }
    let Some(raw) = wire.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let normalized = raw.to_ascii_lowercase().replace(['_', ' '], "-");
    matches!(
        normalized.as_str(),
        "anthropic"
            | "anthropic-messages"
            | "messages"
            | "claude"
            | "anthropic-compatible"
            | "anthropic-compat"
    )
}

fn modelstudio_mode_is_coding_plan(kind: ProviderKind, mode: Option<&str>) -> bool {
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

fn is_modelstudio_family(kind: ProviderKind) -> bool {
    matches!(
        kind,
        ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic
    )
}

fn resolve_modelstudio_base_url(
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

fn resolve_minimax_base_url(
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

fn resolve_deepseek_base_url(
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

fn xiaomi_mimo_base_url_for_mode(mode: &str) -> Option<&'static str> {
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

fn xiaomi_mimo_mode_uses_standard_endpoint(normalized_mode: &str) -> bool {
    matches!(
        normalized_mode,
        "standard" | "default" | "payg" | "paygo" | "pay-as-you-go" | "pay-as-go"
    )
}

fn xiaomi_mimo_base_url_uses_token_plan(base_url: &str) -> bool {
    let normalized = base_url.trim_end_matches('/').to_ascii_lowercase();
    normalized == XIAOMI_MIMO_TOKEN_PLAN_CN_BASE_URL
        || normalized == XIAOMI_MIMO_TOKEN_PLAN_SGP_BASE_URL
        || normalized == XIAOMI_MIMO_TOKEN_PLAN_AMS_BASE_URL
}

const XIAOMI_MIMO_TOKEN_PLAN_ENV_VARS: &[&str] =
    &["XIAOMI_MIMO_TOKEN_PLAN_API_KEY", "MIMO_TOKEN_PLAN_API_KEY"];
const XIAOMI_MIMO_STANDARD_ENV_VARS: &[&str] =
    &["XIAOMI_MIMO_API_KEY", "XIAOMI_API_KEY", "MIMO_API_KEY"];

fn xiaomi_mimo_env_api_key_for_runtime(
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

fn resolve_xiaomi_mimo_base_url(
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

fn xiaomi_mimo_api_key_uses_token_plan(api_key: Option<&str>) -> bool {
    api_key.is_some_and(|key| key.trim_start().starts_with("tp-"))
}

fn xiaomi_mimo_base_url_is_pay_as_you_go(base_url: &str) -> bool {
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

fn base_url_is_custom_for_provider(provider: ProviderKind, base_url: &str) -> bool {
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

fn should_skip_secret_store_for_provider(
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
fn stored_api_key_for_provider(
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
fn env_api_key_for_provider(provider: ProviderKind) -> Option<String> {
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

fn base_url_uses_local_host(base_url: &str) -> bool {
    let Some(host) = base_url_host(base_url) else {
        return false;
    };
    let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
    if matches!(host.as_str(), "localhost" | "0.0.0.0") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|addr| addr.is_loopback() || addr.is_unspecified())
}

fn base_url_host(base_url: &str) -> Option<&str> {
    let without_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = without_scheme.split('/').next()?.rsplit('@').next()?;
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split_once(']').map(|(host, _)| host);
    }
    authority.split(':').next().filter(|host| !host.is_empty())
}

#[derive(Debug, Clone, Default)]
pub struct CliRuntimeOverrides {
    pub provider: Option<ProviderKind>,
    pub model: Option<String>,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    pub auth_mode: Option<String>,
    pub log_level: Option<String>,
    pub telemetry: Option<bool>,
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
    pub yolo: Option<bool>,
    pub verbosity: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeApiKeySource {
    Cli,
    ConfigFile,
    Keyring,
    Env,
}

impl RuntimeApiKeySource {
    #[must_use]
    pub fn as_env_value(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::ConfigFile => "config",
            Self::Keyring => "keyring",
            Self::Env => "env",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSource {
    Cli,
    Env(&'static str),
    Config,
}

/// Where the resolved runtime model id came from.
///
/// This mirrors the precedence chain in
/// [`ConfigToml::resolve_runtime_options_with_secrets`] so diagnostics can say
/// *why* a model was chosen instead of presenting a built-in default as if the
/// user had asked for it. [`Self::ProviderDefault`] is the only variant that
/// means "nothing was configured".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    /// `--model` on the command line.
    Cli,
    /// A `CODEWHALE_*` environment variable.
    Env,
    /// `[providers.<name>].model`.
    ProviderConfig,
    /// The root `default_text_model` key, which is DeepSeek-scoped.
    RootDefaultTextModel,
    /// The provider-neutral root `model` key.
    RootModel,
    /// Nothing was configured; this is the built-in default for the provider.
    ProviderDefault,
}

impl ModelSource {
    /// Whether the id was chosen by the user rather than substituted by us.
    #[must_use]
    pub fn is_explicit(self) -> bool {
        !matches!(self, Self::ProviderDefault)
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "--model",
            Self::Env => "environment",
            Self::ProviderConfig => "config [providers.*].model",
            Self::RootDefaultTextModel => "config default_text_model",
            Self::RootModel => "config model",
            Self::ProviderDefault => "provider default",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResolvedRuntimeOptions {
    pub provider: ProviderKind,
    pub provider_source: ProviderSource,
    pub model: String,
    pub model_source: ModelSource,
    pub api_key: Option<String>,
    pub api_key_source: Option<RuntimeApiKeySource>,
    pub base_url: String,
    pub auth_mode: Option<String>,
    pub insecure_skip_tls_verify: bool,
    pub log_level: Option<String>,
    pub telemetry: bool,
    /// Where the resolved telemetry consent came from (cli | env | config |
    /// default), so doctor and config displays can state the truth about a
    /// machine that never opted in (#5441).
    pub telemetry_source: TelemetrySource,
    /// A human wrote `telemetry = false` into the config file.
    ///
    /// This is the *persistent* opt-out, and it is deliberately narrower than
    /// "telemetry resolved to false". A run-scoped kill switch also resolves
    /// false; treating that as a revocation would destroy the identity and
    /// buffered events of a user who merely set `CODEWHALE_TELEMETRY=0` for one
    /// command. Run-scoped kill switches
    /// (`--telemetry false`, the environment variable) stop the run and leave
    /// every byte on disk alone; only this flag authorizes the wipe.
    pub telemetry_explicit_off: bool,
    /// Where a telemetry batch would be sent, if telemetry were on.
    ///
    /// Already resolved: [`DEFAULT_TELEMETRY_ENDPOINT`] when nobody configured
    /// one, the configured value when somebody did, and `None` when somebody
    /// configured an empty one — which means the dry-run sink, not "unset".
    /// Which schemes are actually contactable is decided where a batch would be
    /// sent, not here — a user must be able to stage a value.
    pub telemetry_endpoint: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
    pub yolo: Option<bool>,
    pub verbosity: Option<String>,
    pub http_headers: BTreeMap<String, String>,
    /// Executable route minted by [`crate::route::RouteResolver`].
    ///
    /// `Err` carries the resolver's rejection (foreign model on a strict
    /// direct provider, empty model, unsupported protocol). `model` and
    /// `base_url` above are still the requested values in that case, so a
    /// caller that reports them as a resolved route must check this first.
    /// Auth/key fields above are independent: the resolver never inspects
    /// credentials.
    pub route: Result<crate::route::ReadyRouteCandidate, crate::route::RouteError>,
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
    pub config: ConfigToml,
    permissions: PermissionsToml,
    /// Original file text, retained so [`save`](Self::save) can merge
    /// comments back after serialisation.
    original_raw: Option<String>,
    /// What loading moved in memory from legacy top-level keys (#6394).
    legacy_root: legacy_root::LegacyRootMigration,
}

/// Parse a [`ConfigToml`] on a dedicated thread with an explicit stack size.
///
/// `ConfigToml` nests the per-provider tables, fleet trust policy, and every
/// typed sub-table in one struct, and the monomorphized toml/serde
/// deserializer frames for a struct this large overflow the 2 MiB default
/// stack of libtest and tokio worker threads in debug builds (the same
/// hazard the TUI fixed for its `ConfigFile`; reproduced as the #5585 stack
/// overflow through the guided-setup save path). Every production
/// `ConfigToml` parse goes through here so config-store loads stay safe
/// regardless of the calling thread's stack budget.
fn parse_config_toml_str(contents: &str) -> Result<ConfigToml, toml::de::Error> {
    parse_config_toml_with_receipt(contents).map(|(config, _)| config)
}

/// [`parse_config_toml_str`] plus the receipt of the legacy top-level keys it
/// moved in memory (#6394).
fn parse_config_toml_with_receipt(
    contents: &str,
) -> Result<(ConfigToml, legacy_root::LegacyRootMigration), toml::de::Error> {
    std::thread::scope(|scope| {
        match std::thread::Builder::new()
            .name("config-toml-parse".to_string())
            .stack_size(16 * 1024 * 1024)
            .spawn_scoped(scope, || parse_config_toml_canonical(contents))
        {
            Ok(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            // Spawning can only fail under resource exhaustion; parsing on
            // the caller's stack is still the best remaining option.
            Err(_) => parse_config_toml_canonical(contents),
        }
    })
}

fn parse_config_toml_canonical(
    contents: &str,
) -> Result<(ConfigToml, legacy_root::LegacyRootMigration), toml::de::Error> {
    let (text, receipt) = legacy_root::canonicalize_text(contents)?;
    let config = toml::from_str::<ConfigToml>(&text)?;
    Ok((config, receipt))
}

/// Where a TOML error sits: `line L, column C` in `source` (the exact text
/// that was parsed), plus the key path when the deserializer recorded one.
///
/// Never the error's message or source snippet: both can quote the offending
/// value (`invalid type: string "sk-…", expected a boolean`), and config and
/// permissions files hold credentials. Pass `None` for `source` when the
/// span refers to text the user did not write.
#[must_use]
pub fn toml_error_location(source: Option<&str>, err: &toml::de::Error) -> String {
    let position = source.zip(err.span()).map(|(source, span)| {
        let mut start = span.start.min(source.len());
        while !source.is_char_boundary(start) {
            start -= 1;
        }
        let before = &source[..start];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        format!("line {line}, column {column}")
    });
    // Without its input, `toml` renders exactly `{message}\n` followed by
    // ``in `{keys}`\n`` when it recorded a key path; take only the latter.
    let mut detached = err.clone();
    detached.set_input(None);
    let rendered = detached.to_string();
    let key_path = rendered
        .strip_prefix(err.message())
        .and_then(|rest| rest.strip_prefix("\nin `"))
        .and_then(|rest| rest.strip_suffix("`\n"))
        .filter(|keys| !keys.is_empty())
        .map(|keys| format!("in `{}`", codewhale_secrets::redact::redact_secrets(keys)));
    match (position, key_path) {
        (Some(position), Some(keys)) => format!("{position}, {keys}"),
        (Some(position), None) => position,
        (None, Some(keys)) => keys,
        (None, None) => "position unknown".to_string(),
    }
}

/// [`toml_error_location`] for an error from [`parse_config_toml_with_receipt`].
/// A file with legacy top-level keys is parsed from a rewritten copy whose
/// offsets name nothing the user wrote, so only the key path is reported.
fn config_toml_error_location(raw: &str, err: &toml::de::Error) -> String {
    let rewritten = matches!(
        legacy_root::canonicalize_text(raw),
        Ok((std::borrow::Cow::Owned(_), _))
    );
    toml_error_location((!rewritten).then_some(raw), err)
}

/// Parse any `config.toml`-shaped document into [`ConfigToml`], moving legacy
/// top-level `base_url` / `api_key` into their provider tables first. Every
/// caller outside this crate (bundle import, tests) parses through here so a
/// top-level key can never land in `extras`.
pub fn parse_config_toml(contents: &str) -> Result<ConfigToml, toml::de::Error> {
    parse_config_toml_str(contents)
}

impl ConfigStore {
    /// The validated file snapshot captured by the last load or successful
    /// save. This read-only view preserves literal provider identities that a
    /// typed serialization may normalize; it excludes unsaved in-memory edits.
    #[must_use]
    pub fn original_body(&self) -> Option<&str> {
        self.original_raw.as_deref()
    }

    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = resolve_config_path(path)?;
        let (config, original_raw, legacy_root) = if checked_path_exists(&path)? {
            let raw = read_checked_config_file(&path)?;
            let (mut parsed, receipt) = parse_config_toml_with_receipt(&raw).map_err(|err| {
                anyhow::anyhow!(
                    "failed to parse config at {} ({}); file contents were omitted",
                    quote_os_path(&path),
                    config_toml_error_location(&raw, &err)
                )
            })?;
            let raw_document: toml::Value = toml::from_str(&raw).map_err(|err| {
                anyhow::anyhow!(
                    "failed to parse config at {} ({}); file contents were omitted",
                    quote_os_path(&path),
                    toml_error_location(Some(&raw), &err)
                )
            })?;
            if let Some(provider_id) = raw_document.get("provider").and_then(toml::Value::as_str) {
                parsed
                    .bind_persisted_provider_id(provider_id)
                    .with_context(|| {
                        format!("failed to parse config at {}", quote_os_path(&path))
                    })?;
            }
            (parsed, Some(raw), receipt)
        } else {
            (
                ConfigToml::default(),
                None,
                legacy_root::LegacyRootMigration::default(),
            )
        };
        let permissions = load_sibling_permissions(&path)?;

        Ok(Self {
            path,
            config,
            permissions,
            original_raw,
            legacy_root,
        })
    }

    /// What loading moved in memory from legacy top-level keys. Loading never
    /// rewrites the file; the next save does, keeping any conflict in place.
    #[must_use]
    pub fn legacy_root_migration(&self) -> &legacy_root::LegacyRootMigration {
        &self.legacy_root
    }

    /// Render the exact body [`save`](Self::save) would write: the serialized
    /// config with comments and disabled keys from the originally-loaded file
    /// merged back in. Exposed so setup flows can stage this body into a
    /// [`persistence::SetupTransaction`] alongside sibling files and keep the
    /// comment-preserving write atomic with the rest of the transaction.
    pub fn rendered_body(&self) -> Result<String> {
        catalog::configured::validate_configured_models(
            self.config.custom_models.as_deref().unwrap_or_default(),
        )?;
        let mut serialized =
            toml::to_string_pretty(&self.config).context("failed to serialize config")?;
        let provider_id = self.config.provider_id();
        if provider_id != self.config.provider.as_str() {
            let mut document = serialized
                .parse::<toml_edit::DocumentMut>()
                .context("failed to edit serialized config")?;
            document["provider"] = toml_edit::value(provider_id);
            serialized = document.to_string();
        }
        if let Some(ref original_raw) = self.original_raw {
            // Comments come from the original with legacy top-level keys
            // already moved (#6394): a comment above a moved key stays in
            // place instead of vanishing with the key.
            let decor_source = legacy_root::migrated_document_text(original_raw);
            let merged = merge_and_preserve_comments(
                &serialized,
                decor_source.as_deref().unwrap_or(original_raw),
            )
            .with_context(|| {
                format!(
                    "cannot safely preserve config at {}; reload it and retry instead of replacing an unmergeable snapshot",
                    quote_os_path(&self.path)
                )
            })?;
            // The typed body has no top-level keys. A conflicting pair the
            // user never touched goes back exactly as it was on disk.
            let mut document = merged
                .parse::<toml_edit::DocumentMut>()
                .context("failed to edit serialized config")?;
            if legacy_root::restore_conflicts(&mut document, original_raw) {
                return Ok(document.to_string());
            }
            Ok(merged)
        } else {
            Ok(serialized)
        }
    }

    pub fn save(&mut self) -> Result<()> {
        let path = normalize_config_file_path(self.path.clone())?;
        let body = self.rendered_body()?;
        if let Some(original_raw) = self.original_raw.as_deref() {
            note_legacy_root_file_migration(&path, original_raw)?;
        }
        replace_config_document_if_unchanged(&path, self.original_raw.as_deref(), &body)?;
        self.original_raw = Some(body);
        Ok(())
    }

    /// Refresh the typed value and byte snapshot after a targeted writer used
    /// the shared config lock. This keeps a long-lived command process from
    /// treating its own successful mutation as an external stale conflict.
    pub fn reload(&mut self) -> Result<()> {
        *self = Self::load(Some(self.path.clone()))?;
        Ok(())
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn permissions(&self) -> &PermissionsToml {
        &self.permissions
    }

    #[must_use]
    pub fn permissions_path(&self) -> PathBuf {
        checked_permissions_path_for_config_path(&self.path)
            .expect("ConfigStore path is validated before construction")
    }

    #[must_use]
    pub fn exec_policy_engine(&self) -> ExecPolicyEngine {
        if self.permissions.is_empty() {
            ExecPolicyEngine::new(Vec::new(), Vec::new())
        } else {
            ExecPolicyEngine::with_rulesets(vec![self.permissions.ruleset()])
        }
    }

    /// Atomically append ask-only permission rules to the sibling
    /// `permissions.toml` file.
    ///
    /// Existing comments and formatting are preserved. Exact duplicate rules
    /// are ignored, and the in-memory permissions snapshot is refreshed after
    /// a successful write.
    pub fn append_ask_rules(&mut self, rules: &[ToolAskRule]) -> Result<usize> {
        self.append_permission_rules(rules, PermissionAction::Ask)
    }

    /// Atomically append exact, repo-scoped allow rules to the sibling
    /// `permissions.toml` file.
    ///
    /// The caller is responsible for deciding which tool calls are eligible;
    /// this boundary rejects broad or incorrectly typed records so a UI bug
    /// cannot persist an unscoped allow grant.
    pub fn append_allow_rules(&mut self, rules: &[ToolAskRule]) -> Result<usize> {
        for rule in rules {
            if rule.action != PermissionAction::Allow {
                bail!("append_allow_rules only accepts action = \"allow\"");
            }
            let Some(workspace) = rule
                .workspace
                .as_deref()
                .and_then(codewhale_execpolicy::normalize_workspace_scope)
            else {
                bail!("persistent allow rules must be scoped to a workspace");
            };
            if rule.command.is_some() && !rule.command_exact {
                bail!("persistent command allow rules must use exact matching");
            }
            if rule.command.is_none() && rule.path.is_none() {
                bail!("persistent allow rules must match an exact command or path");
            }
            if let Some(command) = rule.command.as_deref()
                && command.trim().is_empty()
            {
                bail!("persistent command allow rules must not be empty");
            }
            if let Some(path) = rule.path.as_deref()
                && codewhale_execpolicy::normalize_workspace_relative_path(path, &workspace)
                    .is_none_or(|path| path.is_empty())
            {
                bail!("persistent path allow rules must stay within the workspace");
            }
        }
        self.append_permission_rules(rules, PermissionAction::Allow)
    }

    fn append_permission_rules(
        &mut self,
        rules: &[ToolAskRule],
        expected_action: PermissionAction,
    ) -> Result<usize> {
        if rules.is_empty() {
            return Ok(0);
        }
        if rules.iter().any(|rule| rule.action != expected_action) {
            bail!(
                "permission rule action does not match requested {:?} persistence",
                expected_action
            );
        }

        let path = checked_permissions_path_for_config_path(&self.path)?;
        let (added, persisted) = config_document::with_config_write_lock(&path, |path| {
            let (_, raw, mut permissions) = read_permissions_state(path)?;
            let mut document = parse_permissions_document(path, &raw)?;

            if !document.contains_key("rules") {
                document["rules"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
            }
            let rules_item = document
                .get_mut("rules")
                .expect("rules entry was inserted above");

            let mut added = 0;
            for rule in rules {
                if permissions.rules.contains(rule) {
                    continue;
                }
                append_permission_rule(rules_item, rule)?;
                permissions.rules.push(rule.clone());
                added += 1;
            }
            if added == 0 {
                return Ok((0, permissions));
            }

            let body = document.to_string();
            let persisted = parse_generated_permissions(path, &body)?;
            write_permissions_atomic(path, body.as_bytes())?;
            Ok((added, persisted))
        })?;
        self.permissions = persisted;
        Ok(added)
    }
}

fn config_backup_file_name(path: &Path) -> OsString {
    let mut file_name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(CONFIG_FILE_NAME));
    file_name.push(".bak");
    file_name
}

fn config_sibling_path_unchecked(config_path: &Path, file_name: &OsStr) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(file_name)
}

fn checked_config_sibling_path(config_path: &Path, file_name: &OsStr) -> Result<PathBuf> {
    let config_path = normalize_config_file_path(config_path.to_path_buf())?;
    let parent = config_path
        .parent()
        .context("config path must include a parent directory")?;
    let path = parent.join(file_name);
    reject_path_symlink(&path)?;
    Ok(path)
}

#[cfg(test)]
fn config_backup_path(path: &Path) -> PathBuf {
    config_sibling_path_unchecked(path, &config_backup_file_name(path))
}

fn checked_config_backup_path(path: &Path) -> Result<PathBuf> {
    checked_config_sibling_path(path, &config_backup_file_name(path))
}

/// Remove plaintext `api_key` entries from the one-time config backup, if it
/// exists.
///
/// Credential migration deliberately preserves the rest of `config.toml.bak`
/// while ensuring that moving a key into the durable secret store does not
/// leave the same credential behind in an older backup.
pub fn scrub_plaintext_api_keys_from_config_backup(path: &Path) -> Result<()> {
    scrub_credentials_from_backup_file(&checked_config_backup_path(path)?)
}

/// Rewrite an existing backup without credential-named keys; see
/// [`config_toml_without_plaintext_api_keys`].
fn scrub_credentials_from_backup_file(backup: &Path) -> Result<()> {
    if !backup.exists() {
        return Ok(());
    }

    let raw = read_checked_toml_file(backup, "config backup")?;
    let scrubbed = config_toml_without_plaintext_api_keys(&raw).with_context(|| {
        format!(
            "failed to scrub plaintext API keys from config backup {}",
            backup.display()
        )
    })?;
    if scrubbed != raw {
        persistence::atomic_write(backup, scrubbed.as_bytes()).with_context(|| {
            format!(
                "failed to write credential-free config backup {}",
                backup.display()
            )
        })?;
    }
    Ok(())
}

/// Remove only retired Antigravity state from Codewhale's one-time config
/// backup. This never resolves, reads, writes, or revokes any external Google
/// or Antigravity session; it edits only the checked sibling `.bak` file owned
/// by Codewhale.
pub fn scrub_legacy_antigravity_from_config_backup(path: &Path) -> Result<()> {
    let backup = checked_config_backup_path(path)?;
    if !backup.exists() {
        return Ok(());
    }

    let raw = read_checked_toml_file(&backup, "config backup")?;
    let scrubbed = config_toml_without_legacy_antigravity(&raw).with_context(|| {
        format!(
            "failed to clear retired provider state from config backup {}",
            backup.display()
        )
    })?;
    if scrubbed != raw {
        persistence::atomic_write(&backup, scrubbed.as_bytes()).with_context(|| {
            format!(
                "failed to write retired-provider-free config backup {}",
                backup.display()
            )
        })?;
    }
    Ok(())
}

fn config_toml_without_legacy_antigravity(raw: &str) -> Result<String> {
    let mut document = raw.parse::<toml_edit::DocumentMut>().map_err(|_| {
        anyhow::anyhow!(
            "failed to parse config TOML while clearing retired provider state; file contents were omitted"
        )
    })?;
    let root = document.as_table_mut();

    if root
        .get("provider")
        .and_then(toml_edit::Item::as_str)
        .is_some_and(is_legacy_antigravity_name)
    {
        root.remove("provider");
    }
    if let Some(fallbacks) = root
        .get_mut("fallback_providers")
        .and_then(toml_edit::Item::as_array_mut)
    {
        fallbacks.retain(|value| !value.as_str().is_some_and(is_legacy_antigravity_name));
        if fallbacks.is_empty() {
            root.remove("fallback_providers");
        }
    }
    if let Some(providers) = root
        .get_mut("providers")
        .and_then(toml_edit::Item::as_table_like_mut)
    {
        providers.remove("antigravity");
        providers.remove("agy");
    }

    Ok(document.to_string())
}

fn is_legacy_antigravity_name(value: &str) -> bool {
    value.eq_ignore_ascii_case("antigravity") || value.eq_ignore_ascii_case("agy")
}

/// Before a write moves legacy top-level keys (#6394), keep one credential-free
/// copy of the file as it was, and queue a one-line notice for the caller's
/// status channel. Does nothing when the original has nothing to move.
pub(crate) fn note_legacy_root_file_migration(path: &Path, original_raw: &str) -> Result<()> {
    let Ok(document) = original_raw.parse::<toml_edit::DocumentMut>() else {
        return Ok(());
    };
    let receipt = legacy_root::preview_document(&document, None);
    if !receipt.changes_file() {
        return Ok(());
    }
    let backup = write_legacy_root_backup(path, original_raw)?;
    legacy_root::queue_notice(&receipt, &backup);
    Ok(())
}

/// Keep one credential-free copy of the file as it was before legacy
/// top-level keys first moved. Later migrations never overwrite it.
pub(crate) fn write_legacy_root_backup(path: &Path, original_raw: &str) -> Result<PathBuf> {
    let backup = checked_config_sibling_path(path, &pre_migrate_backup_file_name(path))?;
    if backup.exists() {
        // Written once and kept; still repair one left by a release that
        // scrubbed only `api_key`. Best effort: an unreadable old backup must
        // not block the save that is moving keys now.
        if let Err(err) = scrub_credentials_from_backup_file(&backup) {
            tracing::warn!("{err:#}");
        }
    } else {
        let scrubbed = config_toml_without_plaintext_api_keys(original_raw)?;
        persistence::atomic_write(&backup, scrubbed.as_bytes()).with_context(|| {
            format!(
                "failed to create config backup {} before moving top-level keys",
                backup.display()
            )
        })?;
    }
    Ok(backup)
}

fn pre_migrate_backup_file_name(path: &Path) -> OsString {
    let mut file_name = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from(CONFIG_FILE_NAME));
    file_name.push(".pre-migrate.bak");
    file_name
}

/// Path of the one-time backup written before legacy top-level keys moved.
pub fn legacy_root_backup_path(path: &Path) -> Result<PathBuf> {
    checked_config_sibling_path(path, &pre_migrate_backup_file_name(path))
}

fn write_one_time_config_backup(path: &Path) -> Result<()> {
    let backup = checked_config_backup_path(path)?;
    if backup.exists() {
        return scrub_plaintext_api_keys_from_config_backup(path);
    }

    let raw = read_checked_config_file(path)?;
    let scrubbed = config_toml_without_plaintext_api_keys(&raw).with_context(|| {
        format!(
            "failed to scrub plaintext API keys while creating config backup {}",
            backup.display()
        )
    })?;
    persistence::atomic_write(&backup, scrubbed.as_bytes()).with_context(|| {
        format!(
            "failed to create credential-free config backup {} from {}",
            backup.display(),
            path.display()
        )
    })?;
    Ok(())
}

fn config_toml_without_plaintext_api_keys(raw: &str) -> Result<String> {
    let mut document = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| {
            anyhow::anyhow!(
                "failed to parse config TOML while removing plaintext API keys; file contents were omitted"
            )
        })?;
    remove_plaintext_api_keys_recursive(document.as_table_mut());
    scrub_backup_decor(document.as_table_mut().decor_mut());
    if let Some(trailing) = document.trailing().as_str().map(scrub_backup_comments) {
        document.set_trailing(trailing);
    }
    Ok(document.to_string())
}

/// Drop every credential-named key ([`is_sensitive_config_key`]: `api_key`,
/// `webhook_token`, `Authorization` under `http_headers`, ...) at any depth,
/// including tables inside arrays. The backups are kept indefinitely and are
/// described as credential-free, so a rotated secret must not live on there.
fn remove_plaintext_api_keys_recursive(table: &mut dyn toml_edit::TableLike) {
    let sensitive: Vec<String> = table
        .iter()
        .filter(|(key, item)| {
            is_sensitive_config_key(key) || item.as_value().is_some_and(backup_value_carries_secret)
        })
        .map(|(key, _)| key.to_owned())
        .collect();
    for key in sensitive {
        // Keep a comment written above the key (often the file header).
        config_document::remove_key_preserving_leading_decor(table, &key);
    }
    for (mut key, item) in table.iter_mut() {
        scrub_backup_decor(key.leaf_decor_mut());
        if let toml_edit::Item::Table(nested) = item {
            scrub_backup_decor(nested.decor_mut());
        }
        if let toml_edit::Item::Value(value) = item {
            scrub_backup_decor(value.decor_mut());
        }
        match item {
            toml_edit::Item::ArrayOfTables(tables) => {
                for nested in tables.iter_mut() {
                    scrub_backup_decor(nested.decor_mut());
                    remove_plaintext_api_keys_recursive(nested);
                }
            }
            toml_edit::Item::Value(toml_edit::Value::Array(array)) => {
                remove_plaintext_api_keys_in_array(array);
            }
            _ => {
                if let Some(nested) = item.as_table_like_mut() {
                    remove_plaintext_api_keys_recursive(nested);
                }
            }
        }
    }
}

fn remove_plaintext_api_keys_in_array(array: &mut toml_edit::Array) {
    array.retain(|value| !backup_value_carries_secret(value));
    for value in array.iter_mut() {
        scrub_backup_decor(value.decor_mut());
        match value {
            toml_edit::Value::InlineTable(table) => remove_plaintext_api_keys_recursive(table),
            toml_edit::Value::Array(nested) => remove_plaintext_api_keys_in_array(nested),
            _ => {}
        }
    }
}

fn backup_value_carries_secret(value: &toml_edit::Value) -> bool {
    value
        .as_str()
        .is_some_and(codewhale_secrets::sanitize::contains_secret)
}

fn scrub_backup_decor(decor: &mut toml_edit::Decor) {
    let prefix = decor
        .prefix()
        .and_then(|text| text.as_str())
        .map(scrub_backup_comments);
    let suffix = decor
        .suffix()
        .and_then(|text| text.as_str())
        .map(scrub_backup_comments);
    if let Some(prefix) = prefix {
        decor.set_prefix(prefix);
    }
    if let Some(suffix) = suffix {
        decor.set_suffix(suffix);
    }
}

fn scrub_backup_comments(text: &str) -> String {
    text.split_inclusive('\n')
        .filter(|line| {
            !line
                .trim_start()
                .strip_prefix('#')
                .map(str::trim)
                .is_some_and(|comment| {
                    comment
                        .split_once('=')
                        .is_some_and(|(key, _)| is_sensitive_config_key(key))
                        || codewhale_secrets::sanitize::contains_secret(comment)
                })
        })
        .collect()
}

/// Merge comments and formatting from an original TOML file into a
/// freshly serialized document so user annotations (comments, whitespace,
/// disabled keys) survive config rewrites.
///
/// `original_raw` is the raw text of the file before the change; the
/// function parses it internally with [`toml_edit`] so callers stay free
/// of that dependency.
pub fn merge_and_preserve_comments(serialized: &str, original_raw: &str) -> Result<String> {
    let original = original_raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|_| {
            anyhow::anyhow!(
                "failed to parse original config for comment merge; file contents were omitted"
            )
        })?;

    let mut new_doc = serialized.parse::<toml_edit::DocumentMut>().map_err(|_| {
        anyhow::anyhow!(
            "failed to parse serialized config for comment merge; file contents were omitted"
        )
    })?;

    // Reuse the original document’s trailing text (file-footer comments /
    // disabled keys) so they survive the rewrite.
    new_doc.set_trailing(original.trailing().clone());

    // Copy the top-level table's decor (document-header comments, whitespace
    // before the first key) which `toml_edit` stores on the root `Table` itself.
    *new_doc.as_table_mut().decor_mut() = original.as_table().decor().clone();

    merge_decor_table(new_doc.as_table_mut(), original.as_table());

    Ok(new_doc.to_string())
}

/// Recursively copy `decor` (prefix/suffix comments and whitespace) from
/// every key in `source` that also exists in `target`.
fn merge_decor_table(target: &mut toml_edit::Table, source: &toml_edit::Table) {
    // Collect keys first — the borrow checker won't let us hold
    // `get_key_value_mut` while iterating.
    let keys: Vec<String> = source.iter().map(|(k, _)| k.to_owned()).collect();
    for key in &keys {
        let Some((source_key, source_item)) = source.get_key_value(key) else {
            continue;
        };
        let Some((mut target_key_mut, target_item)) = target.get_key_value_mut(key) else {
            continue;
        };

        // Copy the key-level decor (comments before the key itself)
        *target_key_mut.leaf_decor_mut() = source_key.leaf_decor().clone();

        copy_item_decor(target_item, source_item);

        if let (Some(tt), Some(st)) = (target_item.as_table_mut(), source_item.as_table()) {
            merge_decor_table(tt, st);
        }

        if let (Some(ta), Some(sa)) = (
            target_item.as_array_of_tables_mut(),
            source_item.as_array_of_tables(),
        ) {
            for (i, source_table) in sa.iter().enumerate() {
                if let Some(target_table) = ta.get_mut(i) {
                    copy_item_decor_table(target_table, source_table);
                    merge_decor_table(target_table, source_table);
                }
            }
        }
    }
}

/// Copy the decor (comments and surrounding whitespace) from `source` to `target`,
/// respecting the concrete item type since [`toml_edit::Item`] has no uniform
/// `decor` accessor.
fn copy_item_decor(target: &mut toml_edit::Item, source: &toml_edit::Item) {
    match (target, source) {
        (toml_edit::Item::Table(tt), toml_edit::Item::Table(st)) => {
            *tt.decor_mut() = st.decor().clone();
        }
        (toml_edit::Item::Value(tv), toml_edit::Item::Value(sv)) => {
            *tv.decor_mut() = sv.decor().clone();
        }
        _ => {}
    }
}

fn copy_item_decor_table(target: &mut toml_edit::Table, source: &toml_edit::Table) {
    *target.decor_mut() = source.decor().clone();
}

// ── CodeWhale state root (v0.8.44) ──────────────────────────────────
//
// v0.8.44 migrates product-owned app state from ~/.deepseek/ to
// ~/.codewhale/ while keeping ~/.deepseek/ as a compatibility fallback.
// New installs write to ~/.codewhale/. Existing installs with only
// ~/.deepseek/ continue working without data loss.

pub use codewhale_paths::{CODEWHALE_APP_DIR, LEGACY_APP_DIR};

/// Resolve the primary CodeWhale home directory.
///
/// `$CODEWHALE_HOME` takes precedence when set. Otherwise defaults to
/// `$HOME/.codewhale`. This is the write target for new product state.
pub fn codewhale_home() -> Result<PathBuf> {
    codewhale_paths::codewhale_home()
        .map_err(anyhow::Error::new)?
        .context("failed to resolve home directory")
}

/// Whether `$CODEWHALE_HOME` is set to a non-empty value.
///
/// An explicit CodeWhale home is an isolation boundary: state/config resolvers
/// must not fall back to ambient legacy `~/.deepseek` data outside that root.
pub fn codewhale_home_is_explicit() -> bool {
    codewhale_paths::codewhale_home_is_explicit()
}

/// Resolve the legacy DeepSeek home directory (`$HOME/.deepseek`).
///
/// Always returns the legacy path regardless of whether it exists.
pub fn legacy_deepseek_home() -> Result<PathBuf> {
    codewhale_paths::legacy_deepseek_home().context("failed to resolve home directory")
}

/// Reject state subdirs that could escape the state root via path injection.
///
/// `ensure_state_dir` / `resolve_state_dir` are public APIs taking an arbitrary
/// subdir string; every in-tree caller passes a hardcoded single component
/// (e.g. `"sessions"`, `"."`). This validates defensively so a future caller
/// can never traverse out of the state root via `..` components or an absolute
/// path. Nested relative paths such as `"a/b"` are permitted.
fn ensure_safe_state_subdir(subdir: &str) -> Result<()> {
    if subdir.is_empty() {
        bail!("state subdir must not be empty");
    }
    let path = std::path::Path::new(subdir);
    if path.is_absolute() {
        bail!("state subdir must not be an absolute path: {subdir}");
    }
    if path.components().any(|c| {
        matches!(
            c,
            std::path::Component::RootDir | std::path::Component::Prefix(_)
        )
    }) {
        bail!("state subdir must not contain a root or prefix: {subdir}");
    }
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        bail!("state subdir must not contain parent-dir (..) components: {subdir}");
    }
    Ok(())
}

/// Resolve a state subdirectory, preferring the CodeWhale root if
/// it already exists, otherwise falling back to the legacy root.
///
/// This is the read-path resolver: it returns the primary path when
/// migration has occurred or on a fresh install, but keeps reading
/// from the legacy path for users who haven't migrated yet.
pub fn resolve_state_dir(subdir: &str) -> Result<PathBuf> {
    ensure_safe_state_subdir(subdir)?;
    let explicit_codewhale_home = codewhale_home_is_explicit();
    let primary = codewhale_home()?.join(subdir);
    if explicit_codewhale_home || primary.exists() {
        return Ok(primary);
    }
    let legacy = legacy_deepseek_home()?.join(subdir);
    if legacy.exists() {
        return Ok(legacy);
    }
    // Neither exists — return primary for first-write creation.
    Ok(primary)
}

/// Ensure a state subdirectory exists under the primary CodeWhale root,
/// creating it if necessary. This is the write-path resolver.
///
/// On the first creation of a real subdirectory (not the root sentinel `"."`),
/// if a legacy `~/.deepseek/<subdir>` exists but the primary
/// `~/.codewhale/<subdir>` does not, the legacy directory is relocated into
/// the primary location so the user keeps their data and the legacy tree
/// stops growing (#3240). After migration, [`resolve_state_dir`] finds the
/// data in the primary location; the read resolver itself is unchanged.
pub fn ensure_state_dir(subdir: &str) -> Result<PathBuf> {
    let (dir, migration) = ensure_state_dir_with_migration(subdir)?;
    if let Some(migration) = migration {
        eprintln!("{}", migration.user_notice());
    }
    Ok(dir)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateMigrationKind {
    Relocated,
    Copied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateMigration {
    pub subdir: String,
    pub legacy_path: PathBuf,
    pub primary_path: PathBuf,
    pub kind: StateMigrationKind,
}

impl StateMigration {
    pub fn user_notice(&self) -> String {
        let action = match self.kind {
            StateMigrationKind::Relocated => "relocated",
            StateMigrationKind::Copied => "copied",
        };
        let legacy_detail = match self.kind {
            StateMigrationKind::Relocated => {
                "The legacy .deepseek copy for this state path was removed by the move."
            }
            StateMigrationKind::Copied => {
                "The legacy .deepseek copy was left in place because a direct move failed."
            }
        };

        format!(
            "Codewhale migrated legacy state ({action}):\n  {} -> {}\nYour data was preserved. Use .codewhale as the canonical state location from now on.\n{legacy_detail}\nIf no other apps use it, you can remove the legacy .deepseek tree after confirming everything looks right.",
            self.legacy_path.display(),
            self.primary_path.display(),
        )
    }
}

/// Variant of [`ensure_state_dir`] that exposes whether a legacy state path was
/// migrated. Most callers should use [`ensure_state_dir`]; this is kept for
/// tests and future UI surfaces that want to render the notice themselves.
pub fn ensure_state_dir_with_migration(subdir: &str) -> Result<(PathBuf, Option<StateMigration>)> {
    ensure_safe_state_subdir(subdir)?;
    let explicit_codewhale_home = codewhale_home_is_explicit();
    let dir = codewhale_home()?.join(subdir);
    let migration = if explicit_codewhale_home {
        None
    } else {
        match migrate_legacy_state_dir(&dir, subdir)? {
            LegacyStateMigration::NotNeeded => None,
            LegacyStateMigration::Migrated(migration) => Some(migration),
            // Creating an empty primary here would make it authoritative and
            // hide the legacy data for good. Keep using the legacy directory
            // (the read resolver does the same) and retry on the next call.
            LegacyStateMigration::Failed { legacy } => return Ok((legacy, None)),
        }
    };
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}/", dir.display()))?;
    Ok((dir, migration))
}

enum LegacyStateMigration {
    NotNeeded,
    Migrated(StateMigration),
    /// Neither the move nor the copy completed. The primary was left absent,
    /// so the legacy directory stays authoritative and migration retries.
    Failed {
        legacy: PathBuf,
    },
}

/// One-time relocation of a legacy `~/.deepseek/<subdir>` state directory into
/// the primary `~/.codewhale/<subdir>` location (#3240). No-op once the primary
/// exists, for the root sentinel `"."` (a whole-tree move is owned by the
/// config-file migration), or when no legacy directory is present.
fn migrate_legacy_state_dir(primary: &Path, subdir: &str) -> Result<LegacyStateMigration> {
    if primary.exists() || subdir == "." || subdir.is_empty() {
        return Ok(LegacyStateMigration::NotNeeded);
    }
    let legacy = match legacy_deepseek_home() {
        Ok(home) => home.join(subdir),
        Err(_) => return Ok(LegacyStateMigration::NotNeeded),
    };
    if !legacy.exists() {
        return Ok(LegacyStateMigration::NotNeeded);
    }
    // The primary's parent (the ~/.codewhale root) must exist for the rename.
    if let Some(parent) = primary.parent()
        && let Err(err) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(
            target: "config::migration",
            "Could not create {} for state migration ({}); writing to primary anyway",
            parent.display(),
            err
        );
    }
    match std::fs::rename(&legacy, primary) {
        Ok(()) => {
            tracing::info!(
                target: "config::migration",
                "Migrated legacy state directory {} -> {} (relocated). The .deepseek copy was removed.",
                legacy.display(),
                primary.display()
            );
            return Ok(LegacyStateMigration::Migrated(StateMigration {
                subdir: subdir.to_string(),
                legacy_path: legacy,
                primary_path: primary.to_path_buf(),
                kind: StateMigrationKind::Relocated,
            }));
        }
        Err(err) => {
            // Cross-device rename or permission issue: fall back to a
            // recursive copy so the user keeps their data. The legacy tree is
            // left in place; it stops growing because writes now target the
            // primary path.
            match copy_dir_into_place(&legacy, primary) {
                Ok(()) => {
                    tracing::info!(
                        target: "config::migration",
                        "Migrated legacy state directory {} -> {} (copied; rename failed: {err}). \
                         The legacy .deepseek copy was left in place.",
                        legacy.display(),
                        primary.display()
                    );
                    return Ok(LegacyStateMigration::Migrated(StateMigration {
                        subdir: subdir.to_string(),
                        legacy_path: legacy,
                        primary_path: primary.to_path_buf(),
                        kind: StateMigrationKind::Copied,
                    }));
                }
                Err(copy_err) => {
                    // A concurrent process may have completed the move/copy
                    // after our initial check. Use that completed primary,
                    // rather than resuming writes to the legacy tree.
                    if primary.is_dir() {
                        return Ok(LegacyStateMigration::NotNeeded);
                    }
                    tracing::warn!(
                        target: "config::migration",
                        "Could not migrate legacy state {} -> {} (rename: {err}; copy: {copy_err:#}). \
                         The legacy path stays in use and migration retries next time.",
                        legacy.display(),
                        primary.display()
                    );
                }
            }
        }
    }
    Ok(LegacyStateMigration::Failed { legacy })
}

/// Copy `src` to a staging sibling of `dst` and rename it into place, so a
/// copy that fails midway never leaves a partial `dst` that later runs would
/// treat as a completed migration.
fn copy_dir_into_place(src: &Path, dst: &Path) -> Result<()> {
    let mut staging_name = OsString::from(".");
    staging_name.push(dst.file_name().unwrap_or_default());
    staging_name.push(".migrating-");
    // Each attempt owns only its unique private directory. A fixed sibling
    // could be another process's active copy and must never be cleared.
    let staging = tempfile::Builder::new().prefix(&staging_name).tempdir_in(
        dst.parent()
            .context("migration destination has no parent")?,
    )?;
    copy_dir_recursive(src, staging.path())?;
    std::fs::rename(staging.path(), dst).with_context(|| {
        format!(
            "failed to move {} into place at {}",
            staging.path().display(),
            dst.display()
        )
    })
}

/// Recursively copy a directory tree from `src` to `dst`, creating `dst`.
/// Symlinks and other non-file/non-dir entries are skipped (rare in state dirs).
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("failed to create {}", dst.display()))?;
    for entry in
        std::fs::read_dir(src).with_context(|| format!("failed to read {}", src.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in {}", src.display()))?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("failed to read file type for {}", path.display()))?;
        if file_type.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else if file_type.is_file() {
            std::fs::copy(&path, &target).with_context(|| {
                format!("failed to copy {} -> {}", path.display(), target.display())
            })?;
        }
    }
    Ok(())
}

/// Resolve a project-local state subdirectory, preferring `.codewhale/`
/// when it exists, falling back to `.deepseek/` for legacy projects.
///
/// Returns `(true, path)` when the primary `.codewhale/` path is used,
/// `(false, path)` for the legacy fallback. The boolean helps callers
/// emit a deprecation notice on legacy paths.
pub fn resolve_project_state_dir(workspace: &Path, subdir: &str) -> Result<(bool, PathBuf)> {
    ensure_safe_state_subdir(subdir)?;
    let workspace = normalize_project_workspace(workspace)?;
    let primary = workspace.join(CODEWHALE_APP_DIR).join(subdir);
    if primary.exists() {
        return Ok((true, primary));
    }
    let legacy = workspace.join(LEGACY_APP_DIR).join(subdir);
    Ok((false, legacy))
}

/// Ensure a project-local state subdirectory exists under `.codewhale/`,
/// creating it if necessary. Returns the directory path.
pub fn ensure_project_state_dir(workspace: &Path, subdir: &str) -> Result<PathBuf> {
    ensure_safe_state_subdir(subdir)?;
    let workspace = normalize_project_workspace(workspace)?;
    let dir = workspace.join(CODEWHALE_APP_DIR).join(subdir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}/", dir.display()))?;
    Ok(dir)
}

pub fn resolve_config_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return normalize_config_file_path(path);
    }
    if let Some(path) = codewhale_paths::config_path_override().map_err(anyhow::Error::new)? {
        return normalize_config_file_path(path);
    }
    default_config_path()
}

pub fn require_explicit_config_exists(explicit: Option<PathBuf>) -> Result<()> {
    if let Some(path) = explicit {
        let path = normalize_config_file_path(path)?;
        if !checked_path_exists(&path)? {
            bail!("config file does not exist: {}", quote_os_path(&path));
        }
    }
    Ok(())
}

/// Whether `path` names a workspace-scoped config document —
/// `<repo>/.codewhale/config.toml` (or the legacy `.deepseek` layout) inside a
/// checkout — rather than a user-global config file.
///
/// Credential writes (api_key values, `auth_mode` markers, oauth/external
/// credential pointers) must never target such a document: a key saved while
/// working in one repo would be invisible from every other repo, and the repo
/// file stores it in plaintext where it is easy to commit by accident (#5045,
/// #5193).
///
/// A path is classified workspace-scoped only when its parent directory is a
/// `.codewhale`/`.deepseek` app dir outside the user's home AND the document
/// belongs to a workspace: it is relative (resolves against the process cwd),
/// its base directory contains the process cwd, or its base directory is a
/// checkout (has a `.git` entry). An explicit `$CODEWHALE_HOME` config is
/// user-global wherever that home points, even when the directory itself
/// happens to be named `.codewhale`; other custom locations (for example
/// `CODEWHALE_CONFIG_PATH=~/team.toml` or an isolated test directory) stay
/// honored as deliberate user-scoped choices.
#[must_use]
pub fn config_path_is_workspace_scoped(path: &Path) -> bool {
    config_path_is_workspace_scoped_with_context(
        path,
        codewhale_paths::codewhale_home_override()
            .ok()
            .flatten()
            .as_deref(),
        codewhale_paths::user_home().as_deref(),
        std::env::current_dir().ok().as_deref(),
    )
}

/// Environment-free core of [`config_path_is_workspace_scoped`], split out so
/// scope classification is testable without mutating process-global state.
fn config_path_is_workspace_scoped_with_context(
    path: &Path,
    explicit_codewhale_home: Option<&Path>,
    user_home: Option<&Path>,
    current_dir: Option<&Path>,
) -> bool {
    if let Some(home) = explicit_codewhale_home
        && same_lexical_or_canonical_path(path, &home.join(CONFIG_FILE_NAME))
    {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    let parent_is_app_dir = parent
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name == CODEWHALE_APP_DIR || name == LEGACY_APP_DIR);
    if !parent_is_app_dir {
        return false;
    }
    let Some(base) = parent.parent() else {
        return true;
    };
    if let Some(home) = user_home
        && same_lexical_or_canonical_path(base, home)
    {
        return false;
    }
    if path.is_relative() {
        // Resolves against the process cwd: repo-scoped by construction.
        return true;
    }
    // The document belongs to the workspace the process is sitting in…
    if let Some(cwd) = current_dir
        && canonicalize_or_keep(cwd).starts_with(canonicalize_or_keep(base))
    {
        return true;
    }
    // …or to some other checkout (a `.git` entry beside the app dir).
    base.join(".git").exists()
}

/// Lexical equality first, canonical equality as a fallback so an existing
/// path still matches through symlinked parents (e.g. `/tmp` on macOS).
fn same_lexical_or_canonical_path(a: &Path, b: &Path) -> bool {
    a == b || canonicalize_or_keep(a) == canonicalize_or_keep(b)
}

fn canonicalize_or_keep(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod credential_scope_tests {
    use super::config_path_is_workspace_scoped_with_context;
    use std::path::Path;

    #[test]
    fn config_inside_current_workspace_is_workspace_scoped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let cwd = repo.join("nested/dir");
        for app_dir in [".codewhale", ".deepseek"] {
            let config = repo.join(app_dir).join("config.toml");
            assert!(
                config_path_is_workspace_scoped_with_context(
                    &config,
                    None,
                    Some(Path::new("/home/user")),
                    Some(&cwd),
                ),
                "{} should be workspace-scoped when cwd sits inside the repo",
                config.display()
            );
        }
    }

    #[test]
    fn relative_app_dir_config_is_workspace_scoped() {
        assert!(config_path_is_workspace_scoped_with_context(
            Path::new(".codewhale/config.toml"),
            None,
            Some(Path::new("/home/user")),
            Some(Path::new("/somewhere/else")),
        ));
    }

    #[test]
    fn checkout_config_outside_cwd_is_workspace_scoped_via_git_marker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).expect("git marker");
        std::fs::create_dir_all(repo.join(".codewhale")).expect("app dir");
        assert!(config_path_is_workspace_scoped_with_context(
            &repo.join(".codewhale/config.toml"),
            None,
            Some(Path::new("/home/user")),
            Some(Path::new("/somewhere/else")),
        ));
    }

    #[test]
    fn user_global_and_custom_locations_are_not_workspace_scoped() {
        let home = Path::new("/home/user");
        let elsewhere = Some(Path::new("/somewhere/else"));
        for global_config in [
            "/home/user/.codewhale/config.toml",
            "/home/user/.deepseek/config.toml",
            "/home/user/team-config.toml",
            "/etc/codewhale/config.toml",
        ] {
            assert!(
                !config_path_is_workspace_scoped_with_context(
                    Path::new(global_config),
                    None,
                    Some(home),
                    elsewhere,
                ),
                "{global_config} should stay user-global"
            );
        }
        // An isolated app-dir-shaped location with no workspace relationship
        // (no cwd ancestry, no checkout marker) stays honored: test harnesses
        // and deliberate overrides point there.
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(!config_path_is_workspace_scoped_with_context(
            &temp.path().join(".codewhale/config.toml"),
            None,
            Some(home),
            elsewhere,
        ));
    }

    #[test]
    fn explicit_codewhale_home_config_is_user_global_even_when_dir_is_app_named() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let explicit = repo.join(".codewhale");
        // Even with cwd inside the repo, the explicit CODEWHALE_HOME config is
        // the user-global scope by definition.
        assert!(!config_path_is_workspace_scoped_with_context(
            &explicit.join("config.toml"),
            Some(&explicit),
            Some(Path::new("/home/user")),
            Some(&repo),
        ));
        // A different repo-scoped document is still workspace-scoped.
        assert!(config_path_is_workspace_scoped_with_context(
            &repo.join("other/.codewhale/config.toml"),
            Some(&explicit),
            Some(Path::new("/home/user")),
            Some(&repo.join("other")),
        ));
    }
}

#[must_use]
pub fn permissions_path_for_config_path(config_path: &Path) -> PathBuf {
    config_sibling_path_unchecked(config_path, OsStr::new(PERMISSIONS_FILE_NAME))
}

fn checked_permissions_path_for_config_path(config_path: &Path) -> Result<PathBuf> {
    checked_config_sibling_path(config_path, OsStr::new(PERMISSIONS_FILE_NAME))
}

pub fn resolve_permissions_path(config_path: Option<PathBuf>) -> Result<PathBuf> {
    checked_permissions_path_for_config_path(&resolve_config_path(config_path)?)
}

/// Load the active sibling permission rules with confirmation tokens suitable
/// for a later compare-and-remove operation.
pub fn load_permissions_snapshot(config_path: Option<PathBuf>) -> Result<PermissionsSnapshot> {
    let path = resolve_permissions_path(config_path)?;
    let (file_exists, raw, permissions) = read_permissions_state(&path)?;
    let file_state = if !file_exists {
        PermissionsFileState::Missing
    } else if raw.is_empty() {
        PermissionsFileState::Empty
    } else {
        PermissionsFileState::Present
    };
    let removal_tokens = (0..permissions.rules.len())
        .map(|index| permission_removal_token(&path, &raw, index))
        .collect();
    Ok(PermissionsSnapshot {
        path,
        file_state,
        permissions,
        removal_tokens,
    })
}

/// Remove one zero-based permission rule if `expected_token` still describes
/// that exact index in the current file.
///
/// The file is re-read only after acquiring the same adjacent lock used by
/// append operations. This makes the token check and atomic replacement one
/// transaction, preventing stale list views from deleting a different rule.
pub fn remove_permission_rule(
    config_path: Option<PathBuf>,
    index: usize,
    expected_token: &str,
) -> Result<ToolAskRule> {
    let path = resolve_permissions_path(config_path)?;
    config_document::with_config_write_lock(&path, |path| {
        let (file_exists, raw, permissions) = read_permissions_state(path)?;
        if !file_exists {
            bail!(
                "permissions changed after they were listed; reload {} and retry",
                quote_os_path(path)
            );
        }
        let rule = permissions.rules.get(index).cloned().with_context(|| {
            format!(
                "permission rule {} no longer exists in {}; list rules again",
                index + 1,
                quote_os_path(path)
            )
        })?;
        let current_token = permission_removal_token(path, &raw, index);
        if current_token != expected_token {
            bail!(
                "permissions changed after they were listed; reload {} and retry",
                quote_os_path(path)
            );
        }

        let mut document = parse_permissions_document(path, &raw)?;
        let rules_item = document.get_mut("rules").with_context(|| {
            format!(
                "permissions at {} no longer contain a rules array",
                quote_os_path(path)
            )
        })?;
        let orphaned_header = remove_permission_rule_item(rules_item, index)?;
        if let Some(header) = orphaned_header {
            let trailing = format!(
                "{header}{}",
                document.trailing().as_str().unwrap_or_default()
            );
            document.set_trailing(trailing);
        }
        let body = document.to_string();
        let persisted = parse_generated_permissions(path, &body)?;
        if persisted.rules.len() + 1 != permissions.rules.len() {
            bail!(
                "refusing inconsistent permission removal at {}",
                quote_os_path(path)
            );
        }
        write_permissions_atomic(path, body.as_bytes())?;
        Ok(rule)
    })
}

fn load_sibling_permissions(config_path: &Path) -> Result<PermissionsToml> {
    let permissions_path = checked_permissions_path_for_config_path(config_path)?;
    let (_, _, permissions) = read_permissions_state(&permissions_path)?;
    Ok(permissions)
}

fn read_permissions_state(path: &Path) -> Result<(bool, String, PermissionsToml)> {
    let file_exists = checked_path_exists(path)?;
    let raw = if file_exists {
        read_checked_permissions_file(path)?
    } else {
        String::new()
    };
    let permissions = if raw.trim().is_empty() {
        PermissionsToml::default()
    } else {
        toml::from_str(&raw).map_err(|err| {
            anyhow::anyhow!(
                "failed to parse permissions at {} ({}); file contents were omitted",
                quote_os_path(path),
                toml_error_location(Some(&raw), &err)
            )
        })?
    };
    Ok((file_exists, raw, permissions))
}

fn parse_permissions_document(path: &Path, raw: &str) -> Result<toml_edit::DocumentMut> {
    if raw.trim().is_empty() {
        Ok(toml_edit::DocumentMut::new())
    } else {
        raw.parse::<toml_edit::DocumentMut>().map_err(|_| {
            anyhow::anyhow!(
                "failed to edit permissions at {}; file contents were omitted",
                quote_os_path(path)
            )
        })
    }
}

fn parse_generated_permissions(path: &Path, body: &str) -> Result<PermissionsToml> {
    toml::from_str(body).map_err(|_| {
        anyhow::anyhow!(
            "generated invalid permissions document for {}; file contents were omitted",
            quote_os_path(path)
        )
    })
}

fn permission_removal_token(path: &Path, raw: &str, index: usize) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"codewhale-permission-removal-v1\0");
    hasher.update(quote_os_path(path).as_bytes());
    hasher.update(b"\0");
    hasher.update(index.to_le_bytes());
    hasher.update(b"\0");
    hasher.update(raw.as_bytes());
    let digest = hasher.finalize();
    let mut token = String::with_capacity(24);
    for byte in &digest[..12] {
        use std::fmt::Write as _;
        let _ = write!(&mut token, "{byte:02x}");
    }
    token
}

fn append_permission_rule(item: &mut toml_edit::Item, rule: &ToolAskRule) -> Result<()> {
    match item {
        toml_edit::Item::ArrayOfTables(rules) => {
            rules.push(permission_rule_table(rule));
            Ok(())
        }
        toml_edit::Item::Value(value) => {
            let Some(rules) = value.as_array_mut() else {
                bail!("`rules` in permissions.toml must be an array");
            };
            rules.push(toml_edit::Value::InlineTable(permission_rule_inline_table(
                rule,
            )));
            Ok(())
        }
        _ => bail!("`rules` in permissions.toml must be an array"),
    }
}

fn remove_permission_rule_item(item: &mut toml_edit::Item, index: usize) -> Result<Option<String>> {
    match item {
        toml_edit::Item::ArrayOfTables(rules) => {
            if index >= rules.len() {
                bail!("permission rule index changed before removal");
            }
            let file_header = if index == 0 {
                rules
                    .get(index)
                    .and_then(|rule| rule.decor().prefix())
                    .and_then(toml_edit::RawString::as_str)
                    .map(str::to_owned)
            } else {
                None
            };
            rules.remove(index);
            if let Some(header) = file_header.as_deref()
                && let Some(next_rule) = rules.get_mut(0)
            {
                let next_prefix = next_rule
                    .decor()
                    .prefix()
                    .and_then(toml_edit::RawString::as_str)
                    .unwrap_or_default()
                    .to_owned();
                next_rule
                    .decor_mut()
                    .set_prefix(format!("{header}{next_prefix}"));
                return Ok(None);
            }
            Ok(file_header)
        }
        toml_edit::Item::Value(value) => {
            let Some(rules) = value.as_array_mut() else {
                bail!("`rules` in permissions.toml must be an array");
            };
            if index >= rules.len() {
                bail!("permission rule index changed before removal");
            }
            rules.remove(index);
            Ok(None)
        }
        _ => bail!("`rules` in permissions.toml must be an array"),
    }
}

fn permission_rule_table(rule: &ToolAskRule) -> toml_edit::Table {
    let mut table = toml_edit::Table::new();
    table["tool"] = toml_edit::value(rule.tool.clone());
    if let Some(command) = rule.command.as_deref() {
        table["command"] = toml_edit::value(command);
    }
    if rule.command_exact {
        table["command_exact"] = toml_edit::value(true);
    }
    if let Some(path) = rule.path.as_deref() {
        table["path"] = toml_edit::value(path);
    }
    if let Some(workspace) = rule.workspace.as_deref() {
        table["workspace"] = toml_edit::value(workspace);
    }
    if rule.action != PermissionAction::Ask {
        table["action"] = toml_edit::value(match rule.action {
            PermissionAction::Allow => "allow",
            PermissionAction::Ask => "ask",
            PermissionAction::Deny => "deny",
        });
    }
    table
}

fn permission_rule_inline_table(rule: &ToolAskRule) -> toml_edit::InlineTable {
    let mut table = toml_edit::InlineTable::new();
    table.insert("tool", toml_edit::Value::from(rule.tool.clone()));
    if let Some(command) = rule.command.as_deref() {
        table.insert("command", toml_edit::Value::from(command));
    }
    if rule.command_exact {
        table.insert("command_exact", toml_edit::Value::from(true));
    }
    if let Some(path) = rule.path.as_deref() {
        table.insert("path", toml_edit::Value::from(path));
    }
    if let Some(workspace) = rule.workspace.as_deref() {
        table.insert("workspace", toml_edit::Value::from(workspace));
    }
    if rule.action != PermissionAction::Ask {
        table.insert(
            "action",
            toml_edit::Value::from(match rule.action {
                PermissionAction::Allow => "allow",
                PermissionAction::Ask => "ask",
                PermissionAction::Deny => "deny",
            }),
        );
    }
    table
}

fn write_permissions_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path.parent().with_context(|| {
        format!(
            "permissions path has no parent directory: {}",
            path.display()
        )
    })?;
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "failed to create permissions directory {}",
            parent.display()
        )
    })?;

    let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
        format!(
            "failed to create temporary permissions file in {}",
            parent.display()
        )
    })?;
    #[cfg(unix)]
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .with_context(|| {
            format!(
                "failed to secure temporary permissions file for {}",
                path.display()
            )
        })?;
    temporary
        .write_all(body)
        .with_context(|| format!("failed to write permissions at {}", path.display()))?;
    temporary
        .as_file()
        .sync_all()
        .with_context(|| format!("failed to sync permissions at {}", path.display()))?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to replace permissions at {}", path.display()))?;
    Ok(())
}

pub fn default_config_path() -> Result<PathBuf> {
    // Prefer ~/.codewhale/config.toml when it exists (fresh install or
    // migrated), otherwise fall back to ~/.deepseek/config.toml.
    let primary = codewhale_home()?.join(CONFIG_FILE_NAME);
    if codewhale_home_is_explicit() || primary.exists() {
        return Ok(primary);
    }
    let legacy = legacy_deepseek_home()?.join(CONFIG_FILE_NAME);
    if legacy.exists() {
        return Ok(legacy);
    }
    // Neither exists — return primary so first write creates it there.
    Ok(primary)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigMigration {
    pub legacy_path: PathBuf,
    pub primary_path: PathBuf,
}

impl ConfigMigration {
    pub fn user_notice(&self) -> String {
        format!(
            "Migrated legacy config from {} to {}. Use the .codewhale path for future edits; the .deepseek file remains only as a compatibility fallback.",
            self.legacy_path.display(),
            self.primary_path.display()
        )
    }
}

/// v0.8.44: one-time migration from `~/.deepseek/config.toml` to
/// `~/.codewhale/config.toml`. Called on first launch after the config
/// is loaded; copies the legacy file if the primary doesn't exist yet.
/// Never overwrites an existing primary config.
pub fn migrate_config_if_needed() -> Result<Option<ConfigMigration>> {
    if codewhale_home_is_explicit() {
        return Ok(None);
    }
    let primary = codewhale_home()?.join(CONFIG_FILE_NAME);
    // `exists()` follows links, so a dangling link would read as "absent" and
    // the copy below would create whatever it points at. A link at the primary
    // name is refused instead, dangling or not.
    reject_path_symlink(&primary)?;
    if primary.exists() {
        return Ok(None);
    }
    let legacy = legacy_deepseek_home()?.join(CONFIG_FILE_NAME);
    if !legacy.exists() {
        return Ok(None);
    }
    // Copy the config to the new home, owner-only, through a temporary file
    // renamed into place (the primary name is replaced, never written through).
    let contents = std::fs::read(&legacy).context("failed to read legacy deepseek config")?;
    persistence::atomic_write(&primary, &contents)
        .context("failed to migrate config from deepseek to codewhale home")?;
    tracing::info!(
        "Migrated config from {} to {}",
        legacy.display(),
        primary.display()
    );
    Ok(Some(ConfigMigration {
        legacy_path: legacy,
        primary_path: primary,
    }))
}

/// `value` as the TOML value a [`SettingDef`] declares, or an error naming
/// the key and what it accepts. No partial write happens on error.
fn schema_toml_value(key: &str, def: &SettingDef, value: &str) -> Result<toml::Value> {
    let trimmed = value.trim();
    Ok(match def.kind {
        SettingKind::Bool(_) => toml::Value::Boolean(
            parse_bool(value).with_context(|| format!("invalid value for '{key}'"))?,
        ),
        SettingKind::Int => toml::Value::Integer(trimmed.parse().map_err(|_| {
            anyhow::anyhow!("invalid value '{value}' for '{key}': expected an integer")
        })?),
        SettingKind::Float => toml::Value::Float(
            trimmed
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite())
                .ok_or_else(|| {
                    anyhow::anyhow!("invalid value '{value}' for '{key}': expected a number")
                })?,
        ),
        SettingKind::Enum(options) => {
            let Some(option) = options
                .iter()
                .find(|option| option.value.eq_ignore_ascii_case(trimmed))
            else {
                let expected: Vec<&str> = options.iter().map(|option| option.value).collect();
                bail!(
                    "invalid value '{value}' for '{key}': expected one of {}",
                    expected.join(", ")
                );
            };
            toml::Value::String(option.value.to_string())
        }
        SettingKind::String => toml::Value::String(value.to_string()),
    })
}

fn parse_bool(raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "enabled" => Ok(true),
        "0" | "false" | "no" | "off" | "disabled" => Ok(false),
        _ => bail!("invalid boolean '{raw}'"),
    }
}

fn parse_http_headers(raw: &str) -> Result<BTreeMap<String, String>> {
    let mut headers = BTreeMap::new();
    for pair in raw.trim().split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            bail!("invalid header pair '{pair}', expected name=value");
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            bail!("header name cannot be empty");
        }
        if value.is_empty() {
            continue;
        }
        headers.insert(name.to_string(), value.to_string());
    }
    Ok(headers)
}

fn serialize_http_headers(headers: &BTreeMap<String, String>) -> Option<String> {
    if headers.is_empty() {
        return None;
    }
    Some(
        headers
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn serialize_http_headers_for_display(headers: &BTreeMap<String, String>) -> Option<String> {
    if headers.is_empty() {
        return None;
    }
    Some(
        headers
            .iter()
            .map(|(name, value)| {
                let display_value = if is_sensitive_config_key(name) {
                    redact_secret(value)
                } else {
                    value.clone()
                };
                format!("{name}={display_value}")
            })
            .collect::<Vec<_>>()
            .join(","),
    )
}

fn redact_secret(secret: &str) -> String {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() <= 16 {
        return "********".to_string();
    }
    let prefix: String = chars.iter().take(4).collect();
    let suffix: String = chars
        .iter()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{prefix}***{suffix}")
}

#[must_use]
pub fn is_sensitive_config_key(key: &str) -> bool {
    key.rsplit('.')
        .next()
        .is_some_and(codewhale_secrets::sanitize::is_sensitive_key_name)
}

/// Resolve dotted paths without treating a dotted key as a top-level literal.
fn config_value_at_path<'a>(value: &'a toml::Value, key: &str) -> Option<&'a toml::Value> {
    key.split('.')
        .try_fold(value, |value, part| value.get(part))
}

fn redact_toml_value_for_display(key: &str, value: &toml::Value) -> String {
    redact_toml_value_for_display_inner(key, false, value).to_string()
}

impl ConfigToml {
    /// Redacted TOML rendering of the effective config for `config dump`.
    /// Structure is preserved; strings under sensitive key names (api_key,
    /// token, secret, … — see `is_sensitive_config_key`, nested tables
    /// inherit sensitivity from their ancestors) are redacted. Redaction is
    /// name-based: an unrecognized key holding a secret-looking value would
    /// pass through, so pasting dump output still deserves a glance.
    #[must_use]
    pub fn redacted_toml_value(&self) -> toml::Value {
        let value =
            toml::Value::try_from(self).expect("ConfigToml derives Serialize, so this holds");
        match value {
            toml::Value::Table(table) => toml::Value::Table(
                table
                    .into_iter()
                    .map(|(key, value)| {
                        let redacted = redact_toml_value_for_display_inner(&key, false, &value);
                        (key, redacted)
                    })
                    .collect(),
            ),
            other => other,
        }
    }
}

fn toml_value_as_u64(value: &toml::Value) -> Option<u64> {
    match value {
        toml::Value::Integer(value) => u64::try_from(*value).ok(),
        toml::Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

fn redact_toml_value_for_display_inner(
    key: &str,
    sensitive_ancestor: bool,
    value: &toml::Value,
) -> toml::Value {
    let sensitive = sensitive_ancestor || is_sensitive_config_key(key);
    match value {
        toml::Value::String(value) if sensitive => toml::Value::String(redact_secret(value)),
        toml::Value::Array(values) => toml::Value::Array(
            values
                .iter()
                .map(|value| redact_toml_value_for_display_inner(key, sensitive, value))
                .collect(),
        ),
        toml::Value::Table(table) => {
            let mut redacted = toml::map::Map::new();
            for (child_key, child_value) in table {
                let path = if key.is_empty() {
                    child_key.clone()
                } else {
                    format!("{key}.{child_key}")
                };
                redacted.insert(
                    child_key.clone(),
                    redact_toml_value_for_display_inner(&path, sensitive, child_value),
                );
            }
            toml::Value::Table(redacted)
        }
        _ if sensitive => toml::Value::String("********".to_string()),
        _ => value.clone(),
    }
}

fn normalize_config_file_path(path: PathBuf) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        bail!("config path cannot be empty");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("config path cannot contain '..' components");
    }
    if path.file_name().is_none() {
        bail!("config path must include a file name");
    }
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for config path")?
            .join(path)
    };
    let file_name = absolute
        .file_name()
        .map(OsString::from)
        .context("config path must include a file name")?;
    let parent = absolute
        .parent()
        .context("config path must include a parent directory")?;
    let parent = match parent.canonicalize() {
        Ok(parent) => parent,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => parent.to_path_buf(),
        Err(err) => {
            return Err(err).with_context(|| {
                format!("failed to resolve config directory {}", parent.display())
            });
        }
    };
    let normalized = parent.join(file_name);
    reject_path_symlink(&normalized)?;
    Ok(normalized)
}

fn normalize_project_workspace(workspace: &Path) -> Result<PathBuf> {
    if workspace.as_os_str().is_empty() {
        bail!("project workspace path cannot be empty");
    }
    if workspace
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("project workspace path cannot contain '..' components");
    }
    let absolute = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for project workspace")?
            .join(workspace)
    };
    match absolute.canonicalize() {
        Ok(path) => Ok(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(normalize_path_components(&absolute))
        }
        Err(err) => Err(err).with_context(|| {
            format!(
                "failed to resolve project workspace {}",
                workspace.display()
            )
        }),
    }
}

fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

fn checked_path_exists(path: &Path) -> Result<bool> {
    let path = normalize_config_file_path(path.to_path_buf())?;
    path.try_exists()
        .with_context(|| format!("failed to inspect config path {}", path.display()))
}

fn read_checked_config_file(path: &Path) -> Result<String> {
    read_checked_toml_file(path, "config")
}

fn read_checked_permissions_file(path: &Path) -> Result<String> {
    read_checked_toml_file(path, "permissions")
}

fn read_checked_toml_file(path: &Path, label: &str) -> Result<String> {
    let path = normalize_config_file_path(path.to_path_buf())?;
    read_string_no_follow(&path)
        .with_context(|| format!("failed to read {label} at {}", path.display()))
}

/// Maximum bytes read from a config file. Configs are kilobytes; anything
/// larger is not a config file.
const MAX_CONFIG_FILE_BYTES: u64 = 1024 * 1024;

#[cfg(unix)]
fn read_string_no_follow(path: &Path) -> std::io::Result<String> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let mut raw = String::new();
    file.take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("config file {} exceeds the 1 MiB limit", path.display()),
        ));
    }
    Ok(raw)
}

#[cfg(not(unix))]
fn read_string_no_follow(path: &Path) -> std::io::Result<String> {
    let file = fs::File::open(path)?;
    let mut raw = String::new();
    file.take(MAX_CONFIG_FILE_BYTES + 1)
        .read_to_string(&mut raw)?;
    if raw.len() as u64 > MAX_CONFIG_FILE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("config file {} exceeds the 1 MiB limit", path.display()),
        ));
    }
    Ok(raw)
}

fn reject_path_symlink(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        bail!("config path must not be a symlink: {}", path.display());
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct EnvRuntimeOverrides {
    provider: Option<ProviderKind>,
    provider_source: Option<&'static str>,
    model: Option<String>,
    volcengine_model: Option<String>,
    wanjie_ark_model: Option<String>,
    openrouter_model: Option<String>,
    orcarouter_model: Option<String>,
    moonshot_model: Option<String>,
    xiaomi_mimo_model: Option<String>,
    xiaomi_mimo_mode: Option<String>,
    novita_model: Option<String>,
    fireworks_model: Option<String>,
    arcee_model: Option<String>,
    auth_mode: Option<String>,
    log_level: Option<String>,
    telemetry: Option<bool>,
    /// `CODEWHALE_TELEMETRY`/`DEEPSEEK_TELEMETRY` was set to something
    /// [`parse_bool`] could not read. A typo in a kill switch must never
    /// resolve to "on", so this forces telemetry off the same way an explicit
    /// `false` does.
    telemetry_env_invalid: bool,
    /// An environment-level kill switch is in force for this process.
    ///
    /// See [`telemetry_floor_in_force`] for what sets it and why the dispatcher
    /// has to state it rather than let the child infer it.
    telemetry_floor: bool,
    /// `CODEWHALE_TELEMETRY_ENDPOINT`/`DEEPSEEK_TELEMETRY_ENDPOINT`. Overrides
    /// the config file. A workspace `.env` cannot reach this — the dotenv
    /// allowlist admits only built-in provider credential names.
    telemetry_endpoint: Option<String>,
    approval_policy: Option<String>,
    sandbox_mode: Option<String>,
    yolo: Option<bool>,
    verbosity: Option<String>,
    http_headers: Option<BTreeMap<String, String>>,
    active_route_base_url: Option<String>,
    deepseek_anthropic_base_url: Option<String>,
    nvidia_base_url: Option<String>,
    openai_base_url: Option<String>,
    atlascloud_base_url: Option<String>,
    volcengine_base_url: Option<String>,
    wanjie_ark_base_url: Option<String>,
    openrouter_base_url: Option<String>,
    orcarouter_base_url: Option<String>,
    xiaomi_mimo_base_url: Option<String>,
    novita_base_url: Option<String>,
    fireworks_base_url: Option<String>,
    siliconflow_base_url: Option<String>,
    siliconflow_model: Option<String>,
    arcee_base_url: Option<String>,
    moonshot_base_url: Option<String>,
    sglang_base_url: Option<String>,
    vllm_base_url: Option<String>,
    ollama_base_url: Option<String>,
    ollama_cloud_base_url: Option<String>,
    ollama_cloud_model: Option<String>,
    huggingface_base_url: Option<String>,
    huggingface_model: Option<String>,
    modelscope_base_url: Option<String>,
    modelscope_model: Option<String>,
    together_base_url: Option<String>,
    together_model: Option<String>,
    qianfan_base_url: Option<String>,
    qianfan_model: Option<String>,
    openai_codex_base_url: Option<String>,
    openai_codex_model: Option<String>,
    anthropic_base_url: Option<String>,
    anthropic_model: Option<String>,
    openmodel_base_url: Option<String>,
    openmodel_model: Option<String>,
    zai_base_url: Option<String>,
    zai_model: Option<String>,
    stepfun_base_url: Option<String>,
    stepfun_model: Option<String>,
    minimax_base_url: Option<String>,
    minimax_anthropic_base_url: Option<String>,
    minimax_model: Option<String>,
    deepinfra_base_url: Option<String>,
    deepinfra_model: Option<String>,
    sakana_base_url: Option<String>,
    sakana_model: Option<String>,
    longcat_base_url: Option<String>,
    longcat_model: Option<String>,
    opencode_go_base_url: Option<String>,
    opencode_go_model: Option<String>,
    opencode_zen_base_url: Option<String>,
    opencode_zen_model: Option<String>,
    meta_base_url: Option<String>,
    meta_model: Option<String>,
    xai_base_url: Option<String>,
    xai_model: Option<String>,
    mistral_base_url: Option<String>,
    mistral_model: Option<String>,
    google_base_url: Option<String>,
    google_model: Option<String>,
    telecomjs_base_url: Option<String>,
    telecomjs_model: Option<String>,
    edenai_base_url: Option<String>,
    edenai_model: Option<String>,
    zenmux_base_url: Option<String>,
    zenmux_model: Option<String>,
    csdn_base_url: Option<String>,
    csdn_model: Option<String>,
    concentrate_base_url: Option<String>,
    concentrate_model: Option<String>,
    codewhale_base_url: Option<String>,
    codewhale_model: Option<String>,
    modelstudio_token_plan_base_url: Option<String>,
    modelstudio_token_plan_model: Option<String>,
    modelstudio_coding_plan_base_url: Option<String>,
    modelstudio_coding_plan_model: Option<String>,
}

/// The first of `names` that is set to a non-blank value. A variable exported
/// empty (compose `${VAR:-}`, CI templates) must not shadow the legacy
/// variable or the config file.
fn env_non_blank(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        std::env::var(name)
            .ok()
            .filter(|value| !value.trim().is_empty())
    })
}

impl EnvRuntimeOverrides {
    fn load() -> Self {
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

    fn base_url_for(&self, provider: ProviderKind) -> Option<String> {
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

    fn model_for(&self, provider: ProviderKind, base_url: &str) -> Option<String> {
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

#[cfg(test)]
mod tests;

use crate::config_toml::ConfigToml;
use crate::display_redaction::{
    parse_bool, parse_http_headers, redact_secret, serialize_http_headers,
    serialize_http_headers_for_display,
};
use crate::hotbar::{HotbarConfigResolution, resolve_hotbar_bindings};
use crate::provider;
use crate::provider_kind::ProviderKind;
use crate::provider_toml::{
    LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE, ProviderConfigToml, validate_openrouter_vendor,
};
use anyhow::Context as _;
use anyhow::{Result, bail};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProviderConfigField {
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
    pub(crate) fn parse(key: &str) -> Option<Self> {
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

    pub(crate) fn key(self) -> &'static str {
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

pub(crate) fn parse_provider_config_key(key: &str) -> Option<(ProviderKind, ProviderConfigField)> {
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
pub(crate) fn parse_custom_provider_config_key(key: &str) -> Option<(&str, &str)> {
    let suffix = key.strip_prefix("providers.")?;
    let (provider_id, field_key) = suffix.split_once('.')?;
    (!provider_id.is_empty()).then_some((provider_id, field_key))
}

pub(crate) fn is_builtin_provider_config_id(provider_id: &str) -> bool {
    provider::all_providers()
        .iter()
        .any(|p| p.provider_config_key() == provider_id)
}

pub(crate) fn builtin_provider_kind_for_config_id(provider_id: &str) -> Option<ProviderKind> {
    provider::all_providers()
        .iter()
        .map(|p| p.kind())
        .find(|kind| kind.provider().provider_config_key() == provider_id)
}

/// Split `providers.<id>.model_context_windows.<model>` (#6108). The model leg
/// is the whole remainder, so dotted wire ids like `qwen3.5` stay intact.
pub(crate) fn parse_model_context_window_key(key: &str) -> Option<(&str, &str)> {
    let (provider_id, field_key) = parse_custom_provider_config_key(key)?;
    let model = field_key.strip_prefix("model_context_windows.")?;
    (!model.is_empty()).then_some((provider_id, model))
}

/// Field legs a `[providers.<id>]` custom table accepts through
/// `config set`, including the required `kind` marker.
pub(crate) const CUSTOM_PROVIDER_FIELD_HINT: &str = "api_key, base_url, model, context_window, mode, wire, auth_mode, \
     insecure_skip_tls_verify, allow_insecure_http, http_headers, path_suffix, kind";

pub(crate) fn provider_config_key(provider: ProviderKind, field: ProviderConfigField) -> String {
    format!(
        "providers.{}.{}",
        provider.provider().provider_config_key(),
        field.key()
    )
}

pub(crate) fn get_provider_config_value(
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

pub(crate) fn get_provider_config_display_value(
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

pub(crate) fn parse_context_window(value: &str) -> Result<u32> {
    let parsed = value.trim().parse::<u32>().with_context(|| {
        format!("invalid context_window '{value}': expected a positive token count")
    })?;
    if parsed == 0 {
        bail!("context_window must be greater than 0");
    }
    Ok(parsed)
}

pub(crate) fn set_provider_config_value(
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

pub(crate) fn unset_provider_config_value(
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

pub(crate) fn insert_provider_config_values(
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

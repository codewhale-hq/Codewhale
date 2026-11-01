//! Provider adapter for the existing reviewed plugin manifest and route catalog.
//!
//! Declarations are data, never executable callbacks. Only OpenAI-compatible
//! inference and public OAuth PKCE clients are supported. Plugin changes require
//! a restart to add/remove routes; receipt checks revoke existing routes immediately.
use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, TcpListener};
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    PluginRegistry, activation::PluginActivationCapability,
    registry::verify_plugin_component_authority,
};
use codewhale_config::ProviderKind;
use codewhale_runtime::config::{Config, ProviderConfig};

pub fn load_config(path: Option<std::path::PathBuf>, profile: Option<&str>) -> Result<Config> {
    Config::load_with(
        path,
        profile,
        codewhale_runtime::config::ConfigEnvironmentPolicy::Runtime,
        &mut apply_startup_providers,
    )
}

pub fn load_config_structural(
    path: Option<std::path::PathBuf>,
    profile: Option<&str>,
) -> Result<Config> {
    Config::load_with(
        path,
        profile,
        codewhale_runtime::config::ConfigEnvironmentPolicy::StructuralDiagnostic,
        &mut apply_startup_providers,
    )
}

static STARTUP_REGISTRY: OnceLock<Arc<PluginRegistry>> = OnceLock::new();

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    pub static TEST_REGISTRY: std::cell::RefCell<Option<Arc<PluginRegistry>>> = const { std::cell::RefCell::new(None) };
}

/// Restore the calling test's previous registry even if its operation panics.
#[cfg(any(test, feature = "test-support"))]
pub fn with_test_registry<T>(registry: Arc<PluginRegistry>, operation: impl FnOnce() -> T) -> T {
    struct Restore(Option<Arc<PluginRegistry>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            TEST_REGISTRY.with(|slot| {
                let _ = slot.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(TEST_REGISTRY.with(|slot| slot.replace(Some(registry))));
    operation()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginProviderDeclaration {
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    pub oauth: codewhale_runtime::oauth::PluginOAuthConfig,
    /// Public routing/application metadata only. Authentication stays host-owned.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub http_headers: BTreeMap<String, String>,
}

impl PluginProviderDeclaration {
    pub(super) fn endpoints(&self) -> [&str; 4] {
        [
            &self.base_url,
            &self.oauth.issuer,
            &self.oauth.authorization_endpoint,
            &self.oauth.token_endpoint,
        ]
    }
}

pub fn validate_declarations(
    declarations: &BTreeMap<String, PluginProviderDeclaration>,
) -> Result<(), String> {
    if declarations.len() > 32 {
        return Err("plugin declares more than 32 providers".into());
    }
    for (name, declaration) in declarations {
        if !super::agent_plugin::is_standard_plugin_name(name)
            || ProviderKind::parse(name).is_some()
        {
            return Err("plugin provider name must be a custom lowercase provider identity".into());
        }
        let endpoint = reqwest::Url::parse(&declaration.base_url)
            .map_err(|_| "plugin provider base_url is invalid")?;
        let loopback = matches!(
            endpoint.host_str(),
            Some("127.0.0.1" | "localhost" | "[::1]")
        );
        if !(endpoint.scheme() == "https" || (endpoint.scheme() == "http" && loopback))
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(
                "plugin provider base_url must be HTTPS (or HTTP loopback) without credentials, query or fragment"
                    .into(),
            );
        }
        if declaration.http_headers.len() > 32 {
            return Err("plugin provider declares more than 32 HTTP headers".into());
        }
        let mut header_names = BTreeSet::new();
        for (name, value) in &declaration.http_headers {
            let header = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "plugin provider HTTP header name is invalid")?;
            if !header_names.insert(header.as_str().to_owned())
                || codewhale_config::is_upstream_auth_header(header.as_str())
                || matches!(
                    header.as_str(),
                    "cookie"
                        | "set-cookie"
                        | "proxy-authorization"
                        | "host"
                        | "content-length"
                        | "transfer-encoding"
                        | "connection"
                        | "proxy-connection"
                        | "te"
                        | "trailer"
                        | "upgrade"
                )
            {
                return Err("plugin provider HTTP headers must be unique public metadata, never authentication or transport controls".into());
            }
            if value.len() > 4096 || reqwest::header::HeaderValue::from_str(value).is_err() {
                return Err(
                    "plugin provider HTTP header value is invalid or exceeds 4096 bytes".into(),
                );
            }
        }
        declaration
            .oauth
            .validate()
            .map_err(|error| format!("plugin provider OAuth declaration is invalid: {error}"))?;
        if declaration.models.len() > 256 {
            return Err("plugin provider declares more than 256 models".into());
        }
        let mut seen = BTreeSet::new();
        for model in &declaration.models {
            if model.is_empty()
                || model.len() > 256
                || model.chars().any(char::is_control)
                || !seen.insert(model)
            {
                return Err(
                    "plugin provider model identities must be bounded, unique nonempty strings"
                        .into(),
                );
            }
        }
        if let Some(model) = &declaration.model
            && (model.is_empty()
                || model.len() > 256
                || model.chars().any(char::is_control)
                || (!declaration.models.is_empty() && !seen.contains(model)))
        {
            return Err("plugin provider default model must be a declared model identity".into());
        }
    }
    Ok(())
}

/// Called with the registry captured before workspace dotenv loading.
pub fn install_startup_registry(registry: Arc<PluginRegistry>) {
    let _ = STARTUP_REGISTRY.set(registry);
}

pub fn apply_startup_providers(config: &mut Config) -> anyhow::Result<()> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(registry) = TEST_REGISTRY.with(|slot| slot.borrow().clone()) {
        return apply_providers(config, &registry);
    }
    if let Some(registry) = STARTUP_REGISTRY.get() {
        apply_providers(config, registry)?;
    }
    Ok(())
}

/// Core-owned standard public-client PKCE login. Blocking sockets and secure
/// storage stay on the dedicated worker; plugins never receive token material.
pub async fn plugin_oauth_login(
    provider: String,
    base_url: String,
    descriptor: codewhale_runtime::oauth::PluginOAuthConfig,
    authority: codewhale_config::PluginAuthority,
) -> Result<()> {
    let policy = crate::activation::extension_host_policy_enabled();
    tokio::task::spawn_blocking(move || {
        let _scope = crate::activation::PolicyScope::propagate(policy);
        crate::providers::verify_provider_binding(
            &authority,
            &provider,
            &base_url,
            &descriptor,
            None,
        )
        .map_err(anyhow::Error::msg)?;
        let slot = codewhale_runtime::oauth::plugin_oauth_slot(&provider, &base_url, &descriptor)?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}{}",
            listener.local_addr()?.port(),
            descriptor.callback_path
        );
        let pkce = codewhale_runtime::oauth::generate_pkce();
        let state = codewhale_runtime::oauth::generate_state();
        let authorize_url = descriptor.authorize_url(&redirect_uri, &state, &pkce)?;
        eprintln!("{provider} sign-in (PKCE)\n  Open: {authorize_url}");
        if std::env::var_os("CODEWHALE_PLUGIN_OAUTH_NO_BROWSER").is_none() {
            let _ = webbrowser::open(&authorize_url);
        }
        let (code, _) = codewhale_runtime::oauth::wait_for_callback_validated(
            &[listener],
            &descriptor.callback_params(),
            &state,
            Some(&descriptor.issuer),
        )?;
        crate::providers::verify_provider_binding(
            &authority,
            &provider,
            &base_url,
            &descriptor,
            None,
        )
        .map_err(anyhow::Error::msg)?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("client_id", descriptor.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("code", code.as_str()),
            ("code_verifier", pkce.verifier.as_str()),
        ];
        if let Some(resource) = &descriptor.resource {
            form.push(("resource", resource));
        }
        let token = codewhale_runtime::oauth::plugin_token_response(&descriptor, &form, None)?;
        crate::providers::verify_provider_binding(
            &authority,
            &provider,
            &base_url,
            &descriptor,
            None,
        )
        .map_err(anyhow::Error::msg)?;
        codewhale_secrets::Secrets::auto_detect().set(&slot, &serde_json::to_string(&token)?)?;
        Ok(())
    })
    .await
    .context("Plugin OAuth login worker failed")?
}

pub fn plugin_auth_entry(config: &Config, provider: &str) -> anyhow::Result<ProviderConfig> {
    let mut entry = config
        .providers
        .as_ref()
        .and_then(|providers| providers.custom_provider_config(provider))
        .ok_or_else(|| anyhow::anyhow!("No enabled plugin contributes provider `{provider}`"))?
        .clone();
    entry.plugin_authority.as_ref().ok_or_else(|| {
        anyhow::anyhow!("Provider `{provider}` is not contributed by a reviewed plugin")
    })?;
    if entry.base_url.is_none() || entry.oauth.is_none() {
        return Err(anyhow::anyhow!("Plugin provider has no OAuth route"));
    }
    // Login, logout, readiness and inference must address the same normalized
    // route, even when a declaration includes a trailing slash.
    let identity = config
        .resolve_provider_pin_identity(provider)
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        identity.provider == ProviderKind::Custom,
        "plugin provider must have an admitted custom identity"
    );
    entry.base_url = Some(config.base_url_for_route(&identity));
    Ok(entry)
}

/// Bind an effective route to the exact reviewed declaration, not merely to
/// a still-valid receipt. Call this on a blocking worker at the use boundary.
/// `None` skips public-header comparison for login/logout; inference must pass
/// `Some` with its effective configured headers, including an empty map.
pub fn verify_provider_binding(
    authority: &codewhale_config::PluginAuthority,
    name: &str,
    base_url: &str,
    oauth: &codewhale_runtime::oauth::PluginOAuthConfig,
    http_headers: Option<&std::collections::HashMap<String, String>>,
) -> anyhow::Result<()> {
    verify_plugin_component_authority(authority, PluginActivationCapability::Providers)
        .map_err(anyhow::Error::msg)?;
    let staged = super::manifest::PluginManifest::validate_from_path(&authority.staged_manifest)
        .map_err(anyhow::Error::msg)?;
    anyhow::ensure!(
        staged.content_hash == authority.content_hash
            && staged.capability_hash == authority.capability_hash,
        "plugin provider runtime snapshot changed while checking its route"
    );
    let declaration = staged.manifest.providers.get(name).ok_or_else(|| {
        anyhow::anyhow!("reviewed plugin does not declare this provider identity")
    })?;
    anyhow::ensure!(
        codewhale_runtime::config::normalize_base_url(base_url)
            == codewhale_runtime::config::normalize_base_url(&declaration.base_url),
        "plugin provider endpoint differs from its reviewed declaration"
    );
    anyhow::ensure!(
        &declaration.oauth == oauth,
        "plugin provider OAuth differs from its reviewed declaration"
    );
    if let Some(headers) = http_headers {
        anyhow::ensure!(
            headers.len() == declaration.http_headers.len()
                && declaration
                    .http_headers
                    .iter()
                    .all(|(key, value)| headers.get(key) == Some(value)),
            "plugin provider public headers differ from its reviewed declaration"
        );
    }
    Ok(())
}

// This exhaustive pattern deliberately has no `..`: adding a provider field
// must revisit this boundary instead of silently allowing a new route override.
fn model_preference(entry: &ProviderConfig) -> Option<&str> {
    match entry {
        ProviderConfig {
            vendor: None,
            api_key: None,
            base_url: None,
            model: Some(model),
            context_window: None,
            model_context_windows: None,
            mode: None,
            wire: None,
            auth_mode: None,
            oauth: None,
            oauth_credential_generation: None,
            insecure_skip_tls_verify: None,
            allow_insecure_http: None,
            http_headers: None,
            path_suffix: None,
            reasoning_stream_style: None,
            max_concurrency: None,
            auth: None,
            external_credentials: None,
            kind: None,
            plugin_authority: None,
            api_key_env: None,
        } if !model.trim().is_empty() && !model.chars().any(char::is_control) => Some(model),
        _ => None,
    }
}

pub fn is_account_catalog_scope(provider: &str, fingerprint: &str) -> bool {
    STARTUP_REGISTRY.get().is_some_and(|registry| {
        registry.active_plugins().into_iter().any(|plugin| {
            plugin.manifest.providers.iter().any(|(name, declaration)| {
                provider == format!("custom:{name}")
                    && fingerprint
                        == codewhale_config::catalog::base_url_fingerprint(&declaration.base_url)
            })
        })
    })
}

pub fn apply_providers(config: &mut Config, registry: &PluginRegistry) -> anyhow::Result<()> {
    // Validate every collision and authority before mutating configuration.
    let mut additions = BTreeMap::new();
    for plugin in registry.active_plugins() {
        if plugin.manifest.providers.is_empty() {
            continue;
        }
        validate_declarations(&plugin.manifest.providers).map_err(anyhow::Error::msg)?;
        let authority = registry
            .authority_for(plugin.id.as_str())
            .ok_or_else(|| anyhow::anyhow!("active provider plugin has no review receipt"))?;
        verify_plugin_component_authority(&authority, PluginActivationCapability::Providers)
            .map_err(anyhow::Error::msg)?;
        for (name, declaration) in &plugin.manifest.providers {
            if additions.contains_key(name) {
                anyhow::bail!("plugin provider `{name}` collides with another plugin provider");
            }
            let mut declaration = declaration.clone();
            if let Some(existing) = config
                .providers
                .as_ref()
                .and_then(|providers| providers.custom.get(name))
            {
                declaration.model = Some(
                    model_preference(existing)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "plugin provider `{name}` collides with a configured provider route"
                            )
                        })?
                        .to_owned(),
                );
            }
            additions.insert(name.clone(), (declaration, authority.clone()));
        }
    }
    for (name, (declaration, authority)) in additions {
        config
            .providers
            .get_or_insert_with(Default::default)
            .custom
            .insert(
                name.clone(),
                ProviderConfig {
                    base_url: Some(declaration.base_url.clone()),
                    model: declaration.model,
                    kind: Some("openai-compatible".into()),
                    auth_mode: Some("oauth".into()),
                    oauth: Some(declaration.oauth),
                    http_headers: (!declaration.http_headers.is_empty())
                        .then(|| declaration.http_headers.into_iter().collect()),
                    plugin_authority: Some(authority),
                    ..Default::default()
                },
            );
        for id in declaration.models {
            let model = serde_json::from_value(
                serde_json::json!({"provider": name, "base_url": declaration.base_url, "id": id}),
            )?;
            config
                .custom_models
                .get_or_insert_with(Default::default)
                .push(model);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declaration() -> PluginProviderDeclaration {
        serde_json::from_value(serde_json::json!({
            "base_url": "https://gateway.example/api",
            "model": "tiny-model", "models": ["tiny-model"],
            "oauth": {"issuer":"https://gateway.example", "authorization_endpoint":"https://gateway.example/authorize", "token_endpoint":"https://gateway.example/token", "client_id":"custom-cli", "scopes":["models:invoke"]}
        })).unwrap()
    }

    #[test]
    fn provider_declarations_reject_builtin_names_and_credential_routes() {
        let mut declarations = BTreeMap::from([("openai".into(), declaration())]);
        assert!(validate_declarations(&declarations).is_err());
        let mut entry = declarations.remove("openai").unwrap();
        entry.base_url = "https://secret@gateway.example/api".into();
        declarations.insert("gateway".into(), entry);
        assert!(validate_declarations(&declarations).is_err());
        declarations.get_mut("gateway").unwrap().base_url = "https://gateway.example/api".into();
        validate_declarations(&declarations).unwrap();
        declarations.get_mut("gateway").unwrap().model = Some("undeclared".into());
        assert!(validate_declarations(&declarations).is_err());
    }

    #[test]
    fn provider_headers_allow_routing_metadata_but_never_credentials_or_transport_controls() {
        let mut declaration = declaration();
        declaration
            .http_headers
            .insert("X-Route-Group".into(), "public-group".into());
        let mut declarations = BTreeMap::from([("gateway".into(), declaration)]);
        validate_declarations(&declarations).unwrap();
        for name in [
            "Authorization",
            "X-Api-Key",
            "Cookie",
            "Set-Cookie",
            "Proxy-Authorization",
            "Host",
            "Content-Length",
            "Transfer-Encoding",
            "Connection",
        ] {
            let headers = &mut declarations.get_mut("gateway").unwrap().http_headers;
            headers.insert(name.into(), "blocked".into());
            assert!(
                validate_declarations(&declarations).is_err(),
                "accepted protected header {name}"
            );
            declarations
                .get_mut("gateway")
                .unwrap()
                .http_headers
                .remove(name);
        }
        declarations
            .get_mut("gateway")
            .unwrap()
            .http_headers
            .insert("x-route-group".into(), "duplicate".into());
        assert!(validate_declarations(&declarations).is_err());
        let headers = &mut declarations.get_mut("gateway").unwrap().http_headers;
        headers.remove("x-route-group");
        headers.insert("X-Test".into(), "injected\r\nAuthorization: secret".into());
        assert!(validate_declarations(&declarations).is_err());
    }
}

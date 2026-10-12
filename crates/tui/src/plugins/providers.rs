//! Provider adapter for the existing reviewed plugin manifest and route catalog.
//!
//! Declarations are data, never executable callbacks. Only OpenAI-compatible
//! inference and public OAuth PKCE clients are supported. Plugin changes require
//! a restart to add/remove routes; receipt checks revoke existing routes immediately.
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

use serde::{Deserialize, Serialize};

use super::{
    PluginRegistry, activation::PluginActivationCapability,
    registry::verify_plugin_component_authority,
};
use crate::config::{Config, ProviderConfig, ProviderKind};

static STARTUP_REGISTRY: OnceLock<Arc<PluginRegistry>> = OnceLock::new();

#[cfg(test)]
thread_local! {
    static TEST_REGISTRY: std::cell::RefCell<Option<Arc<PluginRegistry>>> = const { std::cell::RefCell::new(None) };
}

/// Restore the calling test's previous registry even if its operation panics.
#[cfg(test)]
fn with_test_registry<T>(registry: Arc<PluginRegistry>, operation: impl FnOnce() -> T) -> T {
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
    pub oauth: crate::oauth::PluginOAuthConfig,
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
    #[cfg(test)]
    if let Some(registry) = TEST_REGISTRY.with(|slot| slot.borrow().clone()) {
        return apply_providers(config, &registry);
    }
    if let Some(registry) = STARTUP_REGISTRY.get() {
        apply_providers(config, registry)?;
    }
    Ok(())
}

pub(crate) fn plugin_auth_entry(config: &Config, provider: &str) -> anyhow::Result<ProviderConfig> {
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
pub(crate) fn verify_provider_binding(
    authority: &super::types::PluginAuthority,
    name: &str,
    base_url: &str,
    oauth: &crate::oauth::PluginOAuthConfig,
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
        crate::config::normalize_base_url(base_url)
            == crate::config::normalize_base_url(&declaration.base_url),
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

pub(crate) fn is_account_catalog_scope(provider: &str, fingerprint: &str) -> bool {
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
    use crate::plugins::discovery::{DiscoveryConfig, discover_with_config};

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

    #[test]
    fn reviewed_provider_registration_preserves_collisions_and_revokes_existing_routes() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        let plugin = temp.path().join("plugins/provider-demo");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&plugin).unwrap();
        let mut reviewed_declaration = declaration();
        reviewed_declaration
            .http_headers
            .insert("X-Route-Group".into(), "reviewed-group".into());
        let manifest = serde_json::json!({
            "$schema": super::super::agent_plugin::PLUGIN_SCHEMA_URL,
            "name": "provider-demo", "version": "1.0.0",
            "extensions": {"net.codewhale": {"providers": {"gateway": reviewed_declaration}}}
        });
        std::fs::write(
            plugin.join("plugin.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let discovery = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: temp.path().join("plugins"),
            workspace_plugins_dir: workspace.join(".codewhale/plugins"),
            builtin_plugin_dirs: vec![],
            state_path: temp.path().join("state/plugins.json"),
        };
        let mut registry = discover_with_config(&discovery);
        let mut config = Config::default();
        apply_providers(&mut config, &registry).unwrap();
        assert!(
            config
                .providers
                .as_ref()
                .is_none_or(|providers| !providers.custom.contains_key("gateway"))
        );
        registry.trust("provider-demo").unwrap();
        registry.enable("provider-demo").unwrap();
        let registry = discover_with_config(&discovery);
        apply_providers(&mut config, &registry).unwrap();
        let entry = config
            .providers
            .as_ref()
            .unwrap()
            .custom
            .get("gateway")
            .unwrap();
        assert_eq!(entry.auth_mode.as_deref(), Some("oauth"));
        let authority = entry.plugin_authority.clone().unwrap();
        let oauth = entry.oauth.as_ref().unwrap();
        let headers = entry.http_headers.as_ref().unwrap();
        verify_provider_binding(
            &authority,
            "gateway",
            "https://gateway.example/api/",
            oauth,
            Some(headers),
        )
        .unwrap();
        assert!(
            verify_provider_binding(
                &authority,
                "gateway",
                "https://other.example/api",
                oauth,
                Some(headers)
            )
            .is_err()
        );
        let mut altered_oauth = oauth.clone();
        altered_oauth.scopes.push("admin:write".into());
        assert!(
            verify_provider_binding(
                &authority,
                "gateway",
                "https://gateway.example/api",
                &altered_oauth,
                Some(headers)
            )
            .is_err()
        );
        let mut altered_headers = headers.clone();
        altered_headers.insert("X-Route-Group".into(), "unreviewed-group".into());
        assert!(
            verify_provider_binding(
                &authority,
                "gateway",
                "https://gateway.example/api",
                oauth,
                Some(&altered_headers)
            )
            .is_err()
        );
        assert!(
            verify_provider_binding(
                &authority,
                "gateway",
                "https://gateway.example/api",
                oauth,
                Some(&std::collections::HashMap::new())
            )
            .is_err()
        );
        assert!(
            verify_provider_binding(
                &authority,
                "other-provider",
                "https://gateway.example/api",
                oauth,
                Some(headers)
            )
            .is_err()
        );

        assert!(
            config
                .custom_models
                .as_ref()
                .unwrap()
                .iter()
                .any(|model| model.provider == "gateway" && model.id == "tiny-model")
        );
        // Persist only the user's selection; the reviewed route stays in the
        // plugin and is reconstructed after parsing the next startup document.
        let selection_path = temp.path().join("selection.toml");
        let saved = with_test_registry(Arc::new(registry.clone()), || {
            // First use starts from an empty document, with no provider table.
            let identity = config.resolve_provider_pin_identity("gateway").unwrap();
            let mut doc = toml_edit::DocumentMut::new();
            crate::config_persistence::set_provider_model_document(
                &mut doc,
                &identity,
                "selected-model",
            )
            .unwrap();
            let model_only = doc.to_string();
            assert!(!model_only.contains("base_url") && !model_only.contains("oauth"));
            let mut parsed = crate::config::parse_config_base(&model_only).unwrap();
            apply_startup_providers(&mut parsed).unwrap();
            assert_eq!(
                parsed.providers.as_ref().unwrap().custom["gateway"]
                    .model
                    .as_deref(),
                Some("selected-model")
            );
            crate::config_persistence::persist_provider_selection(
                Some(&selection_path),
                &identity,
                Some("selected-model"),
            )
            .unwrap();
            std::fs::read_to_string(&selection_path).unwrap()
        });
        // The scoped registry is gone; reconstruct using the explicit fixture.
        assert!(TEST_REGISTRY.with(|slot| slot.borrow().is_none()));
        let mut restored = crate::config::parse_config_base(&saved).unwrap();
        apply_providers(&mut restored, &registry).unwrap();
        assert_eq!(restored.provider.as_deref(), Some("gateway"));
        let restored_entry = restored
            .providers
            .as_ref()
            .unwrap()
            .custom
            .get("gateway")
            .unwrap();
        assert_eq!(restored_entry.model.as_deref(), Some("selected-model"));
        assert_eq!(
            restored_entry.base_url.as_deref(),
            Some("https://gateway.example/api")
        );
        assert_eq!(restored_entry.auth_mode.as_deref(), Some("oauth"));
        assert_eq!(restored_entry.plugin_authority, Some(authority.clone()));
        assert!(!saved.contains("base_url") && !saved.contains("oauth"));
        for override_field in [
            "vendor = 'other'",
            "api_key = 'other'",
            "base_url = 'https://other.example'",
            "context_window = 1",
            "model_context_windows = { other = 1 }",
            "mode = 'other'",
            "wire = 'responses'",
            "auth_mode = 'none'",
            "oauth_credential_generation = 'other'",
            "insecure_skip_tls_verify = false",
            "allow_insecure_http = false",
            "http_headers = {}",
            "path_suffix = '/other'",
            "reasoning_stream_style = 'other'",
            "max_concurrency = 1",
            "kind = 'openai-compatible'",
            "api_key_env = 'OTHER_KEY'",
        ] {
            let mut collision = crate::config::parse_config_base(&format!(
                "[providers.gateway]\nmodel = 'tiny-model'\n{override_field}\n"
            ))
            .unwrap();
            assert!(
                apply_providers(&mut collision, &registry).is_err(),
                "accepted non-model route override {override_field}"
            );
        }
        let snapshot = config.providers.as_ref().unwrap().custom.len();
        assert!(apply_providers(&mut config, &registry).is_err());
        assert_eq!(config.providers.as_ref().unwrap().custom.len(), snapshot);
        let mut fresh = discover_with_config(&discovery);
        fresh.disable("provider-demo").unwrap();
        assert!(
            verify_plugin_component_authority(&authority, PluginActivationCapability::Providers)
                .is_err()
        );
    }
}

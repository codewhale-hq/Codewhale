use codewhale_plugins::activation::PluginActivationCapability;
use codewhale_plugins::discovery::{DiscoveryConfig, discover_with_config};
use codewhale_plugins::providers::*;
use codewhale_plugins::registry::verify_plugin_component_authority;
use codewhale_runtime::config::Config;
use std::sync::Arc;

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
        "$schema": codewhale_plugins::agent_plugin::PLUGIN_SCHEMA_URL,
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
        codewhale_engine::route_preferences::set_provider_model_document(
            &mut doc,
            &identity,
            "selected-model",
        )
        .unwrap();
        let model_only = doc.to_string();
        assert!(!model_only.contains("base_url") && !model_only.contains("oauth"));
        let mut parsed = codewhale_runtime::config::parse_config_base(&model_only).unwrap();
        apply_startup_providers(&mut parsed).unwrap();
        assert_eq!(
            parsed.providers.as_ref().unwrap().custom["gateway"]
                .model
                .as_deref(),
            Some("selected-model")
        );
        codewhale_engine::route_preferences::persist_provider_selection(
            Some(&selection_path),
            &identity,
            Some("selected-model"),
        )
        .unwrap();
        std::fs::read_to_string(&selection_path).unwrap()
    });
    // The scoped registry is gone; reconstruct using the explicit fixture.
    assert!(TEST_REGISTRY.with(|slot| slot.borrow().is_none()));
    let mut restored = codewhale_runtime::config::parse_config_base(&saved).unwrap();
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
        let mut collision = codewhale_runtime::config::parse_config_base(&format!(
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

fn declaration() -> PluginProviderDeclaration {
    serde_json::from_value(serde_json::json!({
        "base_url": "https://gateway.example/api",
        "model": "tiny-model", "models": ["tiny-model"],
        "oauth": {"issuer":"https://gateway.example", "authorization_endpoint":"https://gateway.example/authorize", "token_endpoint":"https://gateway.example/token", "client_id":"custom-cli", "scopes":["models:invoke"]}
    })).unwrap()
}

use std::time::Duration;

use codewhale_runtime::config::Config;

use codewhale_runtime::oauth::test_support::*;
use codewhale_runtime::oauth::*;

#[test]
fn plugin_oauth_reviewed_provider_real_client_refresh_chat_catalog_and_revocation() {
    use crate::client::CodewhaleClient;
    use crate::llm_client::LlmClient;
    use crate::plugins::discovery::{DiscoveryConfig, discover_with_config};
    use futures_util::StreamExt;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let _env = crate::test_support::lock_test_env();
    let temp = tempfile::tempdir().unwrap();
    let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", temp.path().join("owned"));
    let _backend = crate::test_support::EnvVarGuard::set("CODEWHALE_SECRET_BACKEND", "file");
    let root = temp.path().to_owned();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/token"))
                .and(body_string_contains("refresh_token=old-refresh"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"access_token":"rotated-access", "refresh_token":"rotated-refresh", "expires_in":3600, "token_type":"Bearer"})))
                .expect(1).mount(&server).await;
            Mock::given(method("GET")).and(path("/v1/models"))
                .and(header("authorization", "Bearer rotated-access"))
                .and(header("x-fixture-route", "main"))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data":[{"id":"fixture-model", "object":"model"}]})))
                .expect(1).mount(&server).await;
            Mock::given(method("POST")).and(path("/v1/chat/completions"))
                .and(header("authorization", "Bearer rotated-access"))
                .and(header("x-fixture-route", "main"))
                .and(|request: &wiremock::Request| serde_json::from_slice::<serde_json::Value>(&request.body).is_ok_and(|body| body.get("stream").is_none()))
                .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id":"fixture", "object":"chat.completion", "model":"fixture-model", "choices":[{"index":0,"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}], "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}})))
                .expect(1).mount(&server).await;
            Mock::given(method("POST")).and(path("/v1/chat/completions"))
                .and(header("authorization", "Bearer rotated-access"))
                .and(header("x-fixture-route", "main"))
                .and(body_string_contains("\"stream\":true"))
                .respond_with(ResponseTemplate::new(200).insert_header("content-type", "text/event-stream").set_body_string("data: {\"id\":\"fixture\",\"object\":\"chat.completion.chunk\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n"))
                .expect(1).mount(&server).await;
            let issuer = server.uri();
            let (live, discovery, slot) = tokio::task::spawn_blocking(move || {
                let workspace = root.join("workspace");
                let plugin = root.join("plugins/provider-fixture");
                std::fs::create_dir_all(&workspace).unwrap();
                std::fs::create_dir_all(&plugin).unwrap();
                let mut config = descriptor();
                config.issuer = issuer.clone();
                config.authorization_endpoint = format!("{issuer}/authorize");
                config.token_endpoint = format!("{issuer}/token");
                let declared_base_url = format!("{issuer}/v1/");
                let manifest = serde_json::json!({
                    "$schema": crate::plugins::agent_plugin::PLUGIN_SCHEMA_URL,
                    "name":"provider-fixture", "version":"1.0.0",
                    "extensions":{"net.codewhale":{"providers":{"fixture-gateway":{
                        "base_url":declared_base_url, "model":"fixture-model", "models":["fixture-model"], "http_headers":{"X-Fixture-Route":"main"}, "oauth":config
                    }}}}
                });
                std::fs::write(plugin.join("plugin.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
                let discovery = DiscoveryConfig { workspace:workspace.clone(), user_plugins_dir:root.join("plugins"), workspace_plugins_dir:workspace.join(".codewhale/plugins"), builtin_plugin_dirs:vec![], state_path:root.join("state/plugins.json") };
                let mut registry = discover_with_config(&discovery);
                registry.trust("provider-fixture").unwrap();
                registry.enable("provider-fixture").unwrap();
                let registry = discover_with_config(&discovery);
                let mut live = {
                    let __cw_config_init_0_0 = Some("fixture-gateway".into());
                    let __cw_config_init_0_1 = Some(std::collections::HashMap::from([
                        ("Authorization".into(), "Bearer ambient-global".into()),
                        ("X-Api-Key".into(), "ambient-global".into()),
                        ("Cookie".into(), "secret=ambient-global".into()),
                        ("X-Unreviewed".into(), "ambient-global".into()),
                    ]));
                    let mut __cw_config_init_0_value: Config = Config::default();
                    __cw_config_init_0_value.provider = __cw_config_init_0_0;
                    __cw_config_init_0_value.http_headers = __cw_config_init_0_1;
                    __cw_config_init_0_value
                };
                crate::plugins::providers::apply_providers(&mut live, &registry).unwrap();
                let auth_entry = crate::plugins::providers::plugin_auth_entry(
                    &live,
                    "fixture-gateway",
                )
                .unwrap();
                let base_url = auth_entry.base_url.unwrap();
                assert_eq!(base_url, format!("{issuer}/v1"));
                assert_eq!(base_url, live.base_url_for_route(&live.resolve_provider_pin_identity("fixture-gateway").unwrap()));
                let slot = plugin_oauth_slot("fixture-gateway", &base_url, &config).unwrap();
                let secrets = codewhale_secrets::Secrets::auto_detect();
                let expired = serde_json::json!({"access_token":"private-expired-token", "refresh_token":"old-refresh", "expires_at":1});
                secrets.set(&slot, &serde_json::to_string(&expired).unwrap()).unwrap();
                assert!(crate::provider_readiness::has_api_key(&live));
                assert!(crate::provider_readiness::has_api_key_for(&live, &live.active_provider_identity().unwrap()));
                (live, discovery, slot)
            }).await.unwrap();
            // Exercise the real async constructor with an already-expired
            // stored credential: only the actual request worker may refresh.
            let (generic_key, source) = live.active_route_api_key_with_source().unwrap();
            assert!(generic_key.is_empty());
            assert_eq!(source, "host-managed plugin OAuth");
            assert!(live.active_route_api_key_read_only().unwrap().is_empty());
            let diagnostic = live.with_read_only_api_key_for_diagnostic().unwrap();
            assert!(diagnostic.plugin_oauth_read_only);
            assert!(!live.plugin_oauth_read_only);
            let diagnostic_client = CodewhaleClient::new(&diagnostic).unwrap();
            assert!(diagnostic_client.list_models().await.is_err());
            assert!(server.received_requests().await.unwrap().is_empty());
            // Even a valid stored credential for an in-memory endpoint edit
            // cannot reuse the receipt for the originally reviewed declaration.
            let mut altered = live.clone();
            let altered_base = format!("{}/other", server.uri());
            let altered_entry = altered.providers.as_mut().unwrap().custom.get_mut("fixture-gateway").unwrap();
            altered_entry.base_url = Some(altered_base.clone());
            let altered_descriptor = altered_entry.oauth.clone().unwrap();
            tokio::task::spawn_blocking(move || {
                let altered_slot = plugin_oauth_slot("fixture-gateway", &altered_base, &altered_descriptor).unwrap();
                let credential = serde_json::json!({"access_token":"review-bypass-token", "refresh_token":null, "expires_at":(chrono::Utc::now().timestamp() as u64).saturating_add(3600)});
                codewhale_secrets::Secrets::auto_detect().set(&altered_slot, &serde_json::to_string(&credential).unwrap()).unwrap();
            }).await.unwrap();
            let altered_client = CodewhaleClient::new(&altered).unwrap();
            assert!(altered_client.list_models().await.is_err());
            assert!(server.received_requests().await.unwrap().is_empty());
            let client = CodewhaleClient::new(&live).unwrap();
            assert!(server.received_requests().await.unwrap().is_empty());
            let models = client.list_models().await.unwrap();
            assert!(models.iter().any(|model| model.id == "fixture-model"));
            let input = codewhale_models::MessageRequest {
                model:"fixture-model".into(), messages:vec![codewhale_models::Message { role:codewhale_models::Role::User, content:vec![codewhale_models::ContentBlock::Text { text:"hello".into(), cache_control:None }] }], max_tokens:8,
                system:None, tools:None, tool_choice:None, metadata:None, thinking:None, reasoning_effort:None, stream:Some(false), temperature:None, top_p:None,
            };
            client.create_message(input.clone()).await.unwrap();
            let mut stream_input = input.clone();
            stream_input.stream = Some(true);
            let mut stream = client.create_message_stream(stream_input).await.unwrap();
            let mut count = 0;
            while let Some(event) = stream.next().await { event.unwrap(); count += 1; }
            assert!(count > 0);
            let refresh_started = std::sync::Arc::new(tokio::sync::Notify::new());
            let notify_refresh = std::sync::Arc::clone(&refresh_started);
            Mock::given(method("POST")).and(path("/token"))
                .and(body_string_contains("refresh_token=rotated-refresh"))
                .respond_with(move |_request: &wiremock::Request| {
                    notify_refresh.notify_one();
                    ResponseTemplate::new(200)
                        .set_delay(Duration::from_secs(2))
                        .set_body_json(serde_json::json!({"access_token":"revoked-during-refresh", "refresh_token":"new-refresh", "expires_in":3600, "token_type":"Bearer"}))
                })
                .expect(1).mount(&server).await;
            let seed_slot = slot.clone();
            tokio::task::spawn_blocking(move || {
                let expired = serde_json::json!({"access_token":"rotated-access", "refresh_token":"rotated-refresh", "expires_at":1});
                codewhale_secrets::Secrets::auto_detect().set(&seed_slot, &serde_json::to_string(&expired).unwrap()).unwrap();
            }).await.unwrap();
            let before_refresh = server.received_requests().await.unwrap().len();
            let revoked_config = live.clone();
            let revoke = async move {
                refresh_started.notified().await;
                tokio::task::spawn_blocking(move || {
                let mut registry = discover_with_config(&discovery);
                registry.disable("provider-fixture").unwrap();
                assert!(!crate::provider_readiness::has_api_key_for(&revoked_config, &revoked_config.active_provider_identity().unwrap()));
                // Private material can remain in secure storage; receipt revocation
                // must still prevent reuse by an already constructed client.
                assert!(codewhale_secrets::Secrets::auto_detect().get(&slot).unwrap().is_some());
                }).await.unwrap();
            };
            let (during_refresh, ()) = tokio::join!(client.list_models(), revoke);
            assert!(during_refresh.is_err());
            // The already-authorized refresh may complete; the model request
            // must not leave the host after receipt revocation during that wait.
            assert_eq!(server.received_requests().await.unwrap().len(), before_refresh + 1);
            let before = server.received_requests().await.unwrap().len();
            assert!(client.list_models().await.is_err());
            // Two failures trigger the actual /models recovery probe. It must
            // obey the same review check and make no extra network request.
            for _ in 0..2 { assert!(client.create_message(input.clone()).await.is_err()); }
            assert_eq!(server.received_requests().await.unwrap().len(), before);
            for request in server.received_requests().await.unwrap() {
                for header in ["x-api-key", "cookie", "x-unreviewed"] {
                    assert!(!request.headers.contains_key(header));
                }
                assert!(request.headers.get("authorization").is_none_or(|value| value != "Bearer ambient-global"));
            }
            server.verify().await;
        });
}


    #[test]
    fn untethered_cross_protocol_rebound_fails_closed_without_config() {
        // #6320: the #5042 early-outs cover the already-exact route, but a
        // cross-protocol rebound with no Config to rebuild from still fails
        // closed — this is what still guards half-bound dispatch.
        let (_config, route) =
            deepseek_route_for_test("https://api.deepseek.com/beta", "deepseek-v4-pro");
        let client = CodewhaleClient::new(&route.config).expect("pro client resolves");
        assert_eq!(client.wire_format, WireFormat::ChatCompletions);
        let err = match client.rebound_for_model_protocol(None, "deepseek-v4-flash") {
            Ok(_) => panic!("cross-protocol rebound without config fails closed"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("no configuration is available"),
            "{err}"
        );
    }

    #[test]
    fn from_candidate_binds_custom_provider_base_url_and_model() {
        // #1519: a custom OpenAI-compatible provider resolves to a candidate
        // whose endpoint/model come from the named `[providers.<name>]` table,
        // and `from_candidate` must bind that verbatim base URL + wire model.
        let mut custom = std::collections::HashMap::new();
        custom.insert(
            "my_thing".to_string(),
            ProviderConfig {
                kind: Some("openai-compatible".to_string()),
                base_url: Some("https://api.example.com/v1".to_string()),
                model: Some("custom-model-v1".to_string()),
                api_key_env: Some("EXAMPLE_API_KEY_FROM_CANDIDATE_TEST".to_string()),
                ..Default::default()
            },
        );
        let config = {
            let __cw_config_init_0_0 = Some("my_thing".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                custom,
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };

        // The config names a custom provider, so it must resolve as Custom.
        assert_eq!(
            config.active_provider_identity().unwrap().provider,
            ProviderKind::Custom
        );

        let route = crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Custom, None)
            .expect("custom route should resolve");

        // Provide the key the route's auth path will read.
        let client = {
            let _env = crate::test_support::lock_test_env();
            let _key = crate::test_support::EnvVarGuard::set(
                "EXAMPLE_API_KEY_FROM_CANDIDATE_TEST",
                "sk-custom",
            );
            CodewhaleClient::from_candidate(&route.config, &route.candidate)
                .expect("client should construct from custom candidate")
        };

        assert_eq!(client.base_url, "https://api.example.com/v1");
        assert_eq!(client.default_model, "custom-model-v1");
        assert_eq!(client.api_provider, ProviderKind::Custom);
        // The candidate carried the custom endpoint + verbatim wire model.
        assert_eq!(
            route.candidate.endpoint().base_url,
            "https://api.example.com/v1"
        );
        assert_eq!(route.candidate.wire_model_id().as_str(), "custom-model-v1");
    }
    #[tokio::test]
    async fn incomplete_translation_keeps_exact_route_and_usage_before_rejection() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_partial",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "Parcial"}],
                // A provider-returned alias must not replace the admitted
                // route/model in the frozen cost receipt.
                "model": "provider-alias-after-dispatch",
                "stop_reason": "max_tokens",
                "stop_sequence": null,
                "usage": {"input_tokens": 7, "output_tokens": 2}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_anthropic_client(&server);
        let response = client
            .translate_with_usage("Hello", "deepseek-chat", "Spanish")
            .await
            .expect("decoded provider response retains its receipt");

        assert!(
            response.translated.is_err(),
            "partial text must be rejected"
        );
        let usage = response.usage.expect("provider-reported usage");
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.output_tokens, 2);
        assert_eq!(response.route.provider, ProviderKind::DeepseekAnthropic);
        assert_eq!(response.route.model, "deepseek-chat");
        assert_eq!(response.route.provider_identity, "deepseek-anthropic");
        assert!(response.route.endpoint_fingerprint.is_some());
    }

    #[tokio::test]
    async fn chat_translation_without_usage_keeps_unreceipted_success_outcome() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-no-usage",
                "model": "deepseek-chat",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "Hola"},
                    "finish_reason": "stop"
                }]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client("https://api.deepseek.com/v1", server.uri());
        let response = client
            .translate_with_usage("Hello", "deepseek-chat", "Spanish")
            .await
            .expect("provider success must retain its frozen route");
        assert_eq!(
            response
                .translated
                .expect("useful output remains deliverable"),
            "Hola"
        );
        assert_eq!(response.usage, None, "must not mint a priced-zero receipt");
        assert_eq!(response.route.provider, ProviderKind::Deepseek);
        assert_eq!(
            response.route.model,
            wire_model_for_provider_route(
                ProviderKind::Deepseek,
                "https://api.deepseek.com/v1",
                "deepseek-chat"
            )
        );
    }

    #[tokio::test]
    async fn chat_translation_http_error_is_not_a_provider_success_outcome() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(429).set_body_json(json!({
                "error": {"message": "rate limited"}
            })))
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client("https://api.deepseek.com/v1", server.uri());
        let error = match client
            .translate_with_usage("Hello", "deepseek-chat", "Spanish")
            .await
        {
            Ok(_) => panic!("HTTP failure must not become a provider-success receipt"),
            Err(error) => error,
        };
        let display = error.to_string();
        assert!(
            display.to_ascii_lowercase().contains("rate limit"),
            "{display}"
        );
        assert!(!display.contains("chatcmpl"), "{display}");
    }

    #[tokio::test]
    async fn custom_catalog_schema_ignores_table_name() {
        // The same enriched body served from a non-Baseten endpoint takes
        // the generic branch for every identity: the table name selects
        // ownership, never the wire parser (#6289). Baseten enrichment
        // itself stays covered at the parser's fixture tests.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .and(header("authorization", "Bearer test-custom-key"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": [{
                    "id": "synthetic/custom-model",
                    "context_length": 1_048_576,
                    "pricing": {
                        "prompt": 0.0000014,
                        "completion": 0.0000044
                    },
                    "supported_features": ["reasoning", "tools", "structured_outputs", "vision"]
                }]
            })))
            .mount(&server)
            .await;

        for identity in ["baseten", "renamed-host"] {
            let client = custom_mock_client_for_identity(&server, identity);
            assert_eq!(client.catalog_provider_id(), identity);
            let delta = client.fetch_catalog_delta().await.expect("delta");
            assert_eq!(delta.provider, identity);
            assert_eq!(delta.offerings.len(), 1);
            let offering = &delta.offerings[0];
            assert_eq!(offering.wire_model_id, "synthetic/custom-model");
            assert_eq!(offering.provider, identity);
            // The generic branch reads the ceilings a row states and nothing
            // else: no price, no capability claim. `context_length` is a flat
            // field every OpenAI-shaped listing may carry, so it survives; the
            // Baseten enrichment (pricing, `supported_features`) does not.
            let limit = offering.limit.as_ref().expect("stated context_length");
            assert_eq!(limit.context, Some(1_048_576));
            assert_eq!(limit.input, None);
            assert_eq!(limit.output, None);
            assert!(
                offering.cost.is_none() && offering.tool_call.is_none(),
                "a non-Baseten endpoint takes the generic branch for any identity: {offering:?}"
            );
        }
    }

    #[test]
    fn baseten_dialect_recognized_by_endpoint_not_name() {
        fn client_for(identity: &str, base_url: &str) -> CodewhaleClient {
            let mut providers = ProvidersConfig::default();
            providers.custom.insert(
                identity.to_string(),
                ProviderConfig {
                    kind: Some("openai-compatible".to_string()),
                    api_key: Some("test-key".to_string()),
                    base_url: Some(base_url.to_string()),
                    model: Some("synthetic/custom-model".to_string()),
                    ..ProviderConfig::default()
                },
            );
            CodewhaleClient::new(&{
                let __cw_config_init_1_0 = Some(identity.to_string());
                let __cw_config_init_1_1 = Some(providers);
                let mut __cw_config_init_1_value: Config = Config::default();
                __cw_config_init_1_value.provider = __cw_config_init_1_0;
                __cw_config_init_1_value.providers = __cw_config_init_1_1;
                __cw_config_init_1_value
            })
            .expect("client")
        }

        let baseten_url = codewhale_config::catalog::BASETEN_BASE_URL;
        assert!(client_for("baseten", baseten_url).catalog_endpoint_is_baseten());
        assert!(client_for("renamed-host", baseten_url).catalog_endpoint_is_baseten());
        assert!(
            client_for("baseten", &format!("{baseten_url}/")).catalog_endpoint_is_baseten(),
            "a trailing slash still recognizes the host"
        );
        assert!(!client_for("baseten", "https://127.0.0.1:9/v1").catalog_endpoint_is_baseten());
        assert!(!client_for("groq", "https://api.groq.com/openai/v1").catalog_endpoint_is_baseten());
    }

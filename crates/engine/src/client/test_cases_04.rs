
    #[test]
    fn default_headers_scenario() {
        // Scenario consolidation of: default_headers_include_custom_headers_when_configured, default_headers_ignore_blank_custom_headers
        // from default_headers_include_custom_headers_when_configured
        {
            let mut extra = HashMap::new();
            extra.insert("X-Model-Provider-Id".to_string(), "tongyi".to_string());
            let headers = CodewhaleClient::default_headers("sk-test", &extra).expect("headers");
            assert_eq!(
                headers
                    .get("x-model-provider-id")
                    .and_then(|value| value.to_str().ok()),
                Some("tongyi")
            );
        }
        // from default_headers_ignore_blank_custom_headers
        {
            let mut extra = HashMap::new();
            extra.insert("X-Blank".to_string(), "   ".to_string());
            let headers = CodewhaleClient::default_headers("sk-test", &extra).expect("headers");
            assert!(headers.get("x-blank").is_none());
        }
    }

    #[test]
    fn disabled_auth_strips_every_auth_header_dialect_at_client_sink() {
        let mut extra = HashMap::new();
        extra.insert(
            "aUtHoRiZaTiOn".to_string(),
            "Bearer configured-secret".to_string(),
        );
        extra.insert("X-API-Key".to_string(), "configured-x-key".to_string());
        extra.insert("Api-Key".to_string(), "configured-key".to_string());
        extra.insert(
            "Proxy-Authorization".to_string(),
            "Basic configured-proxy-secret".to_string(),
        );
        extra.insert(
            "X-Auth-Token".to_string(),
            "configured-auth-token".to_string(),
        );
        extra.insert(
            "X-Access-Token".to_string(),
            "configured-access-token".to_string(),
        );
        extra.insert(
            "X-Goog-Api-Key".to_string(),
            "configured-google-key".to_string(),
        );
        extra.insert("Cookie".to_string(), "session=secret".to_string());
        extra.insert("X-Route-Metadata".to_string(), "safe".to_string());

        let headers = CodewhaleClient::default_headers_for_provider_with_auth_disabled(
            "generated-secret",
            &extra,
            ProviderKind::Deepseek,
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
        )
        .expect("headers");

        for name in [
            "authorization",
            "x-api-key",
            "api-key",
            "proxy-authorization",
            "x-auth-token",
            "x-access-token",
            "x-goog-api-key",
            "cookie",
        ] {
            assert!(headers.get(name).is_none(), "disabled auth leaked {name}");
        }
        assert_eq!(
            headers
                .get("x-route-metadata")
                .and_then(|value| value.to_str().ok()),
            Some("safe")
        );
    }

    #[test]
    fn build_http_client_accepts_default_tls_verification() {
        let client = CodewhaleClient::build_http_client(
            "sk-test",
            &HashMap::new(),
            ProviderKind::Deepseek,
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_BASE_URL,
        );

        assert!(client.is_ok());
    }

    #[test]
    fn client_new_rejects_provider_scoped_tls_skip_verify() {
        let mut providers = codewhale_runtime::config::ProvidersConfig::default();
        providers.openai.api_key = Some("sk-test".to_string());
        providers.openai.base_url = Some(codewhale_config::descriptors::defaults::DEFAULT_OPENAI_BASE_URL.to_string());
        providers.openai.insecure_skip_tls_verify = Some(true);
        let config = {
            let __cw_config_init_0_0 = Some("openai".to_string());
            let __cw_config_init_0_1 = Some(providers);
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        assert!(config.insecure_skip_tls_verify());

        let err = match CodewhaleClient::new(&config) {
            Ok(_) => panic!("tls skip verify should be rejected"),
            Err(err) => err,
        };
        let message = err.to_string();
        assert!(message.contains("cannot be disabled"));
        assert!(message.contains("SSL_CERT_FILE"));
    }

    #[test]
    fn client_stream_idle_timeout_uses_tui_config() {
        let client = CodewhaleClient::new(
            &{
                let __cw_config_init_1_0 = Some(codewhale_runtime::config::TuiConfig {
                    stream_chunk_timeout_secs: Some(777),
                    max_model_steps: None,
                    turn_wall_clock_secs: None,
                    stream_max_content_mb: None,
                    stream_max_duration_secs: None,
                    ..codewhale_runtime::config::TuiConfig::default()
                });
                let mut __cw_config_init_1_value: Config = Config::default();
                __cw_config_init_1_value.tui = __cw_config_init_1_0;
                __cw_config_init_1_value
            }
            .with_legacy_root(Some("sk-test".to_string()), None),
        )
        .expect("client");

        assert_eq!(client.stream_idle_timeout, Duration::from_secs(777));
    }

    #[test]
    fn xiaomi_mimo_scenario() {
        // Scenario consolidation of: xiaomi_mimo_token_plan_endpoint_uses_api_key_header, xiaomi_mimo_tp_key_uses_api_key_header_with_custom_base_url, xiaomi_mimo_pay_as_you_go_endpoint_keeps_bearer_header
        // from xiaomi_mimo_token_plan_endpoint_uses_api_key_header
        {
            let headers = CodewhaleClient::default_headers_for_provider(
                "tp-test",
                &HashMap::new(),
                ProviderKind::XiaomiMimo,
                codewhale_config::descriptors::defaults::DEFAULT_XIAOMI_MIMO_BASE_URL,
            )
            .expect("headers");

            assert_eq!(
                headers.get("api-key").and_then(|value| value.to_str().ok()),
                Some("tp-test")
            );
            assert!(
                headers.get(AUTHORIZATION).is_none(),
                "Token Plan requires api-key instead of Authorization Bearer"
            );
        }
        // from xiaomi_mimo_tp_key_uses_api_key_header_with_custom_base_url
        {
            let mut extra = HashMap::new();
            extra.insert("api-key".to_string(), "wrong".to_string());
            extra.insert("Authorization".to_string(), "Bearer wrong".to_string());
            let headers = CodewhaleClient::default_headers_for_provider(
                "tp-custom",
                &extra,
                ProviderKind::XiaomiMimo,
                "https://proxy.example.test/mimo/v1",
            )
            .expect("headers");

            assert_eq!(
                headers.get("api-key").and_then(|value| value.to_str().ok()),
                Some("tp-custom")
            );
            assert!(
                headers.get(AUTHORIZATION).is_none(),
                "tp-* Token Plan keys should use api-key auth even through custom gateways"
            );
        }
        // from xiaomi_mimo_pay_as_you_go_endpoint_keeps_bearer_header
        {
            let headers = CodewhaleClient::default_headers_for_provider(
                "sk-test",
                &HashMap::new(),
                ProviderKind::XiaomiMimo,
                codewhale_runtime::config::XIAOMI_MIMO_PAY_AS_YOU_GO_BASE_URL,
            )
            .expect("headers");

            assert_eq!(
                headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer sk-test")
            );
            assert!(headers.get("api-key").is_none());
        }
    }

    #[test]
    fn openrouter_uses_bearer_header_after_mimo_token_plan_context() {
        let mut extra = HashMap::new();
        extra.insert("api-key".to_string(), "wrong".to_string());
        let headers = CodewhaleClient::default_headers_for_provider(
            "sk-or-test",
            &extra,
            ProviderKind::Openrouter,
            codewhale_config::descriptors::defaults::DEFAULT_OPENROUTER_BASE_URL,
        )
        .expect("headers");

        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer sk-or-test")
        );
        assert!(
            headers.get("api-key").is_none(),
            "OpenRouter must not inherit Xiaomi MiMo's api-key header dialect"
        );
    }

    #[test]
    fn siliconflow_cn_uses_bearer_header_and_pins_content_type() {
        let mut extra = HashMap::new();
        extra.insert("Authorization".to_string(), "Bearer wrong".to_string());
        extra.insert("Content-Type".to_string(), "text/plain".to_string());
        let headers = CodewhaleClient::default_headers_for_provider(
            "sf-cn-test",
            &extra,
            ProviderKind::SiliconflowCN,
            codewhale_config::descriptors::defaults::DEFAULT_SILICONFLOW_CN_BASE_URL,
        )
        .expect("headers");

        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer sf-cn-test")
        );
        assert_eq!(
            headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert!(headers.get("api-key").is_none());
    }

    #[test]
    fn opencode_go_and_zen_requests_carry_stable_session_header() {
        for api_provider in [ProviderKind::OpencodeGo, ProviderKind::OpencodeZen] {
            let headers = CodewhaleClient::default_headers_for_provider(
                "configured-key",
                &HashMap::new(),
                api_provider,
                "https://opencode.ai/zen/go/v1",
            )
            .expect("headers");
            let session = headers
                .get("x-opencode-session")
                .expect("x-opencode-session must be present for OpenCode gateways")
                .to_str()
                .expect("session id must be valid utf-8");
            assert!(!session.is_empty(), "session id must be non-empty");

            // The gateway requires one stable ID per conversation: a second
            // request from the same process must reuse the same value.
            let headers2 = CodewhaleClient::default_headers_for_provider(
                "configured-key",
                &HashMap::new(),
                api_provider,
                "https://opencode.ai/zen/go/v1",
            )
            .expect("headers2");
            assert_eq!(
                headers2
                    .get("x-opencode-session")
                    .and_then(|value| value.to_str().ok()),
                Some(session),
                "session id must be stable within a process"
            );
        }
    }

    #[test]
    fn user_configured_opencode_session_header_wins() {
        let mut extra = HashMap::new();
        extra.insert(
            "x-opencode-session".to_string(),
            "user-configured-id".to_string(),
        );
        let headers = CodewhaleClient::default_headers_for_provider(
            "configured-key",
            &extra,
            ProviderKind::OpencodeGo,
            "https://opencode.ai/zen/go/v1",
        )
        .expect("headers");
        assert_eq!(
            headers
                .get("x-opencode-session")
                .and_then(|value| value.to_str().ok()),
            Some("user-configured-id"),
            "a user-configured x-opencode-session must override the default"
        );
    }

    #[test]
    fn non_opencode_providers_do_not_carry_session_header() {
        for api_provider in [
            ProviderKind::Deepseek,
            ProviderKind::Anthropic,
            ProviderKind::Openai,
        ] {
            let headers = CodewhaleClient::default_headers_for_provider(
                "configured-key",
                &HashMap::new(),
                api_provider,
                "https://example.invalid/v1",
            )
            .expect("headers");
            assert!(
                headers.get("x-opencode-session").is_none(),
                "non-OpenCode provider {api_provider:?} must not send the session header"
            );
        }
    }

    #[test]
    fn tokenhub_openai_compatible_route_uses_bearer_header() {
        let mut extra = HashMap::new();
        extra.insert("api-key".to_string(), "wrong".to_string());
        extra.insert("x-api-key".to_string(), "wrong".to_string());
        let headers = CodewhaleClient::default_headers_for_provider(
            "tokenhub-test",
            &extra,
            ProviderKind::Openai,
            "https://tokenhub.tencentmaas.com/v1",
        )
        .expect("headers");

        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer tokenhub-test")
        );
        assert!(headers.get("api-key").is_none());
        assert!(headers.get("x-api-key").is_none());
    }

    #[test]
    fn codewhale_authenticates_every_protocol_with_bearer_never_x_api_key() {
        // The Codewhale API is a passthrough: it authenticates the account key
        // with `Authorization: Bearer` on the Anthropic Messages route too, so
        // the usual Messages `x-api-key` default must not apply here.
        for wire in [
            WireFormat::ChatCompletions,
            WireFormat::AnthropicMessages,
            WireFormat::Responses,
        ] {
            let headers = build_default_headers(
                "cwc_key_test",
                &HashMap::new(),
                ProviderKind::Codewhale,
                "https://api.codewhale.net/v1",
                wire,
                false,
                AuthHeaderSurface::Inference,
            )
            .expect("headers");
            assert_eq!(
                headers
                    .get(AUTHORIZATION)
                    .and_then(|value| value.to_str().ok()),
                Some("Bearer cwc_key_test"),
                "{wire:?}"
            );
            assert!(headers.get("x-api-key").is_none(), "{wire:?}");
        }
    }

    #[test]
    fn deepseek_anthropic_uses_anthropic_header_dialect() {
        let mut extra = HashMap::new();
        extra.insert("Authorization".to_string(), "Bearer wrong".to_string());
        extra.insert("api-key".to_string(), "wrong".to_string());
        let headers = CodewhaleClient::default_headers_for_provider(
            "ds-test",
            &extra,
            ProviderKind::DeepseekAnthropic,
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_ANTHROPIC_BASE_URL,
        )
        .expect("headers");

        assert_eq!(
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("ds-test")
        );
        assert_eq!(
            headers
                .get("anthropic-version")
                .and_then(|value| value.to_str().ok()),
            Some("2023-06-01")
        );
        assert!(
            headers.get(AUTHORIZATION).is_none(),
            "Anthropic-compatible DeepSeek route must not use Bearer auth"
        );
        assert!(
            headers.get("api-key").is_none(),
            "Anthropic-compatible DeepSeek route must not inherit MiMo auth headers"
        );
    }

    #[test]
    fn minimax_anthropic_uses_anthropic_header_dialect() {
        let headers = CodewhaleClient::default_headers_for_provider(
            "minimax-test",
            &HashMap::new(),
            ProviderKind::MinimaxAnthropic,
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_ANTHROPIC_BASE_URL,
        )
        .expect("headers");

        assert_eq!(
            headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("minimax-test")
        );
        assert_eq!(
            headers
                .get("anthropic-version")
                .and_then(|value| value.to_str().ok()),
            Some("2023-06-01")
        );
        assert!(headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn openmodel_uses_bearer_auth_with_anthropic_version() {
        let mut extra = HashMap::new();
        extra.insert("Authorization".to_string(), "Bearer wrong".to_string());
        extra.insert("api-key".to_string(), "wrong".to_string());
        extra.insert("x-api-key".to_string(), "wrong".to_string());
        let headers = CodewhaleClient::default_headers_for_provider(
            "om-test",
            &extra,
            ProviderKind::Openmodel,
            codewhale_config::descriptors::defaults::DEFAULT_OPENMODEL_BASE_URL,
        )
        .expect("headers");

        assert_eq!(
            headers
                .get(AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer om-test")
        );
        assert_eq!(
            headers
                .get("anthropic-version")
                .and_then(|value| value.to_str().ok()),
            Some("2023-06-01")
        );
        assert!(
            headers.get("x-api-key").is_none(),
            "OpenModel uses Bearer auth so /v1/models and /v1/messages share one client"
        );
        assert!(
            headers.get("api-key").is_none(),
            "OpenModel Messages route must not inherit MiMo auth headers"
        );
    }

    #[tokio::test]
    async fn deepseek_anthropic_translate_uses_messages_endpoint() {
        // `translate` resolves `max_tokens` when building the request and this
        // test recomputes the same route allowance when asserting. That value
        // reads `CODEWHALE_MAX_OUTPUT_TOKENS`/`DEEPSEEK_MAX_OUTPUT_TOKENS` from
        // the process environment, so a concurrent test that redirects either
        // variable between the two reads flips one side and fails the
        // assertion (#5929). Hold the test env barrier for the whole request
        // so both reads observe one stable environment.
        let _env_lock = crate::test_support::lock_test_env();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "Hola"}],
                "model": "deepseek-chat",
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 3, "output_tokens": 1}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_anthropic_client(&server);
        let translated = client
            .translate("Hello", "deepseek-chat", "Spanish")
            .await
            .expect("translation succeeds");

        assert_eq!(translated, "Hola");
        let requests = server.received_requests().await.expect("recorded requests");
        assert_eq!(requests.len(), 1);
        let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("deepseek-chat"),
            "custom Messages endpoints own their model ids: {body}"
        );
        assert_eq!(
            body.pointer("/messages/0/role").and_then(Value::as_str),
            Some("user")
        );
        assert_eq!(
            body.pointer("/messages/0/content/0/text")
                .and_then(Value::as_str),
            Some("Hello")
        );
        assert!(
            body.get("thinking").is_none(),
            "translation disables thinking: {body}"
        );
        assert!(
            body.get("temperature").is_none() && body.get("top_p").is_none(),
            "translation must not inject sampling controls: {body}"
        );
        assert_eq!(
            body.get("max_tokens").and_then(Value::as_u64),
            Some(u64::from(
                crate::route_budget::effective_max_output_tokens_for_route(
                    ProviderKind::DeepseekAnthropic,
                    "deepseek-chat",
                    None,
                )
            )),
            "translation must inherit its resolved route allowance: {body}"
        );
        assert!(
            body.get("system")
                .and_then(Value::as_str)
                .is_some_and(|system| system.contains("Spanish")),
            "target language should be in system prompt: {body}"
        );
    }

    #[tokio::test]
    async fn deepseek_anthropic_scenario() {
        // Scenario consolidation of: deepseek_anthropic_health_check_skips_models_probe, deepseek_anthropic_fim_fails_without_http_request
        // from deepseek_anthropic_health_check_skips_models_probe
        {
            let server = MockServer::start().await;
            let client = deepseek_anthropic_client(&server);

            assert!(client.health_check().await.expect("health check"));
            assert!(!provider_api_key_verification_is_observed(
                ProviderKind::DeepseekAnthropic
            ));
            let requests = server.received_requests().await.expect("recorded requests");
            assert!(
                requests.is_empty(),
                "DeepSeek Anthropic-compatible route must not probe /models"
            );
        }
        // from deepseek_anthropic_fim_fails_without_http_request
        {
            let server = MockServer::start().await;
            let client = deepseek_anthropic_client(&server);

            let err = client
                .fim_completion("deepseek-chat", "fn main() {", "}", 16)
                .await
                .expect_err("FIM is unsupported");
            let message = err.to_string();
            assert!(
                message.contains("FIM completion is not supported"),
                "{message}"
            );
            assert!(message.contains("no proven FIM wire contract"), "{message}");
            let requests = server.received_requests().await.expect("recorded requests");
            assert!(
                requests.is_empty(),
                "unsupported FIM should fail locally before any HTTP call"
            );
        }
    }

    #[tokio::test]
    async fn minimax_anthropic_health_check_uses_models_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/anthropic/v1/models"))
            .and(header("x-api-key", "minimax-test"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": []})))
            .expect(1)
            .mount(&server)
            .await;
        let client = minimax_anthropic_client_with_base_url(format!("{}/anthropic", server.uri()));

        assert!(client.health_check().await.expect("health check"));
    }

    #[tokio::test]
    async fn minimax_anthropic_request_uses_messages_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/anthropic/v1/messages"))
            .and(header("x-api-key", "minimax-test"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "content": [{"type": "text", "text": "ok"}],
                "model": "MiniMax-M3",
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {"input_tokens": 3, "output_tokens": 1}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let mut client = minimax_anthropic_client_with_base_url(
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_ANTHROPIC_BASE_URL.to_string(),
        );
        client.test_messages_transport_base_url = Some(format!("{}/anthropic", server.uri()));
        let response = client
            .create_message(MessageRequest {
                model: "MiniMax-M3".to_string(),
                messages: vec![Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "hello".to_string(),
                        cache_control: None,
                    }],
                }],
                max_tokens: 32,
                system: None,
                tools: None,
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: Some("off".to_string()),
                stream: Some(false),
                temperature: None,
                top_p: None,
            })
            .await
            .expect("message succeeds");

        assert_eq!(response.content.len(), 1);
        let requests = server.received_requests().await.expect("recorded requests");
        let body: Value = serde_json::from_slice(&requests[0].body).expect("request JSON");
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("disabled")
        );
        assert!(body.get("output_config").is_none(), "{body}");
    }

    #[test]
    fn custom_api_key_header_is_allowed_without_primary_provider_key() {
        let mut extra = HashMap::new();
        extra.insert("api-key".to_string(), "gateway-key".to_string());
        let headers = CodewhaleClient::default_headers_for_provider(
            "",
            &extra,
            ProviderKind::Openai,
            "https://gateway.example.test/v1",
        )
        .expect("headers");

        assert_eq!(
            headers.get("api-key").and_then(|value| value.to_str().ok()),
            Some("gateway-key")
        );
        assert!(headers.get(AUTHORIZATION).is_none());
    }

    #[test]
    fn chat_messages_keep_current_turn_reasoning_content() {
        let message = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    signature: None,
                    state: None,
                    redacted_data: None,
                    thinking: "plan".to_string(),
                },
                ContentBlock::Text {
                    text: "done".to_string(),
                    cache_control: None,
                },
            ],
        };
        let out = build_chat_messages(None, &[message], "deepseek-v4-pro");
        let assistant = out
            .iter()
            .find(|value| value.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("assistant message");
        assert_eq!(
            assistant.get("content").and_then(Value::as_str),
            Some("done")
        );
        assert_eq!(
            assistant.get("reasoning_content").and_then(Value::as_str),
            Some("plan"),
            "thinking-mode models keep reasoning_content while still in the current turn"
        );
    }

    #[test]
    fn generic_openai_provider_drops_reasoning_content_for_non_deepseek_models() {
        // #1542 intent (narrowed by #1739/#1694): a *genuine non-DeepSeek*
        // model on the generic openai provider must not carry DeepSeek-only
        // `reasoning_content`. A DeepSeek reasoning model on the openai
        // provider (DeepSeek-compatible endpoint) is now covered separately
        // and DOES replay reasoning_content — see
        // `deepseek_model_on_openai_provider_still_replays_reasoning_content`.
        let request = MessageRequest {
            model: "qwen3-coder".to_string(),
            messages: vec![Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        signature: None,
                        state: None,
                        redacted_data: None,
                        thinking: "plan".to_string(),
                    },
                    ContentBlock::Text {
                        text: "done".to_string(),
                        cache_control: None,
                    },
                ],
            }],
            max_tokens: 16,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("max".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        };

        let openai = build_chat_messages_for_request_and_provider(&request, ProviderKind::Openai);
        let generic_assistant = openai
            .iter()
            .find(|value| value.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("assistant message");
        assert_eq!(
            generic_assistant.get("content").and_then(Value::as_str),
            Some("done")
        );
        assert!(
            generic_assistant.get("reasoning_content").is_none(),
            "generic OpenAI-compatible providers reject DeepSeek-only reasoning_content (#1542)"
        );
    }

    #[test]
    fn chat_messages_replay_tool_round_reasoning_before_new_user_turn() {
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Need the date".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        signature: None,
                        state: None,
                        redacted_data: None,
                        thinking: "Need to call a tool".to_string(),
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "tool-1".to_string(),
                        name: "get_date".to_string(),
                        input: json!({}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "tool-1".to_string(),
                    content: "2026-04-23".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];
        let out = build_chat_messages(None, &messages, "deepseek-v4-pro");
        let tool_assistant = out
            .iter()
            .find(|value| {
                value.get("role").and_then(Value::as_str) == Some("assistant")
                    && value.get("tool_calls").is_some()
            })
            .expect("tool-call assistant message");
        assert_eq!(
            tool_assistant
                .get("reasoning_content")
                .and_then(Value::as_str),
            Some("Need to call a tool"),
            "thinking-mode tool sub-turns must replay reasoning_content until the tool chain finishes"
        );
    }

    #[test]
    fn chat_messages_replay_prior_tool_round_reasoning_after_new_user_turn() {
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Need the date".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        signature: None,
                        state: None,
                        redacted_data: None,
                        thinking: "Need to call a tool".to_string(),
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "tool-1".to_string(),
                        name: "get_date".to_string(),
                        input: json!({}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "tool-1".to_string(),
                    content: "2026-04-23".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "It is 2026-04-23.".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Thanks. Next question.".to_string(),
                    cache_control: None,
                }],
            },
        ];
        let out = build_chat_messages(None, &messages, "deepseek-v4-pro");
        let tool_assistant = out
            .iter()
            .find(|value| {
                value.get("role").and_then(Value::as_str) == Some("assistant")
                    && value.get("tool_calls").is_some()
            })
            .expect("tool-call assistant message");
        assert_eq!(
            tool_assistant
                .get("reasoning_content")
                .and_then(Value::as_str),
            Some("Need to call a tool"),
            "tool-call reasoning_content must be replayed across later user turns"
        );
    }

    #[test]
    fn chat_messages_keep_prior_non_tool_reasoning_after_new_user_turn() {
        // The serialized JSON for a stored assistant message MUST be a pure
        // function of that message — never of what comes after it. DeepSeek's
        // prompt cache hashes the leading bytes of every request; flipping
        // `reasoning_content` on/off across turns rewrites historical bytes
        // and busts the prefix cache from that message onwards. (#583)
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Explain it".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        signature: None,
                        state: None,
                        redacted_data: None,
                        thinking: "Internal explanation plan".to_string(),
                    },
                    ContentBlock::Text {
                        text: "Final answer".to_string(),
                        cache_control: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Next question".to_string(),
                    cache_control: None,
                }],
            },
        ];

        let out = build_chat_messages(None, &messages, "deepseek-v4-pro");
        let assistant = out
            .iter()
            .find(|value| value.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("assistant message");

        assert_eq!(
            assistant.get("content").and_then(Value::as_str),
            Some("Final answer")
        );
        assert_eq!(
            assistant.get("reasoning_content").and_then(Value::as_str),
            Some("Internal explanation plan"),
            "reasoning_content must be preserved across follow-up user turns to keep DeepSeek's prefix cache warm"
        );
    }

    #[test]
    fn chat_messages_assistant_json_is_byte_stable_across_follow_up_user_turn() {
        // Direct prefix-cache regression: the JSON for the assistant message
        // built on turn N must equal the JSON for the same assistant message
        // built on turn N+1, after a new user message has been appended.
        let assistant = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    signature: None,
                    state: None,
                    redacted_data: None,
                    thinking: "I should explain step by step.".to_string(),
                },
                ContentBlock::Text {
                    text: "Here is the explanation.".to_string(),
                    cache_control: None,
                },
            ],
        };
        let user_initial = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Explain it".to_string(),
                cache_control: None,
            }],
        };
        let user_follow_up = Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "Next question".to_string(),
                cache_control: None,
            }],
        };

        let turn_n = build_chat_messages(
            None,
            &[user_initial.clone(), assistant.clone()],
            "deepseek-v4-pro",
        );
        let turn_n_plus_1 = build_chat_messages(
            None,
            &[user_initial, assistant, user_follow_up],
            "deepseek-v4-pro",
        );

        let assistant_n = turn_n
            .iter()
            .find(|v| v.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("assistant present in turn N");
        let assistant_n1 = turn_n_plus_1
            .iter()
            .find(|v| v.get("role").and_then(Value::as_str) == Some("assistant"))
            .expect("assistant present in turn N+1");

        assert_eq!(
            assistant_n, assistant_n1,
            "assistant message JSON must be byte-identical across turns or DeepSeek's prefix cache breaks"
        );
    }

    #[test]
    fn chat_messages_allow_tool_round_without_reasoning_when_thinking_disabled() {
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        execution_id: None,
                        id: "call-no-thinking".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "Cargo.toml"}),
                        caller: None,
                        thought_signature: None,
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        execution_id: None,
                        tool_use_id: "call-no-thinking".to_string(),
                        content: "workspace manifest".to_string(),
                        is_error: None,
                        content_blocks: None,
                    }],
                },
            ],
            max_tokens: 1024,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("off".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        };

        let out = build_chat_messages_for_request(&request);
        assert!(
            out.iter().any(
                |value| value.get("role").and_then(Value::as_str) == Some("assistant")
                    && value.get("tool_calls").is_some()
            ),
            "tool calls remain valid when thinking mode is disabled"
        );
        assert!(
            out.iter()
                .any(|value| value.get("role").and_then(Value::as_str) == Some("tool")),
            "matching tool result should remain"
        );
    }

    #[test]
    fn prompt_builder_keeps_system_first_and_current_user_input_last() {
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "Previous answer".to_string(),
                        cache_control: None,
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![
                        ContentBlock::Text {
                            text: "<turn_meta>\nCurrent local date: 2026-05-08\n</turn_meta>"
                                .to_string(),
                            cache_control: None,
                        },
                        ContentBlock::Text {
                            text: "Current user question".to_string(),
                            cache_control: None,
                        },
                    ],
                },
            ],
            max_tokens: 1024,
            system: Some(SystemPrompt::Text(
                "Stable mode, project rules, and tool policy".to_string(),
            )),
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("max".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        };

        let out = build_chat_messages_for_request(&request);

        assert_eq!(out[0].get("role").and_then(Value::as_str), Some("system"));
        assert_eq!(
            out[0].get("content").and_then(Value::as_str),
            Some("Stable mode, project rules, and tool policy")
        );
        let last = out.last().expect("latest user message");
        assert_eq!(last.get("role").and_then(Value::as_str), Some("user"));
        assert!(
            last.get("content")
                .and_then(Value::as_str)
                .is_some_and(|content| content.ends_with("Current user question")),
            "current-turn user input must be at the tail of the wire prompt: {last:?}"
        );
    }

    #[test]
    fn prompt_inspect_reports_stable_layers_and_dynamic_user_task() {
        let request = MessageRequest {
                model: "deepseek-v4-pro".to_string(),
                messages: vec![
                    Message {
                        role: Role::Assistant,
                        content: vec![ContentBlock::Text {
                            text: "Prior answer".to_string(),
                            cache_control: None,
                        }],
                    },
                    Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "Current task".to_string(),
                            cache_control: None,
                        }],
                    },
                ],
                max_tokens: 1024,
                system: Some(SystemPrompt::Text(
                    "Base policy\n\n<project_instructions source=\"AGENTS.md\">\nRules\n</project_instructions>\n\n## Project Context Pack\n\n<project_context_pack>\n{}\n</project_context_pack>\n\n## Environment\n\n- lang: en"
                        .to_string(),
                )),
                tools: None,
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: Some("max".to_string()),
                stream: None,
                temperature: None,
                top_p: None,
            };

        let inspection = inspect_prompt_for_request(&request);

        assert_eq!(inspection.base_static_prefix_hash.len(), 64);
        assert_eq!(inspection.full_request_prefix_hash.len(), 64);
        assert!(inspection.layers.iter().any(|layer| {
            layer.name == "Global system prefix"
                && layer.stability.label() == "static"
                && layer.char_len == "Base policy".chars().count()
                && layer.sha256.len() == 64
        }));
        assert!(
            inspection.layers.iter().any(|layer| {
                layer.name == "Project context" && layer.stability.label() == "static"
            })
        );
        assert!(inspection.layers.iter().any(|layer| {
            layer.name == "Project context pack" && layer.stability.label() == "static"
        }));
        assert!(inspection.layers.iter().any(|layer| {
            layer.name == "Message #1 assistant" && layer.stability.label() == "history"
        }));
        assert!(
            inspection
                .layers
                .last()
                .is_some_and(|layer| layer.name == "User task" && layer.stability.label() == "dynamic")
        );
    }

    #[test]
    fn prompt_inspect_keeps_static_base_hash_across_different_user_tasks() {
        fn request_with_user_task(task: &str) -> MessageRequest {
            MessageRequest {
                    model: "deepseek-v4-pro".to_string(),
                    messages: vec![
                        Message {
                            role: Role::Assistant,
                            content: vec![ContentBlock::Text {
                                text: "Prior answer".to_string(),
                                cache_control: None,
                            }],
                        },
                        Message {
                            role: Role::User,
                            content: vec![ContentBlock::Text {
                                text: task.to_string(),
                                cache_control: None,
                            }],
                        },
                    ],
                    max_tokens: 1024,
                    system: Some(SystemPrompt::Text(
                        "Base policy\n\n## Environment\n\n- shell: powershell\n\n## Skills\n\n- rust\n\n## Context Management\n\nKeep concise\n\n## Compact\n\nTemplate"
                            .to_string(),
                    )),
                    tools: None,
                    tool_choice: None,
                    metadata: None,
                    thinking: None,
                    reasoning_effort: Some("max".to_string()),
                    stream: None,
                    temperature: None,
                    top_p: None,
                }
        }

        let first = inspect_prompt_for_request(&request_with_user_task("First task"));
        let second = inspect_prompt_for_request(&request_with_user_task("Second task"));
        let mut changed_history_request = request_with_user_task("Second task");
        changed_history_request.messages[0] = Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "Different prior answer".to_string(),
                cache_control: None,
            }],
        };
        let changed_history = inspect_prompt_for_request(&changed_history_request);

        assert_eq!(
            first.base_static_prefix_hash,
            second.base_static_prefix_hash
        );
        assert_eq!(
            first.full_request_prefix_hash, second.full_request_prefix_hash,
            "full request prefix excludes the final dynamic user task"
        );
        assert_ne!(
            second.full_request_prefix_hash, changed_history.full_request_prefix_hash,
            "full request prefix can change when session history changes"
        );
        assert!(
            second
                .layers
                .last()
                .is_some_and(|layer| layer.name == "User task" && layer.stability.label() == "dynamic"),
            "current user task must remain the final layer"
        );
        assert!(second.layers.iter().any(|layer| {
            layer.name == "Message #1 assistant" && layer.stability.label() == "history"
        }));
        assert!(!second.layers.iter().any(
                |layer| layer.name.starts_with("Message #") && layer.stability.label() == "static"
            ));
    }

    #[test]
    fn prompt_inspect_tracks_tool_catalog_in_static_prefix_hash() {
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Current task".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 1024,
            system: Some(SystemPrompt::Text("Base policy".to_string())),
            tools: Some(vec![test_tool("read_file")]),
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("max".to_string()),
            stream: None,
            temperature: None,
            top_p: None,
        };

        let first = inspect_prompt_for_request(&request);
        let mut changed_tools = request.clone();
        changed_tools.tools = Some(vec![test_tool("read_file"), test_tool("grep_files")]);
        let second = inspect_prompt_for_request(&changed_tools);

        assert!(
            first
                .layers
                .iter()
                .any(|layer| { layer.name == "Tool catalog" && layer.stability.label() == "static" })
        );
        assert_ne!(
            first.base_static_prefix_hash, second.base_static_prefix_hash,
            "tool schema changes must be visible to cache-inspect base prefix diagnostics"
        );
        assert_ne!(
            first.full_request_prefix_hash, second.full_request_prefix_hash,
            "tool schema changes must be visible to full reusable-prefix diagnostics"
        );
    }

    #[test]
    fn cache_warmup_request_reuses_stable_prefix_and_fixed_user_tail() {
        let request = MessageRequest {
                model: "deepseek-v4-pro".to_string(),
                messages: vec![
                    Message {
                        role: Role::Assistant,
                        content: vec![ContentBlock::Text {
                            text: "Stable prior answer".to_string(),
                            cache_control: None,
                        }],
                    },
                    Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "Dynamic latest user task".to_string(),
                            cache_control: None,
                        }],
                    },
                ],
                max_tokens: 1024,
                system: Some(SystemPrompt::Text(
                    "Base policy\n\n<project_instructions source=\"AGENTS.md\">\nStable project rules\n</project_instructions>\n\n## Previous Session Relay\n\nDynamic relay"
                        .to_string(),
                )),
                tools: Some(vec![test_tool("read_file")]),
                tool_choice: None,
                metadata: None,
                thinking: None,
                reasoning_effort: Some("max".to_string()),
                stream: Some(true),
                temperature: Some(0.7),
                top_p: None,
            };

        let warmup = build_cache_warmup_request(&request);

        assert_eq!(warmup.max_tokens, 8);
        assert_eq!(warmup.temperature, None);
        assert_eq!(warmup.top_p, None);
        assert_eq!(warmup.reasoning_effort.as_deref(), Some("off"));
        assert_eq!(warmup.tools.as_ref().map(Vec::len), Some(1));
        assert_eq!(warmup.tool_choice, Some(json!("none")));
        assert_eq!(warmup.messages.len(), 2);
        assert_eq!(warmup.messages[0].role, "assistant");
        assert_eq!(warmup.messages[1].role, "user");
        assert_eq!(
            warmup.messages[1].content,
            vec![ContentBlock::Text {
                text: "请只回复 OK".to_string(),
                cache_control: None,
            }]
        );

        let wire = build_chat_messages_for_request(&warmup);
        let system = wire
            .first()
            .and_then(|value| value.get("content"))
            .and_then(Value::as_str)
            .expect("warmup system prompt");
        assert!(system.contains("Stable project rules"));
        assert!(!system.contains("Dynamic relay"));
        assert!(
            !wire
                .iter()
                .any(|value| value.to_string().contains("Dynamic latest user task")),
            "warmup must not include the dynamic latest user task"
        );
    }

    #[test]
    fn reasoning_effort_scenario() {
        // Scenario consolidation of: reasoning_effort_uses_deepseek_top_level_thinking_parameter, reasoning_effort_off_disables_top_level_thinking, reasoning_effort_off_is_omitted_for_strict_openai_like_providers, reasoning_effort_atlascloud_speaks_deepseek_dialect, reasoning_effort_modelstudio_writes_nothing_without_a_verified_route, reasoning_effort_moonshot_toggles_thinking, reasoning_effort_edenai_does_not_guess_a_model_dialect, reasoning_effort_ollama_toggles_think_flag
        // from reasoning_effort_uses_deepseek_top_level_thinking_parameter
        {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("max"), ProviderKind::Deepseek);

            assert_eq!(
                body.get("reasoning_effort").and_then(Value::as_str),
                Some("max")
            );
            assert_eq!(
                body.pointer("/thinking/type").and_then(Value::as_str),
                Some("enabled")
            );
            assert!(body.get("extra_body").is_none());
        }
        // from reasoning_effort_off_disables_top_level_thinking
        {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("off"), ProviderKind::Deepseek);

            assert_eq!(
                body.pointer("/thinking/type").and_then(Value::as_str),
                Some("disabled")
            );
            assert!(body.get("reasoning_effort").is_none());
            assert!(body.get("extra_body").is_none());
        }
        // from reasoning_effort_off_is_omitted_for_strict_openai_like_providers
        {
            for provider in [
                ProviderKind::Openai,
                ProviderKind::WanjieArk,
                ProviderKind::Qianfan,
                ProviderKind::Arcee,
                ProviderKind::Huggingface,
                ProviderKind::Fireworks,
            ] {
                let mut body = json!({});
                apply_reasoning_effort(&mut body, Some("off"), provider);

                assert_eq!(
                    body,
                    json!({}),
                    "provider {provider:?} should not receive unsupported reasoning-off fields"
                );
            }
        }
        // from reasoning_effort_atlascloud_speaks_deepseek_dialect
        {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("high"), ProviderKind::Atlascloud);
            assert_eq!(
                body,
                json!({ "reasoning_effort": "high", "thinking": { "type": "enabled" } })
            );

            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("max"), ProviderKind::Atlascloud);
            assert_eq!(
                body,
                json!({ "reasoning_effort": "max", "thinking": { "type": "enabled" } })
            );

            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("off"), ProviderKind::Atlascloud);
            assert_eq!(body, json!({ "thinking": { "type": "disabled" } }));
        }
        // from reasoning_effort_modelstudio_writes_nothing_without_a_verified_route
        {
            // The provider enum cannot decide DashScope's controls: `enable_thinking`
            // is wrong for the thinking-only models, `reasoning_effort` is only
            // valid for DeepSeek-V4/GLM, and a custom `base_url` on any of these
            // identities is an arbitrary gateway. All four variants must therefore
            // leave the body untouched here — the route shaper in client::chat is
            // the sole writer.
            for provider in [
                ProviderKind::ModelstudioTokenPlan,
                ProviderKind::ModelstudioTokenPlanAnthropic,
                ProviderKind::ModelstudioCodingPlan,
                ProviderKind::ModelstudioCodingPlanAnthropic,
            ] {
                for effort in [None, Some("off"), Some("low"), Some("high"), Some("max")] {
                    let mut body = json!({});
                    apply_reasoning_effort(&mut body, effort, provider);
                    assert_eq!(body, json!({}), "{provider:?} {effort:?}");
                }
            }
        }
        // from reasoning_effort_moonshot_toggles_thinking
        {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("high"), ProviderKind::Moonshot);
            assert_eq!(body, json!({ "thinking": { "type": "enabled" } }));

            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("off"), ProviderKind::Moonshot);
            assert_eq!(body, json!({ "thinking": { "type": "disabled" } }));
        }
        // from reasoning_effort_edenai_does_not_guess_a_model_dialect
        {
            for effort in ["off", "low", "medium", "high", "max", "xhigh"] {
                let mut body = json!({});
                apply_reasoning_effort(&mut body, Some(effort), ProviderKind::Edenai);
                assert_eq!(body, json!({}), "unexpected Eden AI fields for {effort}");
            }
        }
        // from reasoning_effort_ollama_toggles_think_flag
        {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("high"), ProviderKind::Ollama);
            assert_eq!(body, json!({ "think": true }));

            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("off"), ProviderKind::Ollama);
            assert_eq!(body, json!({ "think": false }));
        }
    }

    /// First-party DeepSeek routes document `reasoning_effort` low/high/max on
    /// the wire (no medium): low is a real cheaper tier, medium rounds up to
    /// high (#52). Hosted DeepSeek-compatible routes keep the historic
    /// low/medium → high collapse because their own wire contracts are not
    /// verified here.
    #[test]
    fn stepfun_reasoning_effort_respects_each_model_wire_contract() {
        for (model, effort, expected) in [
            ("step-5-preview", "low", Some("low")),
            ("step-5-preview", "medium", Some("medium")),
            ("step-5-preview", "max", Some("high")),
            ("step-3.7-flash", "medium", Some("medium")),
            ("step-3.5-flash-2603", "medium", Some("high")),
            ("step-3.5-flash-2603", "low", Some("low")),
            ("step-3.5-flash", "high", None),
            ("step-5-preview", "off", None),
            ("step-5-preview", "auto", None),
            ("step-audio-2", "high", None),
        ] {
            let mut body = json!({"model": model});
            apply_reasoning_effort(&mut body, Some(effort), ProviderKind::Stepfun);
            assert_eq!(
                body.get("reasoning_effort").and_then(Value::as_str),
                expected,
                "{model}/{effort}"
            );
            assert!(body.get("thinking").is_none());
        }
    }

    #[test]
    fn stepfun_discovery_excludes_non_coding_and_retired_models() {
        let models = parse_models_response(
            r#"{"data":[
                {"id":"step-5-preview"},{"id":"step-3.7-flash"},
                {"id":"step-3.5-flash"},{"id":"step-3.5-flash-2603"},
                {"id":"step-audio-2"},{"id":"step-tts-2"},
                {"id":"step-image-edit-2"},{"id":"step-2x-large"},{"id":"step-3"}
            ]}"#,
        )
        .unwrap();
        let filtered = apply_provider_model_cutline(ProviderKind::Stepfun, models);
        assert_eq!(
            filtered
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            [
                "step-3.5-flash",
                "step-3.5-flash-2603",
                "step-3.7-flash",
                "step-5-preview"
            ]
        );
    }

    #[test]
    fn reasoning_effort_deepseek_maps_the_documented_wire_ladder() {
        let mut body = json!({});
        apply_reasoning_effort(&mut body, Some("low"), ProviderKind::Deepseek);
        assert_eq!(
            body,
            json!({ "reasoning_effort": "low", "thinking": { "type": "enabled" } })
        );

        let mut body = json!({});
        apply_reasoning_effort(&mut body, Some("medium"), ProviderKind::Deepseek);
        assert_eq!(
            body,
            json!({ "reasoning_effort": "high", "thinking": { "type": "enabled" } })
        );

        for provider in [ProviderKind::Deepseek, ProviderKind::Deepseek] {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("high"), provider);
            assert_eq!(
                body,
                json!({ "reasoning_effort": "high", "thinking": { "type": "enabled" } }),
                "provider {provider:?}"
            );
        }

        for provider in [ProviderKind::Siliconflow, ProviderKind::Deepinfra] {
            let mut body = json!({});
            apply_reasoning_effort(&mut body, Some("low"), provider);
            assert_eq!(
                body,
                json!({ "reasoning_effort": "high", "thinking": { "type": "enabled" } }),
                "hosted route {provider:?} keeps the collapse"
            );
        }
    }

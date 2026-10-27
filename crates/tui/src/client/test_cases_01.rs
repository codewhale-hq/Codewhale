
    use super::*;
    use crate::client::chat::{
        build_chat_messages, build_chat_messages_for_request,
        build_chat_messages_for_request_and_provider, count_reasoning_replay_chars,
        parse_chat_message, parse_sse_chunk, sanitize_thinking_mode_messages, tool_to_chat,
        tool_to_chat_for_base_url,
    };
    use crate::client::responses::build_responses_body;
    use codewhale_runtime::config::{ProviderConfig, ProvidersConfig};
    use codewhale_config::descriptors::defaults::{DEFAULT_CONCENTRATE_BASE_URL, DEFAULT_CONCENTRATE_MODEL, DEFAULT_EDENAI_MODEL, DEFAULT_TELECOMJS_MODEL};
    use codewhale_config::catalog::reviewed::constants::OPENROUTER_QWEN_3_6_FLASH_MODEL;
    use crate::tools::apply_patch::ApplyPatchTool;
    use crate::tools::spec::ToolSpec;
    use crate::tools::{ToolContext, ToolRegistryBuilder};
    use codewhale_models::{
        ContentBlock, ContentBlockStart, Delta, Message, MessageRequest, MessageResponse,
        StreamEvent, Tool,
    };
    use codewhale_protocol::runtime::DynamicToolSpec;
    use serde_json::json;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// OpenRouter app attribution: the headers that put an app on
    /// openrouter.ai's rankings are sent for OpenRouter routes only, and a
    /// user-configured header of the same name wins.
    #[test]
    fn openrouter_routes_carry_app_attribution_headers() {
        let headers = build_default_headers(
            "sk-or-key",
            &HashMap::new(),
            ProviderKind::Openrouter,
            "https://openrouter.ai/api/v1",
            WireFormat::ChatCompletions,
            false,
            AuthHeaderSurface::Inference,
        )
        .expect("headers");
        assert_eq!(
            headers.get("http-referer").and_then(|v| v.to_str().ok()),
            Some("https://codewhale.net")
        );
        assert_eq!(
            headers.get("x-title").and_then(|v| v.to_str().ok()),
            Some("Codewhale")
        );
        // The current display-name header, alongside the legacy one.
        assert_eq!(
            headers
                .get("x-openrouter-title")
                .and_then(|v| v.to_str().ok()),
            Some("Codewhale")
        );
        // Marketplace categories must match OpenRouter's spelling exactly:
        // unrecognised values are dropped silently, so a typo here is
        // invisible in production and only shows up as a missing listing.
        assert_eq!(
            headers
                .get("x-openrouter-categories")
                .and_then(|v| v.to_str().ok()),
            Some("cli-agent,personal-agent")
        );

        // Attribution identifies this app to OpenRouter's rankings and has
        // no business on any other route, whichever provider that is.
        for provider in [
            ProviderKind::Deepseek,
            ProviderKind::Moonshot,
            ProviderKind::Zai,
            ProviderKind::Openai,
            ProviderKind::Custom,
        ] {
            let other = build_default_headers(
                "sk-key",
                &HashMap::new(),
                provider,
                "https://example.invalid/v1",
                WireFormat::ChatCompletions,
                false,
                AuthHeaderSurface::Inference,
            )
            .expect("headers");
            assert!(
                other.get("http-referer").is_none()
                    && other.get("x-title").is_none()
                    && other.get("x-openrouter-title").is_none(),
                "{provider:?} must not receive OpenRouter attribution headers"
            );
        }

        let overridden = build_default_headers(
            "sk-or-key",
            &HashMap::from([("X-Title".to_string(), "My Fork".to_string())]),
            ProviderKind::Openrouter,
            "https://openrouter.ai/api/v1",
            WireFormat::ChatCompletions,
            false,
            AuthHeaderSurface::Inference,
        )
        .expect("headers");
        // A user title on the legacy header alone must follow onto the
        // current one, or OpenRouter would still show "Codewhale".
        for name in ["x-title", "x-openrouter-title"] {
            assert_eq!(
                overridden.get(name).and_then(|v| v.to_str().ok()),
                Some("My Fork"),
                "{name} must carry the user's title"
            );
        }

        // And the other way round: setting only the current header also
        // renames the legacy one, so the two never disagree.
        let current_only = build_default_headers(
            "sk-or-key",
            &HashMap::from([("X-OpenRouter-Title".to_string(), "My Fork".to_string())]),
            ProviderKind::Openrouter,
            "https://openrouter.ai/api/v1",
            WireFormat::ChatCompletions,
            false,
            AuthHeaderSurface::Inference,
        )
        .expect("headers");
        for name in ["x-title", "x-openrouter-title"] {
            assert_eq!(
                current_only.get(name).and_then(|v| v.to_str().ok()),
                Some("My Fork"),
                "{name} must carry the user's title"
            );
        }
    }

    #[test]
    fn openrouter_pricing_maps_cache_write_per_token_to_per_million() {
        let payload = r#"{"data":[{
            "id":"anthropic/claude-sonnet-4-6",
            "pricing":{
                "prompt":"0.000003",
                "completion":"0.000015",
                "input_cache_read":"0.0000003",
                "input_cache_write":"0.00000375"
            }
        },{
            "id":"some/no-write-row",
            "pricing":{"prompt":"0.000001","completion":"0.000002"}
        }]}"#;

        let items = parse_openrouter_models_response(payload).expect("parses");
        let priced = openrouter_to_catalog_offering(&items[0], "openrouter", "fp", 42)
            .expect("valid priced row");
        let cost = priced.cost.as_ref().expect("pricing row");
        assert_eq!(cost.input, Some(3.0));
        assert_eq!(cost.output, Some(15.0));
        assert_eq!(cost.cache_read, Some(0.3));
        assert_eq!(cost.cache_write, Some(3.75));

        // A cache-write premium must actually reach the estimator: the same
        // tokens cost more when they are cache-creation rather than cache-read.
        let pricing = codewhale_config::pricing::OfferingPricing::from_catalog_offering(&priced)
            .expect("priced offering");
        let write = codewhale_config::pricing::TokenUsage {
            cache_write: 1_000_000,
            ..Default::default()
        };
        assert_eq!(pricing.estimate_cost(&write), Some(3.75));
        assert!(pricing.unpriced_used_classes(&write).is_empty());

        // A row without a published write rate stays unknown, not zero, and
        // fails closed for cache-creation turns.
        let unwritten = openrouter_to_catalog_offering(&items[1], "openrouter", "fp", 42)
            .expect("valid row without cache-write rate");
        assert_eq!(
            unwritten.cost.as_ref().and_then(|cost| cost.cache_write),
            None
        );
        let unwritten =
            codewhale_config::pricing::OfferingPricing::from_catalog_offering(&unwritten)
                .expect("priced offering");
        assert_eq!(unwritten.estimate_cost(&write), None);
        assert_eq!(
            unwritten.unpriced_used_classes(&write),
            vec![codewhale_config::pricing::TokenClass::CacheWrite]
        );
    }

    #[test]
    fn baseten_catalog_maps_provider_stated_prices_limits_and_features() {
        // Exact current Baseten shape: pricing is captured from the official
        // baseten-switch repository; Model APIs publishes context_length,
        // max_completion_tokens, and supported_features including `vision`.
        let payload = r#"{"data":[{
            "id":"deepseek-ai/DeepSeek-V4-Pro",
            "context_length":"1048576",
            "max_completion_tokens":262144,
            "pricing":{
                "prompt":0.0000014,
                "completion":"0.0000044",
                "input_cache_read":0.00000014
            },
            "supported_features":["reasoning","vision"],
            "reasoning_options":[{"type":"toggle"}]
        }]}"#;

        let items = parse_baseten_models_response(payload).expect("Baseten catalog");
        let offering =
            baseten_to_catalog_offering(&items[0], "baseten", "baseten-fp", 42).expect("row");
        assert_eq!(offering.provider, "baseten");
        assert_eq!(
            offering.wire_model_id,
            codewhale_config::catalog::BASETEN_DEFAULT_MODEL
        );
        assert!(offering.default_for_provider);
        let limit = offering.limit.expect("published limits");
        assert_eq!(limit.context, Some(1_048_576));
        assert_eq!(limit.input, Some(1_048_576));
        assert_eq!(limit.output, Some(262_144));
        let cost = offering.cost.expect("published pricing");
        assert_eq!(cost.input, Some(1.4));
        assert_eq!(cost.output, Some(4.4));
        assert_eq!(cost.cache_read, Some(0.14));
        assert_eq!(cost.cache_write, None);
        assert_eq!(offering.reasoning, Some(true));
        assert_eq!(offering.tool_call, Some(true));
        assert_eq!(offering.structured_output, Some(true));
        assert_eq!(offering.attachment, Some(true));
        let modalities = offering.modalities.expect("vision feature modalities");
        assert_eq!(modalities.input, vec!["text", "image"]);
        assert_eq!(modalities.output, vec!["text"]);
        assert_eq!(offering.reasoning_options, vec![json!({"type":"toggle"})]);
        assert!(matches!(offering.source, CatalogSource::Live { .. }));
    }

    #[test]
    fn baseten_catalog_rejects_duplicate_ids_and_invalid_known_numbers() {
        let duplicates = r#"{"data":[{"id":"same/model"},{"id":"same/model"}]}"#;
        assert_eq!(
            parse_baseten_models_response(duplicates).unwrap_err(),
            CatalogRefreshError::InvalidResponse
        );

        let negative = r#"{"data":[{
            "id":"synthetic/model",
            "pricing":{"prompt":-0.000001,"completion":0.000002}
        }]}"#;
        let items = parse_baseten_models_response(negative).expect("shape parses");
        assert_eq!(
            baseten_to_catalog_offering(&items[0], "baseten", "fp", 1).unwrap_err(),
            CatalogRefreshError::InvalidResponse
        );
    }

    #[test]
    fn provider_live_price_parsers_reject_present_bad_rates_but_keep_zero_and_omission() {
        for invalid in ["not-a-number", "-0.1", "NaN", "inf", "1e308", "0.100001"] {
            let openrouter = json!({
                "data": [{
                    "id": "synthetic/openrouter-invalid-price",
                    "pricing": { "prompt": invalid, "completion": "0.000001" }
                }]
            })
            .to_string();
            let items = parse_openrouter_models_response(&openrouter).expect("OpenRouter shape");
            let offering = openrouter_to_catalog_offering(&items[0], "openrouter", "fp", 1);
            if invalid.starts_with('-') {
                // OpenRouter's negative sentinel means "no fixed rate" (#6690).
                assert_eq!(offering.expect("variable-price row").cost, None);
            } else {
                assert_eq!(
                    offering.unwrap_err(),
                    CatalogRefreshError::InvalidResponse,
                    "OpenRouter must reject {invalid:?}"
                );
            }

            let baseten = json!({
                "data": [{
                    "id": "synthetic/baseten-invalid-price",
                    "pricing": { "prompt": invalid, "completion": "0.000001" }
                }]
            })
            .to_string();
            let items = parse_baseten_models_response(&baseten).expect("Baseten shape");
            assert_eq!(
                baseten_to_catalog_offering(&items[0], "baseten", "fp", 1).unwrap_err(),
                CatalogRefreshError::InvalidResponse,
                "Baseten must reject {invalid:?}"
            );
        }

        let openrouter = parse_openrouter_models_response(
            r#"{"data":[{"id":"synthetic/openrouter-free","pricing":{"prompt":"0","completion":"0"}}]}"#,
        )
        .expect("OpenRouter zero row");
        let openrouter = openrouter_to_catalog_offering(&openrouter[0], "openrouter", "fp", 1)
            .expect("explicit zero is a valid published price");
        let cost = openrouter.cost.expect("published zero cost");
        assert_eq!(cost.input, Some(0.0));
        assert_eq!(cost.output, Some(0.0));
        assert_eq!(cost.cache_read, None);
        assert_eq!(cost.cache_write, None);

        let baseten = parse_baseten_models_response(
            r#"{"data":[{"id":"synthetic/baseten-free","pricing":{"prompt":"0","completion":0}}]}"#,
        )
        .expect("Baseten zero row");
        let baseten = baseten_to_catalog_offering(&baseten[0], "baseten", "fp", 1)
            .expect("explicit zero is a valid published price");
        let cost = baseten.cost.expect("published zero cost");
        assert_eq!(cost.input, Some(0.0));
        assert_eq!(cost.output, Some(0.0));
        assert_eq!(cost.cache_read, None);
        assert_eq!(cost.cache_write, None);
    }

    #[test]
    fn baseten_feature_names_require_exact_normalized_aliases() {
        let payload = r#"{"data":[{
            "id":"synthetic/text-only",
            "supported_features":["revision","pre_reasoning_filter"]
        }]}"#;
        let items = parse_baseten_models_response(payload).expect("Baseten catalog");
        let offering =
            baseten_to_catalog_offering(&items[0], "baseten", "fp", 1).expect("valid row");
        assert_eq!(offering.reasoning, Some(false));
        assert_eq!(offering.modalities, None);
        assert_eq!(offering.attachment, None);
        assert_eq!(offering.tool_call, Some(true));
        assert_eq!(offering.structured_output, Some(true));
    }

    fn test_tool(name: &str) -> Tool {
        Tool {
            tool_type: None,
            name: name.to_string(),
            description: format!("{name} test tool"),
            input_schema: json!({
                "type": "object",
                "properties": {},
            }),
            allowed_callers: None,
            defer_loading: Some(false),
            input_examples: None,
            strict: Some(true),
            cache_control: None,
        }
    }

    fn apply_patch_request_tool() -> Tool {
        let spec = ApplyPatchTool;
        Tool {
            tool_type: None,
            name: spec.name().to_string(),
            description: spec.description().to_string(),
            input_schema: spec.input_schema(),
            allowed_callers: None,
            defer_loading: Some(false),
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    fn deferred_dynamic_request_tool() -> Tool {
        let registry = ToolRegistryBuilder::new()
            .with_dynamic_tools(&[DynamicToolSpec {
                namespace: Some("capture".to_string()),
                name: "deferred_lookup".to_string(),
                description: "Look up a record after deferred loading".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "mode": {"type": "string", "const": "fast"},
                        "query": {
                            "anyOf": [
                                {"type": "string"},
                                {"type": "null"}
                            ]
                        }
                    },
                    "required": ["mode"]
                }),
                defer_loading: true,
            }])
            .build(ToolContext::new(
                std::env::temp_dir().join("codewhale-k3-deferred-capture"),
            ));
        registry
            .to_api_tools()
            .into_iter()
            .find(|tool| tool.name == "deferred_lookup")
            .expect("dynamic tool remains model-visible")
    }

    fn value_contains_key(value: &Value, needle: &str) -> bool {
        match value {
            Value::Object(object) => {
                object.contains_key(needle)
                    || object
                        .values()
                        .any(|child| value_contains_key(child, needle))
            }
            Value::Array(values) => values.iter().any(|child| value_contains_key(child, needle)),
            _ => false,
        }
    }

    fn captured_function<'a>(body: &'a Value, name: &str) -> &'a Value {
        body["tools"]
            .as_array()
            .and_then(|tools| tools.iter().find(|tool| tool["function"]["name"] == name))
            .map(|tool| &tool["function"])
            .unwrap_or_else(|| panic!("captured tool catalog is missing {name}: {body}"))
    }

    fn moonshot_request_boundary_client(
        route_base_url: &str,
        model: &str,
        transport_base_url: String,
    ) -> CodewhaleClient {
        let mut client = CodewhaleClient::new(&{
            let __cw_config_init_0_0 = Some("moonshot".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                moonshot: ProviderConfig {
                    api_key: Some("moonshot-request-boundary-key".to_string()),
                    base_url: Some(route_base_url.to_string()),
                    model: Some(model.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        })
        .expect("Moonshot request-boundary client");
        assert_eq!(client.base_url, route_base_url);
        client.test_chat_transport_base_url = Some(transport_base_url);
        client
    }

    fn zai_request_boundary_client(
        route_base_url: &str,
        model: &str,
        transport_base_url: String,
    ) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut client = CodewhaleClient::new(&{
            let __cw_config_init_1_0 = Some("zai".to_string());
            let __cw_config_init_1_1 = Some(ProvidersConfig {
                zai: ProviderConfig {
                    api_key: Some("zai-request-boundary-key".to_string()),
                    base_url: Some(route_base_url.to_string()),
                    model: Some(model.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        })
        .expect("Z.ai request-boundary client");
        assert_eq!(client.base_url, route_base_url);
        client.test_chat_transport_base_url = Some(transport_base_url);
        client
    }

    fn minimax_request_boundary_client(
        route_base_url: &str,
        model: &str,
        transport_base_url: String,
    ) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut client = CodewhaleClient::new(&{
            let __cw_config_init_2_0 = Some("minimax".to_string());
            let __cw_config_init_2_1 = Some(ProvidersConfig {
                minimax: ProviderConfig {
                    api_key: Some("minimax-request-boundary-key".to_string()),
                    base_url: Some(route_base_url.to_string()),
                    model: Some(model.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        })
        .expect("MiniMax request-boundary client");
        assert_eq!(client.base_url, route_base_url);
        client.test_chat_transport_base_url = Some(transport_base_url);
        client
    }

    fn deepseek_request_boundary_client(
        route_base_url: &str,
        transport_base_url: String,
    ) -> CodewhaleClient {
        let mut client = CodewhaleClient::new(
            &{
                let __cw_config_init_3_0 = Some("deepseek".to_string());
                let __cw_config_init_3_1 = Some("deepseek-v4-pro".to_string());
                let mut __cw_config_init_3_value: Config = Config::default();
                __cw_config_init_3_value.provider = __cw_config_init_3_0;
                __cw_config_init_3_value.default_text_model = __cw_config_init_3_1;
                __cw_config_init_3_value
            }
            .with_legacy_root(
                Some("deepseek-request-boundary-key".to_string()),
                Some(route_base_url.to_string()),
            ),
        )
        .expect("DeepSeek request-boundary client");
        client.test_chat_transport_base_url = Some(transport_base_url);
        client
    }

    fn ollama_cloud_request_boundary_client(transport_base_url: String) -> CodewhaleClient {
        let mut client = CodewhaleClient::new(&{
            let __cw_config_init_4_0 = Some("ollama-cloud".to_string());
            let __cw_config_init_4_1 = Some(ProvidersConfig {
                ollama_cloud: ProviderConfig {
                    api_key: Some("ollama-cloud-request-boundary-key".to_string()),
                    base_url: Some(
                        codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_CLOUD_BASE_URL
                            .to_string(),
                    ),
                    model: Some("gpt-oss:120b".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_4_value: Config = Config::default();
            __cw_config_init_4_value.provider = __cw_config_init_4_0;
            __cw_config_init_4_value.providers = __cw_config_init_4_1;
            __cw_config_init_4_value
        })
        .expect("Ollama Cloud request-boundary client");
        assert_eq!(
            client.base_url,
            codewhale_config::descriptors::defaults::DEFAULT_OLLAMA_CLOUD_BASE_URL
        );
        client.test_chat_transport_base_url = Some(transport_base_url);
        client
    }

    /// Swap in a short non-streaming envelope for the test's duration,
    /// restoring the previous value on drop.
    struct NonStreamingEnvelopeGuard(u64);

    impl NonStreamingEnvelopeGuard {
        fn millis(ms: u64) -> Self {
            Self(TEST_NON_STREAMING_ENVELOPE_MS.swap(ms, std::sync::atomic::Ordering::SeqCst))
        }
    }

    impl Drop for NonStreamingEnvelopeGuard {
        fn drop(&mut self) {
            TEST_NON_STREAMING_ENVELOPE_MS.store(self.0, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// The provider accepts the connection but stalls far past the budgeted
    /// envelope before answering: the non-streaming request must be cut off
    /// with a timeout instead of wedging the caller (mid-turn compaction,
    /// translate, provider-native search) indefinitely.
    #[tokio::test]
    async fn non_streaming_envelope_bounds_a_stalled_provider() {
        // The injected budget is process-global; serialize against other tests
        // (which may issue non-streaming requests with their own timing
        // assumptions) through the shared test-env lock.
        let _env_lock = crate::test_support::lock_test_env();
        let _envelope = NonStreamingEnvelopeGuard::millis(2000);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "id": "chatcmpl_envelope",
                        "object": "chat.completion",
                        "model": "deepseek-v4-pro",
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "late"},
                            "finish_reason": "stop"
                        }],
                        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                    }))
                    .set_delay(Duration::from_secs(5)),
            )
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client(&server.uri(), server.uri());
        let err = client
            .create_message(k3_request_fixture("deepseek-v4-pro", Some("off"), false))
            .await
            .expect_err("a provider that never answers must hit the envelope");
        assert!(
            err.to_string().to_lowercase().contains("timed out"),
            "envelope timeout must be reported as such; got {err:#}"
        );
    }

    /// Every attempt answers 429 with an hour-long Retry-After. Honoring the
    /// header must not hand the total budget to the server: the retry loop is
    /// capped by the envelope, so a gateway answering 429 + Retry-After: 3600
    /// forever fails the turn in bounded time instead of wedging it for hours.
    #[tokio::test]
    async fn retry_after_honoring_cannot_extend_the_non_streaming_envelope() {
        let _env_lock = crate::test_support::lock_test_env();
        let _envelope = NonStreamingEnvelopeGuard::millis(2000);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "3600")
                    .set_body_string("rate limited"),
            )
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client(&server.uri(), server.uri());
        let started = std::time::Instant::now();
        let err = client
            .create_message(k3_request_fixture("deepseek-v4-pro", Some("off"), false))
            .await
            .expect_err("unbounded Retry-After honoring must still hit the envelope");
        assert!(
            err.to_string().to_lowercase().contains("timed out"),
            "the envelope must cut off the Retry-After wait; got {err:#}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "a 3600s Retry-After must not run past the envelope; took {:?}",
            started.elapsed()
        );
        codewhale_runtime::retry_status::clear();
        codewhale_runtime::retry_status::clear_rate_limit();
    }

    /// The open answers past the injected non-streaming envelope. The
    /// streaming-open path must not inherit any total: reqwest's per-request
    /// timeout wraps the response body, so a total set on the open would ride
    /// on the returned body and hard-cut a live stream mid-generation.
    #[tokio::test]
    async fn stream_open_retry_path_sets_no_total_deadline() {
        let _env_lock = crate::test_support::lock_test_env();
        let _envelope = NonStreamingEnvelopeGuard::millis(2000);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("data: [DONE]\n\n")
                    .set_delay(Duration::from_millis(2500)),
            )
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client(&server.uri(), server.uri());
        let response = client
            .send_stream_open_with_retry(|policy| {
                super::stream_entry::client_for_policy(
                    &client.http_client,
                    client.http1_fallback_client(),
                    policy,
                )
                    .post(format!("{}/chat/completions", server.uri()))
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body("{}".to_string())
            })
            .await
            .expect("stream open must not carry the non-streaming envelope");
        assert!(response.status().is_success());
        let text = response.text().await.expect("read stream-open body");
        assert_eq!(text, "data: [DONE]\n\n");
    }

    /// A caller that pins its own, larger per-attempt total (`list_models`
    /// pins 30s) must keep it: `.timeout()` on the builder is a pure
    /// overwrite, so an unconditional envelope would silently replace the
    /// pinned budget with the injected 2s and fail this 2.5s-late response.
    #[tokio::test]
    async fn pinned_request_total_survives_the_shared_retry_envelope() {
        let _env_lock = crate::test_support::lock_test_env();
        let _envelope = NonStreamingEnvelopeGuard::millis(2000);
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("{}")
                    .set_delay(Duration::from_millis(2500)),
            )
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client(&server.uri(), server.uri());
        let response = client
            .send_with_retry_total_error_body(
                Duration::from_secs(5),
                || client.http_client.get(format!("{}/models", server.uri())),
                &ErrorBodyDisclosure::Full,
            )
            .await
            .expect("caller-pinned total must not be overwritten by the envelope");
        assert!(response.status().is_success());
    }

    /// The per-chunk line cap is backpressure relief, not a data budget. When
    /// one transport chunk carries more SSE lines than the cap, the drain loop
    /// stops mid-buffer and the outer loop waits for the *next* chunk before
    /// draining any more — so whatever is still buffered when the stream ends
    /// never reaches the decoder. `flush_sse_line` cannot rescue it: it treats
    /// the whole remainder as one unterminated line. A long stream of small
    /// deltas (or provider heartbeats) therefore loses its tail — the last
    /// tokens, `finish_reason`, and usage — silently.
    #[tokio::test]
    async fn chat_stream_drains_chunks_carrying_more_lines_than_the_per_chunk_cap() {
        // Comment lines are counted by the drain loop and are cheap enough
        // that one transport read holds far more than SSE_MAX_LINES_PER_CHUNK.
        let heartbeats = ": ping\n".repeat(20 * SSE_MAX_LINES_PER_CHUNK * 4);
        let body = format!(
            "{heartbeats}data: {}\n\ndata: [DONE]\n\n",
            json!({
                "choices": [{
                    "index": 0,
                    "delta": {"content": "pong"},
                    "finish_reason": "stop"
                }]
            })
        );

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client("https://api.deepseek.com/v1", server.uri());
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "sse drain regression".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 64,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("off".to_string()),
            stream: Some(true),
            temperature: None,
            top_p: None,
        };

        let mut stream = client
            .create_message_stream(request)
            .await
            .expect("streaming request succeeds");
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let StreamEvent::ContentBlockDelta {
                delta: Delta::TextDelta { text: chunk },
                ..
            } = event.expect("heartbeat-padded SSE stays valid")
            {
                text.push_str(&chunk);
            }
        }

        assert_eq!(
            text, "pong",
            "the data frame after the heartbeat flood must still be decoded"
        );
    }

    #[tokio::test]
    async fn chat_stream_eof_without_done_or_finish_reason_is_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(": provider heartbeat\n\n"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client("https://api.deepseek.com/v1", server.uri());
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "premature EOF regression".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 64,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("off".to_string()),
            stream: Some(true),
            temperature: None,
            top_p: None,
        };

        let mut stream = client
            .create_message_stream(request)
            .await
            .expect("HTTP request succeeds before the stream closes");
        let mut saw_stop = false;
        let mut failure = None;
        while let Some(event) = stream.next().await {
            match event {
                Ok(StreamEvent::MessageStop) => saw_stop = true,
                Ok(_) => {}
                Err(error) => failure = Some(error.to_string()),
            }
        }

        assert!(
            !saw_stop,
            "premature EOF must not be reported as MessageStop"
        );
        assert!(
            failure
                .as_deref()
                .is_some_and(|message| message.contains("before [DONE] or finish_reason")),
            "premature EOF must remain a typed stream failure: {failure:?}"
        );
    }

    #[tokio::test]
    async fn chat_stream_finish_reason_without_done_is_terminal() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(concat!(
                        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
                        "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                    )),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client("https://api.deepseek.com/v1", server.uri());
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "finish reason regression".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 64,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("off".to_string()),
            stream: Some(true),
            temperature: None,
            top_p: None,
        };

        let mut stream = client
            .create_message_stream(request)
            .await
            .expect("streaming request succeeds");
        let mut saw_stop = false;
        while let Some(event) = stream.next().await {
            if matches!(
                event.expect("terminal stream stays valid"),
                StreamEvent::MessageStop
            ) {
                saw_stop = true;
            }
        }
        assert!(
            saw_stop,
            "finish_reason is valid terminal proof without [DONE]"
        );
    }

    async fn capture_deepseek_chat_request(
        route_base_url: &str,
        strict: bool,
        streaming: bool,
    ) -> (String, Value) {
        let server = MockServer::start().await;
        let response = if streaming {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: [DONE]\n\n")
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-deepseek-request-boundary",
                "object": "chat.completion",
                "model": "deepseek-v4-pro",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            }))
        };
        Mock::given(method("POST"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;

        let mut tool = test_tool("lookup");
        if !strict {
            tool.strict = None;
        }
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "provider-free DeepSeek route fixture".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 64,
            system: None,
            tools: Some(vec![tool]),
            tool_choice: Some(json!(if strict { "required" } else { "auto" })),
            metadata: None,
            thinking: None,
            reasoning_effort: Some("off".to_string()),
            stream: Some(streaming),
            temperature: None,
            top_p: None,
        };
        let client = deepseek_request_boundary_client(route_base_url, server.uri());

        if streaming {
            let mut stream = client
                .create_message_stream(request)
                .await
                .expect("streaming request succeeds");
            while let Some(event) = stream.next().await {
                event.expect("captured SSE response remains valid");
            }
        } else {
            client
                .create_message(request)
                .await
                .expect("non-streaming request succeeds");
        }

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 1);
        let path = requests[0].url.path().to_string();
        let body = serde_json::from_slice(&requests[0].body).expect("captured request JSON");
        (path, body)
    }

    /// #6540: the compaction summary is the parent turn plus one trailing
    /// user instruction. Rendered through the production wire builders, its
    /// prompt must be byte-for-byte the parent's prompt followed by that one
    /// item, with the same model, system/instructions, tools and reasoning
    /// controls — otherwise the provider cache misses the whole history.
    #[test]
    fn compaction_summary_request_extends_the_parent_turn_prompt_bytes() {
        fn text(role: Role, text: &str) -> Message {
            Message {
                role,
                content: vec![ContentBlock::Text {
                    text: text.to_string(),
                    cache_control: None,
                }],
            }
        }
        fn assert_extends(label: &str, parent: &Value, summary: &Value) {
            let parent_items = parent.as_array().expect("parent prompt items");
            let summary_items = summary.as_array().expect("summary prompt items");
            assert_eq!(summary_items.len(), parent_items.len() + 1, "{label}");
            let parent_bytes = serde_json::to_string(parent).expect("serialize parent");
            let summary_bytes = serde_json::to_string(summary).expect("serialize summary");
            let open_prefix = parent_bytes.strip_suffix(']').expect("JSON array");
            assert!(
                summary_bytes.starts_with(open_prefix),
                "{label}: summary prompt diverges from the parent prefix"
            );
        }

        let model = "deepseek-v4-pro";
        let history = vec![
            text(Role::User, "fix the failing session_store test"),
            text(Role::Assistant, "Reading the test first."),
            text(Role::User, "keep the branch name"),
        ];
        let system = SystemPrompt::Text("pinned system prompt".to_string());
        let tools = vec![test_tool("read"), test_tool("bash")];
        let effort = "high";
        let parent = codewhale_core::request::prepare_primary_turn_request(
            codewhale_core::request::PrimaryTurnRequest {
                model: model.to_string(),
                messages: history.clone(),
                max_tokens: 64_000,
                system: Some(system.clone()),
                tools: Some(tools.clone()),
                tool_choice: Some(json!({"type": "auto"})),
                reasoning_effort: Some(effort.to_string()),
            },
        );
        let config = crate::compaction::CompactionConfig {
            model: model.to_string(),
            ..Default::default()
        };
        let mut summary_history = history;
        summary_history.push(text(Role::User, "write the handoff summary"));
        let summary = crate::compaction::compaction_summary_request(
            summary_history,
            &config,
            Some(&system),
            Some(&tools),
            Some(effort),
            8_192,
        );

        // Chat Completions: system and history share one `messages` array.
        let client = deepseek_request_boundary_client(
            "https://api.deepseek.com/v1",
            "http://127.0.0.1:9".into(),
        );
        let parent_body = client
            .prepare_outbound_request(parent.clone(), true)
            .expect("parent prepares")
            .body;
        let summary_body = client
            .prepare_outbound_request(summary.clone(), false)
            .expect("summary prepares")
            .body;
        assert_extends("chat", &parent_body["messages"], &summary_body["messages"]);
        for key in ["model", "tools", "reasoning_effort", "thinking"] {
            assert_eq!(parent_body.get(key), summary_body.get(key), "chat {key}");
        }

        // Responses (the Codex route where the 0% hit was recorded).
        let parent_body =
            responses::build_responses_body_for_provider(&parent, ProviderKind::OpenaiCodex, None);
        let summary_body =
            responses::build_responses_body_for_provider(&summary, ProviderKind::OpenaiCodex, None);
        assert_extends("responses", &parent_body["input"], &summary_body["input"]);
        for key in ["model", "instructions", "tools", "reasoning", "include"] {
            assert_eq!(
                parent_body.get(key),
                summary_body.get(key),
                "responses {key}"
            );
        }
        assert!(parent_body.get("reasoning").is_some());
    }

    #[tokio::test]
    async fn core_primary_request_preparation_matches_captured_transport_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string("data: [DONE]\n\n"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let request = codewhale_core::request::prepare_primary_turn_request(
            codewhale_core::request::PrimaryTurnRequest {
                model: "deepseek-v4-pro".to_string(),
                messages: vec![Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "core request boundary".to_string(),
                        cache_control: None,
                    }],
                }],
                max_tokens: 64,
                system: None,
                tools: Some(vec![Tool {
                    input_schema: json!({
                        "zeta": {"type": "string"},
                        "alpha": {"type": "number"},
                        "type": "object",
                    }),
                    ..test_tool("lookup")
                }]),
                tool_choice: Some(json!({"type": "auto"})),
                reasoning_effort: Some("off".to_string()),
            },
        );
        let client = deepseek_request_boundary_client("https://api.deepseek.com/v1", server.uri());
        let prepared = client
            .prepare_outbound_request(request.clone(), true)
            .expect("core request prepares through the production seam");
        let prepared_bytes = serde_json::to_vec(&prepared.body).expect("prepared body serializes");

        let mut stream = client
            .create_message_stream(request)
            .await
            .expect("production transport accepts the core request");
        while let Some(event) = stream.next().await {
            event.expect("captured SSE response remains valid");
        }

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].body, prepared_bytes);
        let captured = std::str::from_utf8(&requests[0].body).expect("request body is UTF-8 JSON");
        assert!(
            captured.contains(
                r#""parameters":{"zeta":{"type":"string"},"alpha":{"type":"number"},"type":"object"}"#
            ),
            "nested core-owned JSON order drifted: {captured}"
        );
    }

    #[tokio::test]
    async fn ollama_cloud_uses_authenticated_openai_compatible_v1_wire() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header(
                "authorization",
                "Bearer ollama-cloud-request-boundary-key",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-ollama-cloud-request-boundary",
                "object": "chat.completion",
                "model": "gpt-oss:120b",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            })))
            .expect(5)
            .mount(&server)
            .await;

        let client = ollama_cloud_request_boundary_client(server.uri());
        for requested in ["off", "low", "medium", "high", "max"] {
            client
                .create_message(MessageRequest {
                    model: "gpt-oss:120b".to_string(),
                    messages: vec![Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "Ollama Cloud request boundary".to_string(),
                            cache_control: None,
                        }],
                    }],
                    max_tokens: 64,
                    system: None,
                    tools: None,
                    tool_choice: None,
                    metadata: None,
                    thinking: None,
                    reasoning_effort: Some(requested.to_string()),
                    stream: Some(false),
                    temperature: None,
                    top_p: None,
                })
                .await
                .expect("Ollama Cloud request succeeds");
        }

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 5);
        for (request, expected) in requests
            .iter()
            .zip(["none", "low", "medium", "high", "max"])
        {
            let body: Value = serde_json::from_slice(&request.body).expect("captured request JSON");
            assert_eq!(body["model"], "gpt-oss:120b");
            assert_eq!(body["reasoning_effort"], expected);
            assert!(
                body.get("think").is_none(),
                "native Ollama field leaked: {body}"
            );
            assert!(
                body.get("thinking").is_none(),
                "foreign field leaked: {body}"
            );
        }
    }

    // This synchronous guard deliberately spans every await: the assertions
    // require exclusive access to process-global retry state for the full call.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn cache_free_message_call_neither_reads_nor_writes_global_cache() {
        let _retry_guard = codewhale_runtime::retry_status::test_guard();
        codewhale_runtime::retry_status::clear();
        codewhale_runtime::retry_status::clear_rate_limit();
        codewhale_runtime::retry_status::start(7, Duration::from_secs(60), "foreground sentinel");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-cache-free-provider",
                "object": "chat.completion",
                "model": "deepseek-v4-pro",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "provider result"},
                    "finish_reason": "stop"
                }],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_request_boundary_client("https://api.deepseek.com/v1", server.uri());
        codewhale_runtime::retry_status::note_rate_limit(&client.rate_limit_scope(), Duration::from_secs(60));
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "preview-router-cache-isolation-regression".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 128,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("off".to_string()),
            stream: Some(false),
            temperature: Some(0.0),
            top_p: None,
        };
        let prepared = client
            .prepare_outbound_request(request.clone(), false)
            .expect("request prepares");
        let wire_body = serde_json::to_vec(&prepared.body).expect("wire body serializes");
        let cache_key = codewhale_runtime::llm_response_cache::ResponseCache::make_key(
            client.api_provider.as_str(),
            &client.base_url,
            client.path_suffix.as_deref(),
            &client.api_key,
            &wire_body,
        );
        codewhale_runtime::llm_response_cache::response_cache().put(
            cache_key,
            MessageResponse {
                id: "cached-sentinel-must-survive".to_string(),
                r#type: "message".to_string(),
                role: "assistant".to_string(),
                content: Vec::new(),
                model: "deepseek-v4-pro".to_string(),
                stop_reason: Some("end_turn".to_string()),
                stop_sequence: None,
                container: None,
                usage: Default::default(),
            },
        );

        let response = client
            .create_message_without_response_cache(request)
            .await
            .expect("cache-free provider call succeeds");
        assert_eq!(response.id, "chatcmpl-cache-free-provider");
        assert_eq!(
            codewhale_runtime::llm_response_cache::response_cache()
                .get(&cache_key)
                .expect("sentinel remains")
                .id,
            "cached-sentinel-must-survive"
        );
        match codewhale_runtime::retry_status::snapshot() {
            codewhale_runtime::retry_status::RetryState::Active(banner) => {
                assert_eq!(banner.attempt, 7);
                assert_eq!(banner.reason, "foreground sentinel");
            }
            state => panic!("isolated success mutated retry state: {state:?}"),
        }
        assert!(
            codewhale_runtime::retry_status::rate_limit_remaining(&client.rate_limit_scope()).is_some(),
            "isolated success must not clear the foreground provider pause"
        );
        codewhale_runtime::retry_status::clear();
        codewhale_runtime::retry_status::clear_rate_limit();
    }

    // This synchronous guard deliberately spans every await: the assertions
    // require exclusive access to process-global retry state for the full call.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn cache_free_classifier_429_does_not_publish_global_retry_or_rate_limit_state() {
        let _retry_guard = codewhale_runtime::retry_status::test_guard();
        codewhale_runtime::retry_status::clear();
        codewhale_runtime::retry_status::clear_rate_limit();
        codewhale_runtime::retry_status::start(9, Duration::from_secs(60), "foreground sentinel 429");

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "120")
                    .set_body_string("rate limited"),
            )
            .expect(1)
            .mount(&server)
            .await;
        let mut client =
            deepseek_request_boundary_client("https://api.deepseek.com/v1", server.uri());
        client.retry.enabled = false;
        client.retry.max_retries = 0;
        codewhale_runtime::retry_status::note_rate_limit(&client.rate_limit_scope(), Duration::from_secs(60));
        let request = MessageRequest {
            model: "deepseek-v4-pro".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "preview-router-429-isolation-regression".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 64,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: Some("off".to_string()),
            stream: Some(false),
            temperature: Some(0.0),
            top_p: None,
        };
        let error = client
            .create_message_without_response_cache(request)
            .await
            .expect_err("429 must fail when isolated retries are disabled");
        assert!(
            matches!(
                error.downcast_ref::<LlmError>(),
                Some(LlmError::RateLimited { .. })
            ),
            "{error:#}"
        );
        match codewhale_runtime::retry_status::snapshot() {
            codewhale_runtime::retry_status::RetryState::Active(banner) => {
                assert_eq!(banner.attempt, 9);
                assert_eq!(banner.reason, "foreground sentinel 429");
            }
            state => panic!("isolated 429 mutated retry state: {state:?}"),
        }
        let remaining = codewhale_runtime::retry_status::rate_limit_remaining(&client.rate_limit_scope())
            .expect("foreground provider pause remains");
        assert!(
            remaining < Duration::from_secs(70),
            "classifier Retry-After must not extend the global pause: {remaining:?}"
        );
        codewhale_runtime::retry_status::clear();
        codewhale_runtime::retry_status::clear_rate_limit();
    }

    async fn assert_deepseek_strict_request_route_boundary(streaming: bool) {
        for (route_base_url, strict, expected_path, expected_wire_strict) in [
            (
                "https://api.deepseek.com/beta",
                false,
                "/v1/chat/completions",
                None,
            ),
            (
                "https://api.deepseek.com/beta",
                true,
                "/beta/chat/completions",
                Some(true),
            ),
            (
                "https://api.deepseek.com/v1",
                true,
                "/v1/chat/completions",
                None,
            ),
        ] {
            let (captured_path, body) =
                capture_deepseek_chat_request(route_base_url, strict, streaming).await;
            assert_eq!(captured_path, expected_path, "{route_base_url} {body}");
            assert_eq!(
                body.pointer("/tools/0/function/strict")
                    .and_then(Value::as_bool),
                expected_wire_strict,
                "{route_base_url} {body}"
            );
        }
    }

    fn k3_request_fixture(model: &str, effort: Option<&str>, stream: bool) -> MessageRequest {
        MessageRequest {
            model: model.to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "request-boundary fixture".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 64,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: effort.map(str::to_string),
            stream: Some(stream),
            temperature: Some(0.25),
            top_p: Some(0.75),
        }
    }

    async fn capture_moonshot_chat_request(
        route_base_url: &str,
        model: &str,
        effort: Option<&str>,
        streaming: bool,
    ) -> Value {
        let request = k3_request_fixture(model, effort, streaming);
        capture_moonshot_chat_request_body(route_base_url, model, request).await
    }

    async fn capture_moonshot_chat_request_body(
        route_base_url: &str,
        model: &str,
        request: MessageRequest,
    ) -> Value {
        let streaming = request.stream == Some(true);
        let server = MockServer::start().await;
        let response = if streaming {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: [DONE]\n\n")
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-k3-request-boundary",
                "object": "chat.completion",
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            }))
        };
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;

        let client = moonshot_request_boundary_client(route_base_url, model, server.uri());

        if streaming {
            let mut stream = client
                .create_message_stream(request)
                .await
                .expect("streaming request succeeds");
            while let Some(event) = stream.next().await {
                event.expect("captured SSE response remains valid");
            }
        } else {
            client
                .create_message(request)
                .await
                .expect("non-streaming request succeeds");
        }

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 1);
        serde_json::from_slice(&requests[0].body).expect("captured request JSON")
    }

    async fn capture_route_chat_request_body(
        model: &str,
        request: MessageRequest,
        client_for_transport: impl FnOnce(String) -> CodewhaleClient,
    ) -> (String, Value) {
        let streaming = request.stream == Some(true);
        let server = MockServer::start().await;
        let response = if streaming {
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string("data: [DONE]\n\n")
        } else {
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "chatcmpl-provider-request-boundary",
                "object": "chat.completion",
                "model": model,
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "total_tokens": 2
                }
            }))
        };
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;

        let client = client_for_transport(server.uri());
        if streaming {
            let mut stream = client
                .create_message_stream(request)
                .await
                .expect("streaming request succeeds");
            while let Some(event) = stream.next().await {
                event.expect("captured SSE response remains valid");
            }
        } else {
            client
                .create_message(request)
                .await
                .expect("non-streaming request succeeds");
        }

        let requests = server.received_requests().await.expect("recorded request");
        assert_eq!(requests.len(), 1);
        (
            requests[0].url.path().to_string(),
            serde_json::from_slice(&requests[0].body).expect("captured request JSON"),
        )
    }

    async fn capture_zai_chat_request(
        route_base_url: &str,
        model: &str,
        effort: Option<&str>,
        streaming: bool,
    ) -> (String, Value) {
        capture_route_chat_request_body(
            model,
            k3_request_fixture(model, effort, streaming),
            |uri| zai_request_boundary_client(route_base_url, model, uri),
        )
        .await
    }

    async fn capture_minimax_chat_request(
        route_base_url: &str,
        model: &str,
        effort: Option<&str>,
        streaming: bool,
    ) -> (String, Value) {
        capture_route_chat_request_body(
            model,
            k3_request_fixture(model, effort, streaming),
            |uri| minimax_request_boundary_client(route_base_url, model, uri),
        )
        .await
    }

    fn modelstudio_request_boundary_client(
        route_base_url: &str,
        model: &str,
        transport_base_url: String,
    ) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut client = CodewhaleClient::new(&{
            let __cw_config_init_5_0 = Some("modelstudio-token-plan".to_string());
            let __cw_config_init_5_1 = Some(ProvidersConfig {
                modelstudio_token_plan: ProviderConfig {
                    api_key: Some("modelstudio-request-boundary-key".to_string()),
                    base_url: Some(route_base_url.to_string()),
                    model: Some(model.to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_5_value: Config = Config::default();
            __cw_config_init_5_value.provider = __cw_config_init_5_0;
            __cw_config_init_5_value.providers = __cw_config_init_5_1;
            __cw_config_init_5_value
        })
        .expect("Model Studio request-boundary client");
        assert_eq!(client.base_url, route_base_url);
        client.test_chat_transport_base_url = Some(transport_base_url);
        client
    }

    async fn capture_modelstudio_chat_request(
        route_base_url: &str,
        model: &str,
        effort: Option<&str>,
        streaming: bool,
    ) -> (String, Value) {
        capture_route_chat_request_body(
            model,
            k3_request_fixture(model, effort, streaming),
            |uri| modelstudio_request_boundary_client(route_base_url, model, uri),
        )
        .await
    }

    async fn assert_modelstudio_request_truth(streaming: bool) {
        // Token Plan and Coding Plan share DashScope's reasoning controls on
        // their OpenAI-compatible Chat Completions endpoints — but the fields
        // are model-specific, not provider-wide.
        for base_url in [
            codewhale_config::descriptors::defaults::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL,
            codewhale_config::descriptors::defaults::DEFAULT_MODELSTUDIO_CODING_PLAN_BASE_URL,
        ] {
            // The default model, qwen3.8-max, is thinking-only: the bundled
            // catalog records it as `thinking: always_on`, and
            // qwen3.8-max-preview has effort/budget options with no toggle.
            // Neither accepts an enable/disable switch, so CodeWhale must not
            // send one — not even `false` for an explicit `off`. This assertion
            // used to pin the opposite; PR #5233 caught it.
            for effort in [None, Some("off"), Some("high"), Some("max")] {
                let (path, body) = capture_modelstudio_chat_request(
                    base_url,
                    codewhale_config::descriptors::defaults::DEFAULT_MODELSTUDIO_TOKEN_PLAN_MODEL,
                    effort,
                    streaming,
                )
                .await;
                assert_eq!(path, "/v1/chat/completions");
                assert!(
                    body.get("enable_thinking").is_none(),
                    "{base_url} {effort:?}: {body}"
                );
                assert!(
                    body.get("thinking").is_none(),
                    "{base_url} {effort:?}: {body}"
                );
                assert!(
                    body.get("reasoning_effort").is_none(),
                    "{base_url} {effort:?}: {body}"
                );
            }

            // A hybrid model does get the documented switch, plus
            // `preserve_thinking` so the next turn keeps its trace.
            for (effort, enabled) in [(None, true), (Some("high"), true), (Some("off"), false)] {
                let (_, body) =
                    capture_modelstudio_chat_request(base_url, "qwen3.7-plus", effort, streaming)
                        .await;
                assert_eq!(
                    body["enable_thinking"],
                    json!(enabled),
                    "{base_url} {effort:?}: {body}"
                );
                assert_eq!(
                    body["preserve_thinking"],
                    json!(enabled),
                    "{base_url} {effort:?}: {body}"
                );
                // The hybrid Qwen families have no effort ladder on the wire.
                assert!(
                    body.get("reasoning_effort").is_none(),
                    "{base_url} {effort:?}: {body}"
                );
            }

            // DeepSeek-V4 is one of the two families with a documented effort
            // ladder (`high` / `max`).
            let (_, deepseek) = capture_modelstudio_chat_request(
                base_url,
                "deepseek-v4-pro",
                Some("xhigh"),
                streaming,
            )
            .await;
            assert_eq!(
                deepseek["enable_thinking"],
                json!(true),
                "{base_url}: {deepseek}"
            );
            assert_eq!(
                deepseek["reasoning_effort"],
                json!("max"),
                "{base_url}: {deepseek}"
            );
        }

        // Fail closed: the same provider identity pointed at a custom gateway
        // must not be handed Alibaba's dialect.
        let (_, proxied) = capture_modelstudio_chat_request(
            "https://proxy.example/v1",
            "qwen3.7-plus",
            Some("high"),
            streaming,
        )
        .await;
        assert!(proxied.get("enable_thinking").is_none(), "{proxied}");
        assert!(proxied.get("preserve_thinking").is_none(), "{proxied}");
        assert!(proxied.get("reasoning_effort").is_none(), "{proxied}");
    }
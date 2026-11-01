
    #[tokio::test]
    async fn fetch_catalog_delta_rejects_oversized_bodies_and_rosters() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                "x".repeat(PROVIDER_CATALOG_MAX_RESPONSE_BYTES + 1),
                "application/json",
            ))
            .mount(&server)
            .await;
        assert_eq!(
            openrouter_client_for(&server)
                .fetch_catalog_delta()
                .await
                .expect_err("oversized body"),
            CatalogRefreshError::InvalidResponse
        );

        let server = MockServer::start().await;
        let rows: Vec<_> = (0..=PROVIDER_CATALOG_MAX_ROWS)
            .map(|index| json!({"id": format!("synthetic-model-{index}")}))
            .collect();
        mount_models_json(&server, 200, json!({"data": rows})).await;
        assert_eq!(
            openrouter_client_for(&server)
                .fetch_catalog_delta()
                .await
                .expect_err("oversized roster"),
            CatalogRefreshError::InvalidResponse
        );
    }

    #[tokio::test]
    async fn refresh_catalog_cache_records_success_then_preserves_rows_on_failure() {
        // First refresh succeeds and caches live rows.
        let server = MockServer::start().await;
        mount_models_json(
            &server,
            200,
            json!({"data": [{"id": "synthetic-model-gamma"}]}),
        )
        .await;
        let client = openrouter_client_for(&server);
        let mut cache = ProviderCatalogCache::new();

        let status = client.refresh_catalog_cache(&mut cache, 3600).await;
        assert_eq!(status, CatalogStatus::Fresh);
        let fp = base_url_fingerprint(&server.uri());
        let cached = cache.get("openrouter", &fp).expect("cached entry");
        assert_eq!(cached.offerings.len(), 1);
        assert_eq!(cached.offerings[0].wire_model_id, "synthetic-model-gamma");

        // A later failing refresh on the same base URL flips status to Failed
        // but PRESERVES the rows.
        server.reset().await;
        mount_models_json(&server, 401, json!({"error": "denied"})).await;
        let status = client.refresh_catalog_cache(&mut cache, 3600).await;
        assert!(matches!(
            status,
            CatalogStatus::Failed {
                reason: CatalogRefreshError::Unauthorized,
                ..
            }
        ));
        let cached = cache.get("openrouter", &fp).expect("entry still present");
        assert_eq!(
            cached.offerings.len(),
            1,
            "rows from the prior success must survive a failed refresh"
        );
        assert!(matches!(cached.status, CatalogStatus::Failed { .. }));

        // #4139: failed/stale rows must still publish into ProviderLake so
        // pickers keep live coverage instead of dropping back to bundled-only.
        let visible = cache.all_visible_offerings(now_unix());
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].wire_model_id, "synthetic-model-gamma");
        assert!(
            cache.all_fresh_offerings(now_unix()).is_empty(),
            "Failed entries are not fresh, but they remain visible"
        );
    }

    #[tokio::test]
    async fn invalid_live_prices_fail_refresh_and_preserve_each_provider_last_known_good() {
        let openrouter_server = MockServer::start().await;
        mount_models_json(
            &openrouter_server,
            200,
            json!({"data": [{
                "id": "synthetic/openrouter-priced",
                "pricing": {"prompt": "0.000001", "completion": "0.000002"}
            }]}),
        )
        .await;
        let openrouter = openrouter_client_for(&openrouter_server);
        let mut openrouter_cache = ProviderCatalogCache::new();
        assert_eq!(
            openrouter
                .refresh_catalog_cache(&mut openrouter_cache, 3_600)
                .await,
            CatalogStatus::Fresh
        );
        let openrouter_fp = base_url_fingerprint(&openrouter_server.uri());
        let openrouter_lkg = openrouter_cache
            .get("openrouter", &openrouter_fp)
            .expect("OpenRouter LKG")
            .offerings
            .clone();

        openrouter_server.reset().await;
        mount_models_json(
            &openrouter_server,
            200,
            json!({"data": [{
                "id": "synthetic/openrouter-priced",
                "pricing": {"prompt": "1e308", "completion": "0.000002"}
            }]}),
        )
        .await;
        assert!(matches!(
            openrouter
                .refresh_catalog_cache(&mut openrouter_cache, 3_600)
                .await,
            CatalogStatus::Failed {
                reason: CatalogRefreshError::InvalidResponse
            }
        ));
        assert_eq!(
            openrouter_cache
                .get("openrouter", &openrouter_fp)
                .expect("preserved OpenRouter LKG")
                .offerings,
            openrouter_lkg
        );

        // Same plumbing for an ordinary custom host: the generic branch
        // ignores unknown pricing fields, so the failure injector here is a
        // body that is not a model list at all. Absurd-price rejection stays
        // covered at the Baseten builder's own fixture tests.
        let custom_server = MockServer::start().await;
        mount_models_json(
            &custom_server,
            200,
            json!({"data": [{
                "id": "synthetic/custom-model"
            }]}),
        )
        .await;
        let custom = custom_mock_client_for_identity(&custom_server, "custom-lkg");
        let mut custom_cache = ProviderCatalogCache::new();
        assert_eq!(
            custom.refresh_catalog_cache(&mut custom_cache, 3_600).await,
            CatalogStatus::Fresh
        );
        let custom_fp = base_url_fingerprint(&format!("{}/v1", custom_server.uri()));
        let custom_lkg = custom_cache
            .get("custom-lkg", &custom_fp)
            .expect("custom LKG")
            .offerings
            .clone();

        custom_server.reset().await;
        mount_models_json(&custom_server, 200, json!("not a model list")).await;
        assert!(matches!(
            custom.refresh_catalog_cache(&mut custom_cache, 3_600).await,
            CatalogStatus::Failed {
                reason: CatalogRefreshError::InvalidResponse
            }
        ));
        assert_eq!(
            custom_cache
                .get("custom-lkg", &custom_fp)
                .expect("preserved custom LKG")
                .offerings,
            custom_lkg
        );
    }

    #[tokio::test]
    async fn live_catalog_is_scoped_by_base_url_fingerprint() {
        // Same provider, two different base URLs -> two distinct cache scopes.
        let server_a = MockServer::start().await;
        mount_models_json(&server_a, 200, json!({"data": [{"id": "synthetic-a"}]})).await;
        let server_b = MockServer::start().await;
        mount_models_json(&server_b, 200, json!({"data": [{"id": "synthetic-b"}]})).await;

        let mut cache = ProviderCatalogCache::new();
        openrouter_client_for(&server_a)
            .refresh_catalog_cache(&mut cache, 3600)
            .await;
        openrouter_client_for(&server_b)
            .refresh_catalog_cache(&mut cache, 3600)
            .await;

        let fp_a = base_url_fingerprint(&server_a.uri());
        let fp_b = base_url_fingerprint(&server_b.uri());
        assert_ne!(
            fp_a, fp_b,
            "different base URLs must fingerprint differently"
        );
        assert_eq!(
            cache.get("openrouter", &fp_a).expect("a").offerings[0].wire_model_id,
            "synthetic-a"
        );
        assert_eq!(
            cache.get("openrouter", &fp_b).expect("b").offerings[0].wire_model_id,
            "synthetic-b"
        );
    }

    #[tokio::test]
    async fn static_rows_survive_a_live_refresh_failure() {
        // Bundled/static rows compile through even when the live layer is empty
        // (the state after a failed refresh with no prior success).
        let server = MockServer::start().await;
        mount_models_json(&server, 503, json!({"error": "down"})).await;
        let client = openrouter_client_for(&server);
        let mut cache = ProviderCatalogCache::new();
        let status = client.refresh_catalog_cache(&mut cache, 3600).await;
        assert!(matches!(status, CatalogStatus::Failed { .. }));

        let static_row = CatalogOffering {
            provider: "openrouter".to_string(),
            wire_model_id: "synthetic-static".to_string(),
            endpoint_key: "chat".to_string(),
            ..CatalogOffering::default()
        };
        let fp = base_url_fingerprint(&server.uri());
        let fresh_live: Vec<CatalogOffering> = cache
            .get("openrouter", &fp)
            .filter(|entry| entry.is_fresh(now_unix()))
            .map(|entry| entry.offerings.clone())
            .unwrap_or_default();
        let snapshot = codewhale_config::catalog::CatalogCompiler::new()
            .with_bundled(vec![static_row])
            .with_live(fresh_live)
            .compile();
        assert!(
            snapshot
                .offerings
                .iter()
                .any(|offering| offering.wire_model_id == "synthetic-static"),
            "static fallback row must remain available after a failed refresh"
        );
    }

    #[test]
    fn client_route_envelope_freezes_saved_minimax_billing_mode_and_wire_model() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = {
            let __cw_config_init_0_0 = Some("minimax".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                minimax: ProviderConfig {
                    api_key: Some("test-key".to_string()),
                    mode: Some("pay-as-you-go".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        let client = CodewhaleClient::new(&config).expect("MiniMax client");
        let dispatched_at =
            chrono::DateTime::<chrono::Utc>::from_timestamp(1_234, 0).expect("timestamp");
        let route = client.effective_route_envelope("MiniMax-M3", dispatched_at);

        assert_eq!(route.provider, ProviderKind::Minimax);
        assert_eq!(route.provider_identity, "minimax");
        assert_eq!(route.model, "MiniMax-M3");
        assert_eq!(
            route.billing_surface.as_deref(),
            Some(crate::pricing::MINIMAX_PAYG_BILLING_SURFACE)
        );
        assert_eq!(
            route.billing_mode,
            crate::cost_status::RouteBillingMode::Metered
        );
        assert_eq!(route.dispatched_at.timestamp(), 1_234);
    }

    #[test]
    fn sanitize_thinking_mode_counts_reasoning_replay_across_assistant_turns() {
        // Multi-turn body that mimics two prior tool-calling rounds: each
        // assistant message carries its `reasoning_content`. The sanitizer
        // should keep all of them and the count helper should tally bytes
        // across every assistant message.
        let mut body = json!({
            "model": "deepseek-v4-pro",
            "messages": [
                { "role": "system", "content": "you are helpful" },
                { "role": "user", "content": "step 1" },
                {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "I need to call tool A first.",
                    "tool_calls": [{ "id": "1", "type": "function" }]
                },
                { "role": "tool", "tool_call_id": "1", "content": "ok" },
                {
                    "role": "assistant",
                    "content": "",
                    "reasoning_content": "Now I call tool B.",
                    "tool_calls": [{ "id": "2", "type": "function" }]
                },
                { "role": "tool", "tool_call_id": "2", "content": "ok" },
                { "role": "user", "content": "step 2" }
            ]
        });

        let approx_tokens = sanitize_thinking_mode_messages(
            &mut body,
            "deepseek-v4-pro",
            Some("max"),
            ProviderKind::Deepseek,
        )
        .expect("multi-turn thinking-mode conversation should report replay tokens");
        // ~4 chars/token; 46 bytes of reasoning -> 11 tokens.
        assert_eq!(approx_tokens, 11);

        let chars = count_reasoning_replay_chars(&body);
        // "I need to call tool A first." (28) + "Now I call tool B." (18) = 46
        assert_eq!(chars, 46);

        // No assistant messages should have lost or had their reasoning_content blanked.
        let messages = body["messages"].as_array().unwrap();
        let assistant_with_reasoning: usize = messages
            .iter()
            .filter(|m| m["role"] == "assistant")
            .filter(|m| {
                m["reasoning_content"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            })
            .count();
        assert_eq!(assistant_with_reasoning, 2);
    }

    /// Issue #30: when no thinking-mode replay applies (non-thinking model or
    /// empty conversation), the sanitizer returns `None` so the footer chip
    /// stays hidden.
    #[test]
    fn sanitize_thinking_mode_returns_none_for_non_thinking_model() {
        let mut body = json!({
            "model": "deepseek-v4-flash",
            "messages": [
                { "role": "user", "content": "hi" }
            ]
        });
        let result = sanitize_thinking_mode_messages(
            &mut body,
            "deepseek-v4-flash",
            None,
            ProviderKind::Deepseek,
        );
        // reasoning_effort is None → no thinking injection, result is None
        assert!(result.is_none());
    }

    #[test]
    fn sanitize_thinking_mode_counts_substituted_placeholder() {
        // An assistant tool-call message is missing reasoning_content; the
        // sanitizer must inject the placeholder, and the count helper must
        // include the placeholder in the total (since it's in the wire
        // payload that ships to DeepSeek).
        let mut body = json!({
            "model": "deepseek-v4-pro",
            "messages": [
                { "role": "user", "content": "hi" },
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{ "id": "1", "type": "function" }]
                }
            ]
        });

        sanitize_thinking_mode_messages(
            &mut body,
            "deepseek-v4-pro",
            Some("max"),
            ProviderKind::Deepseek,
        );

        let chars = count_reasoning_replay_chars(&body);
        // "(reasoning omitted)" is 19 bytes.
        assert_eq!(chars, 19);
    }

    #[test]
    fn sanitize_thinking_mode_skips_generic_openai_provider() {
        // #1542 intent (narrowed by #1739/#1694): the sanitizer only skips for
        // a *genuine non-DeepSeek* model on the generic openai provider. A
        // DeepSeek reasoning model on the openai provider still gets sanitized
        // (see chat.rs `deepseek_model_on_openai_provider_still_replays_*`).
        let mut body = json!({
            "model": "qwen3-coder",
            "messages": [
                { "role": "user", "content": "hi" },
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{ "id": "1", "type": "function" }]
                }
            ]
        });

        let result = sanitize_thinking_mode_messages(
            &mut body,
            "qwen3-coder",
            Some("max"),
            ProviderKind::Openai,
        );

        assert!(result.is_none());
        let assistant = body["messages"]
            .as_array()
            .and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message["role"] == "assistant")
            })
            .expect("assistant message");
        assert!(
            assistant.get("reasoning_content").is_none(),
            "generic OpenAI-compatible provider payload must not get reasoning_content (#1542)"
        );
    }

    #[test]
    fn sanitize_thinking_mode_keeps_tool_call_placeholder_after_new_user_turn() {
        let mut body = json!({
            "model": "deepseek-v4-pro",
            "messages": [
                { "role": "user", "content": "step 1" },
                {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{ "id": "1", "type": "function" }]
                },
                { "role": "tool", "tool_call_id": "1", "content": "ok" },
                { "role": "user", "content": "step 2" }
            ]
        });

        sanitize_thinking_mode_messages(
            &mut body,
            "deepseek-v4-pro",
            Some("max"),
            ProviderKind::Deepseek,
        );

        let messages = body["messages"].as_array().unwrap();
        let assistant = messages
            .iter()
            .find(|m| m["role"] == "assistant")
            .expect("assistant tool-call message");
        assert_eq!(
            assistant.get("reasoning_content").and_then(Value::as_str),
            Some("(reasoning omitted)")
        );
    }

    #[test]
    fn token_bucket_enforces_delay_when_empty() {
        let now = Instant::now();
        let mut bucket = TokenBucket {
            enabled: true,
            capacity: 1.0,
            tokens: 1.0,
            refill_per_sec: 2.0,
            last_refill: now,
        };

        assert!(bucket.delay_until_available(1.0).is_none());
        let delay = bucket
            .delay_until_available(1.0)
            .expect("bucket should require refill delay");
        assert!(
            delay >= Duration::from_millis(400) && delay <= Duration::from_millis(600),
            "unexpected refill delay: {delay:?}"
        );
    }

    /// Every queued waiter must be given a *distinct* wake time. `client.rs`
    /// releases the bucket lock before sleeping (`wait_for_rate_limit`), and a
    /// clone of the client shares one `Arc<AsyncMutex<TokenBucket>>` across
    /// sub-agents, so if the bucket hands two waiters the same delay they both
    /// wake at the same instant and fire together — a burst the configured
    /// limit was supposed to prevent.
    #[test]
    fn token_bucket_queues_concurrent_waiters_instead_of_stacking_them() {
        let now = Instant::now();
        let mut bucket = TokenBucket {
            enabled: true,
            capacity: 1.0,
            tokens: 1.0,
            refill_per_sec: 1.0,
            last_refill: now,
        };

        assert!(bucket.delay_until_available(1.0).is_none());
        let first = bucket
            .delay_until_available(1.0)
            .expect("second caller waits for a refill");
        let second = bucket
            .delay_until_available(1.0)
            .expect("third caller waits for a refill");

        assert!(
            first >= Duration::from_millis(900) && first <= Duration::from_millis(1100),
            "unexpected first wait: {first:?}"
        );
        assert!(
            second >= Duration::from_millis(1900) && second <= Duration::from_millis(2100),
            "third caller must queue behind the second, not wake with it: {second:?}"
        );
    }

    #[test]
    fn stream_buffer_pool_reuses_released_buffers() {
        let mut first = acquire_stream_buffer();
        first.extend_from_slice(b"hello");
        let released_capacity = first.capacity();
        release_stream_buffer(first);

        let second = acquire_stream_buffer();
        assert!(second.is_empty());
        assert!(
            second.capacity() >= released_capacity,
            "pooled buffer capacity should be reused"
        );
    }

    #[test]
    fn base_url_scenario() {
        // Scenario consolidation of: base_url_security_rejects_insecure_non_local_http, base_url_security_errors_redact_sensitive_url_parts, base_url_security_allows_localhost_http, base_url_security_allows_non_local_http_with_explicit_opt_in
        // from base_url_security_rejects_insecure_non_local_http
        {
            let _lock = crate::test_support::lock_test_env();
            let _guard = AllowInsecureHttpEnvGuard::capture();
            unsafe { std::env::remove_var(ALLOW_INSECURE_HTTP_ENV) };

            let err = validate_base_url_security("http://api.deepseek.com", false)
                .expect_err("non-local insecure HTTP should be rejected");
            assert!(err.to_string().contains("Refusing insecure base URL"));
        }
        // from base_url_security_errors_redact_sensitive_url_parts
        {
            let _lock = crate::test_support::lock_test_env();
            let _guard = AllowInsecureHttpEnvGuard::capture();
            unsafe { std::env::remove_var(ALLOW_INSECURE_HTTP_ENV) };

            let err = validate_base_url_security(
                "http://user:secret@example.com/v1?api_key=sk-test&ok=1",
                false,
            )
            .expect_err("non-local insecure HTTP should be rejected");
            let message = err.to_string();

            assert!(message.contains("http://***:***@example.com/v1?api_key=***&ok=1"));
            assert!(!message.contains("user:secret"));
            assert!(!message.contains("sk-test"));
        }
        // from base_url_security_allows_localhost_http
        {
            let _lock = crate::test_support::lock_test_env();
            let _guard = AllowInsecureHttpEnvGuard::capture();
            unsafe { std::env::remove_var(ALLOW_INSECURE_HTTP_ENV) };

            for url in [
                "http://localhost:8080",
                "http://LOCALHOST:8080",
                "http://127.0.0.1:8080",
                "http://127.0.0.2:8080",
                "http://[::1]:8080",
                "https://provider.example/v1",
            ] {
                assert!(validate_base_url_security(url, false).is_ok(), "{url}");
            }
            for url in [
                "http://localhost.attacker.example/v1",
                "http://127.0.0.1.attacker.example/v1",
                "http://localhost@attacker.example/v1",
                "http://127.0.0.1@attacker.example/v1",
                "HTTP://localhost.attacker.example/v1",
            ] {
                assert!(validate_base_url_security(url, false).is_err(), "{url}");
                assert!(validate_base_url_security(url, true).is_ok(), "{url}");
            }
            for url in ["https://", "http://[::1", "file:///tmp/provider"] {
                assert!(validate_base_url_security(url, true).is_err(), "{url}");
            }
        }
        // from base_url_security_allows_non_local_http_with_explicit_opt_in
        {
            let _lock = crate::test_support::lock_test_env();
            let _guard = AllowInsecureHttpEnvGuard::capture();
            unsafe { std::env::set_var(ALLOW_INSECURE_HTTP_ENV, "1") };

            assert!(validate_base_url_security("http://192.168.0.110:8000/v1", false).is_ok());
        }
        // #5991: a provider that opts in via its [providers.<name>] table may
        // use a plain-HTTP base URL without any env var. This is the
        // 0.9.11-and-earlier behavior the key silently stopped providing.
        {
            let _lock = crate::test_support::lock_test_env();
            let _guard = AllowInsecureHttpEnvGuard::capture();
            unsafe { std::env::remove_var(ALLOW_INSECURE_HTTP_ENV) };

            assert!(validate_base_url_security("http://192.168.0.110:8000/v1", true).is_ok());
            // The refusal message now leads with the config key.
            let err = validate_base_url_security("http://api.deepseek.com", false)
                .expect_err("still refused without either opt-in");
            assert!(err.to_string().contains("allow_insecure_http = true"));
        }
    }

    struct AllowInsecureHttpEnvGuard {
        prior: Option<std::ffi::OsString>,
        prior_legacy: Option<std::ffi::OsString>,
    }
    impl AllowInsecureHttpEnvGuard {
        fn capture() -> Self {
            let guard = Self {
                prior: std::env::var_os(ALLOW_INSECURE_HTTP_ENV),
                prior_legacy: std::env::var_os(LEGACY_ALLOW_INSECURE_HTTP_ENV),
            };
            // Clear the legacy alias so ambient shell state cannot satisfy
            // the CODEWHALE-first fallback chain behind a test's back.
            unsafe { std::env::remove_var(LEGACY_ALLOW_INSECURE_HTTP_ENV) };
            guard
        }
    }
    impl Drop for AllowInsecureHttpEnvGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(v) => unsafe { std::env::set_var(ALLOW_INSECURE_HTTP_ENV, v) },
                None => unsafe { std::env::remove_var(ALLOW_INSECURE_HTTP_ENV) },
            }
            match &self.prior_legacy {
                Some(v) => unsafe { std::env::set_var(LEGACY_ALLOW_INSECURE_HTTP_ENV, v) },
                None => unsafe { std::env::remove_var(LEGACY_ALLOW_INSECURE_HTTP_ENV) },
            }
        }
    }

    #[test]
    fn connection_health_degrades_and_recovers() {
        let now = Instant::now();
        let mut health = ConnectionHealth::default();
        assert_eq!(health.state, ConnectionState::Healthy);

        apply_request_failure(&mut health, now);
        assert_eq!(health.state, ConnectionState::Healthy);

        apply_request_failure(&mut health, now + Duration::from_millis(1));
        assert_eq!(health.state, ConnectionState::Degraded);
        assert_eq!(health.consecutive_failures, 2);

        let recovered = apply_request_success(&mut health, now + Duration::from_secs(1));
        assert!(recovered);
        assert_eq!(health.state, ConnectionState::Healthy);
        assert_eq!(health.consecutive_failures, 0);
    }

    #[test]
    fn recovery_probe_respects_cooldown() {
        let now = Instant::now();
        let mut health = ConnectionHealth {
            state: ConnectionState::Degraded,
            ..ConnectionHealth::default()
        };

        assert!(mark_recovery_probe_if_due(&mut health, now));
        assert_eq!(health.state, ConnectionState::Recovering);
        assert!(!mark_recovery_probe_if_due(
            &mut health,
            now + Duration::from_secs(1)
        ));
        assert!(mark_recovery_probe_if_due(
            &mut health,
            now + RECOVERY_PROBE_COOLDOWN + Duration::from_millis(1)
        ));
    }

    // === #103 Phase 2: HTTP/1 escape hatch ===================================

    struct ForceHttp1EnvGuard {
        prior: Option<std::ffi::OsString>,
    }
    impl ForceHttp1EnvGuard {
        fn capture() -> Self {
            Self {
                prior: std::env::var_os("DEEPSEEK_FORCE_HTTP1"),
            }
        }
    }
    impl Drop for ForceHttp1EnvGuard {
        fn drop(&mut self) {
            // Safety: scoped to test process; reverts to the captured value.
            match &self.prior {
                Some(v) => unsafe { std::env::set_var("DEEPSEEK_FORCE_HTTP1", v) },
                None => unsafe { std::env::remove_var("DEEPSEEK_FORCE_HTTP1") },
            }
        }
    }

    #[tokio::test]
    async fn configured_http2_keepalive_reaches_real_client_transport() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let config: Config = toml::from_str(
            "[stream]\nhttp2_keep_alive_interval_secs=1\nhttp2_keep_alive_timeout_secs=1\n",
        )
        .unwrap();
        let client = CodewhaleClient::http_client_builder_with_auth_mode(
            "",
            &HashMap::new(),
            ProviderKind::Deepseek,
            &url,
            WireFormat::ChatCompletions,
            true,
            false,
            &config,
            AuthHeaderSurface::Inference,
        )
        .unwrap()
        .no_proxy()
        .http2_prior_knowledge()
        .build()
        .unwrap();
        let request = tokio::spawn(async move { client.get(url).send().await });
        // Minimal HTTP/2 peer: handshake, leave the request open, and observe
        // the actual PING. No additional dependency or external provider.
        let peer = tokio::time::timeout(Duration::from_secs(5), async {
            let (mut peer, _) = listener.accept().await.unwrap();
            let mut preface = [0; 24];
            peer.read_exact(&mut preface).await.unwrap();
            assert_eq!(&preface, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
            peer.write_all(&[0, 0, 0, 4, 0, 0, 0, 0, 0]).await.unwrap();
            loop {
                let mut header = [0; 9];
                peer.read_exact(&mut header).await.unwrap();
                let len = (usize::from(header[0]) << 16)
                    | (usize::from(header[1]) << 8)
                    | usize::from(header[2]);
                assert!(len <= 65536, "bounded test frame");
                let mut body = vec![0; len];
                peer.read_exact(&mut body).await.unwrap();
                if header[3] == 4 && header[4] & 1 == 0 {
                    peer.write_all(&[0, 0, 0, 4, 1, 0, 0, 0, 0]).await.unwrap();
                }
                if header[3] == 6 && header[4] & 1 == 0 {
                    assert_eq!(len, 8);
                    return peer; // Deliberately withhold the PING ACK.
                }
            }
        })
        .await
        .expect("configured one-second interval must send a PING before the default 15 seconds");
        let result = tokio::time::timeout(Duration::from_secs(3), request).await
                    .expect("configured one-second acknowledgement timeout must end the request before default 20 seconds")
                    .unwrap();
        assert!(
            result.is_err(),
            "an unacknowledged PING must fail the request"
        );
        drop(peer); // Keep the peer open until the client's own timer fires.
    }

    #[test]
    fn force_http1_scenario() {
        // Scenario consolidation of: force_http1_unset_is_false, force_http1_truthy_values, force_http1_falsy_values
        // from force_http1_unset_is_false
        {
            let _lock = crate::test_support::lock_test_env();
            let _guard = ForceHttp1EnvGuard::capture();
            unsafe { std::env::remove_var("DEEPSEEK_FORCE_HTTP1") };
            assert!(!codewhale_config::force_http1_from_env());
        }
        // from force_http1_truthy_values
        {
            let _lock = crate::test_support::lock_test_env();
            let _guard = ForceHttp1EnvGuard::capture();
            for value in ["1", "true", "True", "YES", "on", " 1 "] {
                // Safety: serialized by lock_test_env; reverted by guard.
                unsafe { std::env::set_var("DEEPSEEK_FORCE_HTTP1", value) };
                assert!(
                    codewhale_config::force_http1_from_env(),
                    "{value:?} should be parsed as truthy",
                );
            }
        }
        // from force_http1_falsy_values
        {
            let _lock = crate::test_support::lock_test_env();
            let _guard = ForceHttp1EnvGuard::capture();
            for value in ["0", "false", "no", "off", "", "garbage", "2"] {
                unsafe { std::env::set_var("DEEPSEEK_FORCE_HTTP1", value) };
                assert!(
                    !codewhale_config::force_http1_from_env(),
                    "{value:?} should NOT be parsed as truthy"
                );
            }
        }
    }

    #[test]
    fn redact_url_for_display_masks_userinfo_and_sensitive_query_values() {
        let redacted = redact_url_for_display(
            "https://user:secret@example.com/v1?api_key=sk-test&region=us&refresh-token=abc",
        );

        assert_eq!(
            redacted,
            "https://***:***@example.com/v1?api_key=***&region=us&refresh-token=***"
        );
    }

    /// Build a DeepSeek config with an inline key/base URL plus the resolved
    /// runtime route for it. `RouteResolver` (reached through
    /// `resolve_runtime_route`) is the only producer of `ReadyRouteCandidate`,
    /// so we mint candidates the same way the engine does at switch time.
    fn deepseek_route_for_test(
        base_url: &str,
        model: &str,
    ) -> (Config, crate::route_runtime::ResolvedRuntimeRoute) {
        let config = {
            let __cw_config_init_1_0 = Some("deepseek".to_string());
            let __cw_config_init_1_1 = Some(model.to_string());
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.default_text_model = __cw_config_init_1_1;
            __cw_config_init_1_value
        }
        .with_legacy_root(Some("ds-test".to_string()), Some(base_url.to_string()));
        let route =
            crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Deepseek, Some(model))
                .expect("deepseek route should resolve");
        (config, route)
    }

    #[test]
    fn from_candidate_scenario() {
        // Scenario consolidation of: from_candidate_uses_candidate_base_url_and_wire_model, from_candidate_matches_new_when_config_agrees
        // from from_candidate_uses_candidate_base_url_and_wire_model
        {
            let (_config, route) =
                deepseek_route_for_test("https://route.example.com/v1", "deepseek-v4-pro");

            let client = CodewhaleClient::from_candidate(&route.config, &route.candidate)
                .expect("client should construct from candidate");

            // The transport is bound to the candidate, not re-derived from Config.
            assert_eq!(client.base_url, route.candidate.endpoint().base_url);
            assert_eq!(
                client.default_model,
                route.candidate.wire_model_id().as_str()
            );
        }
        // from from_candidate_matches_new_when_config_agrees
        {
            // For a normal route, the resolver writes the candidate's wire model and
            // endpoint back into `route.config`, so constructing from the candidate
            // must be byte-identical to constructing from that config. This pins the
            // "no behavior change today" guarantee for Slice A.
            let (_config, route) =
                deepseek_route_for_test("https://api.deepseek.com/v1", "deepseek-v4-pro");

            let from_new = CodewhaleClient::new(&route.config).expect("new client");
            let from_candidate = CodewhaleClient::from_candidate(&route.config, &route.candidate)
                .expect("candidate client");

            assert_eq!(from_candidate.base_url, from_new.base_url);
            assert_eq!(from_candidate.default_model, from_new.default_model);
            assert_eq!(from_candidate.api_provider, from_new.api_provider);
        }
    }

    fn route_cap_test_client(wire_format: WireFormat, limits: RouteLimits) -> CodewhaleClient {
        let config = {
            let __cw_config_init_2_0 = Some("custom".to_string());
            let __cw_config_init_2_1 = Some("DeepSeek-V4-Flash".to_string());
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.default_text_model = __cw_config_init_2_1;
            __cw_config_init_2_value
        }
        .with_legacy_root(
            Some("route-cap-test".to_string()),
            Some("https://route-cap.example/v1".to_string()),
        );
        CodewhaleClient::from_parts(
            "https://route-cap.example/v1".to_string(),
            "DeepSeek-V4-Flash".to_string(),
            wire_format,
            Some(limits),
            &config,
        )
        .expect("route cap test client")
    }

    #[test]
    fn tool_pairing_admission_uses_the_frozen_wire_protocol() {
        for wire in [
            WireFormat::ChatCompletions,
            WireFormat::AnthropicMessages,
            WireFormat::Responses,
        ] {
            let client = route_cap_test_client(wire, RouteLimits::default());
            assert!(
                client
                    .validate_tool_call_ids(["unique-a", "unique-b"])
                    .is_ok()
            );
            for ids in [["", "valid"], ["   ", "valid"], ["same", "same"]] {
                assert!(
                    client.validate_tool_call_ids(ids).is_err(),
                    "{wire:?}: {ids:?}"
                );
            }
            let result = client.validate_tool_call_ids(["call|item-a", "call|item-b"]);
            assert_eq!(result.is_err(), wire == WireFormat::Responses);
            assert_eq!(
                client.validate_tool_call_ids(["|item"]).is_err(),
                wire == WireFormat::Responses
            );
            // Each response is independent: provider reuse on another round is valid.
            assert!(client.validate_tool_call_ids(["reused"]).is_ok());
            assert!(client.validate_tool_call_ids(["reused"]).is_ok());
        }
    }

    #[test]
    fn outbound_seam_excludes_host_execution_identity_for_every_dialect() {
        let _lock = crate::test_support::lock_test_env();
        const IMAGE: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";
        for (wire, google_route) in [
            (WireFormat::ChatCompletions, false),
            (WireFormat::ChatCompletions, true),
            (WireFormat::Responses, false),
            (WireFormat::AnthropicMessages, false),
        ] {
            let model = if google_route {
                "gemini-2.5-flash"
            } else {
                "DeepSeek-V4-Flash"
            };
            let client = if google_route {
                let base_url = "https://generativelanguage.googleapis.com/v1beta/openai";
                let config = {
                    let __cw_config_init_3_0 = Some("custom".to_string());
                    let __cw_config_init_3_1 = Some(model.to_string());
                    let mut __cw_config_init_3_value: Config = Config::default();
                    __cw_config_init_3_value.provider = __cw_config_init_3_0;
                    __cw_config_init_3_value.default_text_model = __cw_config_init_3_1;
                    __cw_config_init_3_value
                }
                .with_legacy_root(Some("fixture".to_string()), Some(base_url.to_string()));
                CodewhaleClient::from_parts(
                    base_url.to_string(),
                    model.to_string(),
                    wire,
                    Some(RouteLimits::default()),
                    &config,
                )
                .unwrap()
            } else {
                route_cap_test_client(wire, RouteLimits::default())
            };
            let provider_id = if wire == WireFormat::Responses {
                "wire-call|provider-item"
            } else {
                "wire-call"
            };
            let mut request =
                translation_message_request("inspect", model.to_string(), "English", 1024);
            request.messages.extend([
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolUse {
                        id: provider_id.to_string(),
                        execution_id: Some("local-execution-sentinel".to_string()),
                        name: "read".to_string(),
                        input: json!({"path": "shot.png"}),
                        caller: Some(codewhale_models::ToolCaller {
                            caller_type: "code_execution".to_string(),
                            tool_id: Some("parent-wire".to_string()),
                        }),
                        thought_signature: Some("provider-signature".to_string()),
                    }],
                },
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: provider_id.to_string(),
                        execution_id: Some("local-execution-sentinel".to_string()),
                        content: "captured image".to_string(),
                        is_error: Some(false),
                        content_blocks: Some(vec![
                            json!({"type": "image", "mime_type": "image/png", "data": IMAGE}),
                        ]),
                    }],
                },
            ]);
            let mut legacy = request.clone();
            for block in legacy
                .messages
                .iter_mut()
                .flat_map(|message| &mut message.content)
            {
                match block {
                    ContentBlock::ToolUse { execution_id, .. }
                    | ContentBlock::ToolResult { execution_id, .. } => *execution_id = None,
                    _ => {}
                }
            }
            for streaming in [false, true] {
                let prepared = client
                    .prepare_outbound_request(request.clone(), streaming)
                    .unwrap();
                let without_local = client
                    .prepare_outbound_request(legacy.clone(), streaming)
                    .unwrap();
                assert_eq!(
                    prepared.body, without_local.body,
                    "{wire:?}, stream={streaming}"
                );
                let bytes = prepared.body.to_string();
                assert!(!bytes.contains("execution_id") && !bytes.contains("local-execution-sentinel"));
                assert!(bytes.contains("wire-call") && bytes.contains(IMAGE));
                if wire == WireFormat::ChatCompletions {
                    assert!(bytes.contains("parent-wire"));
                    assert_eq!(
                        bytes.contains("provider-signature"),
                        google_route,
                        "Google signatures remain restricted to Google's route"
                    );
                }
                if wire == WireFormat::Responses {
                    let input = prepared.body["input"].as_array().unwrap();
                    assert!(
                        input
                            .iter()
                            .any(|item| item["type"] == "function_call"
                                && item["call_id"] == "wire-call")
                    );
                    assert!(
                        input
                            .iter()
                            .any(|item| item["type"] == "function_call_output"
                                && item["call_id"] == "wire-call")
                    );
                }
            }
            assert_eq!(
                request.messages[1].content[0].tool_call_key(),
                Some(codewhale_models::ToolCallKey::Execution(
                    "local-execution-sentinel"
                ))
            );
        }
    }

    #[test]
    fn unresolved_ollama_client_waits_for_catalog_but_probe_can_bootstrap() {
        use codewhale_config::catalog::CatalogOffering;
        let _env = crate::test_support::lock_test_env();
        let _live = crate::provider_lake::lock_live_snapshot();
        let home = tempfile::tempdir().unwrap();
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", home.path());
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
        let endpoint = "http://127.0.0.1:11452/v1";
        let mut config = {
            let __cw_config_init_4_0 = Some("ollama".into());
            let mut __cw_config_init_4_value: Config = Default::default();
            __cw_config_init_4_value.provider = __cw_config_init_4_0;
            __cw_config_init_4_value
        };
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::Ollama))
            .unwrap()
            .base_url = Some(endpoint.into());
        assert!(
            CodewhaleClient::new(&config).is_err(),
            "unknown must not become a dispatch model"
        );
        let probe = CodewhaleClient::for_catalog_refresh(&config).expect("catalog bootstrap client");
        assert_eq!(probe.base_url, endpoint);
        let ticket = crate::provider_catalog_live::begin_refresh_for_identity(
            ProviderKind::Ollama,
            "ollama",
            endpoint,
        );
        let fingerprint = base_url_fingerprint(endpoint);
        let fetched_at = now_unix();
        crate::provider_catalog_live::record_success_if_current(
            &ticket,
            ProviderCatalogDelta {
                provider: "ollama".into(),
                base_url_fingerprint: fingerprint.clone(),
                fetched_at,
                offerings: ["zeta:tag", "alpha:tag"]
                    .into_iter()
                    .map(|id| CatalogOffering {
                        provider: "ollama".into(),
                        wire_model_id: id.into(),
                        endpoint_key: "chat".into(),
                        default_for_provider: id == "alpha:tag",
                        source: CatalogSource::Live {
                            base_url_fingerprint: fingerprint.clone(),
                            fetched_at,
                        },
                        ..Default::default()
                    })
                    .collect(),
            },
        );
        let client = CodewhaleClient::new(&config).expect("fresh local model client");
        assert_eq!(client.default_model, "alpha:tag");
        let route =
            crate::route_runtime::resolve_runtime_route(&config, ProviderKind::Ollama, None).unwrap();
        assert_eq!(
            client.default_model,
            route.candidate.wire_model_id().as_str()
        );
        config
            .set_provider_model_override(
                &config.test_identity_for_kind(ProviderKind::Ollama),
                Some("saved:tag".into()),
            )
            .unwrap();
        assert_eq!(
            CodewhaleClient::new(&config).unwrap().default_model,
            "saved:tag"
        );
        crate::provider_catalog_live::reset_cache_for_test();
        crate::provider_lake::clear_live_snapshot();
    }

    #[test]
    fn provider_regression_5820_ollama_config_reaches_the_wire_with_safe_output() {
        let _lock = crate::test_support::lock_test_env();
        let _canonical = crate::test_support::EnvVarGuard::remove("CODEWHALE_MAX_OUTPUT_TOKENS");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");
        let config = {
            let __cw_config_init_5_0 = Some("ollama".to_string());
            let __cw_config_init_5_1 = Some(ProvidersConfig {
                ollama: ProviderConfig {
                    model: Some("qwen2.5:7b".to_string()),
                    context_window: Some(32_768),
                    base_url: Some("http://127.0.0.1:11434".to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_5_value: Config = Default::default();
            __cw_config_init_5_value.provider = __cw_config_init_5_0;
            __cw_config_init_5_value.providers = __cw_config_init_5_1;
            __cw_config_init_5_value
        };
        let client = CodewhaleClient::new(&config).unwrap();
        assert_eq!(
            client
                .route_limits()
                .and_then(|limits| limits.context_tokens),
            Some(32_768)
        );
        assert_eq!(client.effective_max_output_tokens("qwen2.5:7b"), 8_192);
        let prepared = client
            .prepare_outbound_request(
                MessageRequest {
                    model: "qwen2.5:7b".to_string(),
                    messages: vec![Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "hello".to_string(),
                            cache_control: None,
                        }],
                    }],
                    max_tokens: 64_000,
                    system: None,
                    tools: None,
                    tool_choice: None,
                    metadata: None,
                    thinking: None,
                    reasoning_effort: None,
                    stream: Some(false),
                    temperature: None,
                    top_p: None,
                },
                false,
            )
            .unwrap();
        assert_eq!(prepared.body["max_tokens"], 8_192);
    }

    #[test]
    fn outbound_seam_clamps_every_dialect_to_the_exact_route_envelope() {
        let _lock = crate::test_support::lock_test_env();
        let _canonical = crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "384000");
        let _legacy = crate::test_support::EnvVarGuard::remove("DEEPSEEK_MAX_OUTPUT_TOKENS");

        for (limits, expected) in [
            (
                RouteLimits {
                    context_tokens: Some(327_680),
                    ..RouteLimits::default()
                },
                325_632_u64,
            ),
            (
                RouteLimits {
                    context_tokens: Some(327_680),
                    output_tokens: Some(100_000),
                    ..RouteLimits::default()
                },
                100_000,
            ),
            (
                RouteLimits {
                    context_tokens: Some(327_680),
                    output_tokens: Some(128),
                    ..RouteLimits::default()
                },
                128,
            ),
        ] {
            for (wire_format, body_field) in [
                (WireFormat::ChatCompletions, "max_tokens"),
                (WireFormat::Responses, "max_output_tokens"),
                (WireFormat::AnthropicMessages, "max_tokens"),
            ] {
                let client = route_cap_test_client(wire_format, limits);
                let prepared = client
                    .prepare_outbound_request(
                        MessageRequest {
                            model: "DeepSeek-V4-Flash".to_string(),
                            messages: vec![Message {
                                role: Role::User,
                                content: vec![ContentBlock::Text {
                                    text: "route cap".to_string(),
                                    cache_control: None,
                                }],
                            }],
                            max_tokens: 384_000,
                            system: None,
                            tools: None,
                            tool_choice: None,
                            metadata: None,
                            thinking: None,
                            reasoning_effort: Some("max".to_string()),
                            stream: Some(false),
                            temperature: None,
                            top_p: None,
                        },
                        false,
                    )
                    .expect("request prepares through preview/wire seam");
                assert_eq!(
                    prepared.body[body_field].as_u64(),
                    Some(expected),
                    "wire={wire_format:?} limits={limits:?}"
                );
            }
        }
    }

    #[test]
    fn same_protocol_model_switch_rebinds_exact_candidate_identity_and_limits() {
        let _lock = crate::test_support::lock_test_env();
        let _canonical = crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "384000");
        let config = {
            let __cw_config_init_6_0 = Some("openrouter".to_string());
            let __cw_config_init_6_1 = Some(ProvidersConfig {
                openrouter: ProviderConfig {
                    api_key: Some("openrouter-route-cap-test".to_string()),
                    base_url: Some("https://openrouter.ai/api/v1".to_string()),
                    model: Some("deepseek/deepseek-v4-pro".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_6_value: Config = Config::default();
            __cw_config_init_6_value.provider = __cw_config_init_6_0;
            __cw_config_init_6_value.providers = __cw_config_init_6_1;
            __cw_config_init_6_value
        };
        let client = CodewhaleClient::new(&config).expect("OpenRouter client resolves");
        assert_eq!(client.wire_format, WireFormat::ChatCompletions);
        assert!(client.route_limits.is_some());

        let rebound = client
            .rebound_for_model_protocol(Some(&config), OPENROUTER_QWEN_3_6_FLASH_MODEL)
            .expect("same-protocol alternate route resolves")
            .expect("model/limit identity change requires a rebound");
        assert_eq!(rebound.wire_format, WireFormat::ChatCompletions);
        assert_eq!(rebound.default_model, OPENROUTER_QWEN_3_6_FLASH_MODEL);
        assert_ne!(rebound.route_limits, client.route_limits);

        let pro_cap = client.effective_max_output_tokens("deepseek/deepseek-v4-pro");
        let alternate_cap = client.effective_max_output_tokens(OPENROUTER_QWEN_3_6_FLASH_MODEL);
        assert!(
            alternate_cap < pro_cap,
            "fixture must prove a smaller same-protocol alternate route: pro={pro_cap}, alternate={alternate_cap}"
        );
        assert_eq!(
            alternate_cap,
            rebound.effective_max_output_tokens(OPENROUTER_QWEN_3_6_FLASH_MODEL),
            "the original bound client and rebound client must resolve the same alternate envelope"
        );
        let prepared = client
            .prepare_outbound_request(
                MessageRequest {
                    model: OPENROUTER_QWEN_3_6_FLASH_MODEL.to_string(),
                    messages: vec![Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "alternate route cap".to_string(),
                            cache_control: None,
                        }],
                    }],
                    max_tokens: 384_000,
                    system: None,
                    tools: None,
                    tool_choice: None,
                    metadata: None,
                    thinking: None,
                    reasoning_effort: Some("max".to_string()),
                    stream: Some(false),
                    temperature: None,
                    top_p: None,
                },
                false,
            )
            .expect("same-protocol alternate prepares");
        assert_eq!(prepared.body["max_tokens"], json!(alternate_cap));
    }

    #[allow(clippy::await_holding_lock)]
    #[tokio::test(flavor = "current_thread")]
    async fn fim_non_message_request_is_clamped_to_bound_route() {
        let _lock = crate::test_support::lock_test_env();
        let _canonical = crate::test_support::EnvVarGuard::set("CODEWHALE_MAX_OUTPUT_TOKENS", "384000");
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/beta/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"text": "middle"}]
            })))
            .expect(1)
            .mount(&server)
            .await;
        let base_url = format!("{}/v1", server.uri());
        let config = {
            let __cw_config_init_7_0 = Some("custom".to_string());
            let __cw_config_init_7_1 = Some("local-fim".to_string());
            let mut __cw_config_init_7_value: Config = Config::default();
            __cw_config_init_7_value.provider = __cw_config_init_7_0;
            __cw_config_init_7_value.default_text_model = __cw_config_init_7_1;
            __cw_config_init_7_value
        }
        .with_legacy_root(Some("fim-cap-test".to_string()), Some(base_url.clone()));
        let client = CodewhaleClient::from_parts(
            base_url,
            "local-fim".to_string(),
            WireFormat::ChatCompletions,
            Some(RouteLimits {
                context_tokens: Some(327_680),
                output_tokens: Some(128),
                ..RouteLimits::default()
            }),
            &config,
        )
        .expect("FIM route client");

        assert_eq!(
            client
                .fim_completion("local-fim", "prefix", "suffix", 4_096)
                .await
                .expect("FIM response"),
            "middle"
        );
        let requests = server
            .received_requests()
            .await
            .expect("recorded FIM request");
        let body: Value = serde_json::from_slice(&requests[0].body).expect("FIM JSON");
        assert_eq!(body["max_tokens"], json!(128));
    }

    #[test]
    fn official_deepseek_flash_binds_responses_request_and_endpoint() {
        let (_config, route) =
            deepseek_route_for_test("https://api.deepseek.com/beta", "deepseek-v4-flash");
        assert_eq!(route.candidate.protocol(), WireFormat::Responses);

        let client = CodewhaleClient::new(&route.config).expect("Flash client resolves");
        assert_eq!(client.wire_format, WireFormat::Responses);

        let prepared = client
            .prepare_outbound_request(
                MessageRequest {
                    model: "deepseek-v4-flash".to_string(),
                    messages: vec![Message {
                        role: Role::User,
                        content: vec![ContentBlock::Text {
                            text: "hello".to_string(),
                            cache_control: None,
                        }],
                    }],
                    max_tokens: 64,
                    system: None,
                    tools: None,
                    tool_choice: None,
                    metadata: None,
                    thinking: None,
                    reasoning_effort: Some("max".to_string()),
                    stream: Some(true),
                    temperature: None,
                    top_p: None,
                },
                true,
            )
            .expect("Flash Responses request prepares");

        assert_eq!(prepared.dialect, WireDialect::OpenAiResponses);
        assert_eq!(prepared.endpoint.url, "https://api.deepseek.com/responses");
        assert_eq!(prepared.body["model"], "deepseek-v4-flash");
        assert_eq!(prepared.body["reasoning"]["effort"], "max");
    }

    #[test]
    fn uncatalogued_deepseek_preview_binds_chat_request_without_model_fallback() {
        let model = "deepseek-v4.1-flash-expires-on-0910";
        let (_config, route) = deepseek_route_for_test("https://api.deepseek.com", model);
        assert_eq!(route.candidate.protocol(), WireFormat::ChatCompletions);
        assert_eq!(route.candidate.wire_model_id().as_str(), model);
        assert!(route.candidate.canonical_model().is_none());

        for client in [
            CodewhaleClient::new(&route.config).expect("preview client resolves"),
            CodewhaleClient::from_candidate(&route.config, &route.candidate)
                .expect("preview client binds the admitted candidate"),
        ] {
            assert_eq!(client.wire_format, WireFormat::ChatCompletions);
            assert_eq!(client.default_model, model);
            let prepared = client
                .prepare_outbound_request(
                    MessageRequest {
                        model: model.to_string(),
                        messages: vec![Message {
                            role: Role::User,
                            content: vec![ContentBlock::Text {
                                text: "hello".to_string(),
                                cache_control: None,
                            }],
                        }],
                        max_tokens: 64,
                        system: None,
                        tools: None,
                        tool_choice: None,
                        metadata: None,
                        thinking: None,
                        reasoning_effort: None,
                        stream: Some(true),
                        temperature: None,
                        top_p: None,
                    },
                    true,
                )
                .expect("preview Chat request prepares");
            assert_eq!(prepared.dialect, WireDialect::ChatCompletions);
            assert_eq!(
                prepared.endpoint.url,
                "https://api.deepseek.com/v1/chat/completions"
            );
            assert_eq!(prepared.body["model"], model);
            assert_eq!(prepared.body["messages"][0]["content"], "hello");
        }
    }

    #[test]
    fn exact_catalog_deepseek_preview_binding_survives_prepare_and_same_model_rebind() {
        use codewhale_config::route::{
            PricingSku, ProviderId, RouteCapabilities, WireModelId, offering::ProviderModelOffering,
        };

        let model = "deepseek-v4.1-flash-expires-on-0910";
        // Synthetic catalog evidence: the offline catalog has no such row.
        // This fixture does not assert that the real preview supports Responses.
        let resolver = RouteResolver::from_offerings(vec![ProviderModelOffering {
            provider: ProviderId::from("deepseek"),
            canonical_model: None,
            wire_model_id: WireModelId::from(model),
            endpoint_key: "responses".to_string(),
            default_for_provider: false,
            limits: RouteLimits {
                output_tokens: Some(777),
                ..Default::default()
            },
            capabilities: RouteCapabilities::default(),
            pricing: PricingSku::UnknownOrStale,
        }]);
        let candidate = resolver
            .resolve(&RouteRequest {
                explicit_provider: Some(codewhale_config::ProviderKind::Deepseek),
                model_selector: Some(LogicalModelRef::from(model)),
                base_url_override: Some("https://api.deepseek.com".to_string()),
                ..Default::default()
            })
            .expect("exact synthetic catalog offering resolves");
        assert_eq!(candidate.protocol(), WireFormat::Responses);
        assert_eq!(candidate.wire_model_id().as_str(), model);

        let config = {
            let __cw_config_init_8_0 = Some("deepseek".to_string());
            let __cw_config_init_8_1 = Some(model.to_string());
            let mut __cw_config_init_8_value: Config = Default::default();
            __cw_config_init_8_value.provider = __cw_config_init_8_0;
            __cw_config_init_8_value.default_text_model = __cw_config_init_8_1;
            __cw_config_init_8_value
        }
        .with_legacy_root(
            Some("ds-test".to_string()),
            Some("https://api.deepseek.com".to_string()),
        );
        let client = CodewhaleClient::from_candidate(&config, &candidate)
            .expect("client binds exact synthetic catalog offering");
        assert!(
            client
                .rebound_for_model_protocol(None, model)
                .expect("the same admitted model needs no offline lookup")
                .is_none()
        );
        let prepared = client
            .prepare_outbound_request(
                translation_message_request("hello", model.to_string(), "English", 4_096),
                true,
            )
            .expect("same-model request preserves the exact catalog binding");
        assert_eq!(prepared.dialect, WireDialect::OpenAiResponses);
        assert_eq!(prepared.endpoint.url, "https://api.deepseek.com/responses");
        assert_eq!(prepared.body["model"], model);
        assert_eq!(prepared.body["max_output_tokens"], 777);

        let other_model = "deepseek-v4-pro";
        assert!(
            client
                .prepare_outbound_request(
                    translation_message_request("hello", other_model.to_string(), "English", 4_096,),
                    true,
                )
                .is_err(),
            "a different model must still obey the protocol switch guard"
        );
        let rebound = client
            .rebound_for_model_protocol(Some(&config), other_model)
            .expect("different model resolves independently")
            .expect("Pro requires a Chat client");
        let prepared = rebound
            .prepare_outbound_request(
                translation_message_request("hello", other_model.to_string(), "English", 4_096),
                true,
            )
            .expect("rebound Pro prepares Chat");
        assert_eq!(prepared.dialect, WireDialect::ChatCompletions);
        assert_eq!(
            prepared.endpoint.url,
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(prepared.body["model"], other_model);
    }

    #[test]
    fn rebinding_a_chat_bound_client_for_flash_switches_to_responses() {
        // #5042: fleet dispatch binds the child client before the profile
        // model is resolved; a chat-bound DeepSeek client asked to run flash
        // must be rebuilt on the Responses protocol by the central resolver
        // instead of failing deterministically at first send.
        let (_config, route) =
            deepseek_route_for_test("https://api.deepseek.com/beta", "deepseek-v4-pro");
        let client = CodewhaleClient::new(&route.config).expect("pro client resolves");
        assert_eq!(client.wire_format, WireFormat::ChatCompletions);

        let rebound = client
            .rebound_for_model_protocol(Some(&route.config), "deepseek-v4-flash")
            .expect("flash rebind resolves")
            .expect("flash requires a different protocol");
        assert_eq!(rebound.wire_format, WireFormat::Responses);
        assert_eq!(rebound.default_model, "deepseek-v4-flash");

        assert!(
            client
                .rebound_for_model_protocol(Some(&route.config), "deepseek-v4-pro")
                .expect("pro rebind resolves")
                .is_none(),
            "a matching protocol must not rebuild the client"
        );
    }

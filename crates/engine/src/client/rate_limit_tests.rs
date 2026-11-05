#[tokio::test]
async fn rate_limit_unknown_quota_sender_and_billing_boundary() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for (provider, message, detail, expected_wires) in [
        ("custom", "TPM exceeded", "", 2),
        ("custom", "Credit balance exhausted", "", 1),
        ("openai", "TPM exceeded", "", 1),
        (
            "custom",
            "Too many requests",
            "Billing quota has been exhausted",
            1,
        ),
    ] {
        let server = MockServer::start().await;
        let mut config = Config::default();
        config.provider = Some(provider.into());
        config.providers = Some(ProvidersConfig {
            openai: ProviderConfig {
                api_key: Some("unknown-quota-fixture".into()),
                base_url: Some(server.uri()),
                ..Default::default()
            },
            ..Default::default()
        });
        let mut client = CodewhaleClient::new(
            &config.with_legacy_root(Some("unknown-quota-fixture".into()), Some(server.uri())),
        )
        .unwrap();
        client.retry.rate_limit.initial_delay_secs = 1;
        client.retry.rate_limit.max_delay_secs = 1;
        let calls = AtomicUsize::new(0);
        let first = std::sync::Mutex::new(None::<tokio::time::Instant>);
        let result = client.send_retry_loop(|| async {
            let response = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                *first.lock().unwrap() = Some(tokio::time::Instant::now());
                http::Response::builder().status(429).header("retry-after", "1")
                    .body(reqwest::Body::from(serde_json::json!({"error":{"type":if detail.is_empty() { "insufficient_quota" } else { "gateway_limit" },"message":message},"detail":detail}).to_string())).unwrap()
            } else {
                assert!(first.lock().unwrap().unwrap().elapsed() >= Duration::from_secs(1));
                http::Response::builder().body(reqwest::Body::from("ok")).unwrap()
            };
            Ok(reqwest::Response::from(response))
        }, None, &ErrorBodyDisclosure::Full, crate::llm_client::RetryOperation::ReadOnly).await;
        assert_eq!(calls.load(Ordering::SeqCst), expected_wires);
        if expected_wires == 2 {
            assert!(result.is_ok(), "{result:?}");
        } else {
            assert!(matches!(
                result.unwrap_err().downcast_ref::<LlmError>(),
                Some(LlmError::QuotaExhausted(_))
            ));
        }
        codewhale_runtime::retry_status::clear_rate_limit();
    }
}

#[tokio::test]
async fn rate_limit_inherited_minimum_parks_before_wire() {
    let server = MockServer::start().await;
    let client = deepseek_request_boundary_client(&server.uri(), server.uri());
    let scope = client.rate_limit_scope();
    codewhale_runtime::retry_status::note_rate_limit(&scope, Duration::from_secs(3600));
    let calls = std::sync::atomic::AtomicU32::new(0);
    let error = client
        .send_retry_loop(
            || async {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(reqwest::Response::from(
                    http::Response::builder()
                        .body(reqwest::Body::from("ok"))
                        .unwrap(),
                ))
            },
            Some(Duration::from_secs(2)),
            &ErrorBodyDisclosure::Full,
            crate::llm_client::RetryOperation::ReadOnly,
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<LlmError>(),
        Some(LlmError::QuotaWindow {
            transmission: crate::llm_client::RequestTransmission::NotDispatched,
            ..
        })
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    codewhale_runtime::retry_status::clear_rate_limit();
}

#[tokio::test]
async fn rate_limit_guarded_native_json_uses_actual_disclosure_boundary() {
    let server = MockServer::start().await;
    let client = minimax_request_boundary_client(&server.uri(), "MiniMax-M2.5", server.uri());
    let disclosure = ErrorBodyDisclosure::Guarded {
        request_secrets: vec!["request-fixture".into()],
    };
    let error = client.send_retry_loop(|| async {
        Ok(reqwest::Response::from(http::Response::builder()
            .header("content-type", "application/json").header("retry-after", "3600")
            .body(reqwest::Body::from(r#"{"base_resp":{"status_code":1002,"status_msg":"minimax-request-boundary-key request-fixture"}}"#)).unwrap()))
    }, None, &disclosure, crate::llm_client::RetryOperation::ReadOnly).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<LlmError>(),
        Some(LlmError::QuotaWindow { .. })
    ));
    assert!(!error.to_string().contains("minimax-request-boundary-key"));
    assert!(!error.to_string().contains("request-fixture"));
    codewhale_runtime::retry_status::clear_rate_limit();
}

#[tokio::test]
async fn rate_limit_extreme_header_parks_without_instant_overflow() {
    let server = MockServer::start().await;
    let client = deepseek_request_boundary_client(&server.uri(), server.uri());
    let error = client
        .send_retry_loop(
            || async {
                Ok(reqwest::Response::from(
                    http::Response::builder()
                        .status(429)
                        .header("retry-after", "18446744073709551615")
                        .body(reqwest::Body::from("fixture throttle"))
                        .unwrap(),
                ))
            },
            None,
            &ErrorBodyDisclosure::Full,
            crate::llm_client::RetryOperation::ReadOnly,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<LlmError>()
            .unwrap()
            .suggested_retry_delay(),
        Some(Duration::from_secs(u64::MAX))
    );
    let remaining =
        codewhale_runtime::retry_status::rate_limit_remaining(&client.rate_limit_scope()).unwrap();
    assert!(remaining <= Duration::from_secs(3600));
    assert!(remaining > Duration::from_secs(3598));
    codewhale_runtime::retry_status::clear_rate_limit();
}

#[tokio::test]
async fn rate_limit_inherited_wait_is_outside_actual_nonstream_active_envelope() {
    let _env_lock = crate::test_support::lock_test_env();
    let _envelope = NonStreamingEnvelopeGuard::millis(2000);
    let server = MockServer::start().await;
    let client = deepseek_request_boundary_client(&server.uri(), server.uri());
    codewhale_runtime::retry_status::note_rate_limit(
        &client.rate_limit_scope(),
        Duration::from_millis(1200),
    );
    let result = client
        .send_retry_loop(
            || async {
                tokio::time::sleep(Duration::from_millis(1200)).await;
                Ok(reqwest::Response::from(
                    http::Response::builder()
                        .body(reqwest::Body::from("ok"))
                        .unwrap(),
                ))
            },
            Some(Duration::from_secs(2)),
            &ErrorBodyDisclosure::Full,
            crate::llm_client::RetryOperation::ReadOnly,
        )
        .await;
    assert!(result.is_ok(), "{result:?}");
    codewhale_runtime::retry_status::clear_rate_limit();
}

#[tokio::test]
async fn rate_limit_isolated_unknown_quota_uses_route_classification() {
    let server = MockServer::start().await;
    let mut config = Config::default();
    config.provider = Some("custom".into());
    let mut client = CodewhaleClient::new(
        &config.with_legacy_root(Some("isolated-quota-fixture".into()), Some(server.uri())),
    )
    .unwrap();
    client.retry.enabled = false;
    let error = client.send_with_isolated_retry(|| async {
        Ok(reqwest::Response::from(http::Response::builder().status(429).body(reqwest::Body::from(
            r#"{"error":{"code":null,"type":"insufficient_quota","message":"TPM exceeded"}}"#
        )).unwrap()))
    }, &ErrorBodyDisclosure::Full,crate::llm_client::RetryOperation::ReadOnly).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<LlmError>(),
        Some(LlmError::RateLimited { .. })
    ));
}

#[tokio::test]
async fn rate_limit_native_envelope_settles_both_sender_attempts() {
    use crate::llm_client::{
        RequestRetryObservation, RequestTransmission, observe_request_retries,
    };
    for (isolated, billing) in [(false, false), (true, false), (false, true), (true, true)] {
        let status = if billing { 429 } else { 200 };
        let body = if billing {
            serde_json::json!({"error":{"message":"Too many requests"},"detail":"Billing quota has been exhausted"})
        } else {
            serde_json::json!({"base_resp":{"status_code":2045,"status_msg":"retry in 30s"}})
        };
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(body))
            .mount(&server)
            .await;
        let mut client =
            minimax_request_boundary_client(&server.uri(), "MiniMax-M2.5", server.uri());
        client.isolated_request_state = isolated;
        let observation = RequestRetryObservation {
            request_id: "envelope-settlement".into(),
            wire_observations: Default::default(),
            retries: Default::default(),
            emit: std::sync::Arc::new(|_| Box::pin(async {})),
        };
        let wire = observation.wire_observations.clone();
        let error = observe_request_retries(
            Some(observation),
            client.send_with_retry(|| client.http_client.post(server.uri()).body("{}")),
        )
        .await
        .unwrap_err();
        let error = error.downcast_ref::<LlmError>().unwrap();
        if billing {
            assert!(matches!(error, LlmError::QuotaExhausted(_)));
        } else {
            assert!(matches!(error, LlmError::QuotaWindow { .. }));
            assert_eq!(error.suggested_retry_delay(), Some(Duration::from_secs(30)));
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        let attempts = wire.snapshot().transmissions;
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].http_status, Some(status));
        assert_eq!(attempts[0].transmission, RequestTransmission::Rejected);
        codewhale_runtime::retry_status::clear_rate_limit();
    }
}

#[tokio::test]
async fn rate_limit_local_bucket_still_consumes_the_nonstream_active_envelope() {
    let _env_lock = crate::test_support::lock_test_env();
    let _envelope = NonStreamingEnvelopeGuard::millis(2000);
    let server = MockServer::start().await;
    let client = deepseek_request_boundary_client(&server.uri(), server.uri());
    *client.rate_limiter.lock().await = TokenBucket {
        enabled: true,
        capacity: 1.0,
        tokens: 0.0,
        refill_per_sec: 0.0001,
        last_refill: Instant::now(),
    };
    let calls = std::sync::atomic::AtomicU32::new(0);
    let error = tokio::time::timeout(
        Duration::from_secs(5),
        client.send_retry_loop(
            || async {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(reqwest::Response::from(
                    http::Response::builder()
                        .body(reqwest::Body::from("ok"))
                        .unwrap(),
                ))
            },
            Some(Duration::from_secs(2)),
            &ErrorBodyDisclosure::Full,
            crate::llm_client::RetryOperation::Inference,
        ),
    )
    .await
    .expect("local token admission remains inside the active deadline")
    .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<LlmError>(),
        Some(LlmError::Timeout(_))
    ));
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn rate_limit_official_quota_retains_route_guidance_in_both_senders() {
    for isolated in [false, true] {
        for code in [
            "subscription_sharing_usage_limit_exceeded",
            "subscription_sharing_usage_unavailable",
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(
                    ResponseTemplate::new(429).set_body_json(
                        serde_json::json!({"error":{"code":code,"message":"opaque"}}),
                    ),
                )
                .mount(&server)
                .await;
            let mut config = Config::default();
            config.provider = Some("openai-codex".into());
            config.providers = Some(ProvidersConfig {
                openai_codex: ProviderConfig {
                    api_key: Some("guidance-fixture-token".into()),
                    base_url: Some(server.uri()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut client = CodewhaleClient::new(&config).unwrap();
            client.isolated_request_state = isolated;
            client.subscription_limit_guidance = Some("fixture-owned-account-guidance".into());
            let error = client
                .send_with_retry(|| client.http_client.post(server.uri()).body("{}"))
                .await
                .unwrap_err();
            let typed = error.downcast_ref::<LlmError>().unwrap();
            assert!(matches!(typed, LlmError::QuotaExhausted(_)));
            assert!(!typed.is_retryable());
            assert!(
                typed.to_string().contains("ChatGPT Settings > Usage"),
                "{typed}"
            );
            assert!(
                typed.to_string().contains("fixture-owned-account-guidance"),
                "{typed}"
            );
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
            codewhale_runtime::retry_status::clear_rate_limit();
        }
    }
}

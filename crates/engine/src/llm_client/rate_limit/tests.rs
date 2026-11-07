use super::*;
use crate::llm_client::{RetryConfig, RetryResult, with_retry};
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

fn refused(hint: Option<Duration>) -> LlmError {
    LlmError::RateLimited {
        message: "fixture refusal".into(),
        retry_after: hint,
    }
}

#[test]
fn server_minima_and_provider_codes() {
    assert_eq!(server_delay("86400"), Some(Duration::from_secs(86400)));
    for invalid in ["-1", "NaN", "inf", "1e300", "bad"] {
        assert_eq!(server_delay(invalid), None);
    }
    let date = chrono::Utc::now() + chrono::Duration::hours(1);
    let parsed = server_delay(&date.to_rfc2822()).unwrap();
    assert!(parsed >= Duration::from_secs(3598) && parsed <= Duration::from_secs(3600));
    let raw = r#"{"error":{"code":"insufficient_quota","message":"TPM exceeded"}}"#;
    assert!(matches!(
        provider_error("dashscope", 429, raw, "safe", None),
        Some(LlmError::RateLimited { .. })
    ));
    assert!(provider_error("openai", 429, raw, "safe", None).is_none());
    assert!(matches!(
        LlmError::from_http_response(429, raw),
        LlmError::QuotaExhausted(_)
    ));
    let raw = r#"{"base_resp":{"status_code":1002,"status_msg":"retry in 30s"}}"#;
    assert_eq!(
        provider_error("minimax", 429, raw, "safe", Some(Duration::from_secs(2)))
            .unwrap()
            .suggested_retry_delay(),
        Some(Duration::from_secs(30))
    );
    for raw in [
        r#"{"base_resp":{"status_code":1002,"status_msg":"retry in 7200s"}}"#,
        r#"{"error":{"code":1002,"details":[{"retryDelay":"7200s"}]}}"#,
    ] {
        assert_eq!(
            provider_error("minimax", 429, raw, "safe", None)
                .unwrap()
                .suggested_retry_delay(),
            Some(Duration::from_secs(7200))
        );
    }
    for (provider, code, expected) in [
        ("minimax", 1002, "retry"),
        ("minimax", 1008, "billing"),
        ("minimax", 1039, "input"),
        ("minimax", 2045, "window"),
        ("zai", 1308, "window"),
        ("zai", 1302, "retry"),
    ] {
        let raw = serde_json::json!({"error":{"code":code,"message":"fixture"}}).to_string();
        let error = provider_error(provider, 429, &raw, "safe", None).unwrap();
        if expected == "window" {
            assert_eq!(
                error.transmission(),
                super::super::RequestTransmission::Rejected
            );
            assert!(!error.is_retryable());
            assert!(!error.to_string().contains("checkpoint retained"));
            let envelope = codewhale_error_taxonomy::ErrorEnvelope::from(
                provider_error(provider, 429, &raw, "safe", None).unwrap(),
            );
            assert!(!envelope.recoverable);
        }
        assert!(
            match expected {
                "retry" => matches!(error, LlmError::RateLimited { .. }),
                "billing" => matches!(error, LlmError::QuotaExhausted(_)),
                "window" => matches!(error, LlmError::QuotaWindow { .. }),
                _ => matches!(error, LlmError::InvalidRequest { .. }),
            },
            "{provider}/{code}: {error:?}"
        );
    }
    assert_eq!(
        body_delay(r#"{"error":{"retry_after":300}}"#),
        Some(Duration::from_secs(300))
    );
    assert_eq!(
        body_delay(r#"{"error":{"retryDelay":"120s"}}"#),
        Some(Duration::from_secs(120))
    );
    let window = provider_error(
        "minimax",
        200,
        r#"{"base_resp":{"status_code":2045,"status_msg":"weekly usage limit reached"}}"#,
        "safe",
        Some(Duration::from_secs(7200)),
    )
    .unwrap();
    assert!(matches!(window, LlmError::QuotaWindow { .. }));
    assert_eq!(
        window.suggested_retry_delay(),
        Some(Duration::from_secs(7200))
    );
    let reset = (chrono::Utc::now() + chrono::Duration::hours(5)).timestamp();
    let hint =
        body_delay(&serde_json::json!({"error":{"next_flush_time":reset}}).to_string()).unwrap();
    assert!(hint > Duration::from_secs(17998));
}

#[test]
fn policy_validation_and_equal_jitter() {
    assert!(serde_json::from_str::<RateLimitRetryPolicy>(r#"{"max_attempts":0}"#).is_err());
    assert!(
        serde_json::from_str::<RateLimitRetryPolicy>(
            r#"{"initial_delay_secs":70,"max_delay_secs":60}"#
        )
        .is_err()
    );
    let p = RateLimitRetryPolicy::default();
    assert_eq!(p.max_attempts, 20);
    assert_eq!(p.recovery_budget_secs, 600);
    for n in 0..100 {
        let delay = retry_delay(&p, n);
        let base = (5u64.saturating_mul(2u64.saturating_pow(n.min(63)))).min(60);
        assert!(
            delay >= Duration::from_secs(base).div_f64(2.0) && delay <= Duration::from_secs(base)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn rate_budget_is_independent_from_transport_retries() {
    let config = RetryConfig {
        max_retries: 0,
        rate_limit: RateLimitRetryPolicy {
            max_attempts: 3,
            ..Default::default()
        },
        ..Default::default()
    };
    let calls = AtomicU32::new(0);
    let result: RetryResult<()> = with_retry(
        &config,
        || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(refused(None))
        },
        None,
    )
    .await;
    assert_eq!(result.unwrap_err().attempts, 3);
    assert_eq!(calls.load(Ordering::SeqCst), 3);
}

#[tokio::test(start_paused = true)]
async fn long_hints_and_quota_windows_park_without_early_retry() {
    let mut config = RetryConfig::default();
    config.rate_limit.recovery_budget_secs = 7200;
    for (window, enabled) in [(false, true), (true, true), (false, false), (true, false)] {
        config.enabled = enabled;
        let calls = AtomicU32::new(0);
        let start = tokio::time::Instant::now();
        let result: RetryResult<()> = with_retry(
            &config,
            || async {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(if window {
                    LlmError::QuotaWindow {
                        message: "window".into(),
                        retry_after: Some(Duration::from_secs(2)),
                        transmission: super::super::RequestTransmission::Rejected,
                    }
                } else {
                    refused(Some(Duration::from_secs(86400)))
                })
            },
            None,
        )
        .await;
        assert!(matches!(
            result.unwrap_err().last_error,
            LlmError::QuotaWindow { .. }
        ));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(start.elapsed(), Duration::ZERO);
    }
}

#[tokio::test(start_paused = true)]
async fn active_work_deadline_excludes_backoff_and_cancellation_drops_retries() {
    assert!(
        active_timeout(Duration::from_secs(3), async {
            wait(Duration::from_secs(30)).await;
            tokio::time::sleep(Duration::from_secs(2)).await;
        })
        .await
        .is_ok()
    );
    assert!(
        active_timeout(
            Duration::from_secs(3),
            tokio::time::sleep(Duration::from_secs(5))
        )
        .await
        .is_err()
    );
    let calls = Arc::new(AtomicU32::new(0));
    let count = calls.clone();
    let task = tokio::spawn(async move {
        let _: RetryResult<()> = with_retry(
            &RetryConfig::default(),
            || async {
                count.fetch_add(1, Ordering::SeqCst);
                Err(refused(None))
            },
            None,
        )
        .await;
    });
    tokio::task::yield_now().await;
    task.abort();
    let _ = task.await;
    tokio::time::advance(Duration::from_secs(1000)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn native_json_envelopes_are_classified_before_model_parsing() {
    let make = |body: &str| {
        reqwest::Response::from(
            http::Response::builder()
                .header("content-type", "application/json")
                .body(reqwest::Body::from(body.to_owned()))
                .unwrap(),
        )
    };
    let error = inspect_json_envelope(
        "minimax",
        make(r#"{"base_resp":{"status_code":1002,"status_msg":"throttled"}}"#),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, LlmError::RateLimited { .. }));
    let body = r#"{"base_resp":{"status_code":0},"choices":[]}"#;
    let response = inspect_json_envelope("minimax", make(body)).await.unwrap();
    assert_eq!(response.text().await.unwrap(), body);
}

#[tokio::test(start_paused = true)]
async fn successful_json_refusal_preserves_header_minimum_and_disclosure() {
    let response = reqwest::Response::from(http::Response::builder()
        .header("content-type", "Application/JSON; charset=utf-8")
        .header("retry-after", "3600")
        .body(reqwest::Body::from(r#"{"base_resp":{"status_code":1002,"status_msg":"fixture-secret"},"retry_after":7200}"#)).unwrap());
    let error = inspect_json_envelope_with_disclosure("minimax", response, None, |_, body| {
        body.replace("fixture-secret", "[redacted]")
    })
    .await
    .unwrap_err();
    assert_eq!(
        error.suggested_retry_delay(),
        Some(Duration::from_secs(7200))
    );
    assert!(!error.to_string().contains("fixture-secret"));
    // A larger/header-only minimum must also survive; the body-max case
    // above alone would not detect a dropped Retry-After header.
    let response = reqwest::Response::from(
        http::Response::builder()
            .header("content-type", "application/json")
            .header("retry-after", "3600")
            .body(reqwest::Body::from(
                r#"{"base_resp":{"status_code":1002,"status_msg":"throttled"}}"#,
            ))
            .unwrap(),
    );
    let header_only = inspect_json_envelope("minimax", response)
        .await
        .unwrap_err();
    assert_eq!(
        header_only.suggested_retry_delay(),
        Some(Duration::from_secs(3600))
    );
    let config = RetryConfig::default();
    let mut refusal = Some(error);
    let calls = AtomicU32::new(0);
    let result: RetryResult<()> = with_retry(
        &config,
        || {
            calls.fetch_add(1, Ordering::SeqCst);
            let error = refusal
                .take()
                .expect("long hint must not dispatch a second attempt");
            async move { Err(error) }
        },
        None,
    )
    .await;
    assert_eq!(result.unwrap_err().attempts, 1);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn long_backoff_does_not_consume_the_nonstream_active_envelope() {
    let mut config = RetryConfig::default();
    config.rate_limit.recovery_budget_secs = 3600;
    let calls = AtomicU32::new(0);
    let result = active_timeout_total(
        Duration::from_secs(1800),
        with_retry(
            &config,
            || async {
                if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(refused(Some(Duration::from_secs(1000))))
                } else {
                    Ok(())
                }
            },
            None,
        ),
    )
    .await;
    assert!(result.unwrap().is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    // Excluding waits must not disable a real active-work timeout.
    assert!(
        active_timeout_total(
            Duration::from_secs(3),
            tokio::time::sleep(Duration::from_secs(4))
        )
        .await
        .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn mixed_transport_failure_cannot_restart_expired_rate_recovery() {
    let config = RetryConfig {
        operation: super::super::RetryOperation::ReadOnly,
        rate_limit: RateLimitRetryPolicy {
            initial_delay_secs: 1,
            max_delay_secs: 1,
            ..Default::default()
        },
        ..Default::default()
    };
    let calls = AtomicU32::new(0);
    let result: RetryResult<()> = with_retry(
        &config,
        || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(refused(None))
            } else {
                tokio::time::sleep(Duration::from_secs(601)).await;
                Err(LlmError::ServerError {
                    status: 503,
                    message: "fixture".into(),
                })
            }
        },
        None,
    )
    .await;
    assert!(matches!(
        result.unwrap_err().last_error,
        LlmError::ServerError { status: 503, .. }
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn unknown_rate_limit_quota_code_requires_independent_billing_evidence() {
    let hint = Some(Duration::from_secs(30));
    for raw in [
        r#"{"error":{"code":null,"type":"insufficient_quota","message":"TPM exceeded"},"retry_after":40}"#,
        r#"{"error":{"code":"gateway_limit","error_code":"insufficient_quota","message":"TPM exceeded"},"retry_after":40}"#,
        r#"{"error":{"code":"insufficient_quota","message":"TPM exceeded"},"retry_after":40}"#,
        r#"{"error":{"type":"insufficient_quota","message":"quota exceeded"},"retry_after":40}"#,
        r#"{"error_code":"insufficient_quota","message":"quota exceeded","retry_after":40}"#,
    ] {
        let error = provider_error("custom", 429, raw, "safe", hint).unwrap();
        assert!(matches!(error, LlmError::RateLimited { .. }));
        assert_eq!(error.suggested_retry_delay(), Some(Duration::from_secs(40)));
        assert!(!error.to_string().contains("TPM"));
    }
    let raw = "TPM exceeded (provider error code: insufficient_quota)";
    assert!(matches!(
        provider_error("custom", 429, raw, "safe", hint),
        Some(LlmError::RateLimited { .. })
    ));
    for raw in [
        r#"{"error":{"code":"insufficient_quota","message":"Credit balance exhausted"}}"#,
        r#"{"error":{"code":"insufficient_quota","message":"Billing quota has been exhausted"}}"#,
        r#"{"error":{"code":"insufficient_quota","message":"Account quota exhausted"}}"#,
        r#"{"error":{"code":"insufficient_quota","message":"Plan quota is exhausted"}}"#,
        r#"{"error":{"code":"insufficient_quota","message":"TPM exceeded"},"detail":"Billing quota has been exhausted"}"#,
        r#"{"error":{"message":"Too many requests"},"detail":"Billing quota has been exhausted"}"#,
        r#"{"error":{"code":"gateway_limit","message":"Too many requests"},"detail":"Billing quota has been exhausted"}"#,
        r#"{"error":{"code":"insufficient_quota","type":"billing_hard_limit_reached","message":"fixture"}}"#,
        r#"{"error":{"code":"insufficient_quota","message":"\u4f59\u989d\u4e0d\u8db3"}}"#,
    ] {
        for provider in ["custom", "dashscope", "qwen"] {
            assert!(matches!(
                provider_error(provider, 429, raw, "safe", hint),
                Some(LlmError::QuotaExhausted(_))
            ));
        }
    }
    // Upstream's existing generic plan/billing guidance remains bounded when
    // a reset hint is present; explicit billing/account exhaustion above wins.
    let guidance = r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota. Check plan and billing"}}"#;
    assert!(matches!(
        provider_error("custom", 429, guidance, "safe", hint),
        Some(LlmError::RateLimited { .. })
    ));
    assert!(matches!(
        provider_error("custom", 429, guidance, "safe", None),
        Some(LlmError::QuotaExhausted(_))
    ));
    let raw = r#"{"error":{"code":"insufficient_quota","message":"TPM exceeded"}}"#;
    assert!(provider_error("openai", 429, raw, "safe", hint).is_none());
    assert!(matches!(
        LlmError::from_http_response(429, raw),
        LlmError::QuotaExhausted(_)
    ));
    assert!(provider_error("custom", 401, raw, "safe", hint).is_none());
}

#[tokio::test(start_paused = true)]
async fn native_json_envelope_bounds_first_byte_idle_and_size() {
    use futures_util::StreamExt;
    for sent in [false, true] {
        let prefix = futures_util::stream::iter(if sent {
            vec![Ok::<_, std::io::Error>("{")]
        } else {
            vec![]
        });
        let body = reqwest::Body::wrap_stream(prefix.chain(futures_util::stream::pending()));
        let response = reqwest::Response::from(
            http::Response::builder()
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        );
        let error = inspect_json_envelope_with_disclosure(
            "minimax",
            response,
            Some((Duration::from_secs(2), Duration::from_secs(3))),
            |_, _| "safe".into(),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, LlmError::Timeout(delay) if delay == Duration::from_secs(if sent {3} else {2}))
        );
        assert_eq!(
            error.transmission(),
            super::super::RequestTransmission::OutcomeUnknown
        );
    }
    for declared in [false, true] {
        let body = reqwest::Body::wrap_stream(futures_util::stream::iter(
            (0..33).map(|_| Ok::<_, std::io::Error>(vec![b' '; 1024 * 1024])),
        ));
        let mut response = http::Response::builder().header("content-type", "application/json");
        if declared {
            response = response.header("content-length", 33 * 1024 * 1024);
        }
        let response = reqwest::Response::from(response.body(body).unwrap());
        let error = inspect_json_envelope("minimax", response)
            .await
            .unwrap_err();
        assert!(matches!(error, LlmError::ParseError(ref message) if message.contains("32 MiB")));
        assert_eq!(
            error.transmission(),
            super::super::RequestTransmission::OutcomeUnknown
        );
    }
}

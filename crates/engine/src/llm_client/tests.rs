use super::*;

#[test]
fn missing_google_thought_signature_errors_explain_recovery() {
    for detail in [
        "Function call is missing a thought_signature in functionCall parts.",
        "Function call is missing thought_signature.",
        "The thought_signature is missing from the function call.",
    ] {
        for body in [
            detail.to_string(),
            serde_json::json!({"error": {"message": detail, "code": 400}}).to_string(),
            serde_json::json!({"error": "Bad Request", "message": detail}).to_string(),
        ] {
            let message = sanitize_http_error_body(Some("Custom"), 400, &body);
            assert!(message.contains("built-in `google` provider"), "{message}");
            assert!(message.contains("start a new session"), "{message}");
            assert!(message.contains(detail), "provider detail must survive");
            assert_eq!(sanitize_http_error_body(None, 400, &message), message);
            let error = LlmError::from_http_response(400, &message);
            assert!(matches!(
                error,
                LlmError::InvalidRequest { status: 400, .. }
            ));
            assert!(!error.is_retryable());
        }
    }
}

#[test]
fn google_thought_signature_hint_requires_a_missing_signature_400() {
    for (status, detail) in [
        (400, "Invalid model name"),
        (400, "Invalid thought_signature in functionCall parts"),
        (400, "Unsupported parameter: thought_signature"),
        (
            401,
            "Function call is missing a thought_signature in functionCall parts.",
        ),
        (
            429,
            "Function call is missing a thought_signature in functionCall parts.",
        ),
        (
            500,
            "Function call is missing a thought_signature in functionCall parts.",
        ),
    ] {
        let body = serde_json::json!({"error": {"message": detail}}).to_string();
        assert_eq!(
            sanitize_http_error_body(Some("Custom"), status, &body),
            detail
        );
    }
}

#[test]
fn google_thought_signature_hint_keeps_large_provider_errors_bounded() {
    let body = format!(
        "Function call is missing a thought_signature in functionCall parts. {}",
        "界".repeat(3_000)
    );
    let message = sanitize_http_error_body(None, 400, &body);
    assert!(message.contains("start a new session"));
    assert!(message.chars().count() < 2_000);
    assert_eq!(sanitize_http_error_body(None, 400, &message), message);
}

#[test]
fn google_thought_signature_hint_preserves_quota_and_html_handling() {
    let detail = "Function call is missing a thought_signature in functionCall parts.";
    let body = serde_json::json!({
        "error": {"message": detail, "code": "insufficient_quota"}
    })
    .to_string();
    let message = sanitize_http_error_body(None, 400, &body);
    assert!(matches!(
        LlmError::from_http_response(400, &message),
        LlmError::QuotaExhausted(_)
    ));
    let html = format!("<!doctype html><html><body>{detail}</body></html>");
    let message = sanitize_http_error_body(None, 400, &html);
    assert!(message.contains("HTML error page"));
    assert!(!message.contains("<html>"));
}

#[test]
fn retryability_distinguishes_transient_failures_from_durable_failures() {
    for error in [
        LlmError::RateLimited {
            message: "too many requests".into(),
            retry_after: None,
        },
        LlmError::ServerError {
            status: 500,
            message: "internal error".into(),
        },
        LlmError::NetworkError("connection refused".into()),
        LlmError::Timeout(Duration::from_secs(30)),
    ] {
        assert!(error.is_retryable(), "expected transient error: {error}");
    }
    for error in [
        LlmError::authentication_error("invalid key"),
        LlmError::AuthorizationError("blocked".into()),
        LlmError::InvalidRequest {
            status: 400,
            message: "bad json".into(),
        },
        LlmError::ContentPolicyError("unsafe content".into()),
        LlmError::ContextLengthError("too long".into()),
    ] {
        assert!(!error.is_retryable(), "expected durable error: {error}");
    }
}

#[test]
fn http_response_boundary_classifies_status_contract() {
    let body = LlmError::from_http_response(413, "maximum images exceeded model token limit");
    assert!(matches!(body, LlmError::InvalidRequest { status: 413, .. }));
    assert!(!body.is_retryable());
    assert!(matches!(
        LlmError::from_http_response(429, "rate limit exceeded"),
        LlmError::RateLimited { .. }
    ));
    assert!(matches!(
        LlmError::from_http_response(401, "invalid api key"),
        LlmError::AuthenticationError(_)
    ));
    assert!(matches!(
        LlmError::from_http_response(403, "forbidden"),
        LlmError::AuthorizationError(_)
    ));
    assert!(matches!(
        LlmError::from_http_response(403, "invalid api key"),
        LlmError::AuthenticationError(_)
    ));
    let cancelled = LlmError::from_http_response(499, "upstream request cancelled");
    assert!(matches!(
        &cancelled,
        LlmError::ServerError { status: 499, .. }
    ));
    assert!(cancelled.is_retryable());
    assert!(matches!(
        LlmError::from_http_response(500, "internal server error"),
        LlmError::ServerError { status: 500, .. }
    ));
    assert!(matches!(
        LlmError::from_http_response(503, "service unavailable"),
        LlmError::ServerError { status: 503, .. }
    ));
    assert!(matches!(
        LlmError::from_http_response(400, "context_length_exceeded"),
        LlmError::ContextLengthError(_)
    ));
    assert!(matches!(
        LlmError::from_http_response(400, "content_policy_violation"),
        LlmError::ContentPolicyError(_)
    ));
    assert!(matches!(
        LlmError::from_http_response(400, "invalid json"),
        LlmError::InvalidRequest { status: 400, .. }
    ));
    for context in [
        "This model's maximum context length is 131072 tokens.",
        "prompt is too long: 250000 tokens > 200000 maximum",
        "input tokens exceed the configured limit",
        "The input token count (2000000) exceeds the maximum number of tokens allowed (1048576).",
        // xAI, Moonshot, Anthropic and Bedrock wording.
        "This model's maximum prompt length is 131072 but the request contains 200000 tokens.",
        "Invalid request: Your request exceeded model token limit: 262144",
        "input length and `max_tokens` exceed context limit: 187254 + 20000 > 204798",
        "Input is too long for requested model.",
    ] {
        assert!(
            matches!(
                LlmError::from_http_response(400, context),
                LlmError::ContextLengthError(_)
            ),
            "{context}"
        );
    }
    for invalid in [
        "max_tokens must be less than or equal to 8192",
        "Invalid 'messages[1].name': string too long. Expected a maximum length of 64.",
        "invalid token in JSON body",
    ] {
        assert!(
            matches!(
                LlmError::from_http_response(400, invalid),
                LlmError::InvalidRequest { status: 400, .. }
            ),
            "{invalid}"
        );
    }
    // "Unsupported parameter: max_output_tokens" names a *token* field, which
    // the generic keyword rules misread as a context-window overflow. It is a
    // request-shape error, and retrying or compacting cannot fix it.
    assert!(matches!(
        LlmError::from_http_response(
            400,
            "{\"error\":{\"code\":\"unsupported_parameter\",\"message\":\"Unsupported parameter: max_output_tokens\"}}"
        ),
        LlmError::InvalidRequest { status: 400, .. }
    ));
    assert!(matches!(
        LlmError::from_http_response(
            400,
            "{\"error\":{\"type\":\"invalid_request_error\",\"message\":\"Unsupported parameter: temperature\"}}"
        ),
        LlmError::InvalidRequest { status: 400, .. }
    ));
}

#[test]
fn explicit_400_402_and_429_quota_responses_are_typed_and_non_retryable() {
    for (status, body) in [
        (
            400,
            r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}"#,
        ),
        (
            429,
            r#"{"error":{"type":"insufficient_quota","message":"Billing limit reached"}}"#,
        ),
        (
            402,
            r#"{"error":{"code":"billing_hard_limit_reached","message":"Payment required"}}"#,
        ),
        (
            429,
            "You exceeded your current quota. Please check your plan and billing details.",
        ),
        (429, "Account quota exhausted"),
        (
            429,
            "You've reached your weekly usage limit for your plan. Your limit resets at 12:00.",
        ),
        (
            429,
            "You've reached your 5-hour usage limit. Your limit resets at 12:00.",
        ),
        (429, "You exceeded your five-hour usage limit."),
        (
            429,
            "Your plan usage limit has been reached; the limit resets tomorrow.",
        ),
    ] {
        let error = LlmError::from_http_response(status, body);
        assert!(matches!(error, LlmError::QuotaExhausted(_)));
        assert!(!error.is_retryable());
    }

    let raw = r#"{"error":{"code":"billing_hard_limit_reached","message":"Account unavailable"}}"#;
    let safe = sanitize_http_error_body(Some("fixture"), 429, raw);
    assert!(matches!(
        LlmError::from_http_response(429, &safe),
        LlmError::QuotaExhausted(_)
    ));
}

#[test]
fn chatgpt_usage_limit_is_quota_and_carries_account_guidance() {
    // Shape of the ChatGPT Codex backend's subscription-window 429, as
    // openai/codex `codex-api/src/api_bridge.rs` parses it.
    let raw = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1790000000}}"#;
    let safe = sanitize_http_error_body(Some("OpenAI Codex"), 429, raw);
    let error = LlmError::from_http_response(429, &safe);
    assert!(!error.is_retryable());
    let LlmError::QuotaExhausted(evidence) = error else {
        panic!("usage_limit_reached must be typed quota, got {error:?}");
    };
    let rendered = LlmError::QuotaExhausted(evidence.with_guidance(
        "This limit belongs to the ChatGPT account a@example.com (plus). Run `codewhale auth chatgpt`.",
    ))
    .to_string();
    assert!(
        rendered.contains("The usage limit has been reached"),
        "{rendered}"
    );
    assert!(rendered.contains("a@example.com (plus)"), "{rendered}");
    assert!(rendered.contains("`codewhale auth chatgpt`"), "{rendered}");

    // Same backend branch: the signed-in plan does not include Codex.
    // Retrying cannot help, so it must not be a retryable rate limit.
    let raw =
        r#"{"error":{"type":"usage_not_included","message":"Your plan does not include Codex"}}"#;
    let safe = sanitize_http_error_body(Some("OpenAI Codex"), 429, raw);
    let error = LlmError::from_http_response(429, &safe);
    assert!(!error.is_retryable());
    assert!(matches!(error, LlmError::QuotaExhausted(_)), "{error:?}");
}

#[test]
fn generic_429_stays_rate_limited_and_retryable() {
    for body in [
        "Too Many Requests",
        "Rate limit on your API quota exceeded",
        "Requests per minute quota exceeded",
        "usage limit reached",
        "Requests per minute usage limit reached; limit resets in 1 second.",
        "Quota rate limit exceeded; retry after 10 seconds",
    ] {
        let error = LlmError::from_http_response(429, body);
        assert!(
            matches!(error, LlmError::RateLimited { .. }),
            "expected transient rate limit for {body:?}, got {error:?}"
        );
        assert!(error.is_retryable());
    }

    let raw = r#"{"error":{"code":"RESOURCE_EXHAUSTED","message":"Rate limit on your API quota exceeded"}}"#;
    let safe = sanitize_http_error_body(Some("fixture"), 429, raw);
    let error = LlmError::from_http_response(429, &safe);
    assert!(matches!(error, LlmError::RateLimited { .. }));
    assert!(error.is_retryable());
}

#[test]
fn missing_google_signature_400_explains_recovery_without_widening_gateway_preflight() {
    let message = "Function call is missing a thought_signature in functionCall parts";
    for body in [
        message.to_string(),
        serde_json::json!({"error": {"message": message}}).to_string(),
        format!("{message} {}", "详情".repeat(2_000)),
    ] {
        let safe = sanitize_http_error_body(Some("OpenAI-compatible"), 400, &body);
        assert!(safe.contains(message));
        assert!(safe.contains("built-in `google` provider"));
        assert!(safe.contains("start a new session"));
        assert!(safe.contains("gateway"));
        assert!(
            safe.chars().count() <= 2_003,
            "error bound survives the hint"
        );
    }
    for (status, body) in [
        (429, message),
        (200, message),
        (400, "Invalid thought_signature"),
        (400, "Missing required parameter: model"),
    ] {
        assert_eq!(sanitize_http_error_body(None, status, body), body);
    }
}

#[tokio::test]
async fn retry_loop_stops_after_one_typed_quota_failure() {
    let mut calls = 0;
    let result: RetryResult<i32> = with_retry(
        &RetryConfig::default(),
        || {
            calls += 1;
            async {
                Err(LlmError::from_http_response(
                    429,
                    r#"{"error":{"code":"insufficient_quota"}}"#,
                ))
            }
        },
        None,
    )
    .await;
    assert_eq!(result.unwrap_err().attempts, 1);
    assert_eq!(calls, 1);
}

#[tokio::test]
async fn retry_loop_stops_after_one_authentication_failure() {
    let mut calls = 0;
    let result: RetryResult<i32> = with_retry(
        &RetryConfig::default(),
        || {
            calls += 1;
            async { Err(LlmError::authentication_error("bad key")) }
        },
        None,
    )
    .await;
    assert!(result.is_err());
    assert_eq!(calls, 1);
}

fn retry_receipt_observation() -> (
    RequestRetryObservation,
    std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) {
    let receipts = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let captured = receipts.clone();
    (
        RequestRetryObservation {
            wire_observations: Default::default(),
            retries: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            emit: std::sync::Arc::new(move |message| {
                let captured = captured.clone();
                Box::pin(async move {
                    captured.lock().unwrap().push(message);
                })
            }),
        },
        receipts,
    )
}

fn immediate_retry_policy(max_retries: u32) -> RetryConfig {
    RetryConfig {
        max_retries,
        initial_delay: 0.0,
        jitter: false,
        ..RetryConfig::default()
    }
}

#[tokio::test]
async fn scoped_retry_records_first_transport_failure_and_recovery_without_remote_payload() {
    let (observation, receipts) = retry_receipt_observation();
    let count = observation.retries.clone();
    let mut calls = 0;
    let result = observe_request_retries(
        Some(observation),
        with_retry(
            &immediate_retry_policy(2),
            || {
                calls += 1;
                let call = calls;
                async move {
                    if call == 1 {
                        Err(LlmError::ServerError {
                            status: 503,
                            message: "PRIVATE-PROVIDER-BODY".into(),
                        })
                    } else {
                        Ok(17)
                    }
                }
            },
            None,
        ),
    )
    .await
    .unwrap();
    assert_eq!(result, 17);
    assert_eq!(calls, 2);
    assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        *receipts.lock().unwrap(),
        [
            "Retry attempt: transport 1/2; upstream 503; waiting 0.00s",
            "Retry recovery: transport request recovered after 1 retries",
        ]
    );
}

#[tokio::test]
async fn scoped_retry_exhaustion_retains_typed_original_error_and_every_attempt() {
    let (observation, receipts) = retry_receipt_observation();
    let count = observation.retries.clone();
    let mut calls = 0;
    let result: RetryResult<()> = observe_request_retries(
        Some(observation),
        with_retry(
            &immediate_retry_policy(2),
            || {
                calls += 1;
                async {
                    Err(LlmError::ServerError {
                        status: 503,
                        message: "RAW-TERMINAL-PROVIDER-BODY".into(),
                    })
                }
            },
            None,
        ),
    )
    .await;
    let error = result.unwrap_err();
    assert_eq!(calls, 3);
    assert_eq!(error.attempts, 3);
    assert!(
        matches!(error.last_error, LlmError::ServerError { status: 503, ref message } if message == "RAW-TERMINAL-PROVIDER-BODY")
    );
    assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 2);
    assert_eq!(
        *receipts.lock().unwrap(),
        [
            "Retry attempt: transport 1/2; upstream 503; waiting 0.00s",
            "Retry attempt: transport 2/2; upstream 503; waiting 0.00s",
            "Retry exhaustion: transport request stopped after 2 retries; upstream 503",
        ]
    );
}

#[tokio::test]
async fn isolated_retry_scope_never_observes_foreground_attempts() {
    let (observation, receipts) = retry_receipt_observation();
    let count = observation.retries.clone();
    let mut calls = 0;
    let result = observe_request_retries(
        Some(observation),
        observe_request_retries(
            None,
            with_retry(
                &immediate_retry_policy(2),
                || {
                    calls += 1;
                    let call = calls;
                    async move {
                        if call == 1 {
                            Err(LlmError::NetworkError("private endpoint".into()))
                        } else {
                            Ok(())
                        }
                    }
                },
                None,
            ),
        ),
    )
    .await;
    assert!(result.is_ok());
    assert_eq!(calls, 2);
    assert_eq!(count.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(receipts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn concurrent_request_retry_observations_keep_exact_producing_scope() {
    let (first, first_receipts) = retry_receipt_observation();
    let (second, second_receipts) = retry_receipt_observation();
    let first_count = first.retries.clone();
    let second_count = second.retries.clone();
    let mut first_calls = 0;
    let mut second_calls = 0;
    let first_policy = immediate_retry_policy(2);
    let second_policy = immediate_retry_policy(4);
    let (a, b) = tokio::join!(
        observe_request_retries(
            Some(first),
            with_retry(
                &first_policy,
                || {
                    first_calls += 1;
                    let call = first_calls;
                    async move {
                        if call == 1 {
                            Err(LlmError::Timeout(Duration::from_secs(1)))
                        } else {
                            Ok(())
                        }
                    }
                },
                None
            )
        ),
        observe_request_retries(
            Some(second),
            with_retry(
                &second_policy,
                || {
                    second_calls += 1;
                    let call = second_calls;
                    async move {
                        if call == 1 {
                            Err(LlmError::ServerError {
                                status: 502,
                                message: "opaque".into(),
                            })
                        } else {
                            Ok(())
                        }
                    }
                },
                None
            )
        ),
    );
    assert!(a.is_ok() && b.is_ok());
    assert_eq!((first_calls, second_calls), (2, 2));
    assert_eq!(first_count.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(second_count.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(
        first_receipts.lock().unwrap()[0],
        "Retry attempt: transport 1/2; timeout; waiting 0.00s"
    );
    assert_eq!(
        second_receipts.lock().unwrap()[0],
        "Retry attempt: transport 1/4; upstream 502; waiting 0.00s"
    );
}

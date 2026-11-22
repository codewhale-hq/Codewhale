use crate::llm_client::{LlmError, RequestTransmission};
use codewhale_error_taxonomy::{
    ErrorCategory, ErrorEnvelope, ErrorSeverity, classify_error_message,
};

/// Classify a boundary error from the typed [`LlmError`] when one is
/// available, keeping the caller's display message.
///
/// Boundaries that only have an `anyhow::Error` historically stringified it
/// and classified the *string* with `recoverable = true`. That downgraded
/// terminal provider rejections such as `Model error: Model not exist.`
/// (typed `LlmError::ModelError` → InvalidInput / `Error` severity /
/// not recoverable) into `Internal` + `Warning` + recoverable noise, so the
/// transcript showed a dismissable "Warn" row for a failure that actually
/// ended the turn. When the typed error survives to the boundary, preserve
/// its category, severity, and recovery contract verbatim; only genuinely
/// untyped errors fall back to string classification.
#[must_use]
pub fn envelope_for_llm_error(error: anyhow::Error, display_message: String) -> ErrorEnvelope {
    let llm = error.downcast_ref::<LlmError>();
    let transmission = llm.map_or(RequestTransmission::OutcomeUnknown, LlmError::transmission);
    let http_status = llm.and_then(|error| match error {
        LlmError::ServerError { status, .. } | LlmError::InvalidRequest { status, .. } => {
            Some(*status)
        }
        _ => None,
    });
    let timeout_ms = llm.and_then(|error| match error {
        LlmError::Timeout(duration) if !duration.is_zero() => Some(duration.as_millis()),
        LlmError::HeaderTimeout { waited, .. } => Some(waited.as_millis()),
        _ => None,
    });
    let mut envelope = match error.downcast::<LlmError>() {
        Ok(llm) => {
            let mut envelope = ErrorEnvelope::from(llm);
            envelope.message = display_message;
            envelope
        }
        Err(error) => {
            drop(error);
            ErrorEnvelope::classify(display_message, false)
        }
    };
    envelope.request_transmission = Some(transmission);
    tracing::warn!(
        code = %envelope.code,
        category = %envelope.category,
        ?transmission,
        ?http_status,
        ?timeout_ms,
        "provider request failed"
    );
    envelope
}

impl From<LlmError> for ErrorEnvelope {
    fn from(value: LlmError) -> Self {
        match value {
            LlmError::QuotaWindow { message, .. } => Self::new(
                ErrorCategory::RateLimit,
                ErrorSeverity::Error,
                false,
                "llm_quota_window",
                message,
            ),
            LlmError::RateLimited { message, .. } => Self::new(
                ErrorCategory::RateLimit,
                ErrorSeverity::Warning,
                true,
                "llm_rate_limited",
                message,
            ),
            // Keep the broad wire-compatible category while making the typed
            // code and recovery contract distinct from an ordinary 429.
            LlmError::QuotaExhausted(error) => Self::new(
                ErrorCategory::RateLimit,
                ErrorSeverity::Error,
                false,
                "llm_quota_exhausted",
                error.into_message(),
            ),
            LlmError::ServerError { status, message } => Self::new(
                ErrorCategory::Internal,
                ErrorSeverity::Error,
                false,
                format!("llm_server_{status}"),
                message,
            ),
            LlmError::NetworkError(message) => Self::new(
                ErrorCategory::Network,
                ErrorSeverity::Error,
                false,
                "llm_network_error",
                message,
            ),
            LlmError::HeaderTimeout { .. } => Self::new(
                ErrorCategory::Network,
                ErrorSeverity::Error,
                false,
                "llm_network_error",
                value.header_timeout_message(),
            ),
            LlmError::ConnectError {
                message, transient, ..
            } => Self::new(
                ErrorCategory::Network,
                ErrorSeverity::Error,
                transient,
                "llm_connect_before_dispatch",
                message,
            ),
            LlmError::Timeout(duration) if duration.is_zero() => Self::new(
                ErrorCategory::Timeout,
                ErrorSeverity::Warning,
                false,
                "llm_timeout",
                "Request timed out (provider request exceeded its transport \
                 budget; exact duration not recorded)"
                    .to_string(),
            ),
            LlmError::Timeout(duration) => Self::new(
                ErrorCategory::Timeout,
                ErrorSeverity::Warning,
                false,
                "llm_timeout",
                format!("Request timed out after {duration:?}"),
            ),
            LlmError::AuthenticationError(auth) => Self::new(
                ErrorCategory::Authentication,
                ErrorSeverity::Critical,
                false,
                "llm_auth_error",
                auth.to_user_message(),
            ),
            LlmError::AuthorizationError(message) => Self::new(
                ErrorCategory::Authorization,
                ErrorSeverity::Error,
                false,
                "llm_authorization_error",
                message,
            ),
            LlmError::InvalidRequest { message, .. } => Self::new(
                ErrorCategory::InvalidInput,
                ErrorSeverity::Error,
                false,
                "llm_invalid_request",
                message,
            ),
            LlmError::ModelError(message) => Self::new(
                ErrorCategory::InvalidInput,
                ErrorSeverity::Error,
                false,
                "llm_model_error",
                message,
            ),
            LlmError::ContentPolicyError(message) => Self::new(
                ErrorCategory::Authorization,
                ErrorSeverity::Error,
                false,
                "llm_content_policy",
                message,
            ),
            LlmError::ParseError(message) => Self::new(
                ErrorCategory::Parse,
                ErrorSeverity::Error,
                false,
                "llm_parse_error",
                message,
            ),
            LlmError::ContextLengthError(message) => Self::new(
                ErrorCategory::InvalidInput,
                ErrorSeverity::Error,
                false,
                "llm_context_length",
                message,
            ),
            LlmError::Other(message) => envelope_for_other_llm_error(message),
        }
    }
}

/// `LlmError::Other` is the catch-all for an HTTP status with no dedicated
/// variant (a 402 without explicit quota evidence, a 405/409/413/422
/// rejection). Its text still says what happened, so a determinate answer from
/// the string classifier wins over the blanket `Internal` label (#6843): an
/// out-of-credits 402 reads as a spent balance, a 4xx as a rejected input.
/// Only text the classifier cannot place stays `Internal`.
fn envelope_for_other_llm_error(message: String) -> ErrorEnvelope {
    let category = classify_error_message(&message);
    if matches!(
        category,
        ErrorCategory::Tool | ErrorCategory::State | ErrorCategory::Internal
    ) {
        return ErrorEnvelope::new(
            ErrorCategory::Internal,
            ErrorSeverity::Error,
            false,
            "llm_other",
            message,
        );
    }
    let recoverable = false;
    let mut envelope = ErrorEnvelope::classify(message, recoverable);
    // The typed `NetworkError` is an Error-severity card; an `Other` that reads
    // as one must not turn amber for a turn that later fails after its retries.
    if matches!(category, ErrorCategory::Network | ErrorCategory::Budget) {
        envelope.severity = ErrorSeverity::Error;
    }
    envelope
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_envelope_carries_request_transmission() {
        let envelope = envelope_for_llm_error(
            LlmError::Timeout(std::time::Duration::from_secs(1)).into(),
            "Request timed out".into(),
        );

        assert_eq!(
            envelope.request_transmission,
            Some(RequestTransmission::OutcomeUnknown)
        );
    }

    #[test]
    fn request_transmission_is_optional_and_uses_snake_case_on_the_wire() {
        let legacy: ErrorEnvelope = serde_json::from_value(serde_json::json!({
            "category": "network",
            "severity": "error",
            "recoverable": false,
            "code": "llm_network_error",
            "message": "connection reset"
        }))
        .expect("legacy envelope");
        assert_eq!(legacy.request_transmission, None);
        assert!(
            serde_json::to_value(&legacy)
                .expect("serialize legacy envelope")
                .get("request_transmission")
                .is_none()
        );

        let mut current = legacy;
        current.request_transmission = Some(RequestTransmission::OutcomeUnknown);
        assert_eq!(
            serde_json::to_value(current).expect("serialize current envelope")["request_transmission"],
            "outcome_unknown"
        );
    }

    #[test]
    fn provider_failures_reach_info_filtered_logs_without_raw_error_text() {
        use std::io::{Read, Seek, SeekFrom};
        use std::time::Duration;

        let secret = "sk-diagnostic-fixture-secret";
        let display_message = format!("connection reset: {secret}");
        let cases: [(anyhow::Error, &[&str], bool); 6] = [
            (
                anyhow::Error::new(LlmError::Timeout(Duration::from_millis(38_689)))
                    .context(format!("request context: {secret}")),
                &[
                    "code=llm_timeout",
                    "category=timeout",
                    "transmission=OutcomeUnknown",
                    "http_status=None",
                    "timeout_ms=Some(38689)",
                ],
                false,
            ),
            (
                LlmError::Timeout(Duration::ZERO).into(),
                &[
                    "code=llm_timeout",
                    "transmission=OutcomeUnknown",
                    "http_status=None",
                    "timeout_ms=None",
                ],
                false,
            ),
            (
                LlmError::ConnectError {
                    message: secret.into(),
                    transient: true,
                    timed_out: false,
                }
                .into(),
                &[
                    "code=llm_connect_before_dispatch",
                    "category=network",
                    "transmission=NotDispatched",
                    "http_status=None",
                    "timeout_ms=None",
                ],
                true,
            ),
            (
                LlmError::ServerError {
                    status: 503,
                    message: secret.into(),
                }
                .into(),
                &[
                    "code=llm_server_503",
                    "category=internal",
                    "transmission=OutcomeUnknown",
                    "http_status=Some(503)",
                    "timeout_ms=None",
                ],
                false,
            ),
            (
                LlmError::InvalidRequest {
                    status: 413,
                    message: secret.into(),
                }
                .into(),
                &[
                    "code=llm_invalid_request",
                    "category=invalid_input",
                    "transmission=Rejected",
                    "http_status=Some(413)",
                    "timeout_ms=None",
                ],
                false,
            ),
            (
                anyhow::anyhow!("connection reset: {secret}"),
                &[
                    "code=network",
                    "category=network",
                    "transmission=OutcomeUnknown",
                    "http_status=None",
                    "timeout_ms=None",
                ],
                false,
            ),
        ];
        let mut output = tempfile::tempfile().unwrap();
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
            .with_writer(std::sync::Mutex::new(output.try_clone().unwrap()))
            .finish();
        let expected = tracing::subscriber::with_default(subscriber, || {
            cases.map(|(error, fields, recoverable)| {
                let envelope = envelope_for_llm_error(error, display_message.clone());
                assert_eq!(envelope.message, display_message);
                assert_eq!(envelope.recoverable, recoverable);
                fields
            })
        });
        output.seek(SeekFrom::Start(0)).unwrap();
        let mut logs = String::new();
        output.read_to_string(&mut logs).unwrap();
        assert!(!logs.contains(secret), "{logs}");
        assert!(!logs.contains("connection reset"), "{logs}");
        assert!(!logs.contains("request context"), "{logs}");
        let lines: Vec<_> = logs.lines().collect();
        assert_eq!(lines.len(), expected.len(), "{logs}");
        for (line, fields) in lines.into_iter().zip(expected) {
            for field in ["WARN", "provider request failed"]
                .into_iter()
                .chain(fields.iter().copied())
            {
                assert!(line.contains(field), "missing {field}: {line}");
            }
        }
    }

    #[test]
    fn typed_llm_quota_envelope_is_non_recoverable_and_distinct_from_rate_limit() {
        let envelope = ErrorEnvelope::from(LlmError::from_http_response(
            429,
            r#"{"error":{"code":"insufficient_quota"}}"#,
        ));
        assert_eq!(envelope.category, ErrorCategory::RateLimit);
        assert_eq!(envelope.severity, ErrorSeverity::Error);
        assert!(!envelope.recoverable);
        assert_eq!(envelope.code, "llm_quota_exhausted");
    }

    #[test]
    fn llm_auth_error_envelope_renders_context_without_secret() {
        let api_key = "tp-secret-token-plan-value";
        let envelope = ErrorEnvelope::from(LlmError::from_http_response_with_request_context(
            401,
            &format!("Invalid API Key: {api_key}"),
            Some("Xiaomi MiMo"),
            Some("https://token-plan-sgp.xiaomimimo.com/v1"),
            Some("mimo-v2.5"),
            Some("env"),
            Some(api_key),
        ));
        assert_eq!(envelope.category, ErrorCategory::Authentication);
        assert_eq!(envelope.severity, ErrorSeverity::Critical);
        assert!(!envelope.recoverable);
        for expected in [
            "provider: Xiaomi MiMo",
            "base URL authority: token-plan-sgp.xiaomimimo.com",
            "model: mimo-v2.5",
            "key source: env",
            "key fingerprint: tp-... (len=26)",
            "key type: Xiaomi MiMo Token Plan key",
        ] {
            assert!(envelope.message.contains(expected));
        }
        assert!(!envelope.message.contains(api_key));
        assert!(!envelope.message.contains("secret-token-plan-value"));
    }

    #[test]
    fn typed_llm_error_preserves_terminal_severity_across_boundary() {
        // Even where the typed error survives to the boundary (the turn loop's
        // stream-initiation and mid-stream paths), `envelope_for_llm_error`
        // must keep the typed contract instead of re-classifying the string
        // with `recoverable = true`.
        let typed: anyhow::Error = LlmError::ModelError("Model not exist.".to_string()).into();
        let envelope = envelope_for_llm_error(typed, "Model error: Model not exist.".to_string());
        assert_eq!(envelope.category, ErrorCategory::InvalidInput);
        assert_eq!(envelope.severity, ErrorSeverity::Error);
        assert_eq!(envelope.code, "llm_model_error");
        assert_eq!(envelope.message, "Model error: Model not exist.");

        // Untyped errors keep the legacy string fallback.
        let untyped: anyhow::Error = anyhow::anyhow!("stream read error: connection reset");
        let envelope =
            envelope_for_llm_error(untyped, "stream read error: connection reset".into());
        assert_eq!(envelope.category, ErrorCategory::Network);
        assert!(!envelope.recoverable);
    }

    #[test]
    fn untyped_other_llm_error_keeps_its_determinate_class() {
        // Row 4 of the report: `LlmError::Other` carries a 402 without explicit
        // quota evidence. It is a spent balance, not an internal fault, and
        // resending cannot fix it.
        let envelope = ErrorEnvelope::from(LlmError::Other(
            "HTTP 402: This request requires more credits, or fewer max_tokens.".to_string(),
        ));
        assert_eq!(envelope.category, ErrorCategory::RateLimit);
        assert_eq!(envelope.severity, ErrorSeverity::Error);
        assert!(!envelope.recoverable);

        // A rejected input is terminal like the typed `InvalidRequest`.
        let envelope =
            ErrorEnvelope::from(LlmError::Other("HTTP 413: payload too large".to_string()));
        assert_eq!(envelope.category, ErrorCategory::InvalidInput);
        assert!(!envelope.recoverable);

        // Text the classifier cannot place keeps the legacy label.
        let envelope = ErrorEnvelope::from(LlmError::Other("Unknown retry error".to_string()));
        assert_eq!(envelope.category, ErrorCategory::Internal);
        assert_eq!(envelope.code, "llm_other");
        assert!(!envelope.recoverable);
    }

    #[test]
    fn untyped_other_llm_error_keeps_the_retry_tail_unless_it_cannot_heal() {
        // A malformed chunk or a transient class wrapped as `Other` keeps the
        // legacy retry contract; only a rejected input, a refused credential and a
        // spent balance are terminal.
        for message in ["malformed chunk in stream", "error decoding response body"] {
            let envelope = ErrorEnvelope::from(LlmError::Other(message.to_string()));
            assert!(!envelope.recoverable, "{message}");
        }
        // A transient `Other` is an Error-severity card like the typed
        // `NetworkError`, not an amber one that outlives a failed retry.
        let envelope =
            ErrorEnvelope::from(LlmError::Other("error decoding response body".to_string()));
        assert_eq!(envelope.category, ErrorCategory::Network);
        assert_eq!(envelope.severity, ErrorSeverity::Error);
        let envelope = ErrorEnvelope::from(LlmError::Other("HTTP 403: forbidden".to_string()));
        assert_eq!(envelope.category, ErrorCategory::Authorization);
        assert!(!envelope.recoverable);
    }

    #[test]
    fn zero_timeout_envelope_reports_unknown_duration_not_0ns() {
        let envelope = ErrorEnvelope::from(LlmError::Timeout(std::time::Duration::from_secs(0)));
        assert_eq!(envelope.category, ErrorCategory::Timeout);
        assert_eq!(envelope.code, "llm_timeout");
        assert!(
            !envelope.message.contains("0ns"),
            "zero duration leaked as 0ns: {}",
            envelope.message
        );
        assert!(
            envelope.message.contains("duration not recorded"),
            "{}",
            envelope.message
        );

        let measured = ErrorEnvelope::from(LlmError::Timeout(std::time::Duration::from_secs(90)));
        assert!(
            measured.message.contains("Request timed out after 90s"),
            "{}",
            measured.message
        );
    }
}

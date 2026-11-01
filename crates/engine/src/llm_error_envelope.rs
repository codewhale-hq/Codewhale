use crate::llm_client::LlmError;
use codewhale_error_taxonomy::{
    ErrorCategory, ErrorEnvelope, ErrorSeverity, classify_error_message, is_spent_balance_message,
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
    match error.downcast::<LlmError>() {
        Ok(llm) => {
            let mut envelope = ErrorEnvelope::from(llm);
            envelope.message = display_message;
            envelope
        }
        Err(error) => {
            drop(error);
            ErrorEnvelope::classify(display_message, true)
        }
    }
}

impl From<LlmError> for ErrorEnvelope {
    fn from(value: LlmError) -> Self {
        match value {
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
                true,
                format!("llm_server_{status}"),
                message,
            ),
            LlmError::NetworkError(message) => Self::new(
                ErrorCategory::Network,
                ErrorSeverity::Error,
                true,
                "llm_network_error",
                message,
            ),
            LlmError::Timeout(duration) => Self::new(
                ErrorCategory::Timeout,
                ErrorSeverity::Warning,
                true,
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
            true,
            "llm_other",
            message,
        );
    }
    // Mirror the typed variants. Only a credential or authorization refusal, a
    // rejected input and a spent balance do not heal by resending; everything
    // else (a transient class, a spent budget, an unparseable chunk) keeps the
    // retry tail the legacy `Internal` label had.
    let recoverable = match category {
        ErrorCategory::Authentication
        | ErrorCategory::Authorization
        | ErrorCategory::InvalidInput => false,
        ErrorCategory::RateLimit => !is_spent_balance_message(&message),
        _ => true,
    };
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
        assert!(envelope.recoverable);
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
        assert!(envelope.recoverable);
    }

    #[test]
    fn untyped_other_llm_error_keeps_the_retry_tail_unless_it_cannot_heal() {
        // A malformed chunk or a transient class wrapped as `Other` keeps the
        // legacy retry contract; only a rejected input, a refused credential and a
        // spent balance are terminal.
        for message in ["malformed chunk in stream", "error decoding response body"] {
            let envelope = ErrorEnvelope::from(LlmError::Other(message.to_string()));
            assert!(envelope.recoverable, "{message}");
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
}

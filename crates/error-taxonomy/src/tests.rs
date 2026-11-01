use super::*;

#[test]
fn model_output_truncated_classifies_as_invalid_input_not_tool() {
    // The turn-level "Model output truncated" error is a provider/model
    // condition, not a tool failure: it must land in the same bucket as
    // `LlmError::ModelError` so the exec termination classifier reduces it
    // to `RunTerminationReason::ModelError` (never Resolved).
    assert_eq!(
        classify_error_message(
            "Model output truncated: provider stop reason `max_output_tokens`; no complete response or tool call was accepted."
        ),
        ErrorCategory::InvalidInput
    );
    assert_eq!(
        classify_error_message(
            "Model output truncated: provider stop reason `max_tokens`; no complete response or tool call was accepted."
        ),
        ErrorCategory::InvalidInput
    );
    assert_eq!(
        classify_error_message(
            "Model response incomplete: provider stop reason `content_filter`; no complete response or tool call was accepted."
        ),
        ErrorCategory::InvalidInput
    );
}

#[test]
fn raw_rate_and_quota_phrases_remain_coarse_rate_limit_diagnostics() {
    for message in [
        "Rate limit reached for gpt-4",
        "Too Many Requests",
        "HTTP 429 from upstream",
        "Your quota has been exceeded",
        "Authorization failed: You've reached your usage limit for this billing cycle",
    ] {
        assert_eq!(classify_error_message(message), ErrorCategory::RateLimit);
    }
}

#[test]
fn model_not_exist_rejection_is_a_terminal_invalid_input_error() {
    // The reported incident: a provider switch left GLM-5.3 selected on a
    // Model Studio route; the next message failed with
    // `Model error: Model not exist.` and the transcript rendered it as a
    // dismissable *warning* because the stringified error fell through to
    // `Internal` + `recoverable`. A wrong-model rejection is terminal for
    // the request: it must classify as InvalidInput so it renders at Error
    // severity and never as recoverable noise.
    for message in [
        "Model error: Model not exist.",
        "Model not exist",
        "Model not found: glm-5.3 on this endpoint",
        "Unknown model identifier",
        "Invalid model: qwen-flash",
    ] {
        let envelope = ErrorEnvelope::classify(message.to_string(), true);
        assert_eq!(
            envelope.category,
            ErrorCategory::InvalidInput,
            "message must classify as InvalidInput: {message}"
        );
        assert_eq!(
            envelope.severity,
            ErrorSeverity::Error,
            "wrong-model rejections must render at Error severity: {message}"
        );
        // `recoverable` governs offline-mode semantics, not transcript
        // severity: a wrong-model rejection keeps the session online so the
        // operator can repair the route.
        assert!(envelope.recoverable, "session stays online: {message}");
    }
}

/// #6843: determinate rejections and Codewhale's own stops used to fall to
/// `Internal`, which `classify` renders as an amber warning. Each text below
/// is a row of the report's measured table.
#[test]
fn determinate_rejections_and_own_stops_are_not_internal() {
    for (message, expected) in [
        (
            r#"Invalid request (400): {"message":"a single path expansion cannot exceed 512 candidates","type":"invalid_request_error"}"#,
            ErrorCategory::InvalidInput,
        ),
        (
            "SSE stream request failed: HTTP 422 Unprocessable Entity",
            ErrorCategory::InvalidInput,
        ),
        (
            "The request still exceeds this model's context budget and automatic recovery did not complete. The conversation is saved; retry or choose a larger context route.",
            ErrorCategory::InvalidInput,
        ),
        (
            "Model returned terminal stop reason `stop` with no answer or tool call (after 2 retries).",
            ErrorCategory::InvalidInput,
        ),
        (
            "Turn failed: Per-turn wall-clock budget exhausted after 86418s (limit: 86400s). The turn was stopped before another model request.",
            ErrorCategory::Budget,
        ),
        ("ERROR", ErrorCategory::Parse),
        ("  error. ", ErrorCategory::Parse),
        // Still transient, still resumable: only determinate rejections moved.
        (
            "Provider returned an empty response",
            ErrorCategory::Network,
        ),
        ("HTTP 500 Internal Server Error", ErrorCategory::Network),
        ("Upstream idle timeout exceeded", ErrorCategory::Timeout),
    ] {
        assert_eq!(classify_error_message(message), expected, "{message}");
    }

    // A rejection classified here renders as an error, not an amber warning,
    // even through the fallback that assumes `recoverable`.
    let envelope = ErrorEnvelope::classify(
        "Invalid request (400): {\"type\":\"invalid_request_error\"}".to_string(),
        true,
    );
    assert_eq!(envelope.category, ErrorCategory::InvalidInput);
    assert_eq!(envelope.severity, ErrorSeverity::Error);
}

#[test]
fn bare_numbers_are_not_http_statuses() {
    // The status vocabulary needs an "HTTP"/"status" lead-in: a count, an ID
    // or a URL segment that happens to read 400 must not classify a failure.
    for message in [
        "read 400 lines from the file",
        "https://example.com/v1/items/422",
        "request id 500123",
        "HTTP 4000 is not a status",
    ] {
        assert_eq!(
            classify_error_message(message),
            ErrorCategory::Internal,
            "{message}"
        );
    }
}

#[test]
fn unreadable_error_notice_states_it_and_classifies_as_parse() {
    let notice = unreadable_error_notice("ERROR").expect("bare placeholder");
    assert!(notice.contains("unreadable error"), "{notice}");
    assert_eq!(classify_error_message(&notice), ErrorCategory::Parse);
    assert!(unreadable_error_notice("").is_some());
    assert!(unreadable_error_notice("Provider returned an empty response").is_none());
    assert!(unreadable_error_notice("model error: boom").is_none());
}

#[test]
fn a_failure_that_ended_the_work_is_never_a_warning() {
    // #6795: an error frame that fails the turn passes `recoverable = false`.
    // Network is a transient class, but an ended turn is an error.
    for message in ["Provider returned an empty response", "rate limit reached"] {
        let ended = ErrorEnvelope::classify(message.to_string(), false);
        assert_eq!(ended.severity, ErrorSeverity::Error, "{message}");
        assert!(!ended.recoverable);
        let ongoing = ErrorEnvelope::classify(message.to_string(), true);
        assert_eq!(ongoing.severity, ErrorSeverity::Warning, "{message}");
    }
}

#[test]
fn budget_stop_is_an_error_card_that_keeps_the_session_online() {
    let envelope = ErrorEnvelope::budget_stop("Maximum model steps reached before completion");
    assert_eq!(envelope.category, ErrorCategory::Budget);
    assert_eq!(envelope.severity, ErrorSeverity::Error);
    assert!(envelope.recoverable);
}

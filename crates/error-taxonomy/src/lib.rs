//! Shared error taxonomy across client, tools, runtime, and UI.
use std::fmt;

use codewhale_tools::ToolError;

/// Broad category for typed error handling and policy decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    Network,
    Authentication,
    Authorization,
    RateLimit,
    Timeout,
    Budget,
    InvalidInput,
    Parse,
    Tool,
    State,
    Internal,
}

/// Severity hint for UI and logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Error code for a provider credential rejection (401-class) that arrived
/// before any model output, after the engine took the turn's question back
/// out of the session (#6566). Hosts that see it return the text to the
/// person to send again; nothing else about the authentication error changes.
pub const CREDENTIAL_REJECTED_UNSENT_CODE: &str = "llm_auth_rejected_unsent";

/// Unified envelope used when crossing subsystem boundaries.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ErrorEnvelope {
    pub category: ErrorCategory,
    pub severity: ErrorSeverity,
    pub recoverable: bool,
    pub code: String,
    pub message: String,
}

impl fmt::Display for ErrorCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Network => "network",
            Self::Authentication => "authentication",
            Self::Authorization => "authorization",
            Self::RateLimit => "rate_limit",
            Self::Timeout => "timeout",
            Self::Budget => "budget",
            Self::InvalidInput => "invalid_input",
            Self::Parse => "parse",
            Self::Tool => "tool",
            Self::State => "state",
            Self::Internal => "internal",
        };
        f.write_str(label)
    }
}

impl fmt::Display for ErrorSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
            Self::Critical => "critical",
        };
        f.write_str(label)
    }
}

impl fmt::Display for ErrorEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}: {}", self.severity, self.code, self.message)
    }
}

impl std::error::Error for ErrorEnvelope {}

impl ErrorEnvelope {
    #[must_use]
    pub fn new(
        category: ErrorCategory,
        severity: ErrorSeverity,
        recoverable: bool,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            severity,
            recoverable,
            code: code.into(),
            message: message.into(),
        }
    }

    /// Recoverable internal error — stream stalls, transient retries, generic
    /// engine errors that the user can resolve by retrying. Severity is
    /// `Warning` so the UI surfaces it in amber rather than red.
    #[must_use]
    pub fn transient(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCategory::Internal,
            ErrorSeverity::Warning,
            true,
            "transient",
            message,
        )
    }

    /// Non-recoverable internal error — missing client, spawn failure, etc.
    /// Flips the session into offline mode.
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn fatal(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCategory::Internal,
            ErrorSeverity::Error,
            false,
            "fatal",
            message,
        )
    }

    /// Authentication failure — fatal for the attempted operation.
    #[must_use]
    pub fn fatal_auth(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCategory::Authentication,
            ErrorSeverity::Critical,
            false,
            "auth_fatal",
            message,
        )
    }

    /// Context length / overflow — invalid input, recoverable via /compact.
    #[must_use]
    pub fn context_overflow(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCategory::InvalidInput,
            ErrorSeverity::Error,
            true,
            "context_overflow",
            message,
        )
    }

    #[must_use]
    pub fn image_count_exceeded(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCategory::InvalidInput,
            ErrorSeverity::Error,
            true,
            "image_count_exceeded",
            message,
        )
    }

    /// A stop Codewhale applied to itself (step ceiling, per-turn wall clock).
    /// The turn failed, so the card is an error, but the session stays usable:
    /// sending another message continues, so it is `recoverable` and does not
    /// flip the session offline (#6843).
    #[must_use]
    pub fn budget_stop(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCategory::Budget,
            ErrorSeverity::Error,
            true,
            "turn_budget_stop",
            message,
        )
    }

    /// Recoverable network / transport hiccup.
    #[must_use]
    #[cfg(any(test, feature = "test-support"))]
    pub fn network(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCategory::Network,
            ErrorSeverity::Warning,
            true,
            "network_transient",
            message,
        )
    }

    /// Build an envelope by classifying a raw error message string. Used at
    /// boundaries where the underlying error type was already stringified.
    #[must_use]
    pub fn classify(message: impl Into<String>, recoverable: bool) -> Self {
        let message = message.into();
        let category = classify_error_message(&message);
        let severity = match category {
            ErrorCategory::Authentication => ErrorSeverity::Critical,
            // A transient class stays amber only while the caller still
            // expects the work to continue; `recoverable = false` means it
            // ended (an error frame that failed the turn, a spent balance),
            // and an ended turn is never rendered as a warning (#6795).
            ErrorCategory::RateLimit
            | ErrorCategory::Timeout
            | ErrorCategory::Network
            | ErrorCategory::Budget => {
                if recoverable {
                    ErrorSeverity::Warning
                } else {
                    ErrorSeverity::Error
                }
            }
            ErrorCategory::InvalidInput | ErrorCategory::Authorization | ErrorCategory::Parse => {
                ErrorSeverity::Error
            }
            ErrorCategory::Tool | ErrorCategory::State | ErrorCategory::Internal => {
                if recoverable {
                    ErrorSeverity::Warning
                } else {
                    ErrorSeverity::Error
                }
            }
        };
        Self::new(
            category,
            severity,
            recoverable,
            category.to_string(),
            message,
        )
    }
}

/// Classify an error message string into an ErrorCategory.
///
/// Uses heuristic keyword matching on the lowercased message.
/// This is a replacement for ad-hoc string matching in callers.
#[must_use]
pub fn classify_error_message(message: &str) -> ErrorCategory {
    let lower = message.to_lowercase();

    // A bare placeholder ("ERROR") carries no cause to classify. Say so
    // instead of letting it fall to `Internal` (#6843).
    if is_unreadable_error_text(&lower) || lower.contains("unreadable error") {
        return ErrorCategory::Parse;
    }
    // Codewhale's own ceilings (step count, per-turn wall clock) and any
    // provider text that names an exhausted budget.
    if lower.contains("maximum model steps")
        || lower.contains("step budget exhausted")
        || lower.contains("wall-clock budget")
        || lower.contains("wall clock budget")
        || lower.contains("budget exhausted")
    {
        return ErrorCategory::Budget;
    }
    if lower.contains("model output truncated")
        || lower.contains("model response incomplete")
        || lower.contains("maximum context length")
        || lower.contains("context length")
        || lower.contains("context_length")
        || lower.contains("prompt is too long")
        || (lower.contains("requested") && lower.contains("tokens") && lower.contains("maximum"))
        || lower.contains("context window")
        // Codewhale's own overflow stop: the request no longer fits the
        // route's context budget and automatic recovery gave up.
        || lower.contains("context budget")
        // A turn that ended on a terminal stop reason with nothing usable is
        // an incomplete model response, like the two arms above, not a tool
        // fault that merely mentions "tool call".
        || lower.contains("no answer or tool call")
        || lower.contains("model not exist")
        || lower.contains("model not found")
        || lower.contains("no such model")
        || lower.contains("unknown model")
        || lower.contains("invalid model")
        || lower.contains("model does not exist")
        || lower.starts_with("model error:")
    {
        return ErrorCategory::InvalidInput;
    }
    if lower.contains("rate limit")
        || lower.contains("too many requests")
        // Status codes are standalone tokens, not digits inside a URL or ID.
        || lower.split_whitespace().any(|part| {
            part.trim_matches(['(', ')', '[', ']', '{', '}', ':', ';', ',', '.', '\'', '"'])
                == "429"
        })
        || is_spent_balance_text(&lower)
    {
        return ErrorCategory::RateLimit;
    }
    if lower.contains("timeout") || lower.contains("timed out") {
        return ErrorCategory::Timeout;
    }
    if lower.contains("authentication")
        || lower.contains("auth failed")
        || lower.contains("auth error")
        || lower.contains("unauthorized")
        || lower.contains("api key")
        || lower.contains("invalid key")
        || lower.contains("invalid token")
        || lower.contains("bearer token")
    {
        return ErrorCategory::Authentication;
    }
    if lower.contains("authorization")
        || lower.contains("permission")
        || lower.contains("forbidden")
        || lower.contains("denied")
    {
        return ErrorCategory::Authorization;
    }
    // A rejected request is determinate: the same input fails again, so it is
    // an input fault, not an internal one (#6843). Placed after the
    // credential, quota and timeout vocabulary so an explicit billing or auth
    // reason inside a 400 body still wins.
    if lower.contains("invalid request")
        || lower.contains("invalid_request_error")
        || lower.contains("bad request")
        || lower.contains("unprocessable")
        || lower.contains("payload too large")
        || lower.contains("request entity too large")
        || mentions_http_status(&lower, &[400, 405, 409, 410, 413, 415, 422])
    {
        return ErrorCategory::InvalidInput;
    }
    if lower.contains("network")
        || lower.contains("connection")
        || lower.contains("dns")
        || lower.contains("stream read error")
        || lower.contains("error decoding response body")
        || lower.contains("chunk decode error")
        || lower.contains("body decode")
        || lower.contains("temporarily unavailable")
        // Gateways report a failed or empty upstream inside a 200 with this
        // wording (OpenRouter); it is the upstream being unreachable.
        || lower.contains("provider returned error")
        || lower.contains("provider returned an empty response")
        // Upstream 5xx wording, as a status or as the reason phrase.
        || lower.contains("bad gateway")
        || lower.contains("service unavailable")
        || lower.contains("internal server error")
        || lower.contains("overloaded")
        || mentions_http_status(&lower, &[500, 502, 503, 504, 529])
        || lower.contains(" 502 ")
        || lower.contains(" 503 ")
        || lower.contains(" 504 ")
        || lower.starts_with("502 ")
        || lower.starts_with("503 ")
        || lower.starts_with("504 ")
        || lower.ends_with(" 502")
        || lower.ends_with(" 503")
        || lower.ends_with(" 504")
        || lower == "502"
        || lower == "503"
        || lower == "504"
    {
        return ErrorCategory::Network;
    }
    if lower.contains("parse") || lower.contains("syntax") || lower.contains("malformed") {
        return ErrorCategory::Parse;
    }
    if lower.contains("not found")
        || lower.contains("unavailable")
        || lower.contains("not available")
    {
        return ErrorCategory::State;
    }
    if lower.contains("tool") {
        return ErrorCategory::Tool;
    }

    ErrorCategory::Internal
}

/// True when `message` reports an exhausted balance rather than a
/// short-lived limit: quota, usage limit, or a prepaid gateway's HTTP 402
/// ("Insufficient funds"). The operator resolves it by topping up or switching
/// route, so resending the same request cannot help.
pub fn is_spent_balance_message(message: &str) -> bool {
    is_spent_balance_text(&message.to_lowercase())
}

/// True when `lower` (already lowercased) reports an exhausted balance rather
/// than a short-lived limit: quota, usage limit, or a prepaid gateway's HTTP
/// 402 ("Insufficient funds"). The operator resolves it by topping up or
/// switching route, so resending the same request cannot help.
fn is_spent_balance_text(lower: &str) -> bool {
    lower.contains("quota")
        || lower.contains("usage limit")
        || lower.contains("insufficient credits")
        || lower.contains("insufficient funds")
        || lower.contains("payment required")
        || lower.contains("http 402")
}

/// True when a provider "error" is only a placeholder word such as `ERROR`.
/// `lower` is lowercased. An empty string is not a placeholder here: callers
/// that hold an empty diagnostic mean "nothing recorded", not "unreadable".
fn is_unreadable_error_text(lower: &str) -> bool {
    let token = lower.trim().trim_matches(|c: char| !c.is_alphanumeric());
    matches!(token, "error" | "err" | "unknown error")
}

/// What the transcript says instead of a bare placeholder error text, or
/// `None` when the text is readable. The wording classifies as
/// [`ErrorCategory::Parse`], the "unreadable" label (#6843).
#[must_use]
pub fn unreadable_error_notice(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if !trimmed.is_empty() && !is_unreadable_error_text(&message.to_lowercase()) {
        return None;
    }
    Some(if trimmed.is_empty() {
        "The provider reported an unreadable error: it sent no error text.".to_string()
    } else {
        format!(
            "The provider reported an unreadable error: it sent only \"{trimmed}\" with no detail."
        )
    })
}

/// True when `lower` names one of `codes` as an HTTP status ("HTTP 422",
/// "status 413", "status code: 500"). A bare number is never a status: digits
/// inside a URL, an ID or a count must not classify a failure.
fn mentions_http_status(lower: &str, codes: &[u16]) -> bool {
    [
        "http ",
        "status ",
        "status: ",
        "status code ",
        "status code: ",
    ]
    .iter()
    .any(|prefix| {
        lower.match_indices(prefix).any(|(at, _)| {
            let rest = &lower[at + prefix.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            digits.len() == 3
                && digits
                    .parse::<u16>()
                    .is_ok_and(|code| codes.contains(&code))
        })
    })
}

impl From<ToolError> for ErrorEnvelope {
    fn from(value: ToolError) -> Self {
        match value {
            ToolError::InvalidInput { message } => Self::new(
                ErrorCategory::InvalidInput,
                ErrorSeverity::Error,
                false,
                "tool_invalid_input",
                message,
            ),
            ToolError::MissingField { field } => Self::new(
                ErrorCategory::InvalidInput,
                ErrorSeverity::Error,
                false,
                "tool_missing_field",
                format!("Missing required field: {field}"),
            ),
            ToolError::PathEscape { path } => Self::new(
                ErrorCategory::Authorization,
                ErrorSeverity::Error,
                false,
                "tool_path_escape",
                format!("Path escapes workspace: {}", path.display()),
            ),
            ToolError::ExecutionFailed { message, .. } => Self::new(
                ErrorCategory::Tool,
                ErrorSeverity::Error,
                true,
                "tool_execution_failed",
                message,
            ),
            ToolError::Timeout { seconds } => Self::new(
                ErrorCategory::Timeout,
                ErrorSeverity::Warning,
                true,
                "tool_timeout",
                format!("Tool timed out after {seconds}s"),
            ),
            ToolError::Cancelled { message } => Self::new(
                ErrorCategory::Tool,
                ErrorSeverity::Info,
                false,
                "tool_cancelled",
                message,
            ),
            ToolError::NotAvailable { message } => Self::new(
                ErrorCategory::State,
                ErrorSeverity::Error,
                false,
                "tool_not_available",
                message,
            ),
            ToolError::PermissionDenied { message } => Self::new(
                ErrorCategory::Authorization,
                ErrorSeverity::Error,
                false,
                "tool_permission_denied",
                message,
            ),
        }
    }
}

/// Stream‑level error discriminated by origin.
///
/// Each variant maps to an `ErrorCategory` so the UI can render
/// stream‑specific icons or formatting. Wired into engine.rs at the three
/// stream guard sites (chunk timeout, max-bytes overflow, max-duration).
#[derive(Debug, Clone)]
pub enum StreamError {
    /// Stream stalled — no chunk received within the idle timeout.
    Stall { timeout_secs: u64 },
    /// Stream exceeded content size limit.
    Overflow { limit_bytes: usize },
    /// Stream exceeded wall‑clock duration limit.
    DurationLimit { limit_secs: u64 },
}

impl StreamError {
    /// Convert directly into an `ErrorEnvelope` for emission on the engine
    /// event channel. Stalls are warning-severity and recoverable; size and
    /// duration limits are errors (the user must restart the turn).
    #[must_use]
    pub fn into_envelope(self) -> ErrorEnvelope {
        match self {
            Self::Stall { timeout_secs } => ErrorEnvelope::new(
                ErrorCategory::Timeout,
                ErrorSeverity::Warning,
                true,
                "stream_stall",
                format!("Stream stalled: no data received for {timeout_secs}s, closing stream"),
            ),
            Self::Overflow { limit_bytes } => ErrorEnvelope::new(
                ErrorCategory::Internal,
                ErrorSeverity::Error,
                true,
                "stream_overflow",
                format!("Stream exceeded maximum content size of {limit_bytes} bytes, closing"),
            ),
            Self::DurationLimit { limit_secs } => ErrorEnvelope::new(
                ErrorCategory::Timeout,
                ErrorSeverity::Error,
                true,
                "stream_duration_limit",
                format!("Stream exceeded maximum duration of {limit_secs}s, closing"),
            ),
        }
    }
}

impl fmt::Display for StreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stall { timeout_secs } => {
                write!(f, "Stream stalled after {timeout_secs}s idle")
            }
            Self::Overflow { limit_bytes } => {
                write!(f, "Stream exceeded {limit_bytes} bytes limit")
            }
            Self::DurationLimit { limit_secs } => {
                write!(f, "Stream exceeded {limit_secs}s duration limit")
            }
        }
    }
}

impl std::error::Error for StreamError {}

#[cfg(test)]
#[path = "tests.rs"]
mod quota_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn classify(msg: &str) -> ErrorCategory {
        classify_error_message(msg)
    }

    #[test]
    fn invalid_input_catches_context_overflow_phrasings() {
        // Provider phrasing varies: DeepSeek/OpenAI/Anthropic/etc each
        // surface context-overflow as a slightly different string.
        // The classifier needs all of them on the same branch.
        for msg in [
            "This model's maximum context length is 1000000 tokens",
            "Error: context_length_exceeded",
            "Your prompt is too long for the current model",
            "You requested 100000 tokens but the maximum is 65536",
            "request exceeds context window",
        ] {
            assert_eq!(
                classify(msg),
                ErrorCategory::InvalidInput,
                "expected InvalidInput for `{msg}`",
            );
        }
    }

    /// A prepaid gateway's exhausted balance (HTTP 402) is a quota condition:
    /// the request was well-formed and the key was valid.
    #[test]
    fn insufficient_credits_classifies_as_rate_limit_not_auth() {
        for msg in [
            "Responses API error (HTTP 402 Payment Required): {\"error\":\"Insufficient funds\",\"message\":\"Your account has insufficient credits. Please add credits to continue.\"}",
            "insufficient credits",
        ] {
            assert_eq!(
                classify(msg),
                ErrorCategory::RateLimit,
                "expected RateLimit for `{msg}`"
            );
        }
        // The documented 401 body still classifies as authentication.
        assert_eq!(
            classify(
                "Responses API error (HTTP 401 Unauthorized): {\"error\":\"Unauthorized\",\"message\":\"Invalid API key\"}"
            ),
            ErrorCategory::Authentication
        );
    }

    #[test]
    fn standalone_rate_limit_status_beats_authentication() {
        for msg in [
            "429",
            "HTTP 429: rejected",
            "upstream response (429)",
            "[429]: rejected",
            "HTTP 429: Invalid API key",
        ] {
            assert_eq!(classify(msg), ErrorCategory::RateLimit, "{msg}");
        }
    }

    #[test]
    fn numbers_in_urls_and_identifiers_are_not_rate_limit_statuses() {
        for location in [
            "http://127.0.0.1:42981/v1/responses",
            "http://127.0.0.1:14290/v1/responses",
            "http://127.0.0.1:429/v1/responses",
            "https://example.test/429",
            "https://example.test/?request_id=429",
            "/429",
            "request_429",
            "request-429",
            "14290",
        ] {
            let msg = format!(
                "Responses API error (HTTP 401 Unauthorized) at {location}: Invalid API key"
            );
            assert_eq!(classify(&msg), ErrorCategory::Authentication, "{msg}");
        }
        assert_eq!(
            classify("Network request failed for https://example.test/429"),
            ErrorCategory::Network
        );
        assert_eq!(classify("request_429 failed"), ErrorCategory::Internal);
    }

    #[test]
    fn timeout_catches_both_spellings() {
        assert_eq!(classify("connection timeout"), ErrorCategory::Timeout);
        assert_eq!(
            classify("request timed out after 30s"),
            ErrorCategory::Timeout
        );
    }

    #[test]
    fn network_catches_stream_body_decode_failures() {
        for msg in [
            "Warn Stream read error: error decoding response body",
            "Stream read error: error decoding response body",
            "chunk decode error",
            "provider body decode failed mid-stream",
        ] {
            assert_eq!(
                classify(msg),
                ErrorCategory::Network,
                "expected Network for `{msg}`",
            );
        }
    }

    #[test]
    fn authentication_beats_authorization_when_api_key_phrasing_is_used() {
        // "api key" landing on Authentication (not Authorization) keeps
        // the operator-facing message correct: the user needs to fix
        // their key, not their permissions.
        for msg in [
            "Invalid API key provided",
            "Authentication failed",
            "401 Unauthorized",
        ] {
            assert_eq!(
                classify(msg),
                ErrorCategory::Authentication,
                "expected Authentication for `{msg}`",
            );
        }
    }

    #[test]
    fn authorization_catches_forbidden_and_denied() {
        for msg in [
            "403 Forbidden",
            "Authorization failed: Arcee AI API returned Cloudflare Access Denied",
            "Permission denied for resource",
            "Tool 'edit_file' denied by user",
        ] {
            assert_eq!(
                classify(msg),
                ErrorCategory::Authorization,
                "expected Authorization for `{msg}`",
            );
        }
    }

    #[test]
    fn network_catches_dns_connection_5xx() {
        for msg in [
            "Network is unreachable",
            "Connection reset by peer",
            "DNS resolution failed for api.deepseek.com",
            "503 Service Unavailable",
            "Upstream returned 502 Bad Gateway",
            "Service temporarily unavailable",
        ] {
            assert_eq!(
                classify(msg),
                ErrorCategory::Network,
                "expected Network for `{msg}`",
            );
        }
        // Edge-case precedence: "504 Gateway Timeout" mentions both
        // a 504 status code AND the word "timeout". The classifier
        // picks Timeout, which is correct — the operator-actionable
        // category for a 504 is "wait and retry" (Timeout semantics)
        // rather than "DNS / connection broken" (Network semantics).
        assert_eq!(
            classify("504 Gateway Timeout"),
            ErrorCategory::Timeout,
            "504 with the literal word `timeout` resolves as Timeout, not Network"
        );
    }

    #[test]
    fn parse_catches_syntax_and_malformed_json() {
        for msg in [
            "Failed to parse response JSON",
            "Syntax error in tool arguments",
            "Malformed event from stream",
        ] {
            assert_eq!(
                classify(msg),
                ErrorCategory::Parse,
                "expected Parse for `{msg}`",
            );
        }
    }

    #[test]
    fn state_catches_not_found_and_unavailable() {
        for msg in [
            "Session not found",
            "Model is unavailable for this provider",
            "Endpoint not available in this region",
        ] {
            assert_eq!(
                classify(msg),
                ErrorCategory::State,
                "expected State for `{msg}`",
            );
        }
    }

    #[test]
    fn tool_is_a_low_priority_catchall_for_tool_keyword() {
        // The Tool branch is the last keyword check before falling
        // through to Internal. Anything mentioning "tool" that didn't
        // match an earlier category should land here.
        assert_eq!(
            classify("Tool returned non-zero exit status"),
            ErrorCategory::Tool,
        );
    }

    #[test]
    fn unknown_messages_fall_through_to_internal() {
        for msg in [
            "Something exploded",
            "panic at the disco",
            "u-200 something happened",
            "",
        ] {
            assert_eq!(
                classify(msg),
                ErrorCategory::Internal,
                "expected Internal for `{msg}`",
            );
        }
    }

    #[test]
    fn classifier_is_case_insensitive() {
        // The function lowercases internally — every category must
        // match regardless of input casing.
        assert_eq!(classify("RATE LIMIT EXCEEDED"), ErrorCategory::RateLimit);
        assert_eq!(classify("TimeOut"), ErrorCategory::Timeout);
        assert_eq!(classify("UNAUTHORIZED"), ErrorCategory::Authentication);
    }

    #[test]
    fn precedence_invalid_input_beats_tool() {
        // A "context length" tool error should classify as
        // InvalidInput, not Tool — InvalidInput is the more actionable
        // category (the user needs to shorten their prompt; "tool
        // failure" wouldn't tell them that).
        assert_eq!(
            classify("tool returned: maximum context length is 1000000"),
            ErrorCategory::InvalidInput,
        );
    }

    #[test]
    fn precedence_timeout_beats_network() {
        // A timeout that mentions a network call should still classify
        // as Timeout — the retry policy for timeouts is gentler than
        // for outright network failures.
        assert_eq!(
            classify("network call timed out after 30s"),
            ErrorCategory::Timeout,
        );
    }

    #[test]
    fn precedence_rate_limit_beats_authentication() {
        // 429 messages sometimes mention "api" or "auth" tokens, but
        // RateLimit's retry semantics (back off + retry) are what the
        // operator actually wants.
        assert_eq!(
            classify("Rate limit on your API quota exceeded"),
            ErrorCategory::RateLimit,
        );
    }

    #[test]
    fn classifier_handles_unicode_safely() {
        // Unicode shouldn't trip the lowercase step or the keyword
        // scan — Chinese/Japanese error messages from
        // OpenAI-compatible providers go through the same path.
        assert_eq!(
            classify("\u{8d85}\u{51fa}\u{6700}\u{5927}\u{4e0a}\u{4e0b}\u{6587} context length"),
            ErrorCategory::InvalidInput,
        );
        // Pure-Chinese messages with no keyword match land on Internal.
        assert_eq!(
            classify("\u{4e0d}\u{77e5}\u{9053}\u{600e}\u{4e48}\u{56de}\u{4e8b}"),
            ErrorCategory::Internal,
        );
    }

    #[test]
    fn error_envelope_display_includes_severity_code_message() {
        let env = ErrorEnvelope::new(
            ErrorCategory::Network,
            ErrorSeverity::Warning,
            true,
            "net_transient",
            "DNS resolution failed",
        );
        assert_eq!(
            format!("{env}"),
            "[warning] net_transient: DNS resolution failed"
        );
    }

    #[test]
    fn error_category_display_round_trips_via_snake_case() {
        // The snake_case labels are what crosses the wire / hits logs;
        // pin them so a future rename doesn't silently shift consumer
        // contracts.
        assert_eq!(format!("{}", ErrorCategory::Network), "network");
        assert_eq!(format!("{}", ErrorCategory::RateLimit), "rate_limit");
        assert_eq!(format!("{}", ErrorCategory::InvalidInput), "invalid_input");
        assert_eq!(format!("{}", ErrorSeverity::Critical), "critical");
    }
}

//! LLM Client Trait and Retry Logic
//!
//! This module provides a unified interface for LLM providers with robust retry logic,
//! exponential backoff, and proper error classification.
//!
//! # Architecture
//!
//! - `LlmClient` trait: Async interface for LLM providers (DeepSeek, `OpenAI`, etc.)
//! - `RetryConfig`: Configurable retry behavior with exponential backoff and jitter
//! - `LlmError`: Classified errors with retryability information

//! - `with_retry`: Generic retry wrapper for any async operation
//!
//! # Example
//!
//! ```ignore
//! use crate::llm_client::{LlmClient, RetryConfig, with_retry};
//!
//! let config = RetryConfig::default();
//! let result = with_retry(&config, || async {
//!     client.create_message(request).await
//! }, None).await;
//! ```

use crate::config::RetryPolicy;
use anyhow::Result;
use codewhale_models::{MessageRequest, MessageResponse, StreamEvent};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[cfg(test)]
pub mod mock;

// === LlmClient Trait ===

/// Type alias for boxed stream of SSE events
pub type StreamEventBox =
    Pin<Box<dyn futures_util::Stream<Item = Result<StreamEvent>> + Send + 'static>>;

/// Unified interface for LLM providers.
///
/// This trait abstracts over different LLM APIs (DeepSeek, `OpenAI`, etc.)
/// allowing the agent to work with any provider that implements this interface.
///
/// # Implementation Notes
///
/// - All methods are async and require `Send + Sync` for thread safety
/// - The `create_message_stream` method returns a pinned boxed stream for SSE
/// - Implementations should handle their own authentication and base URL configuration
#[allow(async_fn_in_trait, dead_code)] // Trait methods are part of the LLM provider interface
pub trait LlmClient: Send + Sync {
    /// Returns the provider name (e.g., "openai", "deepseek")
    fn provider_name(&self) -> &'static str;

    /// Returns the model identifier being used
    fn model(&self) -> &str;

    /// Creates a non-streaming message completion
    fn create_message(
        &self,
        request: MessageRequest,
    ) -> impl Future<Output = Result<MessageResponse>> + Send;

    /// Dispatch a fresh request. Clients with a local response cache must
    /// override this; authorization decisions cannot reuse earlier answers.
    fn create_message_uncached(
        &self,
        request: MessageRequest,
    ) -> impl Future<Output = Result<MessageResponse>> + Send {
        self.create_message(request)
    }

    /// Creates a streaming message completion
    ///
    /// Returns a stream of SSE events that should be consumed until completion.
    fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> impl Future<Output = Result<StreamEventBox>> + Send;

    /// Optional health check to verify API connectivity
    fn health_check(&self) -> impl Future<Output = Result<bool>> + Send {
        async { Ok(true) }
    }

    /// The concrete base URL requests go to, when the implementation knows it.
    ///
    /// Background cost accrual uses this for billing provenance only: it is
    /// reduced to a non-secret surface classification and a SHA-256 fingerprint
    /// before being recorded, and the URL itself is never persisted or logged
    /// (#4318). The default is `None` so an implementation that cannot report a
    /// stable endpoint yields "unknown endpoint" — which fails closed — rather
    /// than being assumed to be the provider's public API.
    fn billing_base_url(&self) -> Option<&str> {
        None
    }

    /// Non-secret limits frozen with the resolved route, when available.
    fn route_limits(&self) -> Option<codewhale_config::route::RouteLimits> {
        None
    }

    /// Output cap for a request sent through this exact client route.
    fn effective_max_output_tokens(&self, requested_model: &str) -> u32 {
        let route = self.effective_route_envelope(requested_model, chrono::Utc::now());
        crate::route_budget::effective_max_output_tokens_for_route(
            route.provider,
            &route.model,
            self.route_limits(),
        )
    }

    /// Freeze the non-secret effective route immediately before a request is
    /// dispatched. Implementations with richer configured identity/billing
    /// facts should override this fail-closed default.
    fn effective_route_envelope(
        &self,
        requested_model: &str,
        dispatched_at: chrono::DateTime<chrono::Utc>,
    ) -> crate::cost_status::EffectiveRouteEnvelope {
        let provider = crate::config::ProviderKind::parse(self.provider_name())
            .unwrap_or(crate::config::ProviderKind::Custom);
        crate::cost_status::EffectiveRouteEnvelope::capture_observed(
            provider,
            self.provider_name(),
            requested_model,
            self.billing_base_url(),
            dispatched_at,
        )
    }
}

// === Authentication diagnostics ===

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthenticationErrorContext {
    pub provider: Option<String>,
    pub base_url_authority: Option<String>,
    pub model: Option<String>,
    pub key_source: Option<String>,
    pub key_fingerprint: Option<String>,
    pub key_kind: Option<String>,
    /// The one command or action that replaces the rejected credential.
    pub fix: Option<String>,
}

impl AuthenticationErrorContext {
    #[must_use]
    pub fn from_parts(
        provider: Option<&str>,
        base_url: Option<&str>,
        model: Option<&str>,
        key_source: Option<&str>,
        api_key: Option<&str>,
    ) -> Self {
        let api_key = api_key.and_then(non_empty_trimmed);
        Self {
            provider: provider.and_then(non_empty_trimmed).map(str::to_string),
            base_url_authority: base_url.and_then(base_url_authority),
            model: model.and_then(non_empty_trimmed).map(str::to_string),
            key_source: key_source.and_then(non_empty_trimmed).map(str::to_string),
            key_fingerprint: api_key.map(redacted_key_fingerprint),
            key_kind: api_key.map(classify_api_key_prefix).map(str::to_string),
            fix: None,
        }
    }

    #[must_use]
    pub fn with_fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = Some(fix.into()).filter(|fix: &String| !fix.trim().is_empty());
        self
    }

    fn is_empty(&self) -> bool {
        self.provider.is_none()
            && self.base_url_authority.is_none()
            && self.model.is_none()
            && self.key_source.is_none()
            && self.key_fingerprint.is_none()
            && self.key_kind.is_none()
            && self.fix.is_none()
    }

    fn detail_segments(&self) -> Vec<String> {
        let mut segments = Vec::new();
        if let Some(provider) = self.provider.as_deref() {
            segments.push(format!("provider: {provider}"));
        }
        if let Some(authority) = self.base_url_authority.as_deref() {
            segments.push(format!("base URL authority: {authority}"));
        }
        if let Some(model) = self.model.as_deref() {
            segments.push(format!("model: {model}"));
        }
        if let Some(source) = self.key_source.as_deref() {
            segments.push(format!("key source: {source}"));
        }
        if let Some(fingerprint) = self.key_fingerprint.as_deref() {
            segments.push(format!("key fingerprint: {fingerprint}"));
        }
        if let Some(kind) = self.key_kind.as_deref() {
            segments.push(format!("key type: {kind}"));
        }
        if let Some(fix) = self.fix.as_deref() {
            segments.push(format!("fix: {fix}"));
        }
        segments
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticationErrorDetail {
    message: String,
    context: Option<AuthenticationErrorContext>,
}

impl AuthenticationErrorDetail {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            context: None,
        }
    }

    #[must_use]
    pub fn with_context(
        message: impl Into<String>,
        context: Option<AuthenticationErrorContext>,
    ) -> Self {
        let context = context.filter(|context| !context.is_empty());
        Self {
            message: message.into(),
            context,
        }
    }

    #[must_use]
    pub fn to_user_message(&self) -> String {
        let Some(context) = self.context.as_ref() else {
            return self.message.clone();
        };
        let segments = context.detail_segments();
        if segments.is_empty() {
            self.message.clone()
        } else {
            format!("{} ({})", self.message, segments.join(", "))
        }
    }
}

impl From<String> for AuthenticationErrorDetail {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

impl From<&str> for AuthenticationErrorDetail {
    fn from(message: &str) -> Self {
        Self::new(message)
    }
}

#[must_use]
pub fn classify_api_key_prefix(api_key: &str) -> &'static str {
    if api_key.starts_with("tp-") {
        "Xiaomi MiMo Token Plan key"
    } else {
        "API key"
    }
}

fn non_empty_trimmed(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() { None } else { Some(value) }
}

pub(crate) fn base_url_authority(base_url: &str) -> Option<String> {
    let base_url = non_empty_trimmed(base_url)?;
    let without_scheme = base_url
        .split_once("://")
        .map_or(base_url, |(_, rest)| rest);
    let authority = without_scheme.split('/').next().unwrap_or(without_scheme);
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, authority)| authority);
    non_empty_trimmed(authority).map(str::to_string)
}

fn redacted_key_fingerprint(api_key: &str) -> String {
    let api_key = api_key.trim();
    let len = api_key.chars().count();
    match public_key_prefix(api_key) {
        Some(prefix) => format!("{prefix}... (len={len})"),
        None => format!("unprefixed (len={len})"),
    }
}

fn public_key_prefix(api_key: &str) -> Option<&str> {
    ["tp-", "sk-", "hf_", "hf-", "ak-", "rk-"]
        .into_iter()
        .find(|prefix| api_key.starts_with(prefix))
}

#[cfg(test)]
fn redact_api_key_from_message(message: &str, api_key: Option<&str>) -> String {
    let Some(api_key) = api_key.and_then(non_empty_trimmed) else {
        return message.to_string();
    };
    message.replace(api_key, "[redacted API key]")
}

// === LlmError - Classified Error Types ===

/// Evidence captured when a provider response explicitly identifies plan quota
/// exhaustion. The private field prevents callers outside this parser module
/// from manufacturing the durable classification from arbitrary text.
#[derive(Debug)]
pub struct QuotaExhaustionError {
    message: String,
}

impl QuotaExhaustionError {
    fn from_http_message(message: String) -> Self {
        Self { message }
    }

    pub(crate) fn into_message(self) -> String {
        self.message
    }

    /// Append route guidance (which account hit the limit, how to switch)
    /// to already-classified evidence. Cannot manufacture the class.
    #[must_use]
    pub(crate) fn with_guidance(mut self, guidance: &str) -> Self {
        self.message = format!("{}\n{guidance}", self.message);
        self
    }
}

/// Classified LLM errors with retryability information.
///
/// This enum categorizes API errors to enable smart retry decisions.
/// Some errors (rate limits, transient server errors) are retryable,
/// while others (auth failures, invalid requests) should fail immediately.
#[derive(Debug)]
pub enum LlmError {
    /// Rate limit exceeded (HTTP 429)
    /// Contains optional Retry-After duration from server
    RateLimited {
        message: String,
        retry_after: Option<Duration>,
    },

    /// The provider explicitly reported that the account's plan quota is exhausted.
    ///
    /// Unlike an ordinary 429 rate limit, retrying the same request after a short
    /// backoff cannot resolve this condition. This variant is constructed only at
    /// a provider HTTP or structured stream-event boundary from explicit quota evidence.
    QuotaExhausted(QuotaExhaustionError),

    /// Server error (HTTP 5xx)
    ServerError { status: u16, message: String },

    /// Network connectivity error
    NetworkError(String),

    /// Request timed out
    Timeout(Duration),

    /// Authentication failed (HTTP 401, selected HTTP 403)
    AuthenticationError(AuthenticationErrorDetail),

    /// Authorization or provider-side blocking failed (HTTP 403)
    AuthorizationError(String),

    /// Invalid request parameters (HTTP 400)
    InvalidRequest { status: u16, message: String },

    /// Model-specific error (model not found, etc.)
    ModelError(String),

    /// Content policy violation (safety filters)
    ContentPolicyError(String),

    /// Failed to parse API response
    ParseError(String),

    /// Context length exceeded
    ContextLengthError(String),

    /// Catch-all for other errors
    Other(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::RateLimited { message, .. } => write!(f, "Rate limit exceeded: {message}"),
            LlmError::QuotaExhausted(error) => {
                write!(f, "Provider plan quota exhausted: {}", error.message)
            }
            LlmError::ServerError { status, message } => {
                write!(f, "Server error ({status}): {message}")
            }
            LlmError::NetworkError(msg) => write!(f, "Network error: {msg}"),
            LlmError::Timeout(d) => write!(f, "Request timed out after {d:?}"),
            LlmError::AuthenticationError(auth) => {
                write!(f, "Authentication failed: {}", auth.to_user_message())
            }
            LlmError::AuthorizationError(msg) => write!(f, "Authorization failed: {msg}"),
            LlmError::InvalidRequest { status, message } => {
                write!(f, "Invalid request ({status}): {message}")
            }
            LlmError::ModelError(msg) => write!(f, "Model error: {msg}"),
            LlmError::ContentPolicyError(msg) => write!(f, "Content policy violation: {msg}"),
            LlmError::ParseError(msg) => write!(f, "Response parsing error: {msg}"),
            LlmError::ContextLengthError(msg) => write!(f, "Context length exceeded: {msg}"),
            LlmError::Other(msg) => write!(f, "LLM error: {msg}"),
        }
    }
}

impl std::error::Error for LlmError {}

impl LlmError {
    /// Determines if this error is potentially transient and worth retrying.
    ///
    /// Retryable errors:
    /// - Rate limits (with backoff)
    /// - Server errors (5xx)
    /// - Network errors (connection issues)
    /// - Timeouts
    ///
    /// Non-retryable errors:
    /// - Provider plan quota exhaustion
    /// - Authentication failures
    /// - Invalid requests
    /// - Content policy violations
    /// - Context length errors
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            LlmError::RateLimited { .. }
                | LlmError::ServerError { .. }
                | LlmError::NetworkError(_)
                | LlmError::Timeout(_)
        )
    }

    /// Returns the server-suggested retry delay if available.
    ///
    /// This is typically present for rate limit errors when the server
    /// provides a Retry-After header.
    pub fn suggested_retry_delay(&self) -> Option<Duration> {
        match self {
            LlmError::RateLimited { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Constructs an `LlmError` from HTTP status code and response body.
    ///
    /// Performs heuristic classification based on:
    /// - Status code (429 = rate limit, 401/403 = auth, 499/5xx = transient upstream error)
    /// - Response body keywords (`context_length`, `content_policy`, safety, etc.)
    pub fn from_http_response(status: u16, body: &str) -> Self {
        if let Some(error) = explicit_quota_code(body)
            .or_else(|| explicit_quota_code_marker(body))
            .as_deref()
            .and_then(Self::from_subscription_sharing_error_code)
        {
            return error;
        }
        // xAI refuses an exhausted account with a 403, not a 402/429.
        if matches!(status, 400 | 402 | 403 | 429) && has_explicit_quota_evidence(body) {
            return LlmError::QuotaExhausted(QuotaExhaustionError::from_http_message(
                body.to_string(),
            ));
        }

        match status {
            429 => LlmError::RateLimited {
                message: body.to_string(),
                retry_after: None,
            },
            401 => Self::authentication_error(body),
            403 => {
                if looks_like_authentication_failure(body) {
                    Self::authentication_error(body)
                } else {
                    LlmError::AuthorizationError(body.to_string())
                }
            }
            400 => {
                // Classify 400 errors by examining the response body
                let body_lower = body.to_lowercase();
                // An "unsupported parameter" 400 names the offending field
                // (often `max_output_tokens` or another *token* field), which
                // the generic keyword rules below would misread as a context
                // window overflow. Parameter shape errors are invalid
                // requests, not prompt-size errors, so they get their own
                // branch ahead of the heuristic.
                if body_lower.contains("unsupported parameter")
                    || body_lower.contains("invalid_request_error")
                        && body_lower.contains("parameter")
                {
                    LlmError::InvalidRequest {
                        status,
                        message: body.to_string(),
                    }
                } else if is_context_length_message(&body_lower) {
                    LlmError::ContextLengthError(body.to_string())
                } else if body_lower.contains("content_policy")
                    || body_lower.contains("safety")
                    || body_lower.contains("harmful")
                    || body_lower.contains("inappropriate")
                {
                    LlmError::ContentPolicyError(body.to_string())
                } else if body_lower.contains("model") && body_lower.contains("not found") {
                    LlmError::ModelError(body.to_string())
                } else {
                    LlmError::InvalidRequest {
                        status,
                        message: body.to_string(),
                    }
                }
            }
            404 => {
                if body.to_lowercase().contains("model") {
                    LlmError::ModelError(body.to_string())
                } else {
                    LlmError::InvalidRequest {
                        status,
                        message: body.to_string(),
                    }
                }
            }
            // Several OpenAI-compatible gateways use nginx's non-standard
            // 499 for an upstream request that was cancelled before response
            // streaming began. At this boundary no response body stream has
            // been exposed, so it is eligible for the same bounded retry
            // policy as a 5xx gateway failure.
            499..=599 => LlmError::ServerError {
                status,
                message: body.to_string(),
            },
            _ => LlmError::Other(format!("HTTP {status}: {body}")),
        }
    }

    /// Official structured ChatGPT plan errors are terminal account states.
    /// Plain text containing quota words is never enough to mint this type.
    #[must_use]
    pub(crate) fn from_subscription_sharing_error_code(code: &str) -> Option<Self> {
        let message = match code {
            "subscription_sharing_usage_limit_exceeded" => {
                "ChatGPT plan usage limit reached. Check ChatGPT Settings > Usage for your remaining allowance and reset time."
            }
            "subscription_sharing_usage_unavailable" => {
                "ChatGPT plan usage is unavailable. Check ChatGPT Settings > Usage and reconnect if needed."
            }
            _ => return None,
        };
        Some(Self::QuotaExhausted(
            QuotaExhaustionError::from_http_message(message.to_string()),
        ))
    }

    #[must_use]
    pub fn authentication_error(message: impl Into<String>) -> Self {
        LlmError::AuthenticationError(AuthenticationErrorDetail::new(message))
    }

    #[must_use]
    pub fn authentication_error_with_context(
        message: impl Into<String>,
        context: Option<AuthenticationErrorContext>,
    ) -> Self {
        LlmError::AuthenticationError(AuthenticationErrorDetail::with_context(message, context))
    }

    /// Constructs an `LlmError` from HTTP response data plus request context
    /// that is safe to display when authentication fails.
    #[must_use]
    #[cfg(test)]
    pub fn from_http_response_with_request_context(
        status: u16,
        body: &str,
        provider: Option<&str>,
        base_url: Option<&str>,
        model: Option<&str>,
        key_source: Option<&str>,
        api_key: Option<&str>,
    ) -> Self {
        let body = redact_api_key_from_message(body, api_key);
        let context =
            AuthenticationErrorContext::from_parts(provider, base_url, model, key_source, api_key);
        Self::from_http_response_with_auth_context(status, &body, Some(context))
    }

    /// Constructs an `LlmError` from HTTP status code and response body, with
    /// optional structured details for authentication failures.
    ///
    /// The `body` passed here must already be safe for user display. Prefer
    /// [`Self::from_http_response_with_request_context`] when the raw API key is
    /// available so the response body can be redacted before rendering.
    #[must_use]
    pub fn from_http_response_with_auth_context(
        status: u16,
        body: &str,
        auth_context: Option<AuthenticationErrorContext>,
    ) -> Self {
        let classified = Self::from_http_response(status, body);
        if matches!(classified, Self::QuotaExhausted(_)) {
            return classified;
        }
        match status {
            401 => Self::authentication_error_with_context(body, auth_context),
            403 => {
                if looks_like_authentication_failure(body) {
                    Self::authentication_error_with_context(body, auth_context)
                } else {
                    LlmError::AuthorizationError(body.to_string())
                }
            }
            _ => classified,
        }
    }

    /// Constructs an `LlmError` from HTTP status code, body, and optional Retry-After header.
    pub fn from_http_response_with_retry_after(
        status: u16,
        body: &str,
        retry_after: Option<Duration>,
    ) -> Self {
        let mut error = Self::from_http_response(status, body);
        if let LlmError::RateLimited {
            retry_after: ref mut ra,
            ..
        } = error
        {
            *ra = retry_after;
        }
        error
    }

    /// Constructs an `LlmError` from a reqwest error.
    pub fn from_reqwest(err: &reqwest::Error) -> Self {
        if err.is_timeout() {
            LlmError::Timeout(Duration::from_secs(0))
        } else if err.is_connect() {
            LlmError::NetworkError(format!("Connection failed: {err}"))
        } else if err.is_request() {
            LlmError::NetworkError(format!("Request failed: {err}"))
        } else {
            LlmError::Other(err.to_string())
        }
    }
}

/// Format provider HTTP error bodies before they are surfaced in the TUI.
///
/// Providers sometimes return whole HTML error pages for gateway/WAF blocks.
/// Passing those pages through raw floods the transcript and can also make a
/// provider-side 403 look like a broken API key. Keep the useful details and
/// cap everything else.
#[must_use]
pub(crate) fn sanitize_http_error_body(
    provider_label: Option<&str>,
    status: u16,
    body: &str,
) -> String {
    let json_message = extract_json_error_message(body);
    let message = json_message.as_deref().unwrap_or(body);
    // Gate on Google's actual rejection, not the selected provider or model:
    // compatible gateways may manage signatures themselves (#6048). This
    // shared boundary covers both streaming and non-streaming HTTP failures.
    const SIGNATURE_HINT: &str = "Gemini rejected tool-call replay because a thought signature is missing. \
        Use the built-in `google` provider with its default endpoint, or a gateway that preserves \
        Google thought signatures, then start a new session before using tools. \
        Changing reasoning settings will not restore missing signatures.";
    if status == 400
        && !is_probably_html(message)
        && explicit_quota_code(body).is_none()
        && !message.contains(SIGNATURE_HINT)
    {
        let lower = collapse_whitespace(message).to_ascii_lowercase();
        if lower.contains("missing a thought_signature")
            || lower.contains("missing thought_signature")
            || lower.contains("thought_signature is missing")
        {
            let detail = truncate_for_error(&collapse_whitespace(message), 900);
            return format!("{SIGNATURE_HINT} Provider error: {detail}");
        }
    }

    if let Some(message) = json_message {
        let message = truncate_for_error(&collapse_whitespace(&message), 2_000);
        if let Some(code) = explicit_quota_code(body) {
            return format!("{message} (provider error code: {code})");
        }
        return message;
    }

    if is_probably_html(body) {
        let text = html_to_text(body);
        let lower = text.to_ascii_lowercase();
        let provider = provider_label.unwrap_or("Provider");

        // Cloudflare's "Access Denied" interstitial strips the literal word
        // "cloudflare" once tags are removed (it only survives in `<meta>`
        // attributes and the `<style>`/`<script>` blocks we discard). Arcee's
        // 403 page is exactly this shape, so also key off the WAF's stock copy
        // ("security alert", "contact support") and a Cloudflare error/ray ID.
        let error_id = extract_cloudflare_error_id(&text);
        let is_cloudflare = lower.contains("cloudflare");
        let looks_like_access_denied = lower.contains("access denied")
            && (is_cloudflare
                || lower.contains("security alert")
                || lower.contains("contact support")
                || lower.contains("contact us")
                || error_id.is_some());
        if looks_like_access_denied {
            let label = if is_cloudflare {
                "Cloudflare Access Denied"
            } else {
                "Access Denied"
            };
            let mut message = format!(
                "{provider} API returned {label} (HTTP {status}). \
                 The request was blocked before it reached the model; retry with a \
                 smaller request or fewer tools, or contact provider support"
            );
            if let Some(id) = error_id {
                message.push_str(&format!(" with ID {id}"));
            }
            message.push('.');
            return message;
        }

        let text = truncate_for_error(&collapse_whitespace(&text), 900);
        return format!("{provider} API returned an HTML error page (HTTP {status}): {text}");
    }

    truncate_for_error(&collapse_whitespace(body), 2_000)
}

fn looks_like_authentication_failure(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("authentication")
        || lower.contains("unauthorized")
        || lower.contains("api key")
        || lower.contains("invalid key")
        || lower.contains("invalid token")
        || lower.contains("bearer token")
        || lower.contains("missing token")
}

/// A provider error is a context overflow only when it says so. Bare
/// "token", "too long" or "maximum" also appear in ordinary invalid-request
/// errors (`max_tokens must be ...`, a field value too long), which compaction
/// or a bigger window cannot fix. This is the one phrase list: the typed 400
/// classification here and the engine's string classifier both read it.
/// `lower` must already be lowercase.
pub(crate) fn is_context_length_message(lower: &str) -> bool {
    [
        "context_length",
        "context length",
        "context window",
        "context limit",
        "maximum context",
        "prompt is too long",
        "input is too long",
        "maximum prompt length",
        "exceeded model token limit",
        "tokens exceed",
        "exceeds the maximum number of tokens",
        // llama.cpp: "the request exceeds the available context size".
        "available context size",
    ]
    .iter()
    .any(|phrase| lower.contains(phrase))
}

/// Quota exhaustion is a durable account state, not a generic rate-limit
/// synonym. Accept only explicit provider evidence at the HTTP/parser boundary;
/// callers holding a stringified error must never promote it to this type.
fn has_explicit_quota_evidence(body: &str) -> bool {
    explicit_quota_code(body).is_some()
        || explicit_quota_code_marker(body).is_some()
        || has_explicit_quota_phrase(body)
}

fn explicit_quota_code(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    [
        "/error/code",
        "/error/type",
        "/error/error_code",
        "/code",
        "/type",
        "/error_code",
    ]
    .into_iter()
    .filter_map(|pointer| value.pointer(pointer).and_then(Value::as_str))
    .find(|code| is_explicit_quota_code(code))
    .map(ToOwned::to_owned)
}

fn is_explicit_quota_code(code: &str) -> bool {
    let normalized: String = code
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect();
    matches!(
        normalized.as_str(),
        "insufficientquota"
            | "quotaexceeded"
            | "quotaexhausted"
            | "billinghardlimitreached"
            | "billinglimitreached"
            | "creditbalanceexhausted"
            // ChatGPT/Codex subscription window (HTTP 429, `error.type`),
            // as openai/codex `api_bridge.rs` maps it. It resets on the
            // plan's schedule, not after a short backoff.
            | "usagelimitreached"
            // Same backend, same branch: the signed-in plan does not include
            // Codex. Retrying cannot help; switching accounts can.
            | "usagenotincluded"
            | "subscriptionsharingusagelimitexceeded"
            | "subscriptionsharingusageunavailable"
    )
}

fn explicit_quota_code_marker(body: &str) -> Option<String> {
    let lower = body.to_ascii_lowercase();
    let (_, suffix) = lower.split_once("provider error code:")?;
    let code = suffix
        .trim_start()
        .split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')))
        .next()
        .unwrap_or_default();
    is_explicit_quota_code(code).then(|| code.to_string())
}

fn has_explicit_quota_phrase(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let current_quota_exhausted = lower.contains("exceeded your current quota")
        || lower.contains("current quota has been exceeded");
    let plan_and_billing_guidance = lower.contains("plan") && lower.contains("billing");
    let durable_scope_exhausted = [
        "billing quota exceeded",
        "billing quota exhausted",
        "billing quota is exhausted",
        "billing quota has been exceeded",
        "billing quota has been exhausted",
        "account quota exceeded",
        "account quota exhausted",
        "account quota is exhausted",
        "account quota has been exceeded",
        "account quota has been exhausted",
        "plan quota exceeded",
        "plan quota exhausted",
        "plan quota is exhausted",
        "plan quota has been exceeded",
        "plan quota has been exhausted",
    ]
    .into_iter()
    .any(|phrase| lower.contains(phrase));

    // xAI: "You have run out of credits or need a Grok subscription."
    let credits_exhausted = [
        "run out of credits",
        "out of credits",
        "insufficient credits",
        "used all available credits",
        "monthly spending limit",
    ]
    .into_iter()
    .any(|phrase| lower.contains(phrase));

    credits_exhausted
        || lower.contains("billing hard limit has been reached")
        || lower.contains("credit balance exhausted")
        || lower.contains("credit balance is exhausted")
        || durable_scope_exhausted
        || (current_quota_exhausted && plan_and_billing_guidance)
}

fn extract_json_error_message(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    // Flat gateway bodies (`{"error":"Bad Request","message":"Invalid model
    // name: 'x'"}` — Concentrate, among others) keep the class in `error` and
    // the detail in `message`. Surfacing only the class hid the one line the
    // person needed, so carry both when both are present and differ.
    if let (Some(class), Some(detail)) = (
        value.pointer("/error").and_then(Value::as_str),
        value.pointer("/message").and_then(Value::as_str),
    ) && !class.trim().is_empty()
        && !detail.trim().is_empty()
        && !class.trim().eq_ignore_ascii_case(detail.trim())
    {
        return Some(format!("{}: {}", class.trim(), detail.trim()));
    }
    for pointer in [
        "/error/message",
        "/error",
        "/message",
        "/detail",
        "/error_description",
    ] {
        let Some(value) = value.pointer(pointer) else {
            continue;
        };
        if let Some(message) = value.as_str() {
            if !message.trim().is_empty() {
                return Some(message.to_string());
            }
        } else if value.is_object() || value.is_array() {
            return Some(value.to_string());
        }
    }
    None
}

fn is_probably_html(body: &str) -> bool {
    let prefix = body
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    prefix.contains("<!doctype html") || prefix.contains("<html") || prefix.contains("<head")
}

fn html_to_text(html: &str) -> String {
    let without_scripts = strip_html_block(html, "script");
    let without_styles = strip_html_block(&without_scripts, "style");
    let mut text = String::with_capacity(without_styles.len().min(4096));
    let mut in_tag = false;
    for ch in without_styles.chars() {
        match ch {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' => {
                in_tag = false;
                text.push(' ');
            }
            _ if !in_tag => text.push(ch),
            _ => {}
        }
    }
    decode_basic_html_entities(&collapse_whitespace(&text))
}

fn strip_html_block(input: &str, tag: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0usize;
    let lower = input.to_ascii_lowercase();
    let start_marker = format!("<{tag}");
    let end_marker = format!("</{tag}>");

    while let Some(relative_start) = lower[cursor..].find(&start_marker) {
        let start = cursor + relative_start;
        out.push_str(&input[cursor..start]);
        let after_start = start + start_marker.len();
        let Some(relative_end) = lower[after_start..].find(&end_marker) else {
            cursor = input.len();
            break;
        };
        cursor = after_start + relative_end + end_marker.len();
        out.push(' ');
    }
    out.push_str(&input[cursor..]);
    out
}

fn decode_basic_html_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_for_error(input: &str, max_chars: usize) -> String {
    let mut out = String::with_capacity(input.len().min(max_chars + 32));
    for (count, ch) in input.chars().enumerate() {
        if count >= max_chars {
            out.push_str("...");
            return out;
        }
        out.push(ch);
    }
    out
}

fn extract_cloudflare_error_id(text: &str) -> Option<String> {
    let mut last = None;
    for token in text.split(|ch: char| !ch.is_ascii_hexdigit()) {
        if (16..=64).contains(&token.len()) && token.bytes().any(|b| b.is_ascii_alphabetic()) {
            last = Some(token.to_string());
        }
    }
    last
}

impl From<reqwest::Error> for LlmError {
    fn from(err: reqwest::Error) -> Self {
        LlmError::from_reqwest(&err)
    }
}

impl From<serde_json::Error> for LlmError {
    fn from(err: serde_json::Error) -> Self {
        LlmError::ParseError(err.to_string())
    }
}

// === RetryConfig - Exponential Backoff Configuration ===

/// Configuration for retry behavior with exponential backoff.
///
/// This struct controls how retries are performed:
/// - Number of retry attempts
/// - Delay calculation (exponential backoff with optional jitter)
/// - Which HTTP status codes are retryable
/// - Timeout handling
///
/// # Default Values
///
/// - `enabled`: true
/// - `max_retries`: 3
/// - `initial_delay`: 1.0 seconds
/// - `max_delay`: 60.0 seconds
/// - `exponential_base`: 2.0
/// - `jitter`: true (adds randomness to prevent thundering herd)
/// - `jitter_factor`: 0.1 (10% variation)
/// - `retryable_status_codes`: [429, 499, 500, 502, 503, 504]
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Whether retry logic is enabled
    pub enabled: bool,

    /// Maximum number of retry attempts (0 = no retries, 3 = up to 4 total attempts)
    pub max_retries: u32,

    /// Initial delay before first retry (seconds)
    pub initial_delay: f64,

    /// Maximum delay between retries (seconds)
    pub max_delay: f64,

    /// Base for exponential backoff (delay = initial * base^attempt)
    pub exponential_base: f64,

    /// Whether to add random jitter to delays
    pub jitter: bool,

    /// Jitter factor (0.1 = +/- 10% variation)
    pub jitter_factor: f64,

    /// Whether to respect server's Retry-After header
    pub respect_retry_after: bool,

    /// HTTP status codes that should trigger a retry
    pub retryable_status_codes: Vec<u16>,

    /// Timeout for individual requests (seconds, 0 = no timeout)
    pub request_timeout: f64,

    /// Total timeout for all retry attempts (seconds, 0 = no total timeout)
    pub total_timeout: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_retries: 3,
            initial_delay: 1.0,
            max_delay: 60.0,
            exponential_base: 2.0,
            jitter: true,
            jitter_factor: 0.1,
            respect_retry_after: true,
            retryable_status_codes: vec![429, 499, 500, 502, 503, 504],
            request_timeout: 120.0,
            total_timeout: 0.0, // No total timeout by default
        }
    }
}

impl RetryConfig {
    /// Creates a new `RetryConfig` with default values
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a config with retry disabled
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            ..Default::default()
        }
    }

    /// Builder method to set max retries
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    /// Builder method to set initial delay
    pub fn with_initial_delay(mut self, delay: f64) -> Self {
        self.initial_delay = delay;
        self
    }

    /// Builder method to set max delay
    pub fn with_max_delay(mut self, delay: f64) -> Self {
        self.max_delay = delay;
        self
    }

    /// Builder method to enable/disable jitter
    pub fn with_jitter(mut self, enabled: bool) -> Self {
        self.jitter = enabled;
        self
    }

    /// Calculates the delay for a given retry attempt.
    ///
    /// Uses exponential backoff: delay = `initial_delay` * `exponential_base^attempt`
    /// The result is capped at `max_delay` and optionally has jitter applied.
    ///
    /// # Arguments
    ///
    /// * `attempt` - Zero-based attempt number (0 = first retry)
    ///
    /// # Returns
    ///
    /// Duration to wait before the next retry attempt
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let exponent = i32::try_from(attempt).unwrap_or(i32::MAX);
        let base_delay = self.initial_delay * self.exponential_base.powi(exponent);
        let capped_delay = base_delay.min(self.max_delay);

        let final_delay = if self.jitter {
            // Add random jitter to prevent thundering herd problem
            let jitter_range = capped_delay * self.jitter_factor;
            // Use UUID v4 entropy for jitter randomness.
            let bytes = *Uuid::new_v4().as_bytes();
            let sample = u16::from_le_bytes([bytes[0], bytes[1]]);
            let random_factor = f64::from(sample) / f64::from(u16::MAX); // 0.0 to 1.0
            let jitter = jitter_range * (2.0 * random_factor - 1.0); // -range to +range

            (capped_delay + jitter).max(0.0)
        } else {
            capped_delay
        };

        Duration::from_secs_f64(final_delay)
    }

    /// Checks if a given HTTP status code should trigger a retry
    pub fn is_retryable_status(&self, status: u16) -> bool {
        self.retryable_status_codes.contains(&status)
    }
}

/// Converts from the existing `RetryPolicy` in config
impl From<RetryPolicy> for RetryConfig {
    fn from(policy: RetryPolicy) -> Self {
        Self {
            enabled: policy.enabled,
            max_retries: policy.max_retries,
            initial_delay: policy.initial_delay,
            max_delay: policy.max_delay,
            exponential_base: policy.exponential_base,
            jitter: policy.jitter,
            jitter_factor: policy.jitter_factor,
            respect_retry_after: policy.respect_retry_after,
            ..Default::default()
        }
    }
}

/// Converts back to `RetryPolicy` for compatibility
impl From<RetryConfig> for RetryPolicy {
    fn from(config: RetryConfig) -> Self {
        Self {
            enabled: config.enabled,
            max_retries: config.max_retries,
            initial_delay: config.initial_delay,
            max_delay: config.max_delay,
            exponential_base: config.exponential_base,
            jitter: config.jitter,
            jitter_factor: config.jitter_factor,
            respect_retry_after: config.respect_retry_after,
        }
    }
}

// === Retry Error and Result Types ===

/// Error returned when all retry attempts have been exhausted.
#[derive(Debug)]
pub struct RetryError {
    /// The last error encountered
    pub last_error: LlmError,

    /// Total number of attempts made
    pub attempts: u32,

    /// Total time spent across all attempts
    pub total_time: Duration,
}

impl std::fmt::Display for RetryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Retry exhausted after {} attempts ({:?}): {}",
            self.attempts, self.total_time, self.last_error
        )
    }
}

impl std::error::Error for RetryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.last_error)
    }
}

/// Result type for retry operations
pub type RetryResult<T> = Result<T, RetryError>;

/// Callback type for retry notifications
///
/// Called before each retry with:
/// - The error that triggered the retry
/// - The attempt number (0-based)
/// - The delay before the next attempt
pub type RetryCallback = Box<dyn Fn(&LlmError, u32, Duration) + Send + Sync>;

/// An observation of the existing request, never another retry driver. Its
/// lexical scope captures the producing Engine queue, not an ambient session.
pub(crate) type RetryStatusEmitter =
    std::sync::Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct RequestRetryObservation {
    pub(crate) retries: std::sync::Arc<std::sync::atomic::AtomicU32>,
    pub(crate) emit: RetryStatusEmitter,
}

tokio::task_local! {
    static REQUEST_RETRY_OBSERVATION: Option<RequestRetryObservation>;
}

pub(crate) async fn observe_request_retries<F: Future>(
    observation: Option<RequestRetryObservation>,
    future: F,
) -> F::Output {
    REQUEST_RETRY_OBSERVATION.scope(observation, future).await
}

async fn observe_transport_status(message: String) {
    let observation = REQUEST_RETRY_OBSERVATION
        .try_with(Clone::clone)
        .ok()
        .flatten();
    if let Some(observation) = observation {
        (observation.emit)(message).await;
    }
}

fn observe_transport_attempt() {
    let _ = REQUEST_RETRY_OBSERVATION.try_with(|observation| {
        if let Some(observation) = observation {
            let mut count = observation
                .retries
                .load(std::sync::atomic::Ordering::Relaxed);
            loop {
                match observation.retries.compare_exchange_weak(
                    count,
                    count.saturating_add(1),
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(current) => count = current,
                }
            }
        }
    });
}

/// A public receipt must never include the provider-controlled error payload.
/// Raw errors remain intact in the original result and diagnostic/log paths.
pub(crate) fn retry_reason_summary(error: &LlmError) -> String {
    match error {
        LlmError::RateLimited { .. } => "rate limited".into(),
        LlmError::ServerError { status, .. } => format!("upstream {status}"),
        LlmError::NetworkError(_) => "network error".into(),
        LlmError::Timeout(_) => "timeout".into(),
        _ => "non-retryable provider failure".into(),
    }
}

async fn observe_transport_stopped(retries: u32, error: &LlmError, exhausted: bool) {
    if retries > 0 {
        let disposition = if exhausted {
            "Retry exhaustion"
        } else {
            "Retry stopped"
        };
        observe_transport_status(format!(
            "{disposition}: transport request stopped after {retries} retries; {}",
            retry_reason_summary(error),
        ))
        .await;
    }
}

// === with_retry - Generic Retry Wrapper ===

/// Executes an async operation with configurable retry logic.
///
/// This function wraps any async operation that returns `Result<T, LlmError>`
/// and automatically retries on transient failures using exponential backoff.
///
/// # Arguments
///
/// * `config` - Retry configuration (delays, max attempts, etc.)
/// * `operation` - Async closure to execute (will be called multiple times on retry)
/// * `callback` - Optional callback for retry notifications (logging, metrics, etc.)
///
/// # Returns
///
/// * `Ok(T)` - The successful result from the operation
/// * `Err(RetryError)` - All retries exhausted or non-retryable error encountered
///
/// # Known limitation: ambiguous failures are replayed
///
/// A timeout or connection loss after the request was written is retried
/// like a connect failure, so a provider that already accepted the first
/// attempt may bill a second completion. Every caller sends model inference
/// (messages, FIM, translation, speech, provider web search): a replay costs
/// compute but has no external side effect, and the provider APIs used here
/// expose no idempotency key for these requests that could dedupe it. An
/// operation with an external side effect must not be retried through this
/// helper without an idempotency key the server honors.
///
/// # Example
///
/// ```ignore
/// let result = with_retry(
///     &config,
///     || async { client.send_request(&req).await },
///     Some(Box::new(|err, attempt, delay| {
///         eprintln!("Retry {} after {:?}: {}", attempt, delay, err);
///     })),
/// ).await;
/// ```
// Keep the structured error inline: this is a public compatibility surface and
// boxing it in a patch release would force every caller to change ownership
// handling for `last_error`.
#[allow(clippy::result_large_err)]
pub async fn with_retry<F, Fut, T>(
    config: &RetryConfig,
    mut operation: F,
    callback: Option<RetryCallback>,
) -> RetryResult<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, LlmError>>,
{
    // If retries are disabled, just run once
    if !config.enabled {
        return operation().await.map_err(|e| RetryError {
            last_error: e,
            attempts: 1,
            total_time: Duration::ZERO,
        });
    }

    let start_time = Instant::now();
    let total_timeout = if config.total_timeout > 0.0 {
        Some(Duration::from_secs_f64(config.total_timeout))
    } else {
        None
    };

    let mut last_error: Option<LlmError> = None;

    // Attempt 0 is the first try, then up to max_retries additional attempts
    for attempt in 0..=config.max_retries {
        // Check total timeout
        if let Some(timeout) = total_timeout
            && start_time.elapsed() >= timeout
        {
            let error = last_error.unwrap_or(LlmError::Timeout(timeout));
            observe_transport_stopped(attempt.saturating_sub(1), &error, true).await;
            return Err(RetryError {
                last_error: error,
                attempts: attempt,
                total_time: start_time.elapsed(),
            });
        }

        if attempt > 0 {
            observe_transport_attempt();
        }
        match operation().await {
            Ok(result) => {
                if attempt > 0 {
                    observe_transport_status(format!(
                        "Retry recovery: transport request recovered after {attempt} retries"
                    ))
                    .await;
                }
                return Ok(result);
            }
            Err(err) => {
                // Non-retryable errors fail immediately
                if !err.is_retryable() {
                    observe_transport_stopped(attempt, &err, false).await;
                    return Err(RetryError {
                        last_error: err,
                        attempts: attempt + 1,
                        total_time: start_time.elapsed(),
                    });
                }

                // Last attempt - no more retries
                if attempt >= config.max_retries {
                    observe_transport_stopped(attempt, &err, true).await;
                    return Err(RetryError {
                        last_error: err,
                        attempts: attempt + 1,
                        total_time: start_time.elapsed(),
                    });
                }

                // Calculate delay
                // Use server's Retry-After if available and configured
                let base_delay = config.delay_for_attempt(attempt);
                let delay = if config.respect_retry_after {
                    err.suggested_retry_delay().unwrap_or(base_delay)
                } else {
                    base_delay
                };

                // Notify callback if provided
                if let Some(ref cb) = callback {
                    cb(&err, attempt, delay);
                }

                observe_transport_status(format!(
                    "Retry attempt: transport {}/{}; {}; waiting {:.2}s",
                    attempt + 1,
                    config.max_retries,
                    retry_reason_summary(&err),
                    delay.as_secs_f64(),
                ))
                .await;
                last_error = Some(err);

                // Wait before retrying
                tokio::time::sleep(delay).await;
            }
        }
    }

    // Should not reach here, but handle gracefully
    Err(RetryError {
        last_error: last_error.unwrap_or(LlmError::Other("Unknown retry error".to_string())),
        attempts: config.max_retries + 1,
        total_time: start_time.elapsed(),
    })
}

// === Utility Functions ===

/// The longest a `Retry-After` value is ever believed. A server (or a proxy
/// in front of it) can send an arbitrarily large delay; without a ceiling a
/// single `Retry-After: 86400` would wedge the turn for a day. One hour is
/// well past any legitimate rate-limit window.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(3600);

/// Parses the Retry-After header value into a Duration.
///
/// Supports both:
/// - Seconds as integer: "120" -> 120 seconds
/// - HTTP-date format: "Wed, 21 Oct 2015 07:28:00 GMT" (not implemented, returns None)
///
/// The value is server-controlled, so this never panics and never returns an
/// unbounded delay: negative / NaN / infinite / absurd floats are rejected
/// (`Duration::from_secs_f64` panics on a negative — a remote-triggerable
/// crash before this guard), and any result is clamped to [`RETRY_AFTER_MAX`].
pub fn parse_retry_after(value: &str) -> Option<Duration> {
    // Try parsing as seconds
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds).min(RETRY_AFTER_MAX));
    }

    // Try parsing as float seconds. Only a finite, non-negative value is a
    // meaningful delay; everything else (`-5`, `nan`, `inf`) is "no usable
    // hint". Clamp to the ceiling BEFORE `from_secs_f64` so an out-of-range
    // float can never reach its overflow-panic path, while keeping the
    // sub-second precision a legitimate `1.5` carries.
    if let Ok(seconds) = value.parse::<f64>()
        && seconds.is_finite()
        && seconds >= 0.0
    {
        let clamped = seconds.min(RETRY_AFTER_MAX.as_secs_f64());
        return Some(Duration::from_secs_f64(clamped));
    }

    // HTTP-date format not supported yet
    // Could use chrono or httpdate crate if needed
    None
}

/// Extracts Retry-After duration from response headers
pub fn extract_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_retry_after)
}

const PROVIDER_RETRY_HINT_MAX: Duration = Duration::from_secs(120);

pub fn retry_delay_from_error_body(body: &str) -> Option<Duration> {
    static HINTS: std::sync::OnceLock<[regex::Regex; 2]> = std::sync::OnceLock::new();
    let [structured, prose] = HINTS.get_or_init(|| {
        [
            regex::Regex::new(r#"retryDelay\\?"\s*:\s*\\?"\s*(\d+(?:\.\d+)?)\s*s"#).unwrap(),
            regex::Regex::new(r"(?i)retry in\s+(\d+(?:\.\d+)?)\s*s").unwrap(),
        ]
    });
    structured
        .captures(body)
        .or_else(|| prose.captures(body))
        .and_then(|captures| captures.get(1))
        .and_then(|value| value.as_str().parse::<f64>().ok())
        .filter(|seconds| {
            seconds.is_finite() && (0.0..=PROVIDER_RETRY_HINT_MAX.as_secs_f64()).contains(seconds)
        })
        .map(Duration::from_secs_f64)
}

#[cfg(test)]
#[path = "tests.rs"]
mod quota_tests;

// === Tests ===

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_chatgpt_usage_codes_are_terminal_across_http_boundaries() {
        for code in [
            "subscription_sharing_usage_limit_exceeded",
            "subscription_sharing_usage_unavailable",
        ] {
            let body = serde_json::json!({"error":{"code":code,"message":"opaque"}}).to_string();
            for status in [400, 403, 429] {
                for safe_body in [body.clone(), sanitize_http_error_body(None, status, &body)] {
                    let error =
                        LlmError::from_http_response_with_auth_context(status, &safe_body, None);
                    assert!(matches!(error, LlmError::QuotaExhausted(_)), "{error:?}");
                    assert!(!error.is_retryable());
                    assert!(error.to_string().contains("ChatGPT Settings > Usage"));
                    let envelope = crate::error_taxonomy::envelope_for_llm_error(
                        error.into(),
                        "allowance unavailable".into(),
                    );
                    assert!(!envelope.recoverable);
                    assert_eq!(envelope.code, "llm_quota_exhausted");
                }
            }
        }
        for body in [
            "usage limit reached",
            r#"{"error":{"code":"subscription_sharing_usage_limit_exceeded_later"}}"#,
        ] {
            assert!(matches!(
                LlmError::from_http_response(429, body),
                LlmError::RateLimited { .. }
            ));
            assert!(LlmError::from_http_response(429, body).is_retryable());
        }
    }

    fn assert_f64_eq(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < f64::EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    fn auth_user_message(error: LlmError) -> String {
        match error {
            LlmError::AuthenticationError(auth) => auth.to_user_message(),
            other => panic!("expected authentication error, got {other}"),
        }
    }

    #[test]
    fn test_retry_config_defaults() {
        let config = RetryConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_retries, 3);
        assert_f64_eq(config.initial_delay, 1.0);
        assert_f64_eq(config.max_delay, 60.0);
        assert_f64_eq(config.exponential_base, 2.0);
        assert!(config.jitter);
    }

    #[test]
    fn test_retry_config_disabled() {
        let config = RetryConfig::disabled();
        assert!(!config.enabled);
    }

    #[test]
    fn test_retry_config_builder() {
        let config = RetryConfig::new()
            .with_max_retries(5)
            .with_initial_delay(2.0)
            .with_max_delay(120.0)
            .with_jitter(false);

        assert_eq!(config.max_retries, 5);
        assert_f64_eq(config.initial_delay, 2.0);
        assert_f64_eq(config.max_delay, 120.0);
        assert!(!config.jitter);
    }

    #[test]
    fn test_delay_for_attempt_exponential() {
        let config = RetryConfig::new().with_jitter(false);

        // delay = initial * base^attempt
        // 1.0 * 2^0 = 1.0
        let d0 = config.delay_for_attempt(0);
        assert_eq!(d0, Duration::from_secs_f64(1.0));

        // 1.0 * 2^1 = 2.0
        let d1 = config.delay_for_attempt(1);
        assert_eq!(d1, Duration::from_secs_f64(2.0));

        // 1.0 * 2^2 = 4.0
        let d2 = config.delay_for_attempt(2);
        assert_eq!(d2, Duration::from_secs_f64(4.0));

        // 1.0 * 2^3 = 8.0
        let d3 = config.delay_for_attempt(3);
        assert_eq!(d3, Duration::from_secs_f64(8.0));
    }

    #[test]
    fn test_delay_for_attempt_capped() {
        let config = RetryConfig::new().with_jitter(false).with_max_delay(5.0);

        // 1.0 * 2^3 = 8.0, but capped at 5.0
        let d3 = config.delay_for_attempt(3);
        assert_eq!(d3, Duration::from_secs_f64(5.0));
    }

    #[test]
    fn test_delay_for_attempt_with_jitter() {
        let config = RetryConfig::new().with_jitter(true);

        // With jitter, delays should vary slightly
        let d1 = config.delay_for_attempt(1);
        let d2 = config.delay_for_attempt(1);

        // Both should be close to 2.0 seconds (within 10% jitter)
        let base = 2.0;
        let range = base * 0.1;
        assert!(d1.as_secs_f64() >= base - range);
        assert!(d1.as_secs_f64() <= base + range);
        assert!(d2.as_secs_f64() >= base - range);
        assert!(d2.as_secs_f64() <= base + range);
    }

    #[test]
    fn test_is_retryable_status() {
        let config = RetryConfig::default();

        assert!(config.is_retryable_status(429)); // Rate limit
        assert!(config.is_retryable_status(499)); // Upstream request cancelled
        assert!(config.is_retryable_status(500)); // Internal server error
        assert!(config.is_retryable_status(502)); // Bad gateway
        assert!(config.is_retryable_status(503)); // Service unavailable
        assert!(config.is_retryable_status(504)); // Gateway timeout

        assert!(!config.is_retryable_status(400)); // Bad request
        assert!(!config.is_retryable_status(401)); // Unauthorized
        assert!(!config.is_retryable_status(403)); // Forbidden
        assert!(!config.is_retryable_status(404)); // Not found
    }

    #[test]
    fn auth_error_with_context_includes_provider_authority_model_and_key_source() {
        let err = LlmError::from_http_response_with_request_context(
            401,
            "Invalid API Key",
            Some("Xiaomi MiMo"),
            Some("https://token-plan-sgp.xiaomimimo.com/v1"),
            Some("mimo-v2.5"),
            Some("env"),
            Some("tp-secret-token-plan-value"),
        );
        let message = auth_user_message(err);

        assert!(message.contains("Invalid API Key"));
        assert!(message.contains("provider: Xiaomi MiMo"));
        assert!(message.contains("base URL authority: token-plan-sgp.xiaomimimo.com"));
        assert!(message.contains("model: mimo-v2.5"));
        assert!(message.contains("key source: env"));
        assert!(message.contains("key fingerprint: tp-... (len=26)"));
    }

    #[test]
    fn auth_error_redacts_full_api_key_from_body_and_context() {
        let api_key = "tp-secret-token-plan-value";
        let err = LlmError::from_http_response_with_request_context(
            401,
            &format!("Invalid API Key: {api_key}"),
            Some("Xiaomi MiMo"),
            Some("https://token-plan-sgp.xiaomimimo.com/v1"),
            Some("mimo-v2.5"),
            Some("config-file"),
            Some(api_key),
        );
        let message = auth_user_message(err);

        assert!(!message.contains(api_key));
        assert!(!message.contains("secret-token-plan-value"));
        assert!(message.contains("[redacted API key]"));
        assert!(message.contains("key fingerprint: tp-... (len=26)"));
    }

    #[test]
    fn auth_error_classifies_xiaomi_token_plan_key_prefix() {
        let token_plan = AuthenticationErrorContext::from_parts(
            None,
            None,
            None,
            Some("session"),
            Some("tp-secret-token-plan-value"),
        );
        let generic = AuthenticationErrorContext::from_parts(
            None,
            None,
            None,
            Some("session"),
            Some("sk-other"),
        );
        let unprefixed = AuthenticationErrorContext::from_parts(
            None,
            None,
            None,
            Some("session"),
            Some("plainsecretvalue"),
        );

        assert_eq!(
            token_plan.key_kind.as_deref(),
            Some("Xiaomi MiMo Token Plan key")
        );
        assert_eq!(generic.key_kind.as_deref(), Some("API key"));
        assert_eq!(unprefixed.key_kind.as_deref(), Some("API key"));
        assert_eq!(
            unprefixed.key_fingerprint.as_deref(),
            Some("unprefixed (len=16)")
        );
    }

    #[test]
    fn authorization_403_is_not_reclassified_by_auth_context() {
        let err = LlmError::from_http_response_with_request_context(
            403,
            "forbidden",
            Some("Arcee AI"),
            Some("https://api.arcee.ai/v1"),
            Some("auto"),
            Some("env"),
            Some("sk-arcee-secret"),
        );

        assert!(matches!(err, LlmError::AuthorizationError(_)));
    }

    #[test]
    fn auth_error_without_context_preserves_bare_message() {
        let err = LlmError::from_http_response_with_auth_context(
            401,
            "Invalid API Key",
            Some(AuthenticationErrorContext::default()),
        );

        assert_eq!(auth_user_message(err), "Invalid API Key");
    }

    /// A flat `{"error","message"}` body keeps its detail: the class alone
    /// ("Bad Request") told the person nothing about the rejected model.
    #[test]
    fn flat_error_and_message_body_surfaces_both_halves() {
        let body = r#"{"error":"Bad Request","message":"Invalid model name: 'invalid-model-xyz'"}"#;
        assert_eq!(
            sanitize_http_error_body(Some("Concentrate"), 400, body),
            "Bad Request: Invalid model name: 'invalid-model-xyz'"
        );
        // Identical halves are not doubled, and the nested OpenAI shape is
        // untouched.
        assert_eq!(
            sanitize_http_error_body(
                Some("Concentrate"),
                401,
                r#"{"error":"Unauthorized","message":"Unauthorized"}"#
            ),
            "Unauthorized"
        );
        assert_eq!(
            sanitize_http_error_body(
                Some("fixture"),
                400,
                r#"{"error":{"message":"nested detail"}}"#
            ),
            "nested detail"
        );
    }

    #[test]
    fn cloudflare_html_error_is_summarized_without_raw_markup() {
        let body = r#"<!DOCTYPE html><html><head><title>Access Denied</title><style>
            .hidden { display: none; }
            </style></head><body>
            <h1>Access Denied</h1>
            <p>The action you just performed triggered a security alert.</p>
            <script>window.noisy = true;</script>
            <span>2600:1700:467:d410:f137:b94f:1dd0:d1e4</span>
            <span>a059a2873f3fdf82</span>
            <div>Cloudflare Error Pages</div>
            </body></html>"#;

        let message = sanitize_http_error_body(Some("Arcee AI"), 403, body);

        assert!(message.contains("Arcee AI API returned Cloudflare Access Denied"));
        assert!(message.contains("ID a059a2873f3fdf82"));
        assert!(!message.contains("<!DOCTYPE"));
        assert!(!message.contains("tailwindcss"));
        assert!(message.len() < 300);
    }

    #[test]
    fn cloudflare_access_denied_403_is_authorization_not_authentication() {
        let message = sanitize_http_error_body(
            Some("Arcee AI"),
            403,
            r#"<!doctype html><html><body><h1>Access Denied</h1><p>Cloudflare Error Pages</p></body></html>"#,
        );
        let err = LlmError::from_http_response(403, &message);

        assert!(matches!(err, LlmError::AuthorizationError(_)));
    }

    #[test]
    fn arcee_access_denied_without_literal_cloudflare_is_still_summarized() {
        // Mirrors api.arcee.ai's real 403 page: "Cloudflare" appears only in a
        // `<meta>` attribute and the `<style>` block, both stripped, so the
        // visible text never contains it. The summary must still fire from the
        // WAF's stock "security alert" / "Contact Support" copy + error ID.
        let body = r#"<!DOCTYPE html><html lang="en"><head>
            <meta name="description" content="Cloudflare Error Pages">
            <title>Access Denied</title>
            <style>:root{--accent:cloudflare}</style></head><body>
            <h1>Access Denied</h1>
            <p>The action you just performed triggered a security alert.</p>
            <p>Please contact us if this was a mistake.</p>
            <a>Contact Support</a>
            <span>2600:1700:467:d410:f137:b94f:1dd0:d1e4</span>
            <span>a059c0d4caf1f9cc</span>
            </body></html>"#;

        let message = sanitize_http_error_body(Some("Arcee AI"), 403, body);

        assert!(
            message.contains("Arcee AI API returned Access Denied"),
            "got: {message}"
        );
        assert!(message.contains("ID a059c0d4caf1f9cc"), "got: {message}");
        assert!(
            !message.to_ascii_lowercase().contains("cloudflare"),
            "stripped Arcee page has no literal Cloudflare: {message}"
        );
        assert!(!message.contains('<'), "no raw markup: {message}");
        assert!(message.len() < 300, "stays concise: {message}");

        // A WAF block is authorization, not a bad API key.
        let err = LlmError::from_http_response(403, &message);
        assert!(matches!(err, LlmError::AuthorizationError(_)));
    }

    #[test]
    fn test_llm_error_suggested_retry_delay() {
        let err = LlmError::RateLimited {
            message: "slow down".to_string(),
            retry_after: Some(Duration::from_secs(60)),
        };
        assert_eq!(err.suggested_retry_delay(), Some(Duration::from_secs(60)));

        let err = LlmError::ServerError {
            status: 500,
            message: "error".to_string(),
        };
        assert_eq!(err.suggested_retry_delay(), None);
    }

    #[test]
    fn test_parse_retry_after() {
        // Integer seconds
        assert_eq!(parse_retry_after("120"), Some(Duration::from_secs(120)));
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(0)));

        // Float seconds keep sub-second precision
        assert_eq!(parse_retry_after("1.5"), Some(Duration::from_secs_f64(1.5)));

        // Invalid
        assert_eq!(parse_retry_after("invalid"), None);
        assert_eq!(parse_retry_after(""), None);
    }

    /// A `Retry-After` value is server-controlled. Malformed floats used to
    /// reach `Duration::from_secs_f64`, which panics on a negative — a
    /// remote-triggerable crash in the request path (2026-08-04 review).
    #[test]
    fn parse_retry_after_never_panics_and_is_bounded_on_hostile_input() {
        // None of these may panic.
        assert_eq!(parse_retry_after("-5"), None, "negative is not a delay");
        assert_eq!(parse_retry_after("nan"), None);
        assert_eq!(parse_retry_after("inf"), None);
        assert_eq!(parse_retry_after("-inf"), None);
        // Absurdly large values clamp to the ceiling rather than overflowing
        // or wedging the turn for a day.
        assert_eq!(parse_retry_after("1e300"), Some(RETRY_AFTER_MAX));
        assert_eq!(parse_retry_after("86400"), Some(RETRY_AFTER_MAX));
        assert_eq!(
            parse_retry_after("999999999999"),
            Some(RETRY_AFTER_MAX),
            "integer path is clamped too"
        );
        // A normal value still passes through untouched.
        assert_eq!(parse_retry_after("30"), Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_retry_policy_conversion() {
        let policy = RetryPolicy {
            enabled: true,
            max_retries: 5,
            initial_delay: 2.0,
            max_delay: 30.0,
            exponential_base: 3.0,
            jitter: false,
            jitter_factor: 0.25,
            respect_retry_after: false,
        };

        let config: RetryConfig = policy.clone().into();
        assert_eq!(config.enabled, policy.enabled);
        assert_eq!(config.max_retries, policy.max_retries);
        assert_f64_eq(config.initial_delay, policy.initial_delay);
        assert_f64_eq(config.max_delay, policy.max_delay);
        assert_f64_eq(config.exponential_base, policy.exponential_base);

        // #6700: the jitter and Retry-After knobs survive the conversion
        // instead of silently resetting to `RetryConfig::default()`.
        assert!(!config.jitter);
        assert_f64_eq(config.jitter_factor, 0.25);
        assert!(!config.respect_retry_after);

        // Convert back
        let policy2: RetryPolicy = config.into();
        assert_eq!(policy2.enabled, policy.enabled);
        assert_eq!(policy2.max_retries, policy.max_retries);
        assert_eq!(policy2.jitter, policy.jitter);
        assert_f64_eq(policy2.jitter_factor, policy.jitter_factor);
        assert_eq!(policy2.respect_retry_after, policy.respect_retry_after);
    }

    #[tokio::test]
    async fn test_with_retry_success_first_attempt() {
        let config = RetryConfig::default();
        let mut call_count = 0;

        let result = with_retry(
            &config,
            || {
                call_count += 1;
                async { Ok::<_, LlmError>(42) }
            },
            None,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count, 1);
    }

    #[tokio::test]
    async fn test_with_retry_disabled() {
        let config = RetryConfig::disabled();
        let mut call_count = 0;

        let result: RetryResult<i32> = with_retry(
            &config,
            || {
                call_count += 1;
                async {
                    Err(LlmError::ServerError {
                        status: 500,
                        message: "error".to_string(),
                    })
                }
            },
            None,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(call_count, 1); // No retries when disabled
    }

    #[tokio::test]
    async fn test_with_retry_eventual_success() {
        let config = RetryConfig::new()
            .with_max_retries(3)
            .with_initial_delay(0.01); // Fast for testing

        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = call_count.clone();

        let result = with_retry(
            &config,
            || {
                let count = cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async move {
                    if count < 2 {
                        Err(LlmError::ServerError {
                            status: 500,
                            message: "temporary error".to_string(),
                        })
                    } else {
                        Ok::<_, LlmError>(42)
                    }
                }
            },
            None,
        )
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 3); // 2 failures + 1 success
    }

    #[tokio::test]
    async fn test_with_retry_exhausted() {
        let config = RetryConfig::new()
            .with_max_retries(2)
            .with_initial_delay(0.01);

        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = call_count.clone();

        let result: RetryResult<i32> = with_retry(
            &config,
            || {
                cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    Err(LlmError::ServerError {
                        status: 500,
                        message: "persistent error".to_string(),
                    })
                }
            },
            None,
        )
        .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.attempts, 3); // 1 initial + 2 retries
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_with_retry_callback() {
        let config = RetryConfig::new()
            .with_max_retries(2)
            .with_initial_delay(0.01);

        let callback_count = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let cc = callback_count.clone();

        let _: RetryResult<i32> = with_retry(
            &config,
            || async {
                Err(LlmError::ServerError {
                    status: 500,
                    message: "error".to_string(),
                })
            },
            Some(Box::new(move |_err, _attempt, _delay| {
                cc.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            })),
        )
        .await;

        // Callback called once per retry (not for the final failure)
        assert_eq!(callback_count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn test_retry_error_display() {
        let err = RetryError {
            last_error: LlmError::ServerError {
                status: 500,
                message: "internal error".to_string(),
            },
            attempts: 4,
            total_time: Duration::from_secs(10),
        };

        let display = format!("{err}");
        assert!(display.contains("4 attempts"));
        assert!(display.contains("10"));
        assert!(display.contains("Server error"));
    }
}

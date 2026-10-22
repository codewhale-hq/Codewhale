//! HTTP client for the resolved provider route.
//!
//! Routes reach the provider through its OpenAI-compatible or native wire
//! surface; `/chat/completions` is the common primary endpoint.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose};
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{
    Mutex as AsyncMutex, OwnedRwLockReadGuard, OwnedRwLockWriteGuard, OwnedSemaphorePermit, RwLock,
    Semaphore,
};

use codewhale_config::catalog::{
    CatalogOffering, CatalogRefreshError, CatalogSource, ProviderCatalogDelta,
    base_url_fingerprint, now_unix,
};
#[cfg(test)]
use codewhale_config::catalog::{CatalogSnapshot, CatalogStatus, ProviderCatalogCache};
use codewhale_config::provider::WireFormat;
use codewhale_config::route::{
    LogicalModelRef, ReadyRouteCandidate, RouteLimits, RouteRequest, RouteResolver,
};
use codewhale_config::{auth_mode_disables_api_key, is_upstream_auth_header};

use crate::config::{
    Config, ProviderIdentity, ProviderKind, RetryPolicy, validate_route,
    wire_model_for_provider_route,
};
use crate::llm_client::{
    LlmClient, LlmError, RetryConfig as LlmRetryConfig, extract_retry_after,
    retry_delay_from_error_body, sanitize_http_error_body, with_retry,
};
#[cfg(test)]
#[path = "client/catalog_tests.rs"]
mod catalog_tests;

use crate::logging;
use codewhale_models::Role;
use codewhale_models::{
    ContentBlock, Message, MessageRequest, MessageResponse, SystemPrompt, Usage,
};

/// Every provider request that can feed the interactive TUI's attached CWC run
/// takes a shared permit at this lowest common dispatch seam. Runtime Chat holds
/// the exclusive permit from native admission through durable terminal
/// acknowledgement. This covers ordinary turns, auto-route classification,
/// advisor calls, detached subagents, compaction, purge, and streaming without
/// serializing independent RuntimeThreadManager stores.
#[cfg(not(test))]
static RUNTIME_CHAT_INFERENCE_GATE: OnceLock<Arc<RwLock<()>>> = OnceLock::new();

/// Unit tests run many independent Tokio runtimes in one process. Keying the
/// otherwise-identical gate by runtime keeps unrelated libtest cases from
/// manufacturing contention while preserving exact read/write behavior among
/// tasks on the same current-thread or multi-thread runtime.
#[cfg(test)]
static RUNTIME_CHAT_INFERENCE_TEST_GATES: OnceLock<
    StdMutex<HashMap<RuntimeChatInferenceTestScope, std::sync::Weak<RwLock<()>>>>,
> = OnceLock::new();

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum RuntimeChatInferenceTestScope {
    Runtime(tokio::runtime::Id),
    Thread(std::thread::ThreadId),
}

#[cfg(not(test))]
fn runtime_chat_inference_gate() -> Arc<RwLock<()>> {
    Arc::clone(RUNTIME_CHAT_INFERENCE_GATE.get_or_init(|| Arc::new(RwLock::new(()))))
}

#[cfg(test)]
fn runtime_chat_inference_gate() -> Arc<RwLock<()>> {
    let scope = tokio::runtime::Handle::try_current()
        .map(|handle| RuntimeChatInferenceTestScope::Runtime(handle.id()))
        .unwrap_or_else(|_| RuntimeChatInferenceTestScope::Thread(std::thread::current().id()));
    let mut gates = RUNTIME_CHAT_INFERENCE_TEST_GATES
        .get_or_init(|| StdMutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(gate) = gates.get(&scope).and_then(std::sync::Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(RwLock::new(()));
    gates.insert(scope, Arc::downgrade(&gate));
    gate
}

/// Exclusive ownership retained by isolated Runtime Chat.
pub(crate) struct RuntimeChatInferenceOwnership {
    _gate: OwnedRwLockWriteGuard<()>,
}

/// Shared participation retained for the full provider-output lifecycle.
pub(crate) struct RemoteControlInferencePermit {
    _gate: OwnedRwLockReadGuard<()>,
}

#[cfg(test)]
pub(crate) async fn acquire_runtime_chat_inference_ownership() -> RuntimeChatInferenceOwnership {
    let gate = runtime_chat_inference_gate().write_owned().await;
    RuntimeChatInferenceOwnership { _gate: gate }
}

pub(crate) fn try_acquire_runtime_chat_inference_ownership() -> Option<RuntimeChatInferenceOwnership>
{
    let gate = runtime_chat_inference_gate().try_write_owned().ok()?;
    Some(RuntimeChatInferenceOwnership { _gate: gate })
}

/// Join the attached interactive CWC run as a provider-output participant.
/// Standalone background adapters that bypass `CodewhaleClient::create_message`
/// use this guard and retain it through response decoding.
pub(crate) async fn acquire_remote_control_inference_participant() -> RemoteControlInferencePermit {
    let gate = runtime_chat_inference_gate().read_owned().await;
    RemoteControlInferencePermit { _gate: gate }
}

pub(super) fn to_api_tool_name(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            out.push(ch);
        } else if ch == '-' {
            out.push_str("--");
        } else {
            out.push_str("-x");
            out.push_str(&format!("{:06X}", ch as u32));
            out.push('-');
        }
    }
    out
}

pub(super) fn from_api_tool_name(name: &str) -> String {
    let mut out = String::new();
    let mut iter = name.chars().peekable();
    while let Some(ch) = iter.next() {
        if ch != '-' {
            out.push(ch);
            continue;
        }
        if let Some('-') = iter.peek().copied() {
            iter.next();
            out.push('-');
            continue;
        }
        if iter.peek().copied() == Some('x') {
            iter.next();
            let mut hex = String::new();
            for _ in 0..6 {
                if let Some(h) = iter.next() {
                    hex.push(h);
                } else {
                    break;
                }
            }
            // Only decode if we got exactly 6 hex digits (matching encoder output).
            // Fewer digits means a truncated/malformed sequence — pass through as-is.
            if hex.len() == 6
                && let Ok(code) = u32::from_str_radix(&hex, 16)
                && let Some(decoded) = std::char::from_u32(code)
            {
                if let Some('-') = iter.peek().copied() {
                    iter.next();
                }
                out.push(decoded);
                continue;
            }
            out.push('-');
            out.push('x');
            out.push_str(&hex);
            continue;
        }
        out.push('-');
    }

    // Second pass: decode bare hex escapes (e.g. `x00002E`) that the model
    // may produce when it mangles the `-x00002E-` delimiter form.  Only
    // decode when the resulting character is one that `to_api_tool_name`
    // would have encoded (not alphanumeric, not `_`, not `-`).
    decode_bare_hex_escapes(&out)
}

/// Decode bare `x[0-9A-Fa-f]{6}` sequences (optionally followed by `-`)
/// that survive the standard delimiter-based pass.  This handles cases
/// where the model strips or replaces the leading `-` of `-x00002E-`.
pub(super) fn decode_bare_hex_escapes(input: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;

    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"x([0-9A-Fa-f]{6})-?").unwrap());

    let result = re.replace_all(input, |caps: &regex::Captures| {
        let hex = &caps[1];
        if let Ok(code) = u32::from_str_radix(hex, 16)
            && let Some(decoded) = std::char::from_u32(code)
        {
            // Only decode characters that to_api_tool_name would have encoded
            if !decoded.is_ascii_alphanumeric() && decoded != '_' && decoded != '-' {
                return decoded.to_string();
            }
        }
        // Not a character we'd encode — leave as-is
        caps[0].to_string()
    });
    result.into_owned()
}

// === Types ===

/// Model descriptor returned by the provider's `/v1/models` endpoint.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AvailableModel {
    pub id: String,
    pub owned_by: Option<String>,
    pub created: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Request payload for Xiaomi MiMo speech synthesis models.
///
/// MiMo-V2.5-TTS / MiMo-V2-TTS use the OpenAI-compatible
/// `/v1/chat/completions` endpoint: the optional style/voice instruction is
/// sent as a `user` message, while the text to synthesize is sent as an
/// `assistant` message.
#[derive(Debug, Clone)]
pub struct SpeechSynthesisRequest {
    pub model: String,
    pub text: String,
    pub instruction: Option<String>,
    pub audio_format: String,
    pub voice: Option<String>,
}

/// Decoded speech synthesis result.
#[derive(Debug, Clone)]
pub struct SpeechSynthesisResponse {
    pub model: String,
    pub audio_format: String,
    pub audio_bytes: Vec<u8>,
    pub transcript: Option<String>,
    pub voice: Option<String>,
}

/// One decoded provider response from the auxiliary translation path.
///
/// The immutable route and provider-reported usage travel beside the semantic
/// translation result so callers can account for a successful provider call
/// before rejecting an incomplete, empty, or otherwise unusable translation.
/// `usage == None` is distinct from a transport failure: the provider returned
/// a response, but omitted the receipt needed to price it exactly.
pub(crate) struct TranslationProviderResponse {
    pub(crate) translated: Result<String>,
    pub(crate) route: crate::cost_status::EffectiveRouteEnvelope,
    pub(crate) usage: Option<Usage>,
}

/// Universal client for the resolved provider route.
#[must_use]
pub struct CodewhaleClient {
    pub(super) http_client: reqwest::Client,
    // Catalogs and probes must never forward frozen custom auth headers to a
    // provider-supplied redirect destination. Inference keeps its own policy.
    models_http_client: reqwest::Client,
    /// HTTP/1.1-only twin of [`Self::http_client`], used for automatic
    /// stream-header fallback when H2 stalls. Same auth and headers.
    pub(super) http1_client: reqwest::Client,
    api_key: String,
    /// Where `api_key` came from (secret store slot, config file, env var
    /// name, CLI, OAuth, …), named in authentication errors (#6528).
    api_key_source: String,
    /// For a subscription sign-in route (ChatGPT, xAI OAuth): which account
    /// is signed in and how to switch, appended to plan-quota errors. Holds
    /// an account label only, never token material.
    subscription_limit_guidance: Option<String>,
    /// Reviewed runtime authority and OAuth descriptor travel with the frozen route.
    plugin_provider: Option<Box<crate::config::ProviderConfig>>,
    /// Read-only diagnostic probes must never migrate or refresh the grant.
    plugin_oauth_read_only: bool,
    claude_oauth_config: Option<Box<Config>>,
    /// Exact configured credential values removed from model-bound tool
    /// results. Structural redaction handles config/JSON assignments, while
    /// this list closes the gap for bare provider tokens with no recognizable
    /// prefix (for example token-plan and provider-specific keys).
    model_bound_secret_values: Arc<Vec<String>>,
    /// Exact values a catalog endpoint could echo back at us: the active API
    /// key and every user-configured HTTP header value, plus everything in
    /// `model_bound_secret_values`.
    ///
    /// Deliberately a second, wider list rather than a widening of that one:
    /// they answer different questions at different trust boundaries. That
    /// list is "what must never reach a *model*", which is why it covers only
    /// auth-shaped headers and values of at least
    /// `MIN_EXACT_SECRET_CHARS`. This one is "what must never come back out
    /// of an endpoint the user typed during setup" (#6173), where a
    /// three-character key is still a key and a custom header the user
    /// configured is still theirs.
    catalog_error_secret_values: Arc<Vec<String>>,
    /// Whether credential-shaped tool output is masked before it is sent to an
    /// upstream model. The safe default is `true`; it is `false` only after the
    /// user disabled `[redaction] model_bound` and confirmed the opt-out on the
    /// startup gate (see [`codewhale_config::redaction`]). Routing/classification
    /// summaries and durable goal-state text keep their own always-on redaction
    /// regardless of this flag.
    model_bound_masking: bool,
    pub(super) base_url: String,
    pub(super) api_provider: ProviderKind,
    /// Exact configured provider identity and billing mode frozen when this
    /// client is built. Child/tool calls only carry the client at dispatch, so
    /// these route facts must travel with it instead of being reconstructed
    /// from the mutable parent session at completion time.
    admitted_identity: ProviderIdentity,
    openrouter_vendor: Option<String>,
    billing_surface: Option<String>,
    billing_mode: crate::cost_status::RouteBillingMode,
    configured_models: Arc<Vec<codewhale_config::catalog::configured::ConfiguredModel>>,
    /// Non-secret limits frozen from the same resolved candidate as the
    /// endpoint and wire model. Auxiliary calls carry only this client, so
    /// they must not reconstruct output caps with `None` and discard a custom
    /// route's context window.
    route_limits: Option<RouteLimits>,
    route_facts: Option<Arc<codewhale_config::catalog::CatalogOffering>>,
    /// Verified ChatGPT subject captured with Codewhale's own selected grant.
    pub(super) codex_account_id: Option<String>,
    /// Opaque reasoning is bound to this verified grant identity, stable across
    /// token refresh and absent on custom API-key routes.
    pub(super) chatgpt_reasoning_api: Option<String>,
    wire_format: WireFormat,
    retry: RetryPolicy,
    /// Auxiliary inspection calls use the normal bounded retry schedule but
    /// never publish retry/rate-limit state into process-global UI cells.
    isolated_request_state: bool,
    /// Whether this concrete client can contribute provider output to the
    /// interactive TUI's attached CWC run. Isolated Runtime Chat executes under
    /// the host's exclusive permit; independent RuntimeThreadManager stores do
    /// not share that run and remain concurrent.
    remote_control_inference_participant: bool,
    default_model: String,
    connection_health: Arc<AsyncMutex<ConnectionHealth>>,
    rate_limiter: Arc<AsyncMutex<TokenBucket>>,
    request_concurrency: Option<ProviderConcurrencyLimiter>,
    path_suffix: Option<String>,
    /// Unit tests keep the semantic route exact while sending the actual
    /// production request through a local capture server. This field is
    /// compiled out of release builds.
    #[cfg(test)]
    test_chat_transport_base_url: Option<String>,
    /// Messages equivalent of `test_chat_transport_base_url`; keeps exact
    /// route shaping bound to the semantic endpoint while tests capture on a
    /// local server.
    #[cfg(test)]
    test_messages_transport_base_url: Option<String>,
    pub(super) reasoning_stream_style: Option<String>,
    pub(super) stream_idle_timeout: Duration,
    /// Bounded wait for SSE response headers, resolved once from
    /// `[stream].open_timeout_secs`, legacy `[tui]`, or the environment fallback.
    pub(super) stream_open_timeout: Duration,
    /// HTTP/1.1 pin resolved once from `Config::force_http1` (#6700); the
    /// single source every client builder and stream open reads.
    pub(super) force_http1: bool,
}

const CONNECTION_FAILURE_THRESHOLD: u32 = 2;
const RECOVERY_PROBE_COOLDOWN: Duration = Duration::from_secs(15);

const DEFAULT_CLIENT_RATE_LIMIT_RPS: f64 = 8.0;
const DEFAULT_CLIENT_RATE_LIMIT_BURST: f64 = 16.0;
const ALLOW_INSECURE_HTTP_ENV: &str = "CODEWHALE_ALLOW_INSECURE_HTTP";
/// Legacy alias for [`ALLOW_INSECURE_HTTP_ENV`].
const LEGACY_ALLOW_INSECURE_HTTP_ENV: &str = "DEEPSEEK_ALLOW_INSECURE_HTTP";

fn client_user_agent(_api_provider: ProviderKind) -> &'static str {
    concat!(
        "Mozilla/5.0 (compatible; codewhale/",
        env!("CARGO_PKG_VERSION"),
        "; +https://github.com/codewhale-hq/CodeWhale)"
    )
}

/// Upper bound on a single sleep inside the provider-wide rate-limit pause
/// loop in `send_with_retry`. The pause window lives in process-global state
/// (`retry_status`), so waiting requests re-poll it on this cadence instead
/// of committing to the full remaining window up front.
const RATE_LIMIT_PAUSE_RECHECK_INTERVAL: Duration = Duration::from_millis(250);

/// Total budget for one non-streaming request. Two layers use it: each
/// attempt carries it as a reqwest per-request total (connect through body
/// end, so a trickling body cannot extend forever), and the retry loop
/// through `send_with_retry` is wrapped in one outer envelope of the same
/// length (all attempts, backoff, and honored Retry-After included). The
/// shared client intentionally has no client-level total timeout, so without
/// these nothing bounds a non-streaming completion: a provider that accepts
/// the connection and then stalls — or a gateway answering 429 +
/// `Retry-After: 3600` forever — wedged the caller indefinitely.
///
/// Streaming paths never carry it: their opens go through
/// `send_stream_open_with_retry`, which sets no per-request total (a total
/// would ride on the returned body and hard-cut a live stream), so a stream
/// stays bounded by its open cap and per-chunk idle checks only.
pub(super) const NON_STREAMING_REQUEST_ENVELOPE: Duration = Duration::from_secs(1800);

#[cfg(test)]
static TEST_NON_STREAMING_ENVELOPE_MS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn non_streaming_request_envelope() -> Duration {
    #[cfg(test)]
    {
        let ms = TEST_NON_STREAMING_ENVELOPE_MS.load(std::sync::atomic::Ordering::SeqCst);
        if ms > 0 {
            return Duration::from_millis(ms);
        }
    }
    NON_STREAMING_REQUEST_ENVELOPE
}

pub(super) const SSE_BACKPRESSURE_HIGH_WATERMARK: usize = 1024 * 1024; // 1 MB
pub(super) const SSE_BACKPRESSURE_SLEEP_MS: u64 = 10;
pub(super) const SSE_MAX_LINES_PER_CHUNK: usize = 256;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionState {
    Healthy,
    Degraded,
    Recovering,
}

#[derive(Debug)]
struct ConnectionHealth {
    state: ConnectionState,
    consecutive_failures: u32,
    last_failure: Option<Instant>,
    last_success: Option<Instant>,
    last_probe: Option<Instant>,
}

impl Default for ConnectionHealth {
    fn default() -> Self {
        Self {
            state: ConnectionState::Healthy,
            consecutive_failures: 0,
            last_failure: None,
            last_success: None,
            last_probe: None,
        }
    }
}

#[derive(Debug)]
struct TokenBucket {
    enabled: bool,
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last_refill: Instant,
}

#[derive(Debug, Clone)]
struct ProviderConcurrencyLimiter {
    semaphore: Arc<Semaphore>,
    active: Arc<AtomicUsize>,
    limit: usize,
}

struct ProviderRequestPermit {
    _permit: OwnedSemaphorePermit,
    active: Arc<AtomicUsize>,
}

impl ProviderConcurrencyLimiter {
    fn new(limit: usize) -> Self {
        let limit = limit.max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(limit)),
            active: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }

    async fn acquire(&self) -> Option<ProviderRequestPermit> {
        let permit = Arc::clone(&self.semaphore).acquire_owned().await.ok()?;
        self.active.fetch_add(1, Ordering::AcqRel);
        Some(ProviderRequestPermit {
            _permit: permit,
            active: Arc::clone(&self.active),
        })
    }

    fn active(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    fn limit(&self) -> usize {
        self.limit
    }
}

impl Drop for ProviderRequestPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::AcqRel);
    }
}

impl TokenBucket {
    fn from_env() -> Self {
        let rps = std::env::var("CODEWHALE_RATE_LIMIT_RPS")
            .or_else(|_| std::env::var("DEEPSEEK_RATE_LIMIT_RPS"))
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(DEFAULT_CLIENT_RATE_LIMIT_RPS)
            .max(0.0);
        let burst = std::env::var("CODEWHALE_RATE_LIMIT_BURST")
            .or_else(|_| std::env::var("DEEPSEEK_RATE_LIMIT_BURST"))
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(DEFAULT_CLIENT_RATE_LIMIT_BURST)
            .max(1.0);
        let enabled = rps > 0.0;
        Self {
            enabled,
            capacity: burst,
            tokens: burst,
            refill_per_sec: rps,
            last_refill: Instant::now(),
        }
    }

    fn refill(&mut self, now: Instant) {
        if !self.enabled {
            return;
        }
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
    }

    /// Reserve `tokens` and report how long the caller must sleep first.
    ///
    /// The debt is *kept* (the balance is allowed to go negative) rather than
    /// floored at zero. Callers release the bucket lock before sleeping, so a
    /// floored balance would hand every queued waiter the same short delay and
    /// they would all wake — and fire — at the same instant, which is the
    /// burst the configured limit exists to prevent. Carrying the deficit
    /// spaces successive waiters one refill interval apart, and `refill`'s
    /// clamp to `capacity` still caps how much credit an idle bucket banks.
    fn delay_until_available(&mut self, tokens: f64) -> Option<Duration> {
        if !self.enabled {
            return None;
        }
        let now = Instant::now();
        self.refill(now);
        self.tokens -= tokens;
        if self.tokens >= 0.0 {
            return None;
        }
        if self.refill_per_sec <= 0.0 {
            return Some(Duration::from_secs(1));
        }
        Some(Duration::from_secs_f64(-self.tokens / self.refill_per_sec))
    }
}

fn apply_request_success(health: &mut ConnectionHealth, now: Instant) -> bool {
    let recovered = health.state != ConnectionState::Healthy;
    health.state = ConnectionState::Healthy;
    health.consecutive_failures = 0;
    health.last_success = Some(now);
    recovered
}

fn apply_request_failure(health: &mut ConnectionHealth, now: Instant) {
    health.consecutive_failures = health.consecutive_failures.saturating_add(1);
    health.last_failure = Some(now);
    if health.consecutive_failures >= CONNECTION_FAILURE_THRESHOLD {
        health.state = ConnectionState::Degraded;
    }
}

fn mark_recovery_probe_if_due(health: &mut ConnectionHealth, now: Instant) -> bool {
    if health.state == ConnectionState::Healthy {
        return false;
    }
    if health
        .last_probe
        .is_some_and(|last| now.duration_since(last) < RECOVERY_PROBE_COOLDOWN)
    {
        return false;
    }
    health.last_probe = Some(now);
    health.state = ConnectionState::Recovering;
    true
}

/// The one command that replaces a rejected key, by where it came from.
fn auth_fix_hint(route: &str, key_source: &str) -> String {
    if key_source == "Claude sign-in" {
        "codewhale auth claude".to_string()
    } else if key_source.starts_with("--api-key") {
        "pass a valid --api-key".to_string()
    } else if let Some(rest) = key_source.strip_prefix("env var ")
        && rest.contains("api_key_env")
    {
        let name = rest.split_whitespace().next().unwrap_or(rest);
        format!("set {name} to a valid key")
    } else if key_source.contains("OAuth") || key_source.contains("login") {
        format!("sign in again for {route}")
    } else {
        format!("codewhale auth set --provider {route}")
    }
}

fn buffer_pool() -> &'static StdMutex<Vec<Vec<u8>>> {
    static POOL: OnceLock<StdMutex<Vec<Vec<u8>>>> = OnceLock::new();
    POOL.get_or_init(|| StdMutex::new(Vec::new()))
}

fn acquire_stream_buffer() -> Vec<u8> {
    if let Ok(mut pool) = buffer_pool().lock() {
        pool.pop().unwrap_or_else(|| Vec::with_capacity(8192))
    } else {
        Vec::with_capacity(8192)
    }
}

fn release_stream_buffer(mut buf: Vec<u8>) {
    buf.clear();
    if buf.capacity() > 256 * 1024 {
        buf.shrink_to(256 * 1024);
    }
    if let Ok(mut pool) = buffer_pool().lock()
        && pool.len() < 8
    {
        pool.push(buf);
    }
}

impl Clone for CodewhaleClient {
    fn clone(&self) -> Self {
        Self {
            http_client: self.http_client.clone(),
            models_http_client: self.models_http_client.clone(),
            http1_client: self.http1_client.clone(),
            api_key: self.api_key.clone(),
            api_key_source: self.api_key_source.clone(),
            subscription_limit_guidance: self.subscription_limit_guidance.clone(),
            plugin_provider: self.plugin_provider.clone(),
            plugin_oauth_read_only: self.plugin_oauth_read_only,
            claude_oauth_config: self.claude_oauth_config.clone(),
            model_bound_secret_values: Arc::clone(&self.model_bound_secret_values),
            catalog_error_secret_values: Arc::clone(&self.catalog_error_secret_values),
            model_bound_masking: self.model_bound_masking,
            base_url: self.base_url.clone(),
            api_provider: self.api_provider,
            admitted_identity: self.admitted_identity.clone(),
            openrouter_vendor: self.openrouter_vendor.clone(),
            billing_surface: self.billing_surface.clone(),
            billing_mode: self.billing_mode,
            configured_models: Arc::clone(&self.configured_models),
            route_limits: self.route_limits,
            route_facts: self.route_facts.clone(),
            codex_account_id: self.codex_account_id.clone(),
            chatgpt_reasoning_api: self.chatgpt_reasoning_api.clone(),
            wire_format: self.wire_format,
            retry: self.retry.clone(),
            isolated_request_state: self.isolated_request_state,
            remote_control_inference_participant: self.remote_control_inference_participant,
            default_model: self.default_model.clone(),
            connection_health: self.connection_health.clone(),
            rate_limiter: self.rate_limiter.clone(),
            request_concurrency: self.request_concurrency.clone(),
            path_suffix: self.path_suffix.clone(),
            #[cfg(test)]
            test_chat_transport_base_url: self.test_chat_transport_base_url.clone(),
            #[cfg(test)]
            test_messages_transport_base_url: self.test_messages_transport_base_url.clone(),
            reasoning_stream_style: self.reasoning_stream_style.clone(),
            stream_idle_timeout: self.stream_idle_timeout,
            stream_open_timeout: self.stream_open_timeout,
            force_http1: self.force_http1,
        }
    }
}

const MIN_EXACT_SECRET_CHARS: usize = 8;

pub(crate) use codewhale_config::apply_openrouter_vendor;

fn push_model_bound_secret(values: &mut Vec<String>, value: Option<&str>) {
    let Some(value) = value
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.chars().count() >= MIN_EXACT_SECRET_CHARS)
    else {
        return;
    };
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn model_bound_secret_store_slot(provider: ProviderKind) -> Option<&'static str> {
    (provider != ProviderKind::Custom).then(|| provider.secret_store_slot())
}

fn push_file_backed_model_bound_secrets(values: &mut Vec<String>) {
    // Unit tests must never inspect the developer's real credential store.
    // The isolated regression below opts in with a temporary CODEWHALE_HOME,
    // matching Config's existing secret-store test discipline.
    #[cfg(test)]
    if !codewhale_paths::codewhale_home_is_explicit()
        || std::env::var_os("CODEWHALE_SECRET_BACKEND").is_none()
    {
        return;
    }

    // Redaction needs only a best-effort view of inactive file-backed
    // credentials. It must not cause a legacy-store migration merely because a
    // client is being constructed (notably for `doctor`'s live probe). Keep
    // this file-only to avoid a burst of OS-keychain prompts for inactive
    // providers; the active credential is already supplied by the route
    // resolver.
    let secrets = codewhale_secrets::Secrets::file_backed_read_only();
    let mut slots = Vec::new();
    for provider in codewhale_config::descriptors::provider_compatibility()
        .iter()
        .map(|row| row.kind)
    {
        let Some(slot) = model_bound_secret_store_slot(provider) else {
            continue;
        };
        if !slots.contains(&slot) {
            slots.push(slot);
        }
    }
    // The legacy literal `provider = "custom"` route owns this durable slot.
    slots.push("custom");

    for slot in slots {
        if let Ok(Some(secret)) = secrets.get(slot) {
            push_model_bound_secret(values, Some(&secret));
        }
    }
}

pub(crate) fn active_model_bound_secret_values(config: &Config) -> Vec<String> {
    let active_key = config.active_route_api_key_read_only().unwrap_or_default();
    configured_model_bound_secret_values(config, &active_key)
}

pub(crate) fn configured_model_bound_secret_values(
    config: &Config,
    active_api_key: &str,
) -> Vec<String> {
    let mut values = Vec::new();
    push_model_bound_secret(&mut values, Some(active_api_key));
    push_model_bound_secret(&mut values, config.sandbox_api_key.as_deref());
    push_model_bound_secret(
        &mut values,
        config
            .search
            .as_ref()
            .and_then(|search| search.api_key.as_deref()),
    );
    push_model_bound_secret(
        &mut values,
        config
            .vision_model
            .as_ref()
            .and_then(|vision| vision.api_key.as_deref()),
    );

    if let Some(headers) = config.http_headers.as_ref() {
        for (name, value) in headers {
            if is_upstream_auth_header(name) {
                push_model_bound_secret(&mut values, Some(value));
            }
        }
    }

    for row in codewhale_config::descriptors::provider_compatibility()
        .iter()
        .filter(|row| row.kind != ProviderKind::Custom)
    {
        for env_name in row.kind.provider().env_vars() {
            if let Ok(value) = std::env::var(env_name) {
                push_model_bound_secret(&mut values, Some(&value));
            }
        }
        let Some(provider_config) = config
            .providers
            .as_ref()
            .and_then(|tables| codewhale_config::provider_config_table!(@read tables, row.id))
        else {
            continue;
        };
        push_model_bound_secret(&mut values, provider_config.api_key.as_deref());
        if let Some(headers) = provider_config.http_headers.as_ref() {
            for (name, value) in headers {
                if is_upstream_auth_header(name) {
                    push_model_bound_secret(&mut values, Some(value));
                }
            }
        }
    }

    if let Some(providers) = config.providers.as_ref() {
        for provider_config in providers.custom.values() {
            push_model_bound_secret(&mut values, provider_config.api_key.as_deref());
            if let Some(env_name) = provider_config
                .api_key_env
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                && let Ok(value) = std::env::var(env_name)
            {
                push_model_bound_secret(&mut values, Some(&value));
            }
            if let Some(headers) = provider_config.http_headers.as_ref() {
                for (name, value) in headers {
                    if is_upstream_auth_header(name) {
                        push_model_bound_secret(&mut values, Some(value));
                    }
                }
            }
        }
    }

    // The decision router's TypeSafe key is no chat provider's key; its env
    // form must still never reach a model. (`for_decision_route` adds the key
    // from every source to its own client.)
    if let Ok(value) = std::env::var(system_one::TYPESAFE_API_KEY_ENV) {
        push_model_bound_secret(&mut values, Some(&value));
    }

    push_file_backed_model_bound_secrets(&mut values);

    // Replace longer values first in case one credential happens to contain
    // another as a prefix.
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values
}

/// Everything a catalog probe could have sent that must not come back.
///
/// Longest first, so a value that contains another is masked whole.
fn catalog_error_secret_values(
    active_api_key: &str,
    http_headers: &HashMap<String, String>,
    model_bound: &[String],
) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    let mut push = |value: &str| {
        let value = value.trim();
        if !value.is_empty() && !values.iter().any(|existing| existing == value) {
            values.push(value.to_string());
        }
    };
    // No length floor here, unlike `push_model_bound_secret`: a short key
    // echoed back by an untrusted endpoint is still a leaked key. The cost of
    // being wrong is a suppressed message, which is what this path did before.
    push(active_api_key);
    for value in http_headers.values() {
        push(value);
    }
    for value in model_bound {
        push(value);
    }
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values
}

/// The opaque values in a request's URL query — the pagination cursor.
///
/// Collected in both forms: decoded, as a provider that parsed the cursor
/// would echo it, and raw, as one that quoted the URL back would.
fn request_query_secret_values(url: &reqwest::Url) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    let mut push = |value: String| {
        if !value.trim().is_empty() && !values.contains(&value) {
            values.push(value);
        }
    };
    for (_, value) in url.query_pairs() {
        push(value.into_owned());
    }
    for pair in url.query().unwrap_or_default().split('&') {
        if let Some((_, value)) = pair.split_once('=') {
            push(value.to_string());
        }
    }
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values
}

/// Redirects are followed only within one host, over the same or a stronger
/// scheme.
///
/// Provider keys ride in headers this stack may send under names reqwest does
/// not strip on a cross-host hop (`x-api-key`, operator `http_headers`), so a
/// compromised or open-redirecting endpoint could otherwise collect a key by
/// answering `302`. A same-host hop to plaintext would expose the same key to
/// anything on the path, so a downgrade stops too. Upgrades and same-scheme
/// hops still work for the endpoints that use them.
fn same_host_redirects() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let from = attempt.previous().last();
        let next = attempt.url();
        let same_host = from.is_some_and(|previous| previous.host_str() == next.host_str());
        let downgrade =
            from.is_some_and(|previous| previous.scheme() == "https" && next.scheme() == "http");
        if same_host && !downgrade && attempt.previous().len() < 10 {
            attempt.follow()
        } else {
            attempt.stop()
        }
    })
}

pub(crate) fn redact_model_bound_text(text: &str, exact_secret_values: &[String]) -> String {
    let mut redacted = text.to_string();
    for secret in exact_secret_values {
        redacted = redacted.replace(secret, codewhale_config::persistence::REDACTED);
    }
    // Tool results feed exact-match edits, so only credential-shaped values
    // are masked here; key-only hits (`password: credentials?.password`) stay
    // byte-exact. Logs and previews keep the broad key-based scrubber.
    codewhale_config::persistence::redact_model_bound_secrets(&redacted)
}

pub(crate) fn redact_json_model_bound_text(
    value: &serde_json::Value,
    secrets: &[String],
) -> serde_json::Value {
    fn mask_exact_values(value: &mut serde_json::Value, secrets: &[String]) {
        match value {
            serde_json::Value::String(text) => *text = redact_model_bound_text(text, secrets),
            serde_json::Value::Array(values) => {
                for value in values {
                    mask_exact_values(value, secrets);
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values_mut() {
                    mask_exact_values(value, secrets);
                }
            }
            _ => {}
        }
    }
    // Preserve sensitive-key masking and the existing recursion-depth bound.
    let mut redacted = codewhale_config::persistence::redact_json_model_bound_secrets(value);
    mask_exact_values(&mut redacted, secrets);
    redacted
}

// === Helpers ===

/// Maximum bytes to read from an error response body (64 KB).
pub(super) const ERROR_BODY_MAX_BYTES: usize = 64 * 1024;

/// How much of a provider's HTTP error body may be shown to the user.
///
/// The catalog probe is the one request that contacts a `base_url` the user
/// typed during setup, and its URL carries an opaque pagination cursor, so a
/// provider — or anything answering at that URL — can echo a key, a custom
/// header value or the cursor back inside an error body. #3385 answered that
/// by discarding the body entirely, and the cost of the blunt version was
/// #6173: Gemini's real reason ("User location is not supported for the API
/// use") reached the user as `Invalid request (400): ` with nothing after the
/// colon, so a geo-block, a bad key and a wrong endpoint were indistinguishable.
pub(super) enum ErrorBodyDisclosure {
    /// The endpoint is already established. Surface the provider's message.
    Full,
    /// Untrusted endpoint. Surface only what survives redaction — and nothing
    /// at all if a known secret is still in the result.
    Guarded { request_secrets: Vec<String> },
}

/// Read/overall timeout for the shared client's non-streaming requests
/// (`/models` listing, catalog refresh, health probes). Streaming requests
/// keep their own idle-timeout envelope; without this, a provider that accepts
/// the connection and never answers hangs model-list, catalog refresh, and
/// health checks forever (ops R4).
pub(super) const NON_STREAMING_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const PROVIDER_CATALOG_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const PROVIDER_CATALOG_MAX_ROWS: usize = 10_000;

#[derive(Clone, Copy)]
struct ModelsFetchLimits {
    bytes: usize,
    rows: usize,
    pages: usize,
    cursor_bytes: usize,
    timeout: Duration,
}

const MODELS_FETCH_LIMITS: ModelsFetchLimits = ModelsFetchLimits {
    bytes: PROVIDER_CATALOG_MAX_RESPONSE_BYTES,
    rows: PROVIDER_CATALOG_MAX_ROWS,
    pages: 1_000,
    cursor_bytes: 4_096,
    timeout: NON_STREAMING_HTTP_TIMEOUT,
};

#[derive(Clone, Copy)]
enum ModelsRequestMode {
    Interactive,
    Refresh,
}

#[derive(Debug)]
enum ModelsFetchError {
    Catalog(CatalogRefreshError),
    Interactive(anyhow::Error),
}

impl ModelsFetchError {
    fn into_interactive(self) -> anyhow::Error {
        match self {
            Self::Catalog(reason) => anyhow::anyhow!("Failed to list models: {reason:?}"),
            Self::Interactive(error) => error,
        }
    }

    fn into_catalog(self) -> CatalogRefreshError {
        match self {
            Self::Catalog(reason) => reason,
            Self::Interactive(_) => CatalogRefreshError::Network,
        }
    }
}

impl From<CatalogRefreshError> for ModelsFetchError {
    fn from(reason: CatalogRefreshError) -> Self {
        Self::Catalog(reason)
    }
}

// Keep row bytes intact: a Value round trip would silently accept duplicate
// fields that the existing typed provider parsers reject.
#[derive(Deserialize)]
struct ModelsPage<'a> {
    #[serde(borrow)]
    data: Option<&'a serde_json::value::RawValue>,
    #[serde(borrow)]
    models: Option<&'a serde_json::value::RawValue>,
    #[serde(default)]
    has_more: bool,
    last_id: Option<String>,
}

impl<'a> ModelsPage<'a> {
    fn rows(&self) -> Result<&'a serde_json::value::RawValue, CatalogRefreshError> {
        self.data
            .or(self.models)
            .ok_or(CatalogRefreshError::InvalidResponse)
    }
}

struct BoundedModelsRows(usize);

impl<'de> serde::de::Visitor<'de> for BoundedModelsRows {
    type Value = Vec<Box<serde_json::value::RawValue>>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded model array")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut rows = Vec::new();
        while rows.len() < self.0 {
            let Some(row) = seq.next_element()? else {
                return Ok(rows);
            };
            rows.push(row);
        }
        if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom("model row limit exceeded"));
        }
        Ok(rows)
    }
}

impl<'de> serde::de::DeserializeSeed<'de> for BoundedModelsRows {
    type Value = Vec<Box<serde_json::value::RawValue>>;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Self::Value, D::Error> {
        deserializer.deserialize_seq(self)
    }
}

/// One complete traversal, independent of provider row parsing and transport
/// retry policy. Only a verified endpoint contract supplies a cursor query key;
/// generation wire format alone does not prove a model-list dialect.
async fn collect_models_document<F, Fut>(
    endpoint: reqwest::Url,
    cursor_query: Option<&str>,
    limits: ModelsFetchLimits,
    mut fetch: F,
) -> Result<(String, tokio::time::Instant), ModelsFetchError>
where
    F: FnMut(reqwest::Url) -> Fut,
    Fut: std::future::Future<Output = Result<reqwest::Response, ModelsFetchError>>,
{
    use serde::de::DeserializeSeed;
    let deadline = tokio::time::Instant::now() + limits.timeout;
    let body = tokio::time::timeout_at(deadline, async {
        let mut rows = Vec::new();
        let mut bytes = 0usize;
        let mut cursor: Option<String> = None;
        let mut seen = std::collections::HashSet::new();
        for page_index in 0..limits.pages {
            let mut url = endpoint.clone();
            if let (Some(key), Some(value)) = (cursor_query, cursor.as_deref()) {
                let query: Vec<_> = url
                    .query_pairs()
                    .filter(|(name, _)| name != key)
                    .map(|(name, value)| (name.into_owned(), value.into_owned()))
                    .collect();
                url.query_pairs_mut()
                    .clear()
                    .extend_pairs(query)
                    .append_pair(key, value);
            }
            let response = fetch(url).await?;
            let body =
                bounded_provider_catalog_text(response, limits.bytes.saturating_sub(bytes)).await?;
            bytes += body.len();
            let page: ModelsPage<'_> =
                serde_json::from_str(&body).map_err(|_| CatalogRefreshError::InvalidResponse)?;
            let page_rows = BoundedModelsRows(limits.rows.saturating_sub(rows.len()))
                .deserialize(&mut serde_json::Deserializer::from_str(page.rows()?.get()))
                .map_err(|_| CatalogRefreshError::InvalidResponse)?;
            rows.extend(page_rows);
            if !page.has_more {
                if tokio::time::Instant::now() >= deadline {
                    return Err(CatalogRefreshError::Network.into());
                }
                if page_index == 0 {
                    return Ok(body);
                }
                #[derive(Serialize)]
                struct Document {
                    data: Vec<Box<serde_json::value::RawValue>>,
                }
                return serde_json::to_string(&Document { data: rows })
                    .map_err(|_| CatalogRefreshError::InvalidResponse.into());
            }
            let next = page
                .last_id
                .filter(|value| !value.is_empty() && value.len() <= limits.cursor_bytes)
                .ok_or(CatalogRefreshError::InvalidResponse)?;
            if cursor_query.is_none() || !seen.insert(next.clone()) {
                return Err(CatalogRefreshError::InvalidResponse.into());
            }
            cursor = Some(next);
        }
        Err(ModelsFetchError::Catalog(
            CatalogRefreshError::InvalidResponse,
        ))
    })
    .await
    .map_err(|_| CatalogRefreshError::Network)??;
    Ok((body, deadline))
}

/// Read an error response body with a size limit to prevent unbounded allocation.
pub(super) async fn bounded_error_text(response: reqwest::Response, max_bytes: usize) -> String {
    use futures_util::StreamExt;
    let mut stream = response.bytes_stream();
    let mut buf = Vec::with_capacity(max_bytes.min(8192));
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        let remaining = max_bytes.saturating_sub(buf.len());
        if remaining == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

async fn bounded_provider_catalog_text(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<String, CatalogRefreshError> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(CatalogRefreshError::InvalidResponse);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| CatalogRefreshError::Network)?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(CatalogRefreshError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| CatalogRefreshError::InvalidResponse)
}

fn validate_base_url_security(base_url: &str, provider_allows_insecure_http: bool) -> Result<()> {
    let display_base_url = redact_url_for_display(base_url);
    let parsed = reqwest::Url::parse(base_url)
        .map_err(|_| anyhow::anyhow!("Refusing invalid base URL '{display_base_url}'"))?;
    let loopback = parsed.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback())
    });
    if parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback) {
        return Ok(());
    }

    if parsed.scheme() == "http" && provider_allows_insecure_http {
        logging::warn(
            "Using insecure HTTP base URL because this provider sets allow_insecure_http = true in config.toml",
        );
        return Ok(());
    }

    if parsed.scheme() == "http"
        && std::env::var(ALLOW_INSECURE_HTTP_ENV)
            .or_else(|_| std::env::var(LEGACY_ALLOW_INSECURE_HTTP_ENV))
            .ok()
            .as_deref()
            .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        logging::warn(format!(
            "Using insecure HTTP base URL because {ALLOW_INSECURE_HTTP_ENV} is set"
        ));
        return Ok(());
    }

    if parsed.scheme() == "http" {
        anyhow::bail!(
            "Refusing insecure base URL '{display_base_url}'.\n\
             \n\
             Loopback hosts (localhost, 127.0.0.1, [::1]) are auto-allowed.\n\
             For one trusted local provider (LAN, llama.cpp on a private IP, etc.) set\n\
             `allow_insecure_http = true` under its `[providers.<name>]` table in config.toml.\n\
             To allow it for every provider in this shell instead, set the env var\n\
             `{ALLOW_INSECURE_HTTP_ENV}=1` and re-run.",
        );
    }

    anyhow::bail!(
        "Refusing base URL '{display_base_url}': only HTTPS (or explicitly allowed HTTP) URLs are supported.",
    )
}

/// Mask credentials in a URL for display.
///
/// Delegates to the single shared implementation in
/// [`codewhale_secrets::sanitize`] (FEAT-025 D4) so command sanitization and
/// client diagnostics cannot drift.
pub(crate) fn redact_url_for_display(url: &str) -> String {
    codewhale_secrets::sanitize::redact_url_for_display(url)
}

pub(super) fn versioned_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if base_url_has_version_suffix(trimmed) {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1")
    }
}

fn unversioned_base_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    trimmed
        .rsplit_once('/')
        .filter(|(_, segment)| is_version_segment(segment))
        .map(|(base, _)| base)
        .unwrap_or(trimmed)
        .to_string()
}

fn base_url_has_version_suffix(trimmed: &str) -> bool {
    trimmed.rsplit('/').next().is_some_and(is_version_segment)
}

fn is_version_segment(segment: &str) -> bool {
    segment.eq_ignore_ascii_case("beta")
        || segment
            .strip_prefix('v')
            .or_else(|| segment.strip_prefix('V'))
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|ch| ch.is_ascii_digit()))
}

pub(crate) fn api_url(base_url: &str, path: &str) -> String {
    api_url_with_suffix(base_url, path, None)
}

fn responses_api_url(base_url: &str, provider: ProviderKind) -> String {
    let normalized = base_url.trim_end_matches('/').to_ascii_lowercase();
    let official_deepseek = matches!(provider, ProviderKind::Deepseek)
        && matches!(
            normalized.as_str(),
            "https://api.deepseek.com"
                | "https://api.deepseek.com/v1"
                | "https://api.deepseek.com/beta"
                | "https://api.deepseeki.com"
                | "https://api.deepseeki.com/v1"
                | "https://api.deepseeki.com/beta"
        );
    if official_deepseek {
        format!("{}/responses", unversioned_base_url(base_url))
    } else {
        api_url(base_url, "responses")
    }
}

pub(super) fn api_url_with_suffix(base_url: &str, path: &str, path_suffix: Option<&str>) -> String {
    let path = path.trim_start_matches('/');
    if path.starts_with("beta/") {
        return format!("{}/{}", unversioned_base_url(base_url), path);
    }
    if let ("chat/completions", Some(suffix)) = (path, path_suffix) {
        return format!(
            "{}/{}",
            unversioned_base_url(base_url),
            suffix.trim_start_matches('/')
        );
    }
    let mut versioned = versioned_base_url(base_url);
    // The /beta suffix is not a real API version — it is an
    // opt-in surface for beta features.  Only paths with an
    // explicit `beta/` prefix should hit the beta surface;
    // everything else (models, chat/completions, health, …)
    // must go to the standard /v1 surface.
    if versioned.ends_with("beta") {
        versioned = format!("{}/v1", unversioned_base_url(base_url));
    }
    format!("{}/{}", versioned.trim_end_matches('/'), path)
}

/// Route strict DeepSeek tool requests through the beta Chat Completions
/// surface while keeping every ordinary request on the canonical `/v1` path.
///
/// DeepSeek requires its `/beta` base URL when a function opts into
/// `strict: true`. The configured route URL remains semantic here because
/// unit tests may replace only the transport origin with a local capture
/// server.
///
/// Source: <https://api-docs.deepseek.com/guides/tool_calls/> (verified 2026-07-22).
fn chat_completions_url(
    transport_base_url: &str,
    route_base_url: &str,
    provider: ProviderKind,
    path_suffix: Option<&str>,
    body: &Value,
) -> String {
    let uses_deepseek_beta = matches!(provider, ProviderKind::Deepseek)
        && is_official_deepseek_beta_base_url(route_base_url)
        && body_uses_strict_tools(body)
        && path_suffix.is_none();
    let path = if uses_deepseek_beta {
        "beta/chat/completions"
    } else {
        "chat/completions"
    };
    api_url_with_suffix(transport_base_url, path, path_suffix)
}

fn is_official_deepseek_beta_base_url(base_url: &str) -> bool {
    matches!(
        base_url.trim_end_matches('/').to_ascii_lowercase().as_str(),
        "https://api.deepseek.com/beta" | "https://api.deepseeki.com/beta"
    )
}

fn body_uses_strict_tools(body: &Value) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|tool| tool.pointer("/function/strict").and_then(Value::as_bool) == Some(true))
        })
}

fn normalize_audio_format(format: &str) -> String {
    let normalized = format.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        "wav".to_string()
    } else {
        normalized
    }
}

fn parse_speech_audio_response(payload: &Value) -> Result<(Vec<u8>, Option<String>)> {
    let audio = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| {
            choice
                .get("message")
                .and_then(|message| message.get("audio"))
                .or_else(|| choice.get("delta").and_then(|delta| delta.get("audio")))
        })
        .or_else(|| payload.get("audio"))
        .context("Speech synthesis response did not include choices[0].message.audio")?;

    let data = audio
        .get("data")
        .and_then(Value::as_str)
        .context("Speech synthesis response did not include audio.data")?
        .trim();
    let data = data
        .split_once(',')
        .map(|(_, base64)| base64.trim())
        .unwrap_or(data);
    let audio_bytes = general_purpose::STANDARD
        .decode(data)
        .context("Failed to decode speech audio base64 data")?;
    let transcript = audio
        .get("transcript")
        .and_then(Value::as_str)
        .map(str::to_string);

    Ok((audio_bytes, transcript))
}

fn build_speech_synthesis_body(
    model: &str,
    text: &str,
    instruction: Option<&str>,
    audio: Value,
) -> Value {
    let mut messages = Vec::new();
    if let Some(instruction) = instruction.map(str::trim).filter(|value| !value.is_empty()) {
        messages.push(json!({
            "role": "user",
            "content": instruction,
        }));
    }
    messages.push(json!({
        "role": "assistant",
        "content": text,
    }));

    json!({
        "model": model,
        "messages": messages,
        "audio": audio,
    })
}

// === CodewhaleClient ===

/// Returns true when CODEWHALE_FORCE_HTTP1 (legacy alias: DEEPSEEK_FORCE_HTTP1)
/// is set to a truthy value (`1`, `true`, `yes`, `on`, case-insensitive). Read
/// only by `Config::force_http1`, which ORs it with the selected stream config flag; every
/// client builder and stream open takes that resolved value (#103, #6700). Anything else (unset, `0`,
/// `false`, ...) leaves HTTP/2 on.
pub(crate) fn force_http1_from_env() -> bool {
    std::env::var("CODEWHALE_FORCE_HTTP1")
        .or_else(|_| std::env::var("DEEPSEEK_FORCE_HTTP1"))
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
}

/// Read `SSL_CERT_FILE` and add its contents as extra root
/// certificates on the reqwest builder (#418). Tries the PEM-bundle
/// parser first (covers single-cert files too), then falls back to
/// DER. All failures log a warning and return the builder unchanged
/// so a malformed env var degrades gracefully.
fn add_extra_root_certs(
    mut builder: reqwest::ClientBuilder,
    cert_path: &str,
) -> reqwest::ClientBuilder {
    let bytes = match std::fs::read(cert_path) {
        Ok(b) => b,
        Err(err) => {
            logging::warn(format!(
                "SSL_CERT_FILE={cert_path} could not be read: {err}"
            ));
            return builder;
        }
    };

    if let Ok(certs) = reqwest::Certificate::from_pem_bundle(&bytes) {
        let added = certs.len();
        for cert in certs {
            builder = builder.add_root_certificate(cert);
        }
        logging::info(format!(
            "SSL_CERT_FILE={cert_path} loaded ({added} cert(s))"
        ));
        return builder;
    }

    match reqwest::Certificate::from_der(&bytes) {
        Ok(cert) => {
            builder = builder.add_root_certificate(cert);
            logging::info(format!("SSL_CERT_FILE={cert_path} loaded (1 DER cert)"));
        }
        Err(err) => {
            logging::warn(format!(
                "SSL_CERT_FILE={cert_path} could not be parsed as PEM bundle or DER: {err}"
            ));
        }
    }
    builder
}

/// Admit a complete response's tool pairing ids before hooks, approvals or
/// replayable history. Use the protocol frozen with the actual request.
pub(crate) fn validate_tool_call_ids_for_protocol<'a>(
    protocol: WireFormat,
    ids: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let mut seen = std::collections::HashSet::new();
    for id in ids {
        let pairing_id = if protocol == WireFormat::Responses {
            responses::parse_tool_use_id(id).0
        } else {
            id.to_string()
        };
        if pairing_id.trim().is_empty() {
            anyhow::bail!("Provider returned a tool call without a pairing id");
        }
        if !seen.insert(pairing_id) {
            anyhow::bail!("Provider returned duplicate tool call pairing ids in one response");
        }
    }
    Ok(())
}

impl CodewhaleClient {
    pub(crate) fn wire_format(&self) -> WireFormat {
        self.wire_format
    }

    #[cfg(test)]
    pub(crate) fn validate_tool_call_ids<'a>(
        &self,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        validate_tool_call_ids_for_protocol(self.wire_format, ids)
    }

    fn is_local_ds4_model(&self, model: &str) -> bool {
        self.api_provider == ProviderKind::Custom
            && self
                .admitted_identity
                .key
                .as_str()
                .eq_ignore_ascii_case("ds4")
            && crate::config::base_url_uses_local_host(&self.base_url)
            && matches!(
                model.trim().to_ascii_lowercase().as_str(),
                "deepseek-v4-flash" | "deepseek-v4-pro"
            )
    }

    /// DS4 is configured as a named custom route so its endpoint and billing
    /// identity remain exact, but its chat payload deliberately speaks the
    /// first-party DeepSeek reasoning/tool dialect that DS4 implements.
    fn chat_shape_provider(&self, model: &str) -> ProviderKind {
        if self.is_local_ds4_model(model) {
            ProviderKind::Deepseek
        } else {
            self.api_provider
        }
    }

    /// Create a DeepSeek client from CLI configuration.
    pub fn new(config: &Config) -> Result<Self> {
        let identity = config
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;
        let api_provider = identity.provider;
        let model_aware = api_provider.provider().wire_policy()
            == codewhale_config::provider::WirePolicy::ModelAware;
        let default_model = config.default_model();
        let unresolved_local_model = api_provider == ProviderKind::Ollama
            && matches!(default_model.trim(), "" | "auto" | "unknown");
        if model_aware || unresolved_local_model {
            let route =
                crate::route_runtime::resolve_runtime_route_for_identity(config, &identity, None)
                    .map_err(anyhow::Error::msg)?;
            return Self::from_candidate(&route.config, &route.candidate);
        }
        let route_limits = crate::route_runtime::resolve_runtime_route_for_identity(
            config,
            &identity,
            Some(&default_model),
        )
        .ok()
        .and_then(|route| crate::route_budget::known_route_limits(route.candidate.limits()));
        Self::from_parts(
            config.active_route_base_url(),
            default_model,
            provider_wire_format_for_config(&identity, Some(config)),
            route_limits,
            config,
        )
    }

    /// Construct only the model-list probe before a route has a concrete model.
    /// Catalog bootstrap must not depend on the catalog it is about to fetch.
    pub(crate) fn for_catalog_refresh(config: &Config) -> Result<Self> {
        let identity = config
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;
        Self::from_parts(
            config.active_route_base_url(),
            config.default_model(),
            provider_wire_format_for_config(&identity, Some(config)),
            None,
            config,
        )
    }

    /// Create a DeepSeek client whose transport is bound to a runtime-resolved
    /// route (#3384).
    ///
    /// The base URL and default model come from the executable `candidate`, so
    /// the client talks to exactly the endpoint and wire model the resolver
    /// chose instead of re-deriving them from `Config`. Secrets stay in
    /// `Config`: `ReadyRouteCandidate` is secret-free by design (it carries only
    /// an auth-source *class*), so the API key and provider are still read from
    /// `config`.
    pub fn from_candidate(config: &Config, candidate: &ReadyRouteCandidate) -> Result<Self> {
        let identity = config
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            candidate.provider_kind() == identity.provider,
            "resolved candidate does not match admitted provider kind"
        );
        Self::from_parts(
            candidate.endpoint().base_url.clone(),
            candidate.wire_model_id().as_str().to_string(),
            candidate.protocol(),
            crate::route_budget::known_route_limits(candidate.limits()),
            config,
        )
    }

    /// Shared constructor body for [`Self::new`] and [`Self::from_candidate`].
    ///
    /// `base_url` and `default_model` are the only inputs that differ between
    /// the two entry points; everything else (auth, provider, retry, headers,
    /// timeouts) is derived from `config` so the two paths cannot drift.
    fn from_parts(
        base_url: String,
        default_model: String,
        wire_format: WireFormat,
        route_limits: Option<RouteLimits>,
        config: &Config,
    ) -> Result<Self> {
        let admitted_identity = config
            .active_provider_identity()
            .map_err(anyhow::Error::msg)?;
        let api_provider = admitted_identity.provider;
        anyhow::ensure!(
            api_provider != ProviderKind::Antigravity,
            codewhale_config::LEGACY_ANTIGRAVITY_TOMBSTONE_MESSAGE
        );
        config
            .verify_provider_identity(&admitted_identity)
            .map_err(anyhow::Error::msg)?;
        let plugin_provider = config
            .provider_config_for(&admitted_identity)
            .filter(|entry| entry.plugin_authority.is_some())
            .cloned()
            .map(Box::new);
        if let Some(entry) = &plugin_provider {
            anyhow::ensure!(
                entry.oauth.is_some()
                    && config.auth_mode_for_provider(&admitted_identity).as_deref()
                        == Some("oauth"),
                "plugin provider requires host-managed OAuth"
            );
        }
        let base_url = if plugin_provider.is_some() {
            let reviewed = config.base_url_for_route(&admitted_identity);
            anyhow::ensure!(
                reqwest::Url::parse(&base_url)? == reqwest::Url::parse(&reviewed)?,
                "plugin candidate changed its reviewed provider endpoint"
            );
            reviewed
        } else {
            base_url
        };
        let openrouter_vendor = config.openrouter_vendor()?;
        let billing_surface = crate::route_billing::billing_surface_for_dispatch(
            Some(config),
            &admitted_identity,
            Some(&base_url),
        )
        .map(str::to_string);
        let billing_mode =
            crate::route_billing::for_route_with_endpoint(config, &admitted_identity, &base_url)
                .into();
        if api_provider == ProviderKind::OpenaiCodex {
            anyhow::ensure!(
                !reqwest::Url::parse(&base_url)
                    .ok()
                    .is_some_and(|url| url.host_str() == Some("chatgpt.com")),
                "The legacy ChatGPT backend route is retired. Use https://api.openai.com/v1 and run codewhale auth chatgpt."
            );
        }
        if api_provider == ProviderKind::OpencodeGo {
            validate_route(api_provider, &default_model).map_err(anyhow::Error::msg)?;
        }
        anyhow::ensure!(
            api_provider != ProviderKind::OpenaiCodex
                || crate::pricing::is_official_chatgpt_api(&base_url)
                || config.provider_uses_custom_endpoint(&admitted_identity),
            "Refusing to send an official ChatGPT grant to a different endpoint"
        );
        // Guidance and opaque-reasoning scope come from the exact owned
        // credential snapshot this client sends, never a second store read.
        let (
            (api_key, api_key_source),
            codex_account_id,
            subscription_limit_guidance,
            chatgpt_reasoning_api,
        ) = if api_provider == ProviderKind::OpenaiCodex
            && crate::pricing::is_official_chatgpt_api(&base_url)
        {
            let credentials = config.codex_credentials()?;
            let guidance = crate::oauth::usage_limit_guidance(
                crate::oauth::OAuthProvider::Chatgpt,
                credentials.account_label.as_deref(),
            );
            let identity = serde_json::to_vec(&(
                credentials.issuer.as_str(),
                credentials.client_id.as_str(),
                credentials
                    .account_id
                    .as_deref()
                    .context("Verified ChatGPT subject is missing")?,
            ))?;
            let reasoning_api = format!(
                "openai-responses-siwc-v1:{}",
                crate::hashing::sha256_hex(&identity),
            );
            (
                (credentials.access_token, "ChatGPT sign-in".to_string()),
                credentials.account_id,
                Some(guidance),
                Some(reasoning_api),
            )
        } else {
            // Only the resolver's xAI OAuth step carries a sign-in; an
            // OAuth mode that fell through to an API key names no account.
            let (resolved, xai_sign_in) = config.active_route_api_key_with_xai_sign_in()?;
            let guidance = xai_sign_in.map(|label| {
                crate::oauth::usage_limit_guidance(
                    crate::oauth::OAuthProvider::Xai,
                    label.as_deref(),
                )
            });
            (resolved, None, guidance, None)
        };
        let subscription_limit_guidance = if api_key_source == "Claude sign-in" {
            Some(crate::oauth::usage_limit_guidance(
                crate::oauth::OAuthProvider::Claude,
                None,
            ))
        } else {
            subscription_limit_guidance
        };
        let model_bound_secret_values =
            Arc::new(configured_model_bound_secret_values(config, &api_key));
        // The opt-out is effective only after an explicit startup confirmation;
        // every unconfirmed or absent request stays on the safe default.
        let model_bound_masking = !codewhale_config::redaction::effective_masking(
            config.model_bound_redaction(),
            config.loaded_config_path.as_deref(),
        )
        .is_disabled();
        validate_base_url_security(&base_url, config.allow_insecure_http())?;
        let retry = config.retry_policy();
        let stream_idle_timeout = Duration::from_secs(config.stream_chunk_timeout_secs());
        let stream_open_timeout = config.stream_open_timeout();
        let force_http1 = config.force_http1();
        if force_http1 {
            logging::info(
                "HTTP/1.1 pinned (stream configuration or environment) — HTTP/2 disabled",
            );
        }
        // A newly reviewed plugin destination must not inherit credentials or
        // routing headers from unrelated, global provider configuration.
        let mut http_headers = plugin_provider.as_ref().map_or_else(
            || config.http_headers(),
            |entry| entry.http_headers.clone().unwrap_or_default(),
        );
        if api_key_source == "Claude sign-in" {
            http_headers.retain(|name, _| !is_upstream_auth_header(name));
            let mut beta = http_headers
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
                .flat_map(|(_, value)| value.split(',').map(str::trim))
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            http_headers.retain(|name, _| !name.eq_ignore_ascii_case("anthropic-beta"));
            if !beta.iter().any(|value| value == "oauth-2025-04-20") {
                beta.push("oauth-2025-04-20".into());
            }
            http_headers.insert("anthropic-beta".into(), beta.join(","));
            anyhow::ensure!(
                config
                    .provider_config_for(&admitted_identity)
                    .and_then(|entry| entry.oauth_credential_generation.as_deref())
                    .is_some_and(codewhale_config::is_valid_claude_oauth_generation),
                "Claude sign-in generation is missing; sign in again"
            );
        }
        let auth_disabled = auth_mode_disables_api_key(
            config.auth_mode_for_provider(&admitted_identity).as_deref(),
        );
        let insecure_skip_tls_verify = config.insecure_skip_tls_verify();
        let path_suffix = config
            .provider_config_for(&admitted_identity)
            .and_then(|p| p.path_suffix.clone());
        let reasoning_stream_style = config
            .provider_config_for(&admitted_identity)
            .and_then(|p| p.reasoning_stream_style.clone());
        let request_concurrency_limit = config.provider_max_concurrency(&admitted_identity);

        logging::info(format!("API provider: {}", api_provider.as_str()));
        logging::info(format!(
            "API base URL: {}",
            redact_url_for_display(&base_url)
        ));
        if let Some(suffix) = &path_suffix {
            logging::info(format!("API path suffix override: {suffix}"));
        }
        if !http_headers.is_empty() {
            logging::info(format!(
                "{} custom HTTP header(s) configured",
                http_headers.len()
            ));
        }
        if insecure_skip_tls_verify {
            logging::warn(format!(
                "TLS certificate verification cannot be disabled for provider {}; use SSL_CERT_FILE with a trusted custom CA bundle instead",
                api_provider.as_str()
            ));
            bail!(
                "TLS certificate verification cannot be disabled for provider {}; configure SSL_CERT_FILE with a trusted custom CA bundle instead",
                api_provider.as_str()
            );
        }
        logging::info(format!(
            "Retry policy: enabled={}, max_retries={}, initial_delay={}s, max_delay={}s",
            retry.enabled, retry.max_retries, retry.initial_delay, retry.max_delay
        ));
        if let Some(limit) = request_concurrency_limit {
            logging::info(format!(
                "Provider request concurrency cap: {} in-flight request(s)",
                limit
            ));
        }

        let http_client = Self::http_client_builder_with_auth_mode(
            &api_key,
            &http_headers,
            api_provider,
            &base_url,
            wire_format,
            auth_disabled,
            force_http1,
            config,
            AuthHeaderSurface::Inference,
        )?;
        let http_client = if plugin_provider.is_some()
            || api_key_source == "Claude sign-in"
            || api_provider == ProviderKind::OpenaiCodex
        {
            http_client.redirect(reqwest::redirect::Policy::none())
        } else {
            http_client.redirect(same_host_redirects())
        }
        .build()?;
        let models_http_client = Self::http_client_builder_with_auth_mode(
            &api_key,
            &http_headers,
            api_provider,
            &base_url,
            wire_format,
            auth_disabled,
            force_http1,
            config,
            AuthHeaderSurface::Listing,
        )?
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
        // Always keep an HTTP/1.1 twin for automatic stream-header fallback
        // when H2 stalls. When `force_http1` is pinned, both clients are
        // HTTP/1.1 and the fallback is a no-op retry path.
        let http1_client = Self::http_client_builder_with_auth_mode(
            &api_key,
            &http_headers,
            api_provider,
            &base_url,
            wire_format,
            auth_disabled,
            true,
            config,
            AuthHeaderSurface::Inference,
        )?;
        let http1_client = if plugin_provider.is_some() || api_key_source == "Claude sign-in" {
            http1_client.redirect(reqwest::redirect::Policy::none())
        } else {
            http1_client
        }
        .build()?;

        let route_facts = crate::provider_lake::configured_catalog_offering_for_route(
            config,
            api_provider,
            admitted_identity.key.as_str(),
            &base_url,
            &default_model,
        )
        .map(Arc::new);
        let catalog_error_secret_values = Arc::new(catalog_error_secret_values(
            &api_key,
            &http_headers,
            &model_bound_secret_values,
        ));
        Ok(Self {
            http_client,
            models_http_client,
            http1_client,
            api_key,
            claude_oauth_config: (api_key_source == "Claude sign-in")
                .then(|| Box::new(config.clone())),
            api_key_source,
            subscription_limit_guidance,
            plugin_provider,
            plugin_oauth_read_only: config.plugin_oauth_read_only,
            model_bound_secret_values,
            catalog_error_secret_values,
            model_bound_masking,
            base_url,
            api_provider,
            admitted_identity,
            openrouter_vendor,
            billing_surface,
            billing_mode,
            configured_models: Arc::new(config.custom_models.clone().unwrap_or_default()),
            route_limits,
            route_facts,
            codex_account_id,
            chatgpt_reasoning_api,
            wire_format,
            retry,
            isolated_request_state: false,
            remote_control_inference_participant: !config.runtime_chat_isolated
                && !config.runtime_thread_inference_unrelated,
            default_model,
            connection_health: Arc::new(AsyncMutex::new(ConnectionHealth::default())),
            rate_limiter: Arc::new(AsyncMutex::new(TokenBucket::from_env())),
            request_concurrency: request_concurrency_limit.map(ProviderConcurrencyLimiter::new),
            path_suffix,
            #[cfg(test)]
            test_chat_transport_base_url: None,
            #[cfg(test)]
            test_messages_transport_base_url: None,
            reasoning_stream_style,
            stream_idle_timeout,
            stream_open_timeout,
            force_http1,
        })
    }

    /// Revalidate revocation and refresh host-owned OAuth before each actual send.
    /// Plugin code receives neither the access token nor the refresh token.
    pub(super) async fn authorize_plugin_request(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<reqwest::RequestBuilder> {
        if let Some(config) = self.claude_oauth_config.clone() {
            let destination = request
                .try_clone()
                .context("Claude request cannot be inspected")?
                .build()?;
            anyhow::ensure!(
                destination.url().scheme() == "https"
                    && destination.url().host_str() == Some("api.anthropic.com")
                    && destination.url().port_or_known_default() == Some(443),
                "Claude subscription request escaped the native Anthropic origin"
            );
            let read_only = self.plugin_oauth_read_only;
            let token = tokio::task::spawn_blocking(move || {
                if read_only {
                    crate::oauth::get_owned_credentials_read_only(
                        crate::oauth::OAuthProvider::Claude,
                        &config,
                    )
                } else {
                    crate::oauth::get_owned_credentials(
                        crate::oauth::OAuthProvider::Claude,
                        &config,
                    )
                }
            })
            .await
            .context("Claude credential worker failed")??;
            return Ok(request.bearer_auth(token.access_token));
        }
        let Some(provider) = self.plugin_provider.clone() else {
            return Ok(request);
        };
        let authority = provider
            .plugin_authority
            .clone()
            .context("plugin provider has no authority")?;
        let name = self.admitted_identity.key.to_string();
        let base_url = self.base_url.clone();
        let destination = request
            .try_clone()
            .context("plugin request cannot be inspected")?
            .build()?;
        let base = reqwest::Url::parse(&base_url)?;
        anyhow::ensure!(
            destination.url().origin() == base.origin(),
            "plugin request escaped its reviewed provider origin"
        );
        let policy = crate::plugins::activation::extension_host_policy_enabled();
        let read_only = self.plugin_oauth_read_only;
        let token = tokio::task::spawn_blocking(move || -> Result<String> {
            let _scope = crate::plugins::activation::PolicyScope::propagate(policy);
            let descriptor = provider
                .oauth
                .as_ref()
                .context("plugin provider requires host-managed OAuth")?;
            let empty_headers = std::collections::HashMap::new();
            crate::plugins::providers::verify_provider_binding(
                &authority,
                &name,
                &base_url,
                descriptor,
                Some(provider.http_headers.as_ref().unwrap_or(&empty_headers)),
            )
            .map_err(anyhow::Error::msg)?;
            let token =
                crate::oauth::plugin_oauth_access_token(&name, &base_url, descriptor, read_only)?;
            // Refresh may wait on the issuer. Do not send a model request if
            // the review was revoked while that HTTP request was in flight.
            crate::plugins::registry::verify_plugin_component_authority(
                &authority,
                crate::plugins::activation::PluginActivationCapability::Providers,
            )
            .map_err(anyhow::Error::msg)?;
            Ok(token)
        })
        .await??;
        Ok(request.bearer_auth(token))
    }

    /// Map a failed HTTP response, naming the route, host and key source on
    /// authentication and unknown-model failures (#6528) so a rejected key
    /// explains which credential was sent where and how to replace it.
    fn http_error_with_route_context(
        &self,
        status: u16,
        body: &str,
        retry_after: Option<Duration>,
        body_retry_after: Option<Duration>,
    ) -> LlmError {
        let route = self.admitted_identity.key.as_str();
        let error = match status {
            401 | 403 => LlmError::from_http_response_with_auth_context(
                status,
                body,
                Some(
                    crate::llm_client::AuthenticationErrorContext::from_parts(
                        Some(route),
                        Some(&self.base_url),
                        None,
                        Some(&self.api_key_source),
                        Some(&self.api_key),
                    )
                    .with_fix(auth_fix_hint(route, &self.api_key_source)),
                ),
            ),
            _ => LlmError::from_http_response_with_retry_hints(
                status,
                body,
                retry_after,
                body_retry_after,
            ),
        };
        match error {
            LlmError::ModelError(message) => LlmError::ModelError(format!(
                "{message} (provider route: {route}, host: {}; run `codewhale model resolve` or /model to pick a model this route serves)",
                crate::llm_client::base_url_authority(&self.base_url)
                    .unwrap_or_else(|| redact_url_for_display(&self.base_url))
            )),
            LlmError::QuotaExhausted(error) => match self.subscription_limit_guidance.as_deref() {
                Some(guidance) => LlmError::QuotaExhausted(error.with_guidance(guidance)),
                None => LlmError::QuotaExhausted(error),
            },
            other => other,
        }
    }

    /// Transport destination for Chat Completions requests.
    ///
    /// Production always uses the semantic route base URL. Unit tests may
    /// substitute a local capture server without changing the endpoint/model
    /// identity used by exact-route request shaping.
    pub(super) fn chat_transport_base_url(&self) -> &str {
        #[cfg(test)]
        if let Some(base_url) = self.test_chat_transport_base_url.as_deref() {
            return base_url;
        }
        &self.base_url
    }

    /// Redirect Chat Completions *transport* to a local capture server while
    /// the semantic route (`base_url`, model, endpoint identity) stays exact.
    ///
    /// Test-only, and compiled out of release builds. Route shaping reads
    /// [`Self::base_url`], so an exact-route matrix can capture the real
    /// first-turn body for `api.z.ai`, `api.moonshot.ai`, `api.kimi.com`, or
    /// `api.minimax.io` without ever making a live provider call.
    #[cfg(test)]
    pub(crate) fn set_test_chat_transport_base_url(&mut self, base_url: String) {
        self.test_chat_transport_base_url = Some(base_url);
    }

    /// Transport destination for a prepared Anthropic-compatible request.
    /// Production sends the exact prepared endpoint; tests may redirect the
    /// transport while preserving that immutable endpoint for route shaping.
    pub(super) fn messages_transport_url(&self, prepared_url: &str) -> String {
        #[cfg(test)]
        if let Some(base_url) = self.test_messages_transport_base_url.as_deref() {
            return anthropic::anthropic_messages_url(base_url);
        }
        prepared_url.to_string()
    }

    /// Return a request whose tool results are safe to send to an upstream
    /// model provider.
    ///
    /// Tool output is untrusted model-bound data: it can contain a whole
    /// config file, a bare credential emitted by a shell command, or a
    /// spillover receipt whose backing content is later persisted by the chat
    /// adapter. Keep this boundary above all protocol adapters so Chat,
    /// Anthropic Messages, and OpenAI Responses — streaming and non-streaming
    /// alike — receive the same sanitized payload.
    fn prepare_model_bound_request(&self, mut request: MessageRequest) -> MessageRequest {
        let repair =
            crate::tool_history_repair::repair_tool_call_pairs_for_provider(&mut request.messages);
        if !repair.is_empty() {
            tracing::warn!(
                repaired_call_ids = ?repair.repaired_call_ids,
                duplicate_result_ids = ?repair.duplicate_result_ids,
                orphan_result_ids = ?repair.orphan_result_ids,
                "repaired tool call/result history before provider projection"
            );
        }
        for message in &mut request.messages {
            for block in &mut message.content {
                if let ContentBlock::ToolResult { content, .. } = block
                    && self.model_bound_masking
                {
                    *content = redact_model_bound_text(content, &self.model_bound_secret_values);
                }
            }
        }
        request
    }

    /// Redact configured credentials from text that has been flattened into a
    /// normal model-bound text block. Unlike `prepare_model_bound_request`,
    /// this path always redacts: routing/classification prompts (which may
    /// summarize tool output) and durable goal-state text are not covered by
    /// the `[redaction] model_bound` opt-out, which exists so the model can
    /// quote file bytes back for exact edits — never to relax storage or
    /// routing summaries.
    pub(crate) fn redact_model_bound_text(&self, text: &str) -> String {
        redact_model_bound_text(text, &self.model_bound_secret_values)
    }

    /// Redact tool output as it enters the transcript. Unlike the request
    /// boundary, this path always redacts: the `[redaction] model_bound`
    /// opt-out exists so the model can quote file bytes back for exact edits,
    /// never to write a live credential to disk.
    pub(crate) fn redact_tool_output_for_transcript(&self, text: &str) -> String {
        redact_model_bound_text(text, &self.model_bound_secret_values)
    }

    /// Alternate models share this client's frozen endpoint and declarations.
    /// Resolution still owns protocol admission, including closed rosters.
    pub(crate) fn resolve_model_route(&self, model: &str) -> Result<ReadyRouteCandidate> {
        static RESOLVER: OnceLock<RouteResolver> = OnceLock::new();
        let resolver = RESOLVER.get_or_init(RouteResolver::new);
        // Pin the transport's own wire: a custom-table client constructed
        // from a wire-true candidate keeps speaking that wire even when this
        // internal re-resolution runs config-free (an engine-internal model
        // switch must not silently downgrade the table's dialect).
        let request = RouteRequest {
            explicit_provider: Some(self.api_provider),
            model_selector: Some(LogicalModelRef::from(model)),
            saved_provider_model: None,
            base_url_override: Some(self.base_url.clone()),
            limit_overrides: Vec::new(),
            wire_override: (self.api_provider == ProviderKind::Custom).then_some(self.wire_format),
        };
        if self.configured_models.is_empty() || self.api_provider == ProviderKind::OpenaiCodex {
            resolver.resolve(&request)
        } else {
            resolver
                .clone()
                .with_configured_models(
                    &self.configured_models,
                    self.admitted_identity.key.as_str(),
                    self.api_provider,
                    &self.base_url,
                )
                .resolve(&request)
        }
        .map_err(anyhow::Error::msg)
    }

    fn declared_wire_model<'a>(&self, model: &'a str) -> Option<&'a str> {
        if self.api_provider == ProviderKind::OpenaiCodex
            || !self.configured_models.iter().any(|row| {
                row.id == model
                    && row.matches_route(self.admitted_identity.key.as_str(), &self.base_url)
            })
        {
            return None;
        }
        // A declaration cannot admit a new model to a closed protocol roster,
        // or defeat a required protocol alias such as OpenCode Go's allowlist.
        self.resolve_model_route(model)
            .ok()
            .filter(|candidate| candidate.wire_model_id().as_str() == model)
            .map(|_| model)
    }

    fn wire_model_for_route(&self, model: &str) -> String {
        self.declared_wire_model(model)
            .map(str::to_string)
            .unwrap_or_else(|| {
                wire_model_for_provider_route(self.api_provider, &self.base_url, model)
            })
    }

    fn catalog_facts_for_model(
        &self,
        model: &str,
    ) -> Option<Arc<codewhale_config::catalog::CatalogOffering>> {
        let mut config = Config::default();
        let _ = config
            .custom_models
            .replace(self.configured_models.as_ref().clone());
        crate::provider_lake::configured_catalog_offering_for_route(
            &config,
            self.api_provider,
            self.admitted_identity.key.as_str(),
            &self.base_url,
            model,
        )
        .map(Arc::new)
    }

    /// Resolve `model` through the central route resolver and rebuild this
    /// client whenever its exact wire identity, limits, or protocol differs
    /// from the route bound at construction (#5042). `Ok(None)` means the
    /// existing binding is already exact for `model`. This includes
    /// same-protocol switches: an OpenAI-compatible client for model A must not
    /// carry A's route limits into model B merely because both speak Chat.
    pub(crate) fn rebound_for_model_protocol(
        &self,
        config: Option<&Config>,
        model: &str,
    ) -> Result<Option<Self>> {
        // The bound model already owns its admitted protocol and limits,
        // including exact live-catalog facts absent from the offline catalog.
        if model == self.default_model {
            return Ok(None);
        }
        let candidate = self.resolve_model_route(model)?;
        let candidate_limits = crate::route_budget::known_route_limits(candidate.limits());
        if candidate.protocol() == self.wire_format
            && candidate.wire_model_id().as_str() == self.default_model
            && candidate_limits == self.route_limits
        {
            return Ok(None);
        }
        if candidate.protocol() == self.wire_format {
            // Same-protocol model switches keep the already-authenticated,
            // endpoint-bound transport but must freeze the alternate model's
            // exact wire identity and limits. This path is also what makes a
            // model-aware client usable in embedded/test runtimes that do not
            // retain the original Config after construction.
            let mut rebound = self.clone();
            rebound.default_model = candidate.wire_model_id().as_str().to_string();
            rebound.route_limits = candidate_limits;
            rebound.route_facts = rebound.catalog_facts_for_model(&rebound.default_model);
            return Ok(Some(rebound));
        }
        let config = config.ok_or_else(|| {
            anyhow::anyhow!(
                "{} model {:?} uses {:?}, but this client is bound to {:?} and no configuration is available to rebuild it",
                self.api_provider.provider().display_name(),
                model,
                candidate.protocol(),
                self.wire_format
            )
        })?;
        let mut rebound = Self::from_candidate(config, &candidate)?;
        rebound.configured_models = Arc::clone(&self.configured_models);
        rebound.route_facts = rebound.catalog_facts_for_model(&rebound.default_model);
        Ok(Some(rebound))
    }

    fn bind_request_to_protocol(
        &self,
        mut request: MessageRequest,
    ) -> Result<(MessageRequest, Option<RouteLimits>)> {
        let model_aware = self.api_provider.provider().wire_policy()
            == codewhale_config::provider::WirePolicy::ModelAware;
        // Preserve the exact binding for model-aware providers too. Looking
        // up this same model in the offline catalog would discard protocol
        // and limit facts already admitted from an exact provider catalog.
        if request.model == self.default_model
            || (!model_aware && request.model.trim() == self.default_model)
        {
            return Ok((request, self.route_limits));
        }

        let candidate = match self.resolve_model_route(&request.model) {
            Ok(candidate) => candidate,
            Err(error) if model_aware || self.api_provider == ProviderKind::OpencodeGo => {
                return Err(error);
            }
            Err(_) => {
                // A fixed-protocol gateway may legitimately accept an id that
                // is newer than our offline catalog. Preserve the caller's
                // model, but fail closed on limits instead of reusing the
                // bound default model's envelope.
                return Ok((request, None));
            }
        };
        if candidate.protocol() != self.wire_format {
            bail!(
                "{} model {:?} uses {:?}, but this client is bound to {:?}; resolve a new model route before sending",
                self.api_provider.provider().display_name(),
                request.model,
                candidate.protocol(),
                self.wire_format
            );
        }
        request.model = candidate.wire_model_id().as_str().to_string();
        let route_limits = crate::route_budget::known_route_limits(candidate.limits());
        Ok((request, route_limits))
    }

    #[cfg(test)]
    fn build_http_client(
        api_key: &str,
        extra_headers: &HashMap<String, String>,
        api_provider: ProviderKind,
        base_url: &str,
    ) -> Result<reqwest::Client> {
        Self::http_client_builder_with_auth_mode(
            api_key,
            extra_headers,
            api_provider,
            base_url,
            provider_default_wire_format(api_provider),
            false,
            false,
            &Config::default(),
            AuthHeaderSurface::Inference,
        )?
        .build()
        .map_err(Into::into)
    }

    fn http_client_builder_with_auth_mode(
        api_key: &str,
        extra_headers: &HashMap<String, String>,
        api_provider: ProviderKind,
        base_url: &str,
        wire_format: WireFormat,
        auth_disabled: bool,
        force_http1: bool,
        config: &Config,
        surface: AuthHeaderSurface,
    ) -> Result<reqwest::ClientBuilder> {
        let headers = build_default_headers(
            api_key,
            extra_headers,
            api_provider,
            base_url,
            wire_format,
            auth_disabled,
            surface,
        )?;
        let mut builder = crate::tls::reqwest_client_builder()
            .default_headers(headers)
            .user_agent(client_user_agent(api_provider))
            .connect_timeout(config.connect_timeout())
            .tcp_keepalive(config.tcp_keepalive())
            .http2_keep_alive_interval(config.http2_keep_alive_interval())
            .http2_keep_alive_timeout(config.http2_keep_alive_timeout())
            .min_tls_version(reqwest::tls::Version::TLS_1_2);
        if api_provider == ProviderKind::OpenaiCodex {
            builder = builder.redirect(reqwest::redirect::Policy::none());
        }
        if force_http1 {
            builder = builder.http1_only();
        }
        if let Ok(cert_path) = std::env::var("SSL_CERT_FILE")
            && !cert_path.is_empty()
        {
            builder = add_extra_root_certs(builder, &cert_path);
        }
        Ok(builder)
    }

    /// HTTP/1.1 client for automatic stream-header fallback.
    #[must_use]
    pub(crate) fn http1_fallback_client(&self) -> &reqwest::Client {
        &self.http1_client
    }

    #[cfg(test)]
    fn default_headers(
        api_key: &str,
        extra_headers: &HashMap<String, String>,
    ) -> Result<HeaderMap> {
        build_default_headers(
            api_key,
            extra_headers,
            ProviderKind::Deepseek,
            crate::config::DEFAULT_DEEPSEEK_BASE_URL,
            WireFormat::ChatCompletions,
            false,
            AuthHeaderSurface::Inference,
        )
    }

    #[cfg(test)]
    fn default_headers_for_provider(
        api_key: &str,
        extra_headers: &HashMap<String, String>,
        api_provider: ProviderKind,
        base_url: &str,
    ) -> Result<HeaderMap> {
        build_default_headers(
            api_key,
            extra_headers,
            api_provider,
            base_url,
            provider_default_wire_format(api_provider),
            false,
            AuthHeaderSurface::Inference,
        )
    }

    #[cfg(test)]
    fn default_headers_for_provider_with_auth_disabled(
        api_key: &str,
        extra_headers: &HashMap<String, String>,
        api_provider: ProviderKind,
        base_url: &str,
    ) -> Result<HeaderMap> {
        build_default_headers(
            api_key,
            extra_headers,
            api_provider,
            base_url,
            provider_default_wire_format(api_provider),
            true,
            AuthHeaderSurface::Inference,
        )
    }
}

/// Which request surface a client's default auth header is built for.
///
/// Anthropic's `x-api-key` convention belongs to Anthropic's own service, not
/// to the Messages wire format by itself. A custom table speaks Messages
/// because its host does, but it is still reached through the
/// OpenAI-compatible config surface, and its listing follows the OpenAI
/// convention: a relay can answer `/v1/messages` on either header while
/// answering `/v1/models` only on `Authorization: Bearer`. Keeping the two
/// surfaces apart stops a listing from inheriting a header chosen for the
/// Messages endpoint.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthHeaderSurface {
    /// Chat, Messages and Responses requests: the wire dialect decides.
    Inference,
    /// The provider's own `/models` listing.
    Listing,
}

fn build_default_headers(
    api_key: &str,
    extra_headers: &HashMap<String, String>,
    api_provider: ProviderKind,
    base_url: &str,
    wire_format: WireFormat,
    auth_disabled: bool,
    surface: AuthHeaderSurface,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    let api_key = api_key.trim();
    let uses_anthropic_messages = wire_format == WireFormat::AnthropicMessages;
    if uses_anthropic_messages {
        // #3014: most Messages API routes authenticate with `x-api-key`.
        // OpenModel also supports Bearer auth for Messages, and its `/models`
        // endpoint requires it, so the header chooser below keeps OpenModel on
        // Bearer while still pinning the Anthropic wire contract here. The
        // Codewhale API is the same shape: its Anthropic passthrough
        // authenticates the account key with `Authorization: Bearer` on every
        // protocol and does not accept `x-api-key`.
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
    }
    // A custom host's listing never inherits the Messages choice. Its
    // `kind = "openai-compatible"` surface means Bearer, and a relay that
    // accepts Messages on either header may still reject `x-api-key` on
    // `/v1/models` with `401 missing_api_key`.
    let listing_on_custom_host =
        surface == AuthHeaderSurface::Listing && api_provider == ProviderKind::Custom;
    let auth_header_name = if auth_disabled {
        None
    } else if !api_key.is_empty()
        && uses_anthropic_messages
        && !listing_on_custom_host
        && !matches!(
            api_provider,
            ProviderKind::Openmodel | ProviderKind::Codewhale
        )
    {
        Some(HeaderName::from_static("x-api-key"))
    } else if !api_key.is_empty()
        && api_provider == ProviderKind::XiaomiMimo
        && (xiaomi_mimo_base_url_uses_token_plan(base_url)
            || xiaomi_mimo_api_key_uses_token_plan(api_key))
    {
        Some(HeaderName::from_static("api-key"))
    } else if !api_key.is_empty() {
        Some(AUTHORIZATION)
    } else {
        None
    };
    if let Some(header_name) = auth_header_name.as_ref() {
        let header_value = if *header_name == AUTHORIZATION {
            HeaderValue::from_str(&format!("Bearer {api_key}"))?
        } else {
            HeaderValue::from_str(api_key)?
        };
        headers.insert(header_name.clone(), header_value);
    }
    // OpenRouter app attribution: these two headers are how apps appear on
    // openrouter.ai's app rankings — there is no manual submission. They
    // identify the app, never the user, and a user-configured header of the
    // same name below still wins (the extra-header loop overwrites).
    if api_provider == ProviderKind::Openrouter {
        // `HTTP-Referer` is the only required header: it is the app's unique
        // identifier, and without it OpenRouter creates no app page at all.
        headers.insert(
            HeaderName::from_static("http-referer"),
            HeaderValue::from_static("https://codewhale.net"),
        );
        // `X-OpenRouter-Title` is the current display-name header. `X-Title`
        // is only kept for backwards compatibility — OpenRouter still honours
        // it, and a base-URL override can point this provider at an
        // OpenRouter-compatible gateway that knows the old name and not the
        // new one. Two bytes of redundancy is cheaper than losing attribution.
        //
        // Both names carry one display title. A user who configures either
        // one (a fork, say, setting only the legacy `X-Title`) must control
        // the name on both, or the default left on the other header would
        // silently override theirs on OpenRouter.
        let user_title = |wanted: &str| {
            extra_headers.iter().find_map(|(name, value)| {
                let value = value.trim();
                (name.trim().eq_ignore_ascii_case(wanted) && !value.is_empty()).then_some(value)
            })
        };
        let title = HeaderValue::from_str(
            user_title("x-openrouter-title")
                .or_else(|| user_title("x-title"))
                .unwrap_or("Codewhale"),
        )?;
        headers.insert(HeaderName::from_static("x-openrouter-title"), title.clone());
        headers.insert(HeaderName::from_static("x-title"), title);
        // Marketplace categories, at most two per request. These place the app
        // under Coding → CLI Agents and Productivity → Personal Agents on
        // openrouter.ai/apps, which is how the rankings page groups entries.
        // Unrecognised values are dropped silently, so these must stay exactly
        // as OpenRouter spells them.
        headers.insert(
            HeaderName::from_static("x-openrouter-categories"),
            HeaderValue::from_static("cli-agent,personal-agent"),
        );
    }
    // OpenCode Go / OpenCode Zen gateways (https://opencode.ai/docs/go/)
    // ask clients to send a stable `x-opencode-session` header so the
    // service can optimize prompt caching and attribute traffic to a
    // conversation. Generate one UUID v4 per process so every request a
    // session makes to the gateway shares a single ID ("one stable ID per
    // conversation"). A user-configured header of the same name wins: the
    // extra-header loop below overwrites this value.
    if matches!(
        api_provider,
        ProviderKind::OpencodeGo | ProviderKind::OpencodeZen
    ) {
        static OPENCODE_SESSION: OnceLock<String> = OnceLock::new();
        let session = OPENCODE_SESSION.get_or_init(|| uuid::Uuid::new_v4().to_string());
        headers.insert(
            HeaderName::from_static("x-opencode-session"),
            HeaderValue::from_str(session)?,
        );
    }
    for (name, value) in extra_headers {
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        if auth_disabled && is_upstream_auth_header(name) {
            continue;
        }
        let header_name = HeaderName::from_bytes(name.as_bytes())?;
        if header_name == AUTHORIZATION
            || header_name == CONTENT_TYPE
            || auth_header_name.as_ref() == Some(&header_name)
            || (auth_header_name.is_some() && is_auth_dialect_header(&header_name))
        {
            continue;
        }
        headers.insert(header_name, HeaderValue::from_str(value)?);
    }
    Ok(headers)
}

fn is_auth_dialect_header(header_name: &HeaderName) -> bool {
    header_name == AUTHORIZATION
        || header_name == HeaderName::from_static("api-key")
        || header_name == HeaderName::from_static("x-api-key")
}

fn provider_default_wire_format(provider: ProviderKind) -> WireFormat {
    provider
        .provider()
        .wire_policy()
        .fixed()
        .unwrap_or_else(|| {
            if provider == ProviderKind::OpencodeZen {
                WireFormat::Responses
            } else {
                WireFormat::ChatCompletions
            }
        })
}

/// Resolve the wire dialect for a dual-protocol vendor.
///
/// Power-user toggle: `providers.<id>.wire = "openai" | "anthropic" | "responses"`.
/// Legacy dialect kinds (`*Anthropic`) still force Messages. Custom providers
/// honor `wire = "responses" | "anthropic" | "chat"` per-config (see
/// `crates/config/src/provider.rs:Custom`). Everyone else keeps the descriptor's
/// fixed policy (or Chat Completions).
fn provider_wire_format_for_config(
    identity: &ProviderIdentity,
    config: Option<&crate::config::Config>,
) -> WireFormat {
    let api_provider = identity.provider;
    let wire = config.and_then(|cfg| cfg.provider_wire_dialect(identity));
    let prefers_anthropic = matches!(
        api_provider,
        ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlanAnthropic
    ) || wire_config_prefers_anthropic(wire);

    if prefers_anthropic
        && matches!(
            api_provider,
            ProviderKind::Deepseek
                | ProviderKind::Minimax
                | ProviderKind::ModelstudioTokenPlan
                | ProviderKind::DeepseekAnthropic
                | ProviderKind::MinimaxAnthropic
                | ProviderKind::ModelstudioTokenPlanAnthropic
                | ProviderKind::ModelstudioCodingPlan
                | ProviderKind::ModelstudioCodingPlanAnthropic
        )
    {
        return WireFormat::AnthropicMessages;
    }

    // Custom providers honor `wire = "anthropic"` / `wire = "responses"` explicitly.
    // The static `Custom::wire_policy()` remains `Chat` as a safe default; the
    // per-config override lives here (and in `provider_capability`) so existing
    // `[providers.<name>]` tables gain the three-way switch without changing the
    // provider registry trait. Supported aliases:
    //   anthropic: "anthropic" | "messages" | "claude" | "anthropic-messages" | ...
    //   responses: "responses" | "responses-api" | "openai-responses" | "openai_responses" | ...
    if api_provider == ProviderKind::Custom {
        if wire_config_prefers_anthropic(wire) {
            return WireFormat::AnthropicMessages;
        }
        if wire_config_prefers_responses(wire) {
            return WireFormat::Responses;
        }
    }

    provider_default_wire_format(api_provider)
}

fn wire_config_prefers_anthropic(wire: Option<&str>) -> bool {
    codewhale_config::provider::wire_dialect_prefers_anthropic(wire)
}

fn wire_config_prefers_responses(wire: Option<&str>) -> bool {
    codewhale_config::provider::wire_dialect_prefers_responses(wire)
}

fn api_provider_skips_models_probe(api_provider: ProviderKind) -> bool {
    // Concentrate's `GET /v1/models` is explicitly unauthenticated
    // (docs/PROVIDERS.md): a 2xx proves nothing about the key, so guided
    // setup must not count it as verification — the key is observed on the
    // first authenticated call instead.
    matches!(
        api_provider,
        ProviderKind::DeepseekAnthropic | ProviderKind::Concentrate
    )
}

#[must_use]
pub(crate) fn provider_api_key_verification_is_observed(api_provider: ProviderKind) -> bool {
    !api_provider_skips_models_probe(api_provider)
}

/// Verify a provider API key by hitting the `/models` endpoint
/// (#3875). Builds a minimal HTTP client with the canonical auth
/// headers for `provider`, issues a single GET, and returns
/// `Ok(roster)` on a 2xx response or `Err(reason)` on any failure.
///
/// `roster` is the listing that 2xx already carried, projected by the same
/// rules as a catalog refresh and scoped to `provider`'s own id: the caller
/// rescopes it to the exact route identity and publishes it, so guided setup
/// offers the models this key can call today rather than bundled or
/// Models.dev rows the provider may have retired. It is `None` when the body
/// is not one complete, valid roster (a paginated first page, malformed or
/// empty JSON); a valid 2xx still verifies the key, and the existing rows stay.
///
/// This is intentionally a one-shot call — no retry, no rate-limit
/// wait — so a bad key is surfaced immediately.
pub async fn verify_provider_api_key(
    provider: ProviderKind,
    api_key: &str,
    base_url: &str,
) -> Result<Option<ProviderCatalogDelta>, String> {
    if api_provider_skips_models_probe(provider) {
        // Providers without a /models endpoint can't be verified this
        // way; accept the key optimistically (same as health_check).
        return Ok(None);
    }
    let headers = build_default_headers(
        api_key,
        &Default::default(),
        provider,
        base_url,
        provider_default_wire_format(provider),
        false,
        AuthHeaderSurface::Listing,
    )
    .map_err(|err| format!("failed to build auth headers: {err:#}"))?;
    let client = crate::tls::reqwest_client_builder()
        .default_headers(headers)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!(
            "Mozilla/5.0 (compatible; codewhale/",
            env!("CARGO_PKG_VERSION"),
            "; +https://github.com/codewhale-hq/CodeWhale)"
        ))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|err| format!("failed to build HTTP client: {err:#}"))?;
    let url = api_url(base_url, "models");
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|err| format!("request failed: {}", err.without_url()))?;
    let status = response.status();
    if status.is_success() {
        // A valid 2xx verifies the key even when the body is not a usable
        // roster; failure-preserving catalog semantics then keep the
        // existing rows.
        let body = bounded_provider_catalog_text(response, PROVIDER_CATALOG_MAX_RESPONSE_BYTES)
            .await
            .unwrap_or_default();
        let complete =
            serde_json::from_str::<ModelsPage<'_>>(&body).is_ok_and(|page| !page.has_more);
        let secrets = [api_key.trim().to_string()];
        Ok(complete
            .then(|| {
                catalog_delta_from_models_body(
                    provider,
                    provider.as_str().to_string(),
                    base_url,
                    &body,
                    &secrets,
                )
                .ok()
            })
            .flatten())
    } else {
        let body = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
        let body = if api_key.trim().is_empty() {
            body
        } else {
            redact_model_bound_text(&body, &[api_key.trim().to_string()])
        };
        let summary = if body.chars().count() > 200 {
            format!("{}...", body.chars().take(200).collect::<String>())
        } else {
            body
        };
        Err(format!("HTTP {status}: {summary}"))
    }
}

fn translation_system_prompt(target_language: &str) -> String {
    format!(
        "You are a professional translator. Your ONLY task is to translate text to {target_language}. \
         Rules:\n\
         1. Output ONLY the translation, nothing else — no explanations, no notes, no quotes.\n\
         2. Preserve all code blocks (```...```), URLs, file paths, command names, \
         and technical terms like API names, function names, and library names untranslated.\n\
         3. Keep Markdown formatting (headings, lists, bold, italics, links) intact.\n\
         4. Translate all natural-language prose naturally and professionally.\n\
         5. Do NOT add any prefix, suffix, or commentary.\n\
         6. If the input is already in {target_language} or contains no prose to translate, \
         return it as-is."
    )
}

fn translation_message_request(
    text: &str,
    model: String,
    target_language: &str,
    max_tokens: u32,
) -> MessageRequest {
    MessageRequest {
        model,
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }],
        max_tokens,
        system: Some(SystemPrompt::Text(translation_system_prompt(
            target_language,
        ))),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: Some("off".to_string()),
        stream: Some(false),
        temperature: None,
        top_p: None,
    }
}

fn translation_text_from_response(response: &MessageResponse) -> Result<String> {
    let translated = response
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
        .trim()
        .to_string();
    if translated.is_empty() {
        bail!("translate: Anthropic Messages response did not contain text content");
    }
    Ok(translated)
}

fn xiaomi_mimo_base_url_uses_token_plan(base_url: &str) -> bool {
    let normalized = base_url.trim().to_ascii_lowercase();
    let without_scheme = normalized
        .strip_prefix("https://")
        .or_else(|| normalized.strip_prefix("http://"))
        .unwrap_or(&normalized);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = host.split(':').next().unwrap_or(host);
    host.starts_with("token-plan-") && host.ends_with(".xiaomimimo.com")
}

fn xiaomi_mimo_api_key_uses_token_plan(api_key: &str) -> bool {
    api_key.trim_start().starts_with("tp-")
}

impl CodewhaleClient {
    /// Returns the API base URL used by this client.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Prepare — but do not send — the exact outbound request for `request`.
    ///
    /// This is *the* outbound seam (#1004). Production dispatch
    /// (`create_message`, `create_message_stream`) and `/preview-request` both
    /// call it, so a preview cannot describe a request different from the one
    /// a turn would send.
    ///
    /// It runs, in production order:
    ///
    /// 1. tool-history repair and model-bound secret redaction
    ///    ([`Self::prepare_model_bound_request`]);
    /// 2. protocol binding and route model re-resolution
    ///    ([`Self::bind_request_to_protocol`]);
    /// 3. the dialect's own body builder — Chat Completions, Anthropic
    ///    Messages, or OpenAI Responses — including every provider-specific
    ///    sanitizer and reasoning shaper;
    /// 4. exact endpoint resolution for that dialect and route shape.
    ///
    /// It performs no I/O and mutates no client state.
    pub(crate) fn prepare_outbound_request(
        &self,
        request: MessageRequest,
        stream: bool,
    ) -> Result<PreparedOutboundRequest> {
        // Step 0: refuse role/dialect pairs this wire cannot represent, before
        // any dialect builds a body. Doing it here rather than inside each
        // adapter is what stops an unrepresentable role from being discovered
        // as an opaque provider 400 (Anthropic) or from vanishing silently
        // (the OpenAI-shaped dialects) depending on which adapter ran.
        let outbound_dialect = WireDialect::from_wire_format(self.wire_format);
        role_placement::reject_unsupported_roles(&request.messages, outbound_dialect)?;
        let clamp_output_cap = |mut request: MessageRequest, route_limits: Option<RouteLimits>| {
            let route_cap = self.effective_max_output_tokens_with_limits(
                &request.model,
                route_limits,
                request.reasoning_effort.as_deref(),
            );
            if request.max_tokens > route_cap {
                tracing::debug!(
                    requested_max_tokens = request.max_tokens,
                    route_max_tokens = route_cap,
                    model = %request.model,
                    "clamped outbound max_tokens to the resolved route envelope"
                );
                request.max_tokens = route_cap;
            }
            request
        };
        let (request, request_route_limits) =
            self.bind_request_to_protocol(self.prepare_model_bound_request(request))?;
        let mut request = clamp_output_cap(request, request_route_limits);
        request.max_tokens = self.bounded_output_tokens(
            &self.wire_model_for_route(&request.model),
            request_route_limits,
            request.max_tokens,
            crate::compaction::estimate_request_input_tokens(
                &request.messages,
                request.system.as_ref(),
                request.tools.as_deref(),
            ),
        )?;
        let facts = if request.model == self.default_model {
            self.route_facts.clone()
        } else {
            self.catalog_facts_for_model(&request.model)
        }
        .filter(|facts| facts.wire_model_id == request.model);
        let mut published_wire_effort = None;
        if let Some(facts) = facts.as_deref() {
            if facts.temperature == Some(false) {
                request.temperature = None;
                request.top_p = None;
            }
            if facts.tool_call == Some(false)
                && request
                    .tools
                    .as_ref()
                    .is_some_and(|tools| !tools.is_empty())
            {
                anyhow::bail!("The selected provider offering does not support tool calls");
            }
            if codewhale_config::models_dev::image_input_support(facts.modalities.as_ref())
                == codewhale_config::route::CapabilityState::Unsupported
                && request
                    .messages
                    .iter()
                    .flat_map(|message| &message.content)
                    .any(|block| matches!(block, ContentBlock::ImageUrl { .. }))
            {
                anyhow::bail!("The selected provider offering does not support image input");
            }
            if facts.reasoning == Some(false) {
                request.reasoning_effort = None;
                request.thinking = None;
            } else {
                for option in &facts.reasoning_options {
                    if option.get("type").and_then(Value::as_str) != Some("effort") {
                        continue;
                    }
                    if request
                        .reasoning_effort
                        .as_deref()
                        .is_some_and(|effort| matches!(effort, "auto" | "default"))
                    {
                        request.reasoning_effort = option
                            .get("default")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                    }
                    let wire_effort = if matches!(self.wire_format, WireFormat::ChatCompletions) {
                        request.reasoning_effort.as_deref().and_then(|effort| {
                            let provider = self.chat_shape_provider(&request.model);
                            let model = crate::config::wire_model_for_provider_route(
                                provider,
                                &self.base_url,
                                &request.model,
                            );
                            let mut body = json!({ "model": model });
                            chat::apply_route_reasoning_controls(
                                &mut body,
                                provider,
                                &self.base_url,
                                &model,
                                Some(effort),
                            );
                            body.get("reasoning_effort")
                                .or_else(|| body.pointer("/reasoning/effort"))
                                .and_then(Value::as_str)
                                .map(str::to_string)
                        })
                    } else {
                        request.reasoning_effort.clone()
                    };
                    let values = option.get("values").and_then(Value::as_array);
                    let published = |effort: &str| {
                        values.is_some_and(|values| {
                            values.iter().any(|value| value.as_str() == Some(effort))
                        })
                    };
                    if matches!(self.wire_format, WireFormat::ChatCompletions)
                        && let Some(wire) = wire_effort.as_deref()
                        && let Some(requested) = request.reasoning_effort.as_deref()
                        && !published(wire)
                        && published(requested)
                    {
                        published_wire_effort = Some(requested.to_string());
                        continue;
                    }
                    if let Some(effort) = wire_effort.as_deref()
                        && let Some(values) = values
                        && !published(effort)
                        && !(matches!(effort, "off" | "disabled" | "false")
                            && (values.iter().any(|value| value.as_str() == Some("none"))
                                || facts.reasoning_options.iter().any(|option| {
                                    option.get("type").and_then(Value::as_str) == Some("toggle")
                                })))
                    {
                        let mut offered = Vec::new();
                        for label in values
                            .iter()
                            .filter_map(Value::as_str)
                            .filter_map(
                                crate::reasoning_preference::ReasoningEffort::from_catalog_token,
                            )
                            .map(crate::reasoning_preference::ReasoningEffort::as_setting)
                        {
                            if !offered.contains(&label) {
                                offered.push(label);
                            }
                        }
                        anyhow::bail!(
                            "The selected provider offering does not support the resolved reasoning effort `{}`; it publishes: {}. Choose a published value or turn reasoning off",
                            request.reasoning_effort.as_deref().unwrap_or(effort),
                            if offered.is_empty() {
                                "no recognized effort values".to_string()
                            } else {
                                offered.join(", ")
                            }
                        );
                    }
                }
            }
        }
        let declared_wire_model = self.declared_wire_model(&request.model).map(str::to_string);
        if self.is_local_ds4_model(&request.model)
            && let Some(tools) = request.tools.as_mut()
        {
            for tool in tools {
                tool.strict = None;
            }
        }
        let requested_effort = request.reasoning_effort.clone();
        // Same value computed for the seam above.
        let dialect = outbound_dialect;
        // `stream` is the caller's entry point, not a wire fact: each dialect
        // decides for itself what the body's `stream` field says.
        let entrypoint = CallerStreamMode::from_stream_flag(stream);

        match self.wire_format {
            WireFormat::ChatCompletions => {
                let chat_shape_provider = self.chat_shape_provider(&request.model);
                let mut wire = chat::build_chat_wire_body(
                    &request,
                    chat_shape_provider,
                    &self.base_url,
                    stream,
                    request_route_limits,
                )?;
                if let Some(effort) = published_wire_effort.as_deref() {
                    if wire.body.get("reasoning_effort").is_some() {
                        wire.body["reasoning_effort"] = json!(effort);
                    } else if let Some(slot) = wire.body.pointer_mut("/reasoning/effort") {
                        *slot = json!(effort);
                    }
                }
                if let Some(model) = &declared_wire_model {
                    wire.model.clone_from(model);
                    wire.body["model"] = json!(model);
                }
                self.apply_provider_routing(&mut wire.body);
                let url = chat_completions_url(
                    self.chat_transport_base_url(),
                    &self.base_url,
                    self.api_provider,
                    self.path_suffix.as_deref(),
                    &wire.body,
                );
                let shape = prepared::chat_route_shape(
                    self.api_provider,
                    &self.base_url,
                    &wire.model,
                    &url,
                );
                Ok(PreparedOutboundRequest::new(
                    dialect,
                    self.endpoint_identity(url, shape),
                    wire.model,
                    wire.body,
                    requested_effort,
                    wire.replay_input_tokens,
                    entrypoint,
                )
                .with_omitted_tool_names(wire.omitted_tool_names))
            }
            WireFormat::AnthropicMessages => {
                let mut body = self.build_anthropic_body(&request, stream);
                if let Some(model) = &declared_wire_model {
                    body["model"] = json!(model);
                }
                let url = anthropic::anthropic_messages_url(&self.base_url);
                let shape = if self.api_provider == ProviderKind::OpencodeZen {
                    RouteShape::OpencodeZen
                } else if self.api_provider == ProviderKind::Custom {
                    RouteShape::CustomCompatible
                } else {
                    RouteShape::Standard
                };
                let wire_model = body
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(request.model.as_str())
                    .to_string();
                Ok(PreparedOutboundRequest::new(
                    dialect,
                    self.endpoint_identity(url, shape),
                    wire_model,
                    body,
                    requested_effort,
                    None,
                    entrypoint,
                ))
            }
            WireFormat::Responses => {
                let mut body = responses::build_responses_body_for_provider(
                    &request,
                    self.api_provider,
                    self.chatgpt_reasoning_api.as_deref(),
                );
                if let Some(model) = &declared_wire_model {
                    body["model"] = json!(model);
                }
                let is_codex = self.api_provider == ProviderKind::OpenaiCodex;
                let url = responses_api_url(&self.base_url, self.api_provider);
                let shape = if is_codex {
                    RouteShape::CodexResponses
                } else if self.api_provider == ProviderKind::OpencodeZen {
                    RouteShape::OpencodeZen
                } else if self.api_provider == ProviderKind::Custom {
                    RouteShape::CustomCompatible
                } else {
                    RouteShape::Standard
                };
                let wire_model = body
                    .get("model")
                    .and_then(Value::as_str)
                    .unwrap_or(request.model.as_str())
                    .to_string();
                Ok(PreparedOutboundRequest::new(
                    dialect,
                    self.endpoint_identity(url, shape),
                    wire_model,
                    body,
                    requested_effort,
                    None,
                    entrypoint,
                ))
            }
        }
    }

    pub(crate) fn apply_provider_routing(&self, body: &mut Value) {
        apply_openrouter_vendor(body, self.openrouter_vendor.as_deref());
    }

    pub(crate) fn openrouter_vendor(&self) -> Option<&str> {
        self.openrouter_vendor.as_deref()
    }

    /// Typed identity of the endpoint this client would POST to.
    ///
    /// `route_id` is left empty here on purpose: the client knows the provider
    /// and the URL, but only the caller's resolved turn plan knows whether the
    /// user reached this route through a named custom-provider entry. The
    /// engine attaches it with [`PreparedOutboundRequest::with_route_id`].
    fn endpoint_identity(&self, url: String, shape: RouteShape) -> EndpointIdentity {
        EndpointIdentity {
            provider_id: self.api_provider.as_str().to_string(),
            provider_display: self.api_provider.provider().display_name().to_string(),
            route_id: None,
            url,
            shape,
        }
    }

    /// Returns the active API provider for this client.
    pub fn api_provider(&self) -> ProviderKind {
        self.api_provider
    }

    pub(crate) fn admitted_provider_identity(&self) -> &ProviderIdentity {
        &self.admitted_identity
    }

    /// Route limits frozen with this client at resolution time.
    #[must_use]
    pub fn route_limits(&self) -> Option<RouteLimits> {
        self.route_limits
    }

    /// Output cap for a request dispatched by this exact client route.
    #[must_use]
    pub fn effective_max_output_tokens(&self, requested_model: &str) -> u32 {
        self.effective_max_output_tokens_with_limits(
            requested_model,
            self.route_limits_for_model(requested_model),
            None,
        )
    }

    fn route_limits_for_model(&self, requested_model: &str) -> Option<RouteLimits> {
        if requested_model.trim() == self.default_model {
            self.route_limits
        } else {
            self.resolve_model_route(requested_model)
                .ok()
                .and_then(|candidate| crate::route_budget::known_route_limits(candidate.limits()))
        }
    }

    fn bounded_output_tokens(
        &self,
        model: &str,
        route_limits: Option<RouteLimits>,
        max_tokens: u32,
        input_tokens: usize,
    ) -> Result<u32> {
        crate::route_budget::request_output_cap_for_input(
            self.api_provider,
            model,
            route_limits,
            max_tokens,
            u64::try_from(input_tokens).unwrap_or(u64::MAX),
        )
        .ok_or_else(|| {
            crate::llm_client::LlmError::ContextLengthError(format!(
                "about {input_tokens} input tokens leave no output room in this route's context window"
            ))
            .into()
        })
    }

    #[must_use]
    fn effective_max_output_tokens_with_limits(
        &self,
        requested_model: &str,
        route_limits: Option<RouteLimits>,
        reasoning_effort: Option<&str>,
    ) -> u32 {
        let wire_model = self.wire_model_for_route(requested_model);
        crate::route_budget::effective_max_output_tokens_for_turn(
            self.api_provider,
            &wire_model,
            route_limits,
            None,
            reasoning_effort,
        )
    }

    /// Secret-free receipt for the exact base endpoint and credential
    /// generation this client was constructed with.
    ///
    /// This is the only way the API key leaves `client.rs`, and it leaves as a
    /// one-way digest. Minting the receipt here — rather than re-reading config
    /// at some later lifecycle point — is what makes it immutable proof of the
    /// route that was actually installed for the turn.
    #[must_use]
    pub fn turn_route_receipt(&self) -> crate::route_receipt::TurnRouteReceipt {
        let credential =
            self.claude_oauth_config
                .as_ref()
                .map_or(self.api_key.as_str(), |config| {
                    config
                        .provider_config_for(&self.admitted_identity)
                        .and_then(|entry| entry.oauth_credential_generation.as_deref())
                        .unwrap_or_default()
                });
        crate::route_receipt::TurnRouteReceipt::from_admitted(
            &self.admitted_identity,
            &self.default_model,
            &self.base_url,
            credential,
        )
        .with_openrouter_vendor(self.openrouter_vendor.as_deref())
    }

    /// The operator-declared `[[custom_models]]` rate for `model` on this
    /// client's exact endpoint, frozen at `dispatched_at`. Every dispatch
    /// boundary (background envelopes and main interactive turns alike) asks
    /// this before the provider lake, so a declared rate is honored the same
    /// way on each (#6690).
    #[must_use]
    pub(crate) fn configured_pricing_quote_at(
        &self,
        provider: ProviderKind,
        provider_identity: &str,
        model: &str,
        dispatched_at: u64,
    ) -> Option<crate::provider_catalog_live::ProviderLivePricingQuote> {
        crate::provider_catalog_live::configured_dispatch_pricing_quote_at(
            &self.configured_models,
            provider,
            provider_identity,
            model,
            &self.base_url,
            dispatched_at,
        )
    }

    /// Capture the immutable, redacted route envelope at the caller's
    /// application-dispatch/admission time. This is not proof of network
    /// delivery or provider invoice-time pricing. The wire model is normalized
    /// exactly as the transport will normalize it; a provider-returned alias
    /// must never replace this billing identity later.
    #[must_use]
    pub fn effective_route_envelope(
        &self,
        requested_model: &str,
        dispatched_at: chrono::DateTime<chrono::Utc>,
    ) -> crate::cost_status::EffectiveRouteEnvelope {
        let model = self.wire_model_for_route(requested_model);
        let endpoint_fingerprint = crate::cost_status::endpoint_fingerprint(&self.base_url);
        let provider_live_pricing = u64::try_from(dispatched_at.timestamp())
            .ok()
            .and_then(|at| {
                crate::provider_catalog_live::declared_or_catalog_quote(
                    self.configured_pricing_quote_at(
                        self.api_provider,
                        self.admitted_identity.key.as_str(),
                        &model,
                        at,
                    ),
                    || {
                        crate::provider_catalog_live::fresh_dispatch_pricing_quote_at(
                            self.api_provider,
                            self.admitted_identity.key.as_str(),
                            &model,
                            &self.base_url,
                            at,
                        )
                    },
                )
            });
        crate::cost_status::EffectiveRouteEnvelope {
            openrouter_vendor: self.openrouter_vendor.clone(),
            provider: self.api_provider,
            provider_identity: self.admitted_identity.key.to_string(),
            model,
            billing_surface: self.billing_surface.clone(),
            endpoint_fingerprint,
            provider_live_pricing,
            billing_mode: self.billing_mode,
            dispatched_at,
        }
    }

    /// Resolved in-flight provider request cap, if one is active.
    #[must_use]
    pub fn provider_request_concurrency_limit(&self) -> Option<usize> {
        self.request_concurrency
            .as_ref()
            .map(ProviderConcurrencyLimiter::limit)
    }

    /// Number of currently active requests held by this client's shared
    /// provider request limiter.
    #[must_use]
    pub fn active_provider_requests(&self) -> usize {
        self.request_concurrency
            .as_ref()
            .map_or(0, ProviderConcurrencyLimiter::active)
    }

    async fn acquire_provider_request_permit(&self) -> Option<ProviderRequestPermit> {
        match self.request_concurrency.as_ref() {
            Some(limiter) => limiter.acquire().await,
            None => None,
        }
    }

    pub(crate) async fn acquire_remote_control_inference_permit(
        &self,
    ) -> Option<RemoteControlInferencePermit> {
        if !self.remote_control_inference_participant {
            return None;
        }
        Some(acquire_remote_control_inference_participant().await)
    }

    fn hold_provider_request_permit_for_stream(
        stream: crate::llm_client::StreamEventBox,
        permit: Option<ProviderRequestPermit>,
    ) -> crate::llm_client::StreamEventBox {
        Box::pin(async_stream::stream! {
            let _permit = permit;
            let mut stream = stream;
            while let Some(event) = stream.next().await {
                yield event;
            }
        })
    }

    fn prepend_tool_projection_warning(
        stream: crate::llm_client::StreamEventBox,
        provider: String,
        omitted_tool_names: Vec<String>,
        omitted_tool_count: usize,
    ) -> crate::llm_client::StreamEventBox {
        Box::pin(async_stream::stream! {
            yield Ok(codewhale_models::StreamEvent::ToolProjectionWarning {
                provider,
                omitted_tool_names,
                omitted_tool_count,
            });
            let mut stream = stream;
            while let Some(event) = stream.next().await {
                yield event;
            }
        })
    }

    fn hold_remote_control_inference_permit_for_stream(
        stream: crate::llm_client::StreamEventBox,
        permit: Option<RemoteControlInferencePermit>,
    ) -> crate::llm_client::StreamEventBox {
        Box::pin(async_stream::stream! {
            let _permit = permit;
            let mut stream = stream;
            while let Some(event) = stream.next().await {
                yield event;
            }
        })
    }

    /// Translate text to the requested target language using a focused
    /// non-streaming chat completion call on the supplied model.
    ///
    /// This is a lightweight translation service — no tool calls, no
    /// streaming, no conversation history. The dedicated translation agent
    /// receives the source text and returns only the translated result.
    pub(crate) async fn translate_with_usage(
        &self,
        text: &str,
        model: &str,
        target_language: &str,
    ) -> Result<TranslationProviderResponse> {
        // Freeze pricing before either the remote-control gate or the provider
        // permit. A later live-catalog refresh must not reprice this request.
        let route = self.effective_route_envelope(model, chrono::Utc::now());
        let _inference = self.acquire_remote_control_inference_permit().await;
        let _permit = self.acquire_provider_request_permit().await;
        let model = self.wire_model_for_route(model);
        let max_tokens = self.effective_max_output_tokens(&model);
        let probe = translation_message_request(text, model.clone(), target_language, max_tokens);
        let max_tokens = self.bounded_output_tokens(
            &model,
            self.route_limits_for_model(&model),
            max_tokens,
            crate::compaction::estimate_request_input_tokens(
                &probe.messages,
                probe.system.as_ref(),
                None,
            ),
        )?;
        if self.wire_format != WireFormat::ChatCompletions {
            // Non-Chat dialects reuse the prepared-request seam so translation
            // cannot drift from production shaping. Translation is still an
            // *auxiliary* call, not a primary agent turn: the Chat dialect
            // below builds its own small fixed body, and `/preview-request`
            // deliberately does not claim to describe either
            // (see `docs/PREVIEW_REQUEST.md`).
            let prepared = self.prepare_outbound_request(
                translation_message_request(text, model, target_language, max_tokens),
                false,
            )?;
            let response = match prepared.dialect {
                WireDialect::OpenAiResponses => self.handle_responses_message(&prepared).await?,
                WireDialect::AnthropicMessages => self.handle_anthropic_message(&prepared).await?,
                WireDialect::ChatCompletions => unreachable!(),
            };
            let usage = (response.usage != Usage::default()).then_some(response.usage.clone());
            let translated =
                if codewhale_models::is_incomplete_stop_reason(response.stop_reason.as_deref()) {
                    Err(anyhow::anyhow!(
                        "translate: provider response incomplete ({})",
                        codewhale_models::stop_reason_detail(response.stop_reason.as_deref())
                    ))
                } else {
                    translation_text_from_response(&response)
                };
            return Ok(TranslationProviderResponse {
                translated,
                route,
                usage,
            });
        }

        let url = api_url_with_suffix(
            self.chat_transport_base_url(),
            "chat/completions",
            self.path_suffix.as_deref(),
        );
        let mut body = serde_json::json!({
            "model": model,
            "messages": [
                {
                    "role": "system",
                    "content": translation_system_prompt(target_language)
                },
                {
                    "role": "user",
                    "content": text
                }
            ],
            "max_tokens": max_tokens,
            "stream": false
        });
        chat::apply_route_reasoning_controls(
            &mut body,
            self.api_provider,
            &self.base_url,
            &model,
            Some("off"),
        );

        self.apply_provider_routing(&mut body);
        let response = self.send_json_with_retry(&url, &body).await?;
        let status = response.status();
        if !status.is_success() {
            let raw_error_text = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            let error_text = sanitize_http_error_body(
                Some(self.api_provider.provider().display_name()),
                status.as_u16(),
                &raw_error_text,
            );
            anyhow::bail!("translate: HTTP {status}: {error_text}");
        }

        let value: serde_json::Value = response.json().await?;
        let usage_reported = value
            .get("usage")
            .and_then(Value::as_object)
            .is_some_and(|usage| {
                [
                    "input_tokens",
                    "prompt_tokens",
                    "output_tokens",
                    "completion_tokens",
                    "total_tokens",
                ]
                .iter()
                .any(|field| usage.contains_key(*field))
            });
        let usage = parse_usage(value.get("usage"));
        let stop_reason = value["choices"][0]["finish_reason"].as_str();
        let stop_reason = if codewhale_models::stop_contradicts_output_ceiling(
            stop_reason,
            usage_reported.then_some(usage.output_tokens),
            max_tokens,
        ) {
            Some(codewhale_models::OUTPUT_CEILING_STOP_REASON)
        } else {
            stop_reason
        };
        let usage = (usage_reported && usage != Usage::default()).then_some(usage);
        let translated = if codewhale_models::is_incomplete_stop_reason(stop_reason) {
            Err(anyhow::anyhow!(
                "translate: provider response incomplete ({})",
                codewhale_models::stop_reason_detail(stop_reason)
            ))
        } else {
            value["choices"][0]["message"]["content"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("translate: unexpected API response shape"))
                .and_then(|translated| {
                    let translated = translated.trim().to_string();
                    if translated.is_empty() {
                        bail!("translate: provider response did not contain text content");
                    }
                    Ok(translated)
                })
        };

        Ok(TranslationProviderResponse {
            translated,
            route,
            usage,
        })
    }

    /// Test adapter for asserting the translated text; production retains receipts.
    #[cfg(test)]
    async fn translate(&self, text: &str, model: &str, target_language: &str) -> Result<String> {
        self.translate_with_usage(text, model, target_language)
            .await?
            .translated
    }

    /// List every available model under the endpoint's verified pagination contract.
    pub async fn list_models(&self) -> Result<Vec<AvailableModel>> {
        let (body, deadline) = self
            .models_document(ModelsRequestMode::Interactive)
            .await
            .map_err(ModelsFetchError::into_interactive)?;
        let models = parse_models_response_for_provider(&body, self.api_provider)
            .map(|models| apply_provider_model_cutline(self.api_provider, models))
            .map_err(|_| {
                ModelsFetchError::Catalog(CatalogRefreshError::InvalidResponse).into_interactive()
            })?;
        if tokio::time::Instant::now() >= deadline {
            return Err(ModelsFetchError::Catalog(CatalogRefreshError::Network).into_interactive());
        }
        if self.api_provider == ProviderKind::OpenaiCodex
            && models.iter().any(|model| {
                self.model_bound_secret_values.iter().any(|secret| {
                    !secret.is_empty()
                        && (model.id.contains(secret.as_str())
                            || model
                                .display_name
                                .as_deref()
                                .is_some_and(|name| name.contains(secret.as_str())))
                })
            })
        {
            return Err(
                ModelsFetchError::Catalog(CatalogRefreshError::InvalidResponse).into_interactive(),
            );
        }
        Ok(models)
    }

    async fn models_document(
        &self,
        mode: ModelsRequestMode,
    ) -> Result<(String, tokio::time::Instant), ModelsFetchError> {
        let mut endpoint = reqwest::Url::parse(&api_url(&self.base_url, "models"))
            .map_err(|_| CatalogRefreshError::InvalidResponse)?;
        // OrcaRouter scopes `GET /v1/models` to one capability. Ask for the chat
        // roster so the model control is the real chat list rather than every
        // kind the account can reach; other providers take the unfiltered
        // listing. Non-text rows are dropped again by the chat endpoint filter.
        if self.api_provider == ProviderKind::Orcarouter {
            endpoint.query_pairs_mut().append_pair("capability", "chat");
        }
        // https://platform.claude.com/docs/en/api/models/list specifies after_id.
        // Go is unpaginated. A Messages generation dialect or a custom identity
        // resembling a built-in provider does not establish this list contract.
        let cursor_query = (self.api_provider == ProviderKind::Anthropic).then_some("after_id");
        collect_models_document(
            endpoint,
            cursor_query,
            MODELS_FETCH_LIMITS,
            |url| async move {
                let build = || {
                    self.models_http_client
                        .get(url.clone())
                        .timeout(NON_STREAMING_HTTP_TIMEOUT)
                };
                let response = match mode {
                    // #6173: the provider's own words, minus this client's
                    // secrets and this request's cursor. The endpoint is not
                    // established here — it is whatever the user typed during
                    // setup — so the body is guarded rather than trusted.
                    ModelsRequestMode::Interactive => {
                        let disclosure = ErrorBodyDisclosure::Guarded {
                            request_secrets: request_query_secret_values(&url),
                        };
                        // The pinned 30s per-attempt total survives: the retry
                        // loop's shared envelope is not allowed to overwrite a
                        // caller's own budget.
                        self.send_with_retry_total_error_body(
                            NON_STREAMING_HTTP_TIMEOUT,
                            build,
                            &disclosure,
                        )
                        .await
                        .map_err(ModelsFetchError::Interactive)?
                    }
                    ModelsRequestMode::Refresh => self
                        .authorize_plugin_request(build())
                        .await
                        .map_err(|_| CatalogRefreshError::Unauthorized)?
                        .send()
                        .await
                        .map_err(|_| CatalogRefreshError::Network)?,
                };
                if !response.status().is_success() {
                    return Err(ModelsFetchError::Catalog(
                        match response.status().as_u16() {
                            401 => CatalogRefreshError::Unauthorized,
                            403 => CatalogRefreshError::Forbidden,
                            404 => CatalogRefreshError::NotFound,
                            429 => CatalogRefreshError::RateLimited,
                            _ => CatalogRefreshError::Network,
                        },
                    ));
                }
                Ok(response)
            },
        )
        .await
    }

    /// The catalog provider id for this client (the `ProviderKind` slug, falling
    /// back to the `ProviderKind` slug for legacy variants without a kind). This
    /// is the id used as the cache scope and `CatalogOffering.provider`.
    fn catalog_provider_id(&self) -> String {
        self.admitted_identity.key.to_string()
    }

    /// Whether this route speaks Baseten's `/models` dialect.
    ///
    /// Schema recognition is by endpoint, never by table name: whatever the
    /// user called the `[providers.<name>]` table, the wire shape is a fact
    /// about the host (#6289). Catalog ownership still uses the exact
    /// configured identity, so a renamed Baseten table keeps its own
    /// partition.
    #[cfg(test)]
    fn catalog_endpoint_is_baseten(&self) -> bool {
        catalog_endpoint_is_baseten(self.api_provider, &self.base_url)
    }

    /// Fetch the provider's live `/models` listing as a secret-free
    /// [`ProviderCatalogDelta`] (#3385).
    ///
    /// Uses the same URL construction and auth client as [`Self::list_models`],
    /// but fetches pages without `send_with_retry` so a refresh
    /// failure stays typed and non-fatal — bundled / saved / static rows are
    /// untouched. The delta is scoped to the base-URL fingerprint and stamped
    /// with the fetch time; the API key authorizes the request but is **never**
    /// persisted into the delta or cache. Unknown live rows carry no canonical
    /// model, capabilities, or pricing, per the #3385 contract.
    pub async fn fetch_catalog_delta(&self) -> Result<ProviderCatalogDelta, CatalogRefreshError> {
        let (body, deadline) = self
            .models_document(ModelsRequestMode::Refresh)
            .await
            .map_err(ModelsFetchError::into_catalog)?;

        let delta = catalog_delta_from_models_body(
            self.api_provider,
            self.catalog_provider_id(),
            &self.base_url,
            &body,
            &self.model_bound_secret_values,
        )?;
        if tokio::time::Instant::now() >= deadline {
            return Err(CatalogRefreshError::Network);
        }
        Ok(delta)
    }

    /// Refresh `cache` for this client's provider + base URL, recording either a
    /// success or a typed failure (#3385). Returns the resulting status so the UI
    /// can surface a visible "fresh / failed(reason)" chip without inspecting the
    /// cache internals. A failed refresh preserves any previously cached rows.
    #[cfg(test)]
    pub async fn refresh_catalog_cache(
        &self,
        cache: &mut ProviderCatalogCache,
        ttl_secs: u64,
    ) -> CatalogStatus {
        match self.fetch_catalog_delta().await {
            Ok(delta) => {
                let provider = delta.provider.clone();
                let fingerprint = delta.base_url_fingerprint.clone();
                cache.record_success(delta, ttl_secs);
                publish_provider_lake_scope(cache, &provider, &fingerprint);
                CatalogStatus::Fresh
            }
            Err(reason) => {
                let provider = self.catalog_provider_id();
                let fingerprint = base_url_fingerprint(&self.base_url);
                cache.record_failure(&provider, &fingerprint, reason);
                publish_provider_lake_scope(cache, &provider, &fingerprint);
                CatalogStatus::Failed { reason }
            }
        }
    }

    /// Best-effort background refresh of the active provider's own `/v1/models`
    /// catalog, replacing that provider's exact lake partition (#3385).
    ///
    /// Unlike `models_dev_live::spawn_background_refresh` (which fetches the
    /// cross-provider Models.dev catalog), this calls the provider's own
    /// `/v1/models` endpoint and merges the results into the existing live
    /// snapshot via `provider_catalog_live`, preserving other providers while
    /// allowing this provider's successful roster to retire removed ids.
    ///
    /// Activated for model-list authorities that are not satisfied by the
    /// cross-provider Models.dev snapshot: OpenRouter, OrcaRouter, named live gateways,
    /// and Baseten's account-scoped endpoint (no static snapshot can serve a
    /// per-credential roster). Custom OpenAI-compatible hosts are included
    /// too: a private relay is not in the Models.dev snapshot, so without a
    /// probe its `/model` picker stays empty even though the chat route
    /// already talks to the same endpoint (#6289 widened).
    /// The refresh is non-fatal: on failure, persisted prior rows and static
    /// seeds remain available with a typed failed receipt.
    pub fn spawn_active_provider_catalog_refresh(config: &Config) {
        // Unit tests use explicit fixture refreshes; never probe a developer's provider.
        #[cfg(test)]
        let _ = config;
        #[cfg(not(test))]
        {
            let Ok(identity) = config.active_provider_identity() else {
                return;
            };
            let provider = identity.provider;
            // Custom hosts include Baseten (its `/models` dialect is detected
            // at fetch time) and every other custom host. A private route is
            // the only place its roster exists, and a failed probe stays
            // non-fatal.
            if !crate::provider_catalog_live::provider_owns_live_catalog(
                provider,
                &config.active_route_base_url(),
            ) {
                return;
            }

            // Invalidate older in-flight fetches before loading any reusable
            // scope. Baseten's ticket also clears account-scoped rows because the
            // same endpoint can expose a different workspace after a key change.
            let refresh_ticket = crate::provider_catalog_live::begin_refresh_for_identity(
                provider,
                identity.key.as_str(),
                &config.active_route_base_url(),
            );

            // Publish the exact persisted scope immediately so opening `/model`
            // never waits on the network and another endpoint's rows cannot leak
            // into this route. Account-scoped Baseten rows deliberately do not
            // reload from disk until the current credential proves them again.
            crate::provider_catalog_live::maybe_load_persisted_cache_for_config(config);

            let client = match CodewhaleClient::for_catalog_refresh(config) {
                Ok(client) => client,
                Err(err) => {
                    tracing::debug!(
                        target: "provider_catalog",
                        error = %err,
                        "skipping provider catalog refresh: client creation failed"
                    );
                    return;
                }
            };

            tokio::spawn(async move {
                match client.fetch_catalog_delta().await {
                    Ok(delta) => {
                        let count = delta.offerings.len();
                        if crate::provider_catalog_live::record_success_if_current(
                            &refresh_ticket,
                            delta,
                        )
                        .is_none()
                        {
                            tracing::debug!(
                                target: "provider_catalog",
                                "discarded provider catalog response superseded by a newer refresh"
                            );
                            return;
                        }
                        tracing::debug!(
                            target: "provider_catalog",
                            offering_count = count,
                            "provider catalog refresh merged {count} offerings into provider lake"
                        );
                    }
                    Err(err) => {
                        if crate::provider_catalog_live::record_failure_if_current(
                            &refresh_ticket,
                            &client.catalog_provider_id(),
                            &base_url_fingerprint(&client.base_url),
                            err,
                        )
                        .is_none()
                        {
                            tracing::debug!(
                                target: "provider_catalog",
                                "discarded provider catalog failure superseded by a newer refresh"
                            );
                            return;
                        }
                        tracing::debug!(
                            target: "provider_catalog",
                            error = ?err,
                            "provider catalog refresh failed; keeping existing rows"
                        );
                    }
                }
            });
        }
    }

    /// Best-effort refresh of every configured route that owns its own
    /// `/v1/models` roster, so opening `/model` finds each relay already probed
    /// instead of a route still waiting for a provider switch.
    ///
    /// [`Self::spawn_active_provider_catalog_refresh`] keeps its own faster path
    /// for the active route (it also publishes that route's persisted scope
    /// before the probe). This is the wider sweep behind it, for the relays a
    /// session is *not* currently on — the picker lists every configured route,
    /// so every one of them has to be able to answer, not just the active one.
    ///
    /// Each route is probed through the same `update_provider_catalog` the
    /// explicit `models --update` command uses, so the TTL, the typed receipt,
    /// the credential-change guard and the superseded-refresh ticket are shared
    /// rather than reimplemented. Failures stay non-fatal: a route that cannot
    /// answer keeps its persisted and configured rows.
    pub fn spawn_configured_provider_catalog_refresh(config: &Config) {
        #[cfg(test)]
        let _ = config;
        #[cfg(not(test))]
        {
            let active = config.active_provider_identity().ok();
            for identity in crate::provider_lake::configured_providers(config) {
                if active.as_ref() == Some(&identity) {
                    continue;
                }
                let base_url = config.base_url_for_route(&identity);
                if !crate::provider_catalog_live::provider_owns_live_catalog(
                    identity.provider,
                    &base_url,
                ) {
                    continue;
                }
                let route_config = config.clone();
                tokio::spawn(async move {
                    let _ = crate::provider_lake::update_provider_catalog(&route_config, &identity)
                        .await;
                });
            }
        }
    }

    /// Generate speech with Xiaomi MiMo TTS models.
    ///
    /// The spoken text is placed in an `assistant` message because Xiaomi
    /// MiMo's TTS chat-completions surface expects that shape. The optional
    /// `instruction` is a `user` message that controls style, voice design, or
    /// voice-clone performance and is not spoken verbatim.
    pub async fn synthesize_speech(
        &self,
        request: SpeechSynthesisRequest,
    ) -> Result<SpeechSynthesisResponse> {
        let _inference = self.acquire_remote_control_inference_permit().await;
        let _permit = self.acquire_provider_request_permit().await;
        if self.api_provider != crate::config::ProviderKind::XiaomiMimo {
            anyhow::bail!(
                "speech synthesis requires provider 'xiaomi-mimo' (current: {})",
                self.api_provider.as_str()
            );
        }

        let model = request.model.trim().to_string();
        if model.is_empty() {
            anyhow::bail!("Speech model cannot be empty");
        }
        let text = request.text.trim().to_string();
        if text.is_empty() {
            anyhow::bail!("Speech text cannot be empty");
        }

        let audio_format = normalize_audio_format(&request.audio_format);
        let model = self.wire_model_for_route(&model);
        let model_lower = model.to_ascii_lowercase();
        let instruction = request
            .instruction
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let voice = request
            .voice
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        if model_lower.contains("voicedesign") && instruction.is_none() {
            anyhow::bail!(
                "Model '{model}' requires a voice design prompt. Pass --voice-prompt or --instruction."
            );
        }
        if model_lower.contains("voiceclone") && voice.is_none() {
            anyhow::bail!(
                "Model '{model}' requires cloned voice data. Pass --clone-voice <mp3|wav> or --voice <data-uri>."
            );
        }

        let mut audio = json!({
            "format": audio_format.clone(),
        });
        if let Some(voice) = voice.as_deref() {
            audio["voice"] = json!(voice);
        }

        let body = build_speech_synthesis_body(&model, &text, instruction, audio);

        let url = api_url(&self.base_url, "chat/completions");
        let response = self.send_json_with_retry(&url, &body).await?;
        let status = response.status();
        if !status.is_success() {
            let raw_error_text = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            let error_text = sanitize_http_error_body(
                Some(self.api_provider.provider().display_name()),
                status.as_u16(),
                &raw_error_text,
            );
            anyhow::bail!("Speech synthesis failed: HTTP {status}: {error_text}");
        }

        let response_text = response
            .text()
            .await
            .context("Failed to read speech synthesis response body")?;
        let payload: Value = serde_json::from_str(&response_text)
            .context("Failed to parse speech synthesis response JSON")?;
        let (audio_bytes, transcript) = parse_speech_audio_response(&payload)?;

        Ok(SpeechSynthesisResponse {
            model,
            audio_format,
            audio_bytes,
            transcript,
            voice,
        })
    }

    async fn wait_for_rate_limit(&self) {
        let maybe_delay = {
            let mut limiter = self.rate_limiter.lock().await;
            limiter.delay_until_available(1.0)
        };
        if let Some(delay) = maybe_delay {
            tokio::time::sleep(delay).await;
        }
    }

    async fn mark_request_success(&self) {
        let mut health = self.connection_health.lock().await;
        if apply_request_success(&mut health, Instant::now()) {
            logging::info("Connection recovered");
        }
    }

    async fn mark_request_failure(&self, reason: &str) {
        let mut health = self.connection_health.lock().await;
        apply_request_failure(&mut health, Instant::now());
        logging::warn(format!(
            "Connection degraded (failures={}): {}",
            health.consecutive_failures, reason
        ));
    }

    async fn maybe_probe_recovery(&self) {
        let should_probe = {
            let mut health = self.connection_health.lock().await;
            mark_recovery_probe_if_due(&mut health, Instant::now())
        };
        if !should_probe {
            return;
        }
        if api_provider_skips_models_probe(self.api_provider) {
            self.mark_request_success().await;
            logging::info("Skipping /models recovery probe for provider without a models endpoint");
            return;
        }
        let health_url = api_url(&self.base_url, "models");
        let request = self
            .models_http_client
            .get(health_url)
            .timeout(NON_STREAMING_HTTP_TIMEOUT);
        let request = match self.authorize_plugin_request(request).await {
            Ok(request) => request,
            Err(_) => {
                self.mark_request_failure("probe authorization failed")
                    .await;
                return;
            }
        };
        let probe = request.send().await;
        match probe {
            Ok(resp) if resp.status().is_success() => {
                // Consume the response body so the connection can be returned to the pool.
                let _ = bounded_error_text(resp, ERROR_BODY_MAX_BYTES).await;
                self.mark_request_success().await;
                logging::info("Recovery probe succeeded");
            }
            Ok(resp) => {
                self.mark_request_failure(&format!("probe status={}", resp.status()))
                    .await;
            }
            Err(err) => {
                self.mark_request_failure(&format!("probe error={}", err.without_url()))
                    .await;
            }
        }
    }

    /// Apply `disclosure` to one provider error body.
    ///
    /// Redaction runs on the raw bytes, *before* `sanitize_http_error_body`
    /// truncates them, so a secret can never be split across the truncation
    /// boundary and survive as a fragment. The result is then checked against
    /// every value this client knows is secret; if one is still there the
    /// whole body is dropped, which is exactly the behaviour this path had
    /// before #6173. The fallback is the old contract, not a weaker one.
    ///
    /// Known limitation: this removes what the *client* knows is secret. A
    /// credential the user configured outside Codewhale — in a proxy, say —
    /// is not in that set and would pass through, the same limit
    /// `redact_model_bound_text` has.
    fn disclosed_http_error_body(
        &self,
        disclosure: &ErrorBodyDisclosure,
        status: u16,
        raw: &str,
    ) -> String {
        // Rotated opaque bearer values are not part of this client's frozen
        // redaction list. Do not disclose a dynamic OAuth endpoint's body.
        if self.claude_oauth_config.is_some() {
            return "Claude subscription request failed".into();
        }
        if self.plugin_provider.is_some() {
            return "plugin provider request failed".into();
        }
        let provider = Some(self.api_provider.provider().display_name());
        let ErrorBodyDisclosure::Guarded { request_secrets } = disclosure else {
            return sanitize_http_error_body(provider, status, raw);
        };
        let mut redacted = raw.to_string();
        for secret in self
            .catalog_error_secret_values
            .iter()
            .chain(request_secrets.iter())
        {
            redacted = redacted.replace(secret.as_str(), codewhale_config::persistence::REDACTED);
        }
        let message = sanitize_http_error_body(provider, status, &redacted);
        let leaked = self
            .catalog_error_secret_values
            .iter()
            .chain(request_secrets.iter())
            .any(|secret| message.contains(secret.as_str()));
        if leaked { String::new() } else { message }
    }

    pub(super) async fn send_with_retry<F>(&self, build: F) -> Result<reqwest::Response>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        self.send_with_retry_error_body(build, &ErrorBodyDisclosure::Full)
            .await
    }

    /// Model-list errors can echo opaque cursors or credentials. Keep status
    /// and Retry-After classification either way; `disclosure` decides how
    /// much of the body reaches retry logs, state updates and the user.
    async fn send_with_retry_error_body<F>(
        &self,
        build: F,
        disclosure: &ErrorBodyDisclosure,
    ) -> Result<reqwest::Response>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        if self.isolated_request_state {
            return self.send_with_isolated_retry(build, disclosure).await;
        }
        self.send_retry_loop(build, Some(non_streaming_request_envelope()), disclosure)
            .await
    }

    /// [`Self::send_with_retry_error_body`] with a caller-pinned per-attempt
    /// total (connect through body end). `list_models` pins its own 30s: the
    /// plain variant would otherwise stretch that pinned budget out to the
    /// shared envelope, because `.timeout()` on the builder is a pure
    /// overwrite.
    async fn send_with_retry_total_error_body<F>(
        &self,
        total: Duration,
        build: F,
        disclosure: &ErrorBodyDisclosure,
    ) -> Result<reqwest::Response>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        if self.isolated_request_state {
            return self.send_with_isolated_retry(build, disclosure).await;
        }
        self.send_retry_loop(build, Some(total), disclosure).await
    }

    /// The streaming-open twin of [`Self::send_with_retry`]: the same retry
    /// and rate-limit handling with no total deadline anywhere. reqwest's
    /// per-request timeout wraps the response *body*, so a total set on the
    /// open would ride along inside the returned body and hard-cut a live
    /// stream mid-generation. Stream opens stay bounded by the caller's
    /// `stream_open_timeout` around the open and per-chunk idle checks on
    /// the returned body instead.
    pub(super) async fn send_stream_open_with_retry<F>(&self, build: F) -> Result<reqwest::Response>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        if self.isolated_request_state {
            return self
                .send_with_isolated_retry(build, &ErrorBodyDisclosure::Full)
                .await;
        }
        self.send_retry_loop(build, None, &ErrorBodyDisclosure::Full)
            .await
    }

    async fn send_retry_loop<F>(
        &self,
        mut build: F,
        attempt_total: Option<Duration>,
        disclosure: &ErrorBodyDisclosure,
    ) -> Result<reqwest::Response>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        let retry_cfg: LlmRetryConfig = self.retry.clone().into();
        let pause_scope = self.rate_limit_scope();
        let callback_scope = pause_scope.clone();
        // Two bounded layers around a non-streaming completion
        // (`attempt_total` = `Some`): the per-attempt request total (connect
        // through body end) and an envelope around the whole retry loop (all
        // attempts + backoff + honored Retry-After). Streaming opens
        // (`None`) set no deadline at all: any total here would be
        // inherited by the returned body and truncate the stream, so those
        // calls keep only the caller's own open budget.
        let retry_future = with_retry(
            &retry_cfg,
            || {
                // Per-attempt total: unlike the loop envelope below,
                // reqwest's per-request timeout also covers the response
                // body, so a slow-drip body cannot outlive the budget.
                let request = match attempt_total {
                    Some(total) => build().timeout(total),
                    None => build(),
                };
                let pause_scope = pause_scope.as_str();
                async move {
                    // Sleep in bounded slices rather than the full remaining
                    // window: the pause is shared by every request to this
                    // route, so a concurrent `clear_rate_limit()` (or a
                    // shortened deadline) must release requests that are
                    // already waiting instead of stranding them for the whole
                    // original window.
                    while let Some(delay) = crate::retry_status::rate_limit_remaining(pause_scope) {
                        tokio::time::sleep(delay.min(RATE_LIMIT_PAUSE_RECHECK_INTERVAL)).await;
                    }
                    self.wait_for_rate_limit().await;
                    let request = self
                        .authorize_plugin_request(request)
                        .await
                        .map_err(|error| LlmError::Other(error.to_string()))?;
                    let response = request
                        .send()
                        .await
                        .map_err(|err| LlmError::from_reqwest(&err.without_url()))?;
                    let status = response.status();
                    if status.is_success() {
                        return Ok(response);
                    }
                    let header_retry_after = extract_retry_after(response.headers());
                    let raw = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
                    let body_retry_after = retry_delay_from_error_body(&raw);
                    let body = self.disclosed_http_error_body(disclosure, status.as_u16(), &raw);
                    Err(self.http_error_with_route_context(
                        status.as_u16(),
                        &body,
                        header_retry_after,
                        body_retry_after,
                    ))
                }
            },
            Some(Box::new(move |err, attempt, delay| {
                let (reason_label, human_reason) = retry_reason_label_and_human(err);
                logging::warn(format!(
                    "HTTP retry reason={} attempt={} delay={:.2}s",
                    reason_label,
                    attempt + 1,
                    delay.as_secs_f64(),
                ));
                if matches!(err, LlmError::RateLimited { .. }) {
                    crate::retry_status::note_rate_limit(&callback_scope, delay);
                }
                crate::retry_status::start(attempt + 1, delay, human_reason);
            })),
        );
        let request_result = if let Some(total) = attempt_total {
            // The loop envelope must dominate the per-attempt total it
            // wraps: a caller-pinned budget (list_models' 30s) may exceed
            // the shared envelope, and the envelope must never strangle
            // its own attempts.
            let loop_envelope = total.max(non_streaming_request_envelope());
            match tokio::time::timeout(loop_envelope, retry_future).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    let last = LlmError::Timeout(loop_envelope);
                    logging::warn(format!(
                        "non-streaming request envelope exceeded ({loop_envelope:?}); retry loop aborted"
                    ));
                    crate::retry_status::failed(last.to_string());
                    self.mark_request_failure("non-streaming request envelope exceeded")
                        .await;
                    self.maybe_probe_recovery().await;
                    return Err(anyhow::Error::new(last));
                }
            }
        } else {
            retry_future.await
        };

        match request_result {
            Ok(response) => {
                crate::retry_status::succeeded();
                self.mark_request_success().await;
                Ok(response)
            }
            Err(err) => {
                if let LlmError::RateLimited { retry_after, .. } = &err.last_error {
                    crate::retry_status::note_rate_limit(
                        &pause_scope,
                        retry_after
                            .unwrap_or_else(|| retry_cfg.delay_for_attempt(retry_cfg.max_retries)),
                    );
                }
                let last = err.last_error.to_string();
                if err.attempts > 1 {
                    crate::retry_status::failed(last.clone());
                } else {
                    crate::retry_status::clear();
                }
                self.mark_request_failure(&last).await;
                self.maybe_probe_recovery().await;
                // Keep the structured `LlmError` downcastable so failure
                // surfaces can classify auth/rate-limit/invalid-request
                // instead of reporting an opaque string (#3884).
                Err(anyhow::Error::new(err.last_error))
            }
        }
    }

    /// Key for this route's shared `Retry-After` pause: the configured route
    /// identity plus the host it reaches. A 429 from one provider pauses only
    /// requests that would hit the same limit, never another provider, a
    /// local runtime, or a sub-agent on a different route.
    pub(crate) fn rate_limit_scope(&self) -> String {
        let route = self.admitted_identity.key.as_str();
        let host = crate::llm_client::base_url_authority(&self.base_url)
            .unwrap_or_else(|| redact_url_for_display(&self.base_url));
        format!("{route}@{host}")
    }

    /// The same bounded transport retry policy without process-global retry
    /// banners, provider-wide pause cells, or shared connection-health writes.
    /// Used only by the Auto classifier during read-only request inspection.
    async fn send_with_isolated_retry<F>(
        &self,
        mut build: F,
        disclosure: &ErrorBodyDisclosure,
    ) -> Result<reqwest::Response>
    where
        F: FnMut() -> reqwest::RequestBuilder,
    {
        let retry_cfg: LlmRetryConfig = self.retry.clone().into();
        let request_result = crate::llm_client::observe_request_retries(
            None,
            with_retry(
                &retry_cfg,
                || {
                    let request = build();
                    async move {
                        self.wait_for_rate_limit().await;
                        let request = self
                            .authorize_plugin_request(request)
                            .await
                            .map_err(|error| LlmError::Other(error.to_string()))?;
                        let response = request
                            .send()
                            .await
                            .map_err(|err| LlmError::from_reqwest(&err.without_url()))?;
                        let status = response.status();
                        if status.is_success() {
                            return Ok(response);
                        }
                        let header_retry_after = extract_retry_after(response.headers());
                        let raw = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
                        let body_retry_after = retry_delay_from_error_body(&raw);
                        let body =
                            self.disclosed_http_error_body(disclosure, status.as_u16(), &raw);
                        Err(self.http_error_with_route_context(
                            status.as_u16(),
                            &body,
                            header_retry_after,
                            body_retry_after,
                        ))
                    }
                },
                Some(Box::new(|err, attempt, delay| {
                    let (reason_label, _) = retry_reason_label_and_human(err);
                    logging::warn(format!(
                        "Isolated HTTP retry reason={} attempt={} delay={:.2}s",
                        reason_label,
                        attempt + 1,
                        delay.as_secs_f64(),
                    ));
                })),
            ),
        )
        .await;

        request_result.map_err(|err| anyhow::Error::new(err.last_error))
    }

    pub(super) async fn send_json_with_retry(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response> {
        let request_body =
            serde_json::to_vec(body).context("Failed to serialize JSON request body")?;
        self.send_with_retry(|| {
            self.http_client
                .post(url)
                .header(CONTENT_TYPE, "application/json")
                .body(request_body.clone())
        })
        .await
    }
}

/// Record that a request was routed to `provider` and came back with `status`.
///
/// Called at every provider response site, **before** the error is built: an
/// `LlmError` carries the raw provider body verbatim, so the status class has
/// to be taken from the response itself.
///
/// The provider is recorded as a `ProviderKind` by value. Every accessor that
/// looks like the natural seam here — the persistence identity, the stream
/// meta's `provider_id`, the planned route's effective label — returns the
/// customer's own `[providers.<name>]` table key when the route is custom.
/// `ProviderKind::Custom` yields the literal `"custom"` and nothing else, and
/// no model id is sent for any provider.
pub(crate) fn record_provider_response(provider: crate::config::ProviderKind, status: u16) {
    let counters = codewhale_telemetry::session_counters();
    counters.record_provider(provider);
    if let Some(counter) = codewhale_telemetry::counters::http_status_counter(status) {
        counters.bump_error(counter);
    }
}

/// Translate the structured `LlmError` into both a categorical label
/// (for structured logs / metrics) and a short human reason string
/// (for the retry banner). Returning both from one match avoids the
/// double-classification we had before.
fn retry_reason_label_and_human(err: &LlmError) -> (&'static str, String) {
    // The variant, never the payload. Every `LlmError` variant carries the raw
    // provider HTTP body verbatim, and a 400 from a content filter routinely
    // echoes the prompt.
    if matches!(err, LlmError::NetworkError(_) | LlmError::Timeout(_)) {
        codewhale_telemetry::session_counters()
            .bump_error(codewhale_telemetry::ErrorCounter::NetworkError);
    }
    match err {
        LlmError::RateLimited { retry_after, .. } => {
            let human = if let Some(after) = retry_after {
                format!("rate limited (Retry-After {}s)", after.as_secs())
            } else {
                "rate limited".to_string()
            };
            ("rate_limited", human)
        }
        LlmError::ServerError { status, .. } => ("server_error", format!("upstream {status}")),
        LlmError::NetworkError(_) => ("network_error", "network error".to_string()),
        LlmError::Timeout(_) => ("timeout", "timeout".to_string()),
        _ => ("other", "other".to_string()),
    }
}

impl CodewhaleClient {
    /// Execute a non-streaming request without consulting or updating the
    /// process-global response cache.
    ///
    /// Request previews use this only for Auto's auxiliary router classifier:
    /// the classifier may call its configured provider, but an inspection must
    /// not perturb later production routing through shared cache state.
    pub(crate) async fn create_message_without_response_cache(
        &self,
        request: MessageRequest,
    ) -> Result<MessageResponse> {
        let mut isolated = self.clone();
        isolated.isolated_request_state = true;
        // The ordinary clone shares its provider token bucket so concurrent
        // production calls observe one rate budget. Request inspection is an
        // auxiliary classifier call, however: it must neither consume nor
        // inherit that mutable foreground state.
        isolated.rate_limiter = Arc::new(AsyncMutex::new(TokenBucket::from_env()));
        isolated
            .create_message_with_cache_policy(request, false)
            .await
    }

    async fn create_message_with_cache_policy(
        &self,
        request: MessageRequest,
        allow_response_cache: bool,
    ) -> Result<MessageResponse> {
        let _inference = self.acquire_remote_control_inference_permit().await;
        let _permit = self.acquire_provider_request_permit().await;
        let cacheable =
            allow_response_cache && crate::llm_response_cache::request_is_cacheable(&request);
        let prepared = self.prepare_outbound_request(request, false)?;
        match prepared.dialect {
            WireDialect::OpenAiResponses => self.handle_responses_message(&prepared).await,
            WireDialect::AnthropicMessages => self.handle_anthropic_message(&prepared).await,
            WireDialect::ChatCompletions => self.create_message_chat(&prepared, cacheable).await,
        }
    }
}

impl LlmClient for CodewhaleClient {
    fn wire_format(&self) -> Option<WireFormat> {
        Some(CodewhaleClient::wire_format(self))
    }
    fn cache_route_receipt(&self) -> Option<crate::route_receipt::TurnRouteReceipt> {
        Some(self.turn_route_receipt())
    }

    fn provider_name(&self) -> &'static str {
        self.api_provider.as_str()
    }

    fn model(&self) -> &str {
        &self.default_model
    }

    fn billing_base_url(&self) -> Option<&str> {
        Some(&self.base_url)
    }

    fn route_limits(&self) -> Option<RouteLimits> {
        CodewhaleClient::route_limits(self)
    }

    fn effective_max_output_tokens(&self, requested_model: &str) -> u32 {
        CodewhaleClient::effective_max_output_tokens(self, requested_model)
    }

    fn effective_route_envelope(
        &self,
        requested_model: &str,
        dispatched_at: chrono::DateTime<chrono::Utc>,
    ) -> crate::cost_status::EffectiveRouteEnvelope {
        CodewhaleClient::effective_route_envelope(self, requested_model, dispatched_at)
    }

    async fn health_check(&self) -> Result<bool> {
        if api_provider_skips_models_probe(self.api_provider) {
            self.mark_request_success().await;
            return Ok(true);
        }
        let health_url = api_url(&self.base_url, "models");
        self.wait_for_rate_limit().await;
        let response = self
            .authorize_plugin_request(
                self.models_http_client
                    .get(health_url)
                    .timeout(NON_STREAMING_HTTP_TIMEOUT),
            )
            .await?
            .send()
            .await;
        match response {
            Ok(resp) if resp.status().is_success() => {
                // Consume the response body so the connection can be returned to the pool.
                let _ = bounded_error_text(resp, ERROR_BODY_MAX_BYTES).await;
                self.mark_request_success().await;
                Ok(true)
            }
            Ok(resp) => {
                self.mark_request_failure(&format!("health status={}", resp.status()))
                    .await;
                Ok(false)
            }
            Err(err) => {
                self.mark_request_failure(&format!("health error={}", err.without_url()))
                    .await;
                Ok(false)
            }
        }
    }

    async fn create_message(&self, request: MessageRequest) -> Result<MessageResponse> {
        self.create_message_with_cache_policy(request, true).await
    }

    async fn create_message_uncached(&self, request: MessageRequest) -> Result<MessageResponse> {
        // Keep shared provider permits and rate limits. Only the response
        // cache is bypassed; a guardian is still real, metered inference.
        self.create_message_with_cache_policy(request, false).await
    }

    async fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> Result<crate::llm_client::StreamEventBox> {
        let inference = self.acquire_remote_control_inference_permit().await;
        let permit = self.acquire_provider_request_permit().await;
        let prepared = self.prepare_outbound_request(request, true)?;
        let projection_warning = (!prepared.omitted_tool_names.is_empty()).then(|| {
            let omitted_tool_count = prepared.omitted_tool_names.len();
            (
                prepared.endpoint.provider_display.clone(),
                crate::core::events::bounded_tool_projection_warning_names(
                    &prepared.omitted_tool_names,
                ),
                omitted_tool_count,
            )
        });
        let stream = match prepared.dialect {
            WireDialect::OpenAiResponses => self.handle_responses_stream(&prepared).await?,
            WireDialect::AnthropicMessages => self.handle_anthropic_stream(&prepared).await?,
            WireDialect::ChatCompletions => self.handle_chat_completion_stream(prepared).await?,
        };
        let stream = match projection_warning {
            Some((provider, omitted_tool_names, omitted_tool_count)) => {
                Self::prepend_tool_projection_warning(
                    stream,
                    provider,
                    omitted_tool_names,
                    omitted_tool_count,
                )
            }
            None => stream,
        };
        let stream = Self::hold_provider_request_permit_for_stream(stream, permit);
        Ok(Self::hold_remote_control_inference_permit_for_stream(
            stream, inference,
        ))
    }
}

#[derive(Debug, Deserialize)]
struct ModelsListResponse {
    data: Vec<ModelListItem>,
}

/// The list envelope is validated as a whole; each row is decoded on its own
/// so one malformed row cannot fail the entire roster (#6690). Rows stay raw
/// text rather than `serde_json::Value`: a `Value` map keeps the last of a
/// duplicated key, which would silently accept an ambiguous row (two `id`s,
/// two `pricing.prompt`s) instead of skipping it as malformed.
#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<Box<serde_json::value::RawValue>>,
}

#[derive(Debug, Deserialize)]
struct ModelListItem {
    id: String,
    #[serde(default)]
    owned_by: Option<String>,
    #[serde(default)]
    created: Option<u64>,
}

/// OpenRouter `/models` response item with full capability metadata (#3385).
#[derive(Debug, Deserialize)]
struct OpenRouterModelItem {
    id: String,
    // Captured from OpenRouter for future display/deprecation surfaces. The
    // current CatalogOffering shape has no honest fields for these yet.
    #[serde(default)]
    #[expect(dead_code)]
    name: Option<String>,
    #[serde(default)]
    #[expect(dead_code)]
    created: Option<u64>,
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    pricing: Option<OpenRouterPricing>,
    #[serde(default)]
    top_provider: Option<OpenRouterTopProvider>,
    #[serde(default)]
    supported_parameters: Option<Vec<String>>,
    #[serde(default)]
    architecture: Option<OpenRouterArchitecture>,
    /// Endpoint dialects this gateway advertises for the row. OrcaRouter
    /// publishes this for every model it lists; it is what lets chat rows be
    /// separated from image/video/rerank rows without guessing from the name.
    #[serde(default)]
    supported_endpoint_types: Option<Vec<String>>,
    #[serde(default)]
    #[expect(dead_code)]
    expiration_date: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterPricing {
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
    #[serde(default)]
    input_cache_read: Option<String>,
    /// Per-token cache-write (cache-creation) price. OpenRouter publishes this
    /// for the upstreams that charge a write premium (Anthropic, Qwen, …);
    /// dropping it undercounted every cache-creation turn on those routes.
    #[serde(default)]
    input_cache_write: Option<String>,
    #[serde(default)]
    tiers: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct OrcaRouterPriceTier {
    #[serde(default)]
    max_input_tokens: Option<u64>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    completion: Option<String>,
    #[serde(default)]
    input_cache_read: Option<String>,
    #[serde(default)]
    input_cache_write: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterTopProvider {
    #[serde(default)]
    context_length: Option<u32>,
    #[serde(default)]
    max_completion_tokens: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterArchitecture {
    #[serde(default)]
    modality: Option<String>,
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
    #[serde(default)]
    output_modalities: Option<Vec<String>>,
}

/// Baseten Model APIs `/v1/models` item.
///
/// Baseten publishes OpenAI-style ids and per-token prices, while current
/// serving limits and feature fields are additive. Numeric fields accept JSON
/// numbers or numeric strings because both appear in provider catalogs in the
/// wild; malformed or negative known fields reject the refresh so the durable
/// last-known-good snapshot remains authoritative.
#[derive(Debug, Deserialize)]
struct BasetenModelsResponse {
    data: Vec<BasetenModelItem>,
}

#[derive(Debug, Deserialize)]
struct BasetenModelItem {
    id: String,
    #[serde(default)]
    context_length: Option<CatalogNumber>,
    #[serde(default)]
    context_window: Option<CatalogNumber>,
    #[serde(default)]
    max_output_tokens: Option<CatalogNumber>,
    #[serde(default)]
    max_completion_tokens: Option<CatalogNumber>,
    #[serde(default)]
    limits: Option<BasetenLimits>,
    #[serde(default)]
    top_provider: Option<BasetenTopProvider>,
    #[serde(default)]
    pricing: Option<BasetenPricing>,
    #[serde(default)]
    supported_parameters: Option<Vec<String>>,
    #[serde(default)]
    supported_features: Option<Vec<String>>,
    #[serde(default)]
    features: Option<Vec<String>>,
    #[serde(default)]
    architecture: Option<BasetenArchitecture>,
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
    #[serde(default)]
    output_modalities: Option<Vec<String>>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    supports_reasoning: Option<bool>,
    #[serde(default)]
    supports_tools: Option<bool>,
    #[serde(default)]
    supports_structured_output: Option<bool>,
    #[serde(default)]
    reasoning_options: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CatalogNumber {
    Number(serde_json::Number),
    Text(String),
}

#[derive(Debug, Default, Deserialize)]
struct BasetenLimits {
    #[serde(default)]
    context: Option<CatalogNumber>,
    #[serde(default)]
    output: Option<CatalogNumber>,
}

#[derive(Debug, Default, Deserialize)]
struct BasetenTopProvider {
    #[serde(default)]
    context_length: Option<CatalogNumber>,
    #[serde(default)]
    max_completion_tokens: Option<CatalogNumber>,
}

#[derive(Debug, Default, Deserialize)]
struct BasetenPricing {
    #[serde(default)]
    prompt: Option<CatalogNumber>,
    #[serde(default)]
    completion: Option<CatalogNumber>,
    #[serde(default)]
    input_cache_read: Option<CatalogNumber>,
    #[serde(default)]
    input_cache_write: Option<CatalogNumber>,
}

#[derive(Debug, Default, Deserialize)]
struct BasetenArchitecture {
    #[serde(default)]
    input_modalities: Option<Vec<String>>,
    #[serde(default)]
    output_modalities: Option<Vec<String>>,
}

fn parse_models_response_for_provider(
    payload: &str,
    provider: ProviderKind,
) -> Result<Vec<AvailableModel>> {
    if provider != ProviderKind::OpenaiCodex {
        return parse_models_response(payload);
    }
    #[derive(Deserialize)]
    struct Roster {
        models: Vec<Row>,
    }
    #[derive(Deserialize)]
    struct Row {
        slug: String,
        display_name: String,
        visibility: String,
    }
    let roster: Roster = serde_json::from_str(payload).context("Invalid ChatGPT model roster")?;
    anyhow::ensure!(
        roster.models.len() <= PROVIDER_CATALOG_MAX_ROWS,
        "ChatGPT model roster exceeds row limit"
    );
    let mut seen = std::collections::HashSet::new();
    let mut models = Vec::new();
    for row in roster.models {
        if row.visibility != "list" {
            continue;
        }
        anyhow::ensure!(
            crate::provider_lake::valid_catalog_model_id(&row.slug)
                && !row.display_name.trim().is_empty()
                && row.display_name.len() <= 256
                && !row.display_name.chars().any(char::is_control),
            "Invalid ChatGPT model row"
        );
        if seen.insert(row.slug.clone()) {
            models.push(AvailableModel {
                id: row.slug,
                display_name: Some(row.display_name),
                owned_by: None,
                created: None,
            });
        }
    }
    Ok(models)
}

pub(crate) fn parse_models_response(payload: &str) -> Result<Vec<AvailableModel>> {
    let parsed: ModelsListResponse =
        serde_json::from_str(payload).context("Failed to parse model list JSON")?;

    let mut models = parsed
        .data
        .into_iter()
        .map(|item| AvailableModel {
            id: item.id,
            owned_by: item.owned_by,
            created: item.created,
            display_name: None,
        })
        .collect::<Vec<_>>();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models.dedup_by(|a, b| a.id == b.id);
    Ok(models)
}

/// Keep both live-model consumers on each provider's documented coding roster.
/// Unknown models remain hidden until their wire contract is known.
fn apply_provider_model_cutline(
    provider: ProviderKind,
    models: Vec<AvailableModel>,
) -> Vec<AvailableModel> {
    if provider == ProviderKind::Stepfun {
        // The shared /models endpoint also lists speech and image generators.
        // Only expose routes whose text/tool contract is in our catalog.
        let offerings = codewhale_config::catalog::bundled_catalog_offerings();
        return models
            .into_iter()
            .filter(|model| {
                offerings.iter().any(|row| {
                    row.provider == "stepfun"
                        && row.wire_model_id == model.id
                        && row.tool_call == Some(true)
                })
            })
            .collect();
    }
    if provider != ProviderKind::OpencodeGo {
        return models;
    }

    let mut models: Vec<_> = models
        .into_iter()
        .filter_map(|mut model| {
            let canonical = crate::config::opencode_go_model_id(&model.id)?;
            model.id = canonical.to_string();
            Some(model)
        })
        .collect();
    models.sort_by(|left, right| left.id.cmp(&right.id));
    models.dedup_by(|left, right| left.id == right.id);
    models
}

/// Convert a named gateway's `/models` response into truthful provider-scoped
/// catalog rows. Matching model ids on other providers prove no capabilities,
/// limits, or prices; only an explicit same-provider bundled row may enrich a
/// live offering.
/// Parse the Codewhale API's authenticated `GET {base}/models` listing.
///
/// The account control plane returns the OpenAI list shape, but every row
/// carries a `codewhale` block naming the wire protocol Codewhale will use for
/// that model (`chat-completions` → `{base}/chat/completions`,
/// `anthropic-messages` → `{base}/messages`, `responses` →
/// `{base}/responses`). That block is the whole reason this route is
/// model-aware, so the protocol is read from the response rather
/// than inferred — the namespace inference in
/// [`codewhale_config::route::codewhale_endpoint_key_for_model`] is only the
/// offline fallback for a row this listing did not describe.
///
/// The listing is untrusted remote data: unusable rows are dropped rather than
/// failing the refresh, and nothing here claims capabilities, limits, or
/// pricing the account service did not state.
fn codewhale_catalog_offerings_from_body(
    body: &str,
    provider: &str,
    fingerprint: &str,
    fetched_at: u64,
) -> Result<Vec<CatalogOffering>, CatalogRefreshError> {
    #[derive(serde::Deserialize)]
    struct Listing {
        #[serde(default)]
        data: Vec<Row>,
    }
    #[derive(serde::Deserialize)]
    struct Row {
        #[serde(default)]
        id: String,
        #[serde(default)]
        codewhale: Option<RowMeta>,
    }
    #[derive(serde::Deserialize)]
    struct RowMeta {
        #[serde(default)]
        protocol: Option<String>,
        #[serde(default)]
        default: bool,
        #[serde(default)]
        limits: Option<RowLimits>,
    }
    #[derive(serde::Deserialize)]
    struct RowLimits {
        #[serde(default, rename = "maxInputTokens")]
        max_input_tokens: Option<u64>,
        #[serde(default, rename = "maxOutputTokens")]
        max_output_tokens: Option<u64>,
        #[serde(default, rename = "maxContextTokens")]
        max_context_tokens: Option<u64>,
    }

    let listing: Listing =
        serde_json::from_str(body).map_err(|_| CatalogRefreshError::InvalidResponse)?;
    let mut offerings: Vec<CatalogOffering> = Vec::with_capacity(listing.data.len());
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for row in listing.data {
        let id = row.id.trim().to_string();
        if id.is_empty() || !seen.insert(id.clone()) {
            continue;
        }
        // An unstated or unknown protocol falls back to the namespace rule
        // rather than being dropped: the account already proved it serves this
        // model by listing it, and a protocol label this build predates must
        // not make the row unreachable.
        let endpoint_key = match row
            .codewhale
            .as_ref()
            .and_then(|meta| meta.protocol.as_deref())
            .map(str::trim)
        {
            Some("anthropic-messages") => "messages",
            Some("chat-completions") => "chat",
            Some("responses") => "responses",
            _ => codewhale_config::route::codewhale_endpoint_key_for_model(&id),
        };
        let limit = row
            .codewhale
            .as_ref()
            .and_then(|meta| meta.limits.as_ref())
            .map(|limits| codewhale_config::models_dev::ModelsDevLimit {
                context: limits.max_context_tokens.filter(|tokens| *tokens > 0),
                input: limits.max_input_tokens.filter(|tokens| *tokens > 0),
                output: limits.max_output_tokens.filter(|tokens| *tokens > 0),
            })
            .filter(|limit| *limit != codewhale_config::models_dev::ModelsDevLimit::default());
        offerings.push(CatalogOffering {
            cost_source: None,
            modalities_source: None,
            provider: provider.to_string(),
            wire_model_id: id,
            canonical_model: None,
            endpoint_key: endpoint_key.to_string(),
            default_for_provider: row.codewhale.is_some_and(|meta| meta.default),
            family: None,
            limit,
            cost: None,
            modalities: None,
            attachment: None,
            reasoning: None,
            tool_call: None,
            structured_output: None,
            reasoning_options: Vec::new(),
            source: CatalogSource::Live {
                base_url_fingerprint: fingerprint.to_string(),
                fetched_at,
            },
            ..Default::default()
        });
    }
    if offerings.is_empty() {
        return Err(CatalogRefreshError::EmptyList);
    }
    Ok(offerings)
}

/// Whether a route speaks Baseten's `/models` dialect: recognized by
/// endpoint, never by table name (#6289).
fn catalog_endpoint_is_baseten(api_provider: ProviderKind, base_url: &str) -> bool {
    api_provider == ProviderKind::Custom && codewhale_config::catalog::endpoint_is_baseten(base_url)
}

/// Project one `/models` document onto a secret-free, endpoint-scoped roster
/// delta (#3385). Shared by [`CodewhaleClient::fetch_catalog_delta`] and the
/// guided-setup key probe ([`verify_provider_api_key`]), so the listing a key
/// check already downloaded is read by exactly the rules a refresh uses.
fn catalog_delta_from_models_body(
    api_provider: ProviderKind,
    provider: String,
    base_url: &str,
    body: &str,
    model_bound_secret_values: &[String],
) -> Result<ProviderCatalogDelta, CatalogRefreshError> {
    let fingerprint = base_url_fingerprint(base_url);
    let fetched_at = now_unix();

    // OpenRouter returns extended capability metadata in its /models
    // response (#3385). Capture limits, pricing, reasoning, and modalities
    // from the live API instead of leaving them unknown.
    let offerings: Vec<CatalogOffering> = if api_provider == ProviderKind::Orcarouter {
        // OrcaRouter serves the extended capability shape too, and adds
        // `supported_endpoint_types` on every row. That field is what keeps a
        // gateway model list — which mixes chat with image/video generation —
        // from dumping non-text rows into the text selector. A row that does
        // not advertise the Chat Completions dialect this route speaks is
        // dropped here, so the roster the picker and `/model` read is
        // servable chat only.
        let listed = parse_orcarouter_models_response(body)?;
        if listed.is_empty() {
            return Err(CatalogRefreshError::EmptyList);
        }
        let chat_rows: Vec<_> = listed
            .iter()
            .filter(|item| orcarouter_row_is_chat(item))
            .collect();
        let non_chat = listed.len() - chat_rows.len();
        if non_chat > 0 {
            tracing::info!(
                dropped = non_chat,
                listed = listed.len(),
                "OrcaRouter catalog refresh kept chat rows and dropped non-chat dialects"
            );
        }
        let offerings: Vec<_> = chat_rows
            .iter()
            .filter_map(|item| {
                orcarouter_to_catalog_offering(item, &provider, &fingerprint, fetched_at).ok()
            })
            .collect();
        if offerings.is_empty() {
            return Err(CatalogRefreshError::InvalidResponse);
        }
        offerings
    } else if api_provider == ProviderKind::Openrouter {
        let or_models = parse_openrouter_models_response(body)?;
        if or_models.is_empty() {
            return Err(CatalogRefreshError::EmptyList);
        }
        // A row with an unreadable or implausible price is skipped and
        // counted, not allowed to fail every other row (#6690).
        let offerings: Vec<_> = or_models
            .iter()
            .filter_map(|item| {
                openrouter_to_catalog_offering(item, &provider, &fingerprint, fetched_at).ok()
            })
            .collect();
        let skipped = or_models.len() - offerings.len();
        if skipped > 0 {
            tracing::warn!(
                skipped,
                listed = or_models.len(),
                "skipped OpenRouter model rows with invalid pricing"
            );
        }
        if offerings.is_empty() {
            return Err(CatalogRefreshError::InvalidResponse);
        }
        offerings
    } else if catalog_endpoint_is_baseten(api_provider, base_url) {
        let baseten_models = parse_baseten_models_response(body)?;
        if baseten_models.is_empty() {
            return Err(CatalogRefreshError::EmptyList);
        }
        baseten_models
            .iter()
            .map(|item| baseten_to_catalog_offering(item, &provider, &fingerprint, fetched_at))
            .collect::<Result<Vec<_>, _>>()?
    } else if api_provider == ProviderKind::Telecomjs {
        named_gateway_catalog_offerings_from_body(
            body,
            codewhale_config::ProviderKind::Telecomjs,
            &provider,
            &fingerprint,
            fetched_at,
        )?
    } else if api_provider == ProviderKind::Edenai {
        named_gateway_catalog_offerings_from_body(
            body,
            codewhale_config::ProviderKind::Edenai,
            &provider,
            &fingerprint,
            fetched_at,
        )?
    } else if api_provider == ProviderKind::Zenmux {
        named_gateway_catalog_offerings_from_body(
            body,
            codewhale_config::ProviderKind::Zenmux,
            &provider,
            &fingerprint,
            fetched_at,
        )?
    } else if provider == "codewhale" {
        // The Codewhale API's own listing states the wire protocol per
        // model, so it is the catalog authority for this route.
        codewhale_catalog_offerings_from_body(body, &provider, &fingerprint, fetched_at)?
    } else if provider == "concentrate" {
        // Concentrate's unauthenticated `GET /v1/models` is the same
        // OpenAI list shape (`{"object":"list","data":[{"id":..}]}`);
        // Rows state existence; the shared resolver owns metadata completion.
        named_gateway_catalog_offerings_from_body(
            body,
            codewhale_config::ProviderKind::Concentrate,
            &provider,
            &fingerprint,
            fetched_at,
        )?
    } else {
        let models = apply_provider_model_cutline(
            api_provider,
            parse_models_response_for_provider(body, api_provider)
                .map_err(|_| CatalogRefreshError::InvalidResponse)?,
        );
        if models.is_empty() {
            return Err(CatalogRefreshError::EmptyList);
        }
        models
            .into_iter()
            .map(|model| CatalogOffering {
                cost_source: None,
                modalities_source: None,
                provider: provider.clone(),
                endpoint_key: if api_provider == ProviderKind::OpencodeGo {
                    codewhale_config::opencode_go_endpoint_key(&model.id)
                        .expect("filtered Go roster")
                } else {
                    "chat"
                }
                .to_string(),
                wire_model_id: model.id,
                canonical_model: None,
                default_for_provider: false,
                family: None,
                limit: None,
                cost: None,
                modalities: None,
                attachment: None,
                reasoning: None,
                tool_call: None,
                structured_output: None,
                reasoning_options: Vec::new(),
                source: CatalogSource::Live {
                    base_url_fingerprint: fingerprint.clone(),
                    fetched_at,
                },
                ..Default::default()
            })
            .collect()
    };

    if offerings.iter().any(|row| {
        !crate::provider_lake::valid_catalog_model_id(&row.wire_model_id)
            || model_bound_secret_values
                .iter()
                .any(|secret| !secret.is_empty() && row.wire_model_id.contains(secret))
    }) {
        return Err(CatalogRefreshError::InvalidResponse);
    }
    if offerings.len() > PROVIDER_CATALOG_MAX_ROWS {
        return Err(CatalogRefreshError::InvalidResponse);
    }
    Ok(ProviderCatalogDelta {
        provider,
        base_url_fingerprint: fingerprint,
        fetched_at,
        offerings,
    })
}

fn named_gateway_catalog_offerings_from_body(
    body: &str,
    kind: codewhale_config::ProviderKind,
    provider: &str,
    fingerprint: &str,
    fetched_at: u64,
) -> Result<Vec<CatalogOffering>, CatalogRefreshError> {
    let models = parse_models_response(body).map_err(|_| CatalogRefreshError::InvalidResponse)?;
    if models.is_empty() {
        return Err(CatalogRefreshError::EmptyList);
    }

    let default_model_id = kind.provider().default_model();
    Ok(models
        .into_iter()
        .map(|model| {
            let is_default = model.id.eq_ignore_ascii_case(default_model_id);
            CatalogOffering {
                provider: provider.to_string(),
                wire_model_id: model.id,
                endpoint_key: "chat".to_string(),
                default_for_provider: is_default,
                source: CatalogSource::Live {
                    base_url_fingerprint: fingerprint.to_string(),
                    fetched_at,
                },
                ..Default::default()
            }
        })
        .collect())
}

/// Parse an OpenRouter `/models` response, preserving server-side ordering and
/// capturing full capability metadata (#3385).
fn parse_openrouter_models_response(
    payload: &str,
) -> Result<Vec<OpenRouterModelItem>, CatalogRefreshError> {
    let parsed: OpenRouterModelsResponse =
        serde_json::from_str(payload).map_err(|_| CatalogRefreshError::InvalidResponse)?;
    let listed = parsed.data.len();
    let mut seen = std::collections::HashSet::new();
    let mut malformed = 0usize;
    let mut models = Vec::with_capacity(listed);
    for row in parsed.data {
        // `~`-prefixed ids (`~deepseek/deepseek-pro-latest`) are OpenRouter's
        // moving "latest" aliases, not billing identities, so they are dropped
        // silently. Any other row that does not decode or carries an id the
        // catalog cannot hold is skipped and counted: one such row used to
        // fail the whole roster closed, so no OpenRouter route could ever be
        // priced from the lake (#6690).
        let Ok(item) = serde_json::from_str::<OpenRouterModelItem>(row.get()) else {
            malformed += 1;
            continue;
        };
        if item.id.starts_with('~') {
            continue;
        }
        if !crate::provider_lake::valid_catalog_model_id(&item.id) {
            malformed += 1;
            continue;
        }
        if seen.insert(item.id.clone()) {
            models.push(item);
        }
    }
    if malformed > 0 {
        tracing::warn!(
            malformed,
            listed,
            "skipped malformed OpenRouter model rows in the catalog refresh"
        );
    }
    // Rows were listed but not one survived: the response is not a roster.
    if models.is_empty() && malformed > 0 {
        return Err(CatalogRefreshError::InvalidResponse);
    }
    Ok(models)
}

/// Endpoint dialects that mean "a chat/completions text turn can be served".
///
/// OrcaRouter publishes `supported_endpoint_types`, and this route speaks only
/// Chat Completions, which it advertises as `openai`. A row offered only on
/// another dialect (`anthropic`, `gemini`, `openai-response`) or on a non-chat
/// one (`image-generation`, `openai-video`, `jina-rerank`, `embeddings`) is
/// kept out of the chat selector.
const ORCAROUTER_CHAT_ENDPOINT_TYPES: &[&str] = &["openai"];

/// Whether an OrcaRouter row is offered on a chat/completions dialect.
///
/// Fail-closed on silence: a row that declares no `supported_endpoint_types`
/// is **not** assumed to be chat. On a gateway that mixes chat with image and
/// video generation, guessing by omission would put a generation model in the
/// text selector.
fn orcarouter_row_is_chat(item: &OpenRouterModelItem) -> bool {
    item.supported_endpoint_types.as_ref().is_some_and(|types| {
        types.iter().any(|endpoint| {
            let endpoint = endpoint.trim();
            ORCAROUTER_CHAT_ENDPOINT_TYPES
                .iter()
                .any(|chat| endpoint.eq_ignore_ascii_case(chat))
        })
    })
}

/// Parse OrcaRouter's `/v1/models` body.
///
/// The gateway serves the OpenRouter extended shape: every row carries
/// `supported_endpoint_types`, and text models add `architecture`,
/// `context_length`, `top_provider` and `pricing`. Rows are accepted or
/// skipped by the same id rules as [`parse_openrouter_models_response`], so one
/// malformed row cannot fail the whole roster.
fn parse_orcarouter_models_response(
    payload: &str,
) -> Result<Vec<OpenRouterModelItem>, CatalogRefreshError> {
    let parsed: OpenRouterModelsResponse =
        serde_json::from_str(payload).map_err(|_| CatalogRefreshError::InvalidResponse)?;
    let listed = parsed.data.len();
    let mut seen = std::collections::HashSet::new();
    let mut malformed = 0usize;
    let mut models = Vec::with_capacity(listed);
    for row in parsed.data {
        let Ok(item) = serde_json::from_str::<OpenRouterModelItem>(row.get()) else {
            malformed += 1;
            continue;
        };
        if item.id.starts_with('~') {
            continue;
        }
        if !crate::provider_lake::valid_catalog_model_id(&item.id) {
            malformed += 1;
            continue;
        }
        if seen.insert(item.id.clone()) {
            models.push(item);
        }
    }
    if malformed > 0 {
        tracing::warn!(
            malformed,
            listed,
            "skipped malformed OrcaRouter model rows in the catalog refresh"
        );
    }
    if models.is_empty() && malformed > 0 {
        return Err(CatalogRefreshError::InvalidResponse);
    }
    Ok(models)
}

/// Project one OrcaRouter `/v1/models` row onto a catalog offering.
///
/// Pricing and limits reuse the OpenRouter projection (OrcaRouter bills the
/// same extended fields). Three differences matter:
///
/// - OrcaRouter does not publish `supported_parameters` on its rows, so the
///   OpenRouter projection's `reasoning`/`tool_call` would become a factual
///   "no reasoning, no tools". It strips them back to unclaimed instead: on
///   this gateway an absent parameter list is silence, not a refusal. That
///   also keeps Codewhale's tools enabled for OrcaRouter chat models.
/// - A row whose `architecture` names an explicit input set keeps it verbatim,
///   so the multimodal gate downstream is reading a stated fact. Rows with no
///   `architecture` stay unclaimed rather than inheriting a "text" default.
/// - `pricing.tiers` become context tiers. A tier list that cannot be read
///   withholds the row's price rather than quoting long prompts at the base
///   rate.
fn orcarouter_to_catalog_offering(
    item: &OpenRouterModelItem,
    provider: &str,
    base_url_fingerprint: &str,
    fetched_at: u64,
) -> Result<CatalogOffering, CatalogRefreshError> {
    let mut offering =
        openrouter_to_catalog_offering(item, provider, base_url_fingerprint, fetched_at)?;
    if item.supported_parameters.is_none() {
        offering.reasoning = None;
        offering.tool_call = None;
    }
    if item.architecture.is_none() {
        offering.modalities = None;
    }
    let tiers = item
        .pricing
        .as_ref()
        .and_then(|pricing| pricing.tiers.as_ref());
    if let Some(tiers) = tiers
        && let Some(base) = offering.cost.clone()
    {
        let tiered = orcarouter_context_tiers(tiers, &base).map(|tiers| {
            codewhale_config::models_dev::ModelsDevCost {
                tiers: (!tiers.is_empty()).then_some(tiers),
                ..base
            }
        });
        match tiered {
            Some(cost) if codewhale_config::pricing::catalog_cost_is_valid(&cost) => {
                offering.cost = Some(cost);
            }
            _ => {
                offering.cost = None;
                offering.cost_source = Some(offering.source.clone());
            }
        }
    }
    Ok(offering)
}

fn orcarouter_context_tiers(
    tiers: &Value,
    base: &codewhale_config::models_dev::ModelsDevCost,
) -> Option<Vec<codewhale_config::models_dev::ModelsDevCostTier>> {
    use codewhale_config::models_dev::{ModelsDevContextTier, ModelsDevCost, ModelsDevCostTier};

    let tiers: Vec<OrcaRouterPriceTier> = serde_json::from_value(tiers.clone()).ok()?;
    let rate = |value: &Option<String>| -> Option<Option<f64>> {
        match value {
            None => Some(None),
            Some(value) => value
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .and_then(|value| checked_per_token_to_per_million(value).ok())
                .map(Some),
        }
    };
    let mut mapped = Vec::with_capacity(tiers.len().saturating_sub(1));
    let mut floor: Option<u64> = None;
    for (index, tier) in tiers.iter().enumerate() {
        let cost = ModelsDevCost {
            input: rate(&tier.prompt)?,
            output: rate(&tier.completion)?,
            cache_read: rate(&tier.input_cache_read)?,
            cache_write: rate(&tier.input_cache_write)?,
            ..Default::default()
        };
        if index == 0 {
            if (cost.input, cost.output) != (base.input, base.output) {
                return None;
            }
        } else {
            mapped.push(ModelsDevCostTier {
                tier: ModelsDevContextTier {
                    kind: "context".to_string(),
                    size: floor?,
                },
                cost,
            });
        }
        floor = match tier.max_input_tokens {
            Some(max) if max > floor.unwrap_or(0) => Some(max),
            Some(0) | None if index + 1 == tiers.len() => None,
            _ => return None,
        };
    }
    Some(mapped)
}

/// Parse Baseten's authenticated Model APIs catalog without inferring facts
/// from an identically named model on another provider.
fn parse_baseten_models_response(
    payload: &str,
) -> Result<Vec<BasetenModelItem>, CatalogRefreshError> {
    let parsed: BasetenModelsResponse =
        serde_json::from_str(payload).map_err(|_| CatalogRefreshError::InvalidResponse)?;
    let mut seen = std::collections::HashSet::new();
    let mut models = Vec::with_capacity(parsed.data.len());
    for mut item in parsed.data {
        item.id = item.id.trim().to_string();
        if item.id.is_empty() || !seen.insert(item.id.clone()) {
            return Err(CatalogRefreshError::InvalidResponse);
        }
        models.push(item);
    }
    Ok(models)
}

fn catalog_number_f64(value: Option<&CatalogNumber>) -> Result<Option<f64>, CatalogRefreshError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let parsed = match value {
        CatalogNumber::Number(number) => number.as_f64(),
        CatalogNumber::Text(text) => text.trim().parse::<f64>().ok(),
    }
    .filter(|number| number.is_finite() && *number >= 0.0)
    .ok_or(CatalogRefreshError::InvalidResponse)?;
    Ok(Some(parsed))
}

fn catalog_number_u64(value: Option<&CatalogNumber>) -> Result<Option<u64>, CatalogRefreshError> {
    let Some(number) = catalog_number_f64(value)? else {
        return Ok(None);
    };
    if number.fract() != 0.0 || number > u64::MAX as f64 {
        return Err(CatalogRefreshError::InvalidResponse);
    }
    Ok(Some(number as u64))
}

fn checked_per_token_to_per_million(value: f64) -> Result<f64, CatalogRefreshError> {
    let scaled = value * 1_000_000.0;
    (scaled.is_finite()
        && (0.0..=codewhale_config::pricing::MAX_PLAUSIBLE_PRICE_PER_MILLION).contains(&scaled))
    .then_some(scaled)
    .ok_or(CatalogRefreshError::InvalidResponse)
}

fn catalog_price_per_million(
    value: Option<&CatalogNumber>,
) -> Result<Option<f64>, CatalogRefreshError> {
    catalog_number_f64(value)?
        .map(checked_per_token_to_per_million)
        .transpose()
}

fn feature_matches_any(feature: &str, aliases: &[&str]) -> bool {
    let normalized = feature.replace('-', "_");
    aliases.iter().any(|alias| normalized == *alias)
}

fn baseten_features(item: &BasetenModelItem) -> Option<Vec<String>> {
    let sources = [
        item.supported_parameters.as_ref(),
        item.supported_features.as_ref(),
        item.features.as_ref(),
    ];
    let mut features = Vec::new();
    let mut published = false;
    for source in sources.into_iter().flatten() {
        published = true;
        for feature in source {
            let normalized = feature.trim().to_ascii_lowercase();
            if !normalized.is_empty() && !features.contains(&normalized) {
                features.push(normalized);
            }
        }
    }
    published.then_some(features)
}

fn baseten_to_catalog_offering(
    item: &BasetenModelItem,
    provider: &str,
    base_url_fingerprint: &str,
    fetched_at: u64,
) -> Result<CatalogOffering, CatalogRefreshError> {
    use codewhale_config::models_dev::{ModelsDevCost, ModelsDevLimit, ModelsDevModalities};

    let context = catalog_number_u64(
        item.top_provider
            .as_ref()
            .and_then(|provider| provider.context_length.as_ref())
            .or(item.context_length.as_ref())
            .or(item.context_window.as_ref())
            .or_else(|| {
                item.limits
                    .as_ref()
                    .and_then(|limits| limits.context.as_ref())
            }),
    )?;
    let output = catalog_number_u64(
        item.top_provider
            .as_ref()
            .and_then(|provider| provider.max_completion_tokens.as_ref())
            .or(item.max_output_tokens.as_ref())
            .or(item.max_completion_tokens.as_ref())
            .or_else(|| {
                item.limits
                    .as_ref()
                    .and_then(|limits| limits.output.as_ref())
            }),
    )?;
    let limit = (context.is_some() || output.is_some()).then_some(ModelsDevLimit {
        context,
        input: context,
        output,
    });

    let cost = if let Some(pricing) = item.pricing.as_ref() {
        let cost = ModelsDevCost {
            input: catalog_price_per_million(pricing.prompt.as_ref())?,
            output: catalog_price_per_million(pricing.completion.as_ref())?,
            cache_read: catalog_price_per_million(pricing.input_cache_read.as_ref())?,
            cache_write: catalog_price_per_million(pricing.input_cache_write.as_ref())?,
            ..Default::default()
        };
        if !codewhale_config::pricing::catalog_cost_is_valid(&cost) {
            return Err(CatalogRefreshError::InvalidResponse);
        }
        (cost.input.is_some()
            || cost.output.is_some()
            || cost.cache_read.is_some()
            || cost.cache_write.is_some())
        .then_some(cost)
    } else {
        None
    };

    let features = baseten_features(item);
    let has_feature = |needles: &[&str]| {
        features.as_ref().is_some_and(|features| {
            features
                .iter()
                .any(|feature| feature_matches_any(feature, needles))
        })
    };
    let mut input_modalities = item
        .architecture
        .as_ref()
        .and_then(|architecture| architecture.input_modalities.clone())
        .or_else(|| item.input_modalities.clone());
    let mut output_modalities = item
        .architecture
        .as_ref()
        .and_then(|architecture| architecture.output_modalities.clone())
        .or_else(|| item.output_modalities.clone());
    if input_modalities.is_none() {
        let supports_vision = has_feature(&["vision", "image", "image_input"]);
        let supports_audio = has_feature(&["audio", "audio_input"]);
        if supports_vision || supports_audio {
            let mut derived = vec!["text".to_string()];
            if supports_vision {
                derived.push("image".to_string());
            }
            if supports_audio {
                derived.push("audio".to_string());
            }
            input_modalities = Some(derived);
            output_modalities.get_or_insert_with(|| vec!["text".to_string()]);
        }
    }
    let modalities = if input_modalities.is_some() || output_modalities.is_some() {
        Some(ModelsDevModalities {
            input: input_modalities.unwrap_or_default(),
            output: output_modalities.unwrap_or_default(),
        })
    } else {
        None
    };
    let attachment = modalities.as_ref().map(|modalities| {
        modalities
            .input
            .iter()
            .any(|modality| !modality.eq_ignore_ascii_case("text") && !modality.trim().is_empty())
    });

    let feature_support = |needles: &[&str]| {
        features.as_ref().map(|features| {
            features
                .iter()
                .any(|feature| feature_matches_any(feature, needles))
        })
    };
    let reasoning = item
        .reasoning
        .or(item.supports_reasoning)
        .or_else(|| feature_support(&["reasoning", "include_reasoning"]));
    // Baseten's current Model APIs contract states every catalog model supports
    // tool calling and structured outputs. Explicit upstream booleans still
    // win if the endpoint publishes a narrower model-specific fact.
    // Baseten's Model APIs contract applies these two capabilities to every
    // catalog model. `supported_features` is additive and may list only
    // model-variable facts such as `reasoning` or `vision`; absence from that
    // list is therefore not an explicit false. Only an upstream boolean may
    // narrow the universal contract for a specific row.
    let tool_call = item.supports_tools.or(Some(true));
    let structured_output = item.supports_structured_output.or(Some(true));

    Ok(CatalogOffering {
        cost_source: (item.pricing.is_some() && cost.is_none()).then(|| CatalogSource::Live {
            base_url_fingerprint: base_url_fingerprint.to_string(),
            fetched_at,
        }),
        modalities_source: None,
        provider: provider.to_string(),
        wire_model_id: item.id.clone(),
        canonical_model: None,
        endpoint_key: "chat".to_string(),
        default_for_provider: item
            .id
            .eq_ignore_ascii_case(codewhale_config::catalog::BASETEN_DEFAULT_MODEL),
        family: None,
        limit,
        cost,
        modalities,
        attachment,
        reasoning,
        tool_call,
        structured_output,
        reasoning_options: item.reasoning_options.clone().unwrap_or_default(),
        reasoning_options_present: item.reasoning_options.is_some(),
        source: CatalogSource::Live {
            base_url_fingerprint: base_url_fingerprint.to_string(),
            fetched_at,
        },
        ..Default::default()
    })
}

#[cfg(test)]
fn publish_provider_lake_scope(cache: &ProviderCatalogCache, provider: &str, fingerprint: &str) {
    // Publish fresh *and* stale/prior rows so pickers keep live catalog coverage
    // after TTL expiry or a failed refresh (#4139). Exact replacement is
    // essential: a successful smaller roster must remove upstream-retired ids,
    // while a failure preserves the rows already stored in this cache scope.
    let offerings = cache
        .get(provider, fingerprint)
        .map(|entry| entry.offerings.clone())
        .unwrap_or_default();
    crate::provider_lake::replace_provider_live_snapshot(provider, CatalogSnapshot { offerings });
}

/// Convert an OpenRouter model item into a [`CatalogOffering`] with live-sourced
/// limits, pricing, reasoning, and modalities (#3385).
fn openrouter_to_catalog_offering(
    item: &OpenRouterModelItem,
    provider: &str,
    base_url_fingerprint: &str,
    fetched_at: u64,
) -> Result<CatalogOffering, CatalogRefreshError> {
    use codewhale_config::models_dev::{ModelsDevCost, ModelsDevLimit, ModelsDevModalities};

    let context_length = item
        .top_provider
        .as_ref()
        .and_then(|tp| tp.context_length)
        .or(item.context_length);

    let max_output = item
        .top_provider
        .as_ref()
        .and_then(|tp| tp.max_completion_tokens);

    let limit = if context_length.is_some() || max_output.is_some() {
        Some(ModelsDevLimit {
            context: context_length.map(u64::from),
            input: context_length.map(u64::from),
            output: max_output.map(u64::from),
        })
    } else {
        None
    };

    let cost = if let Some(p) = item.pricing.as_ref() {
        // OpenRouter quotes per-token USD strings; ModelsDevCost is per million.
        // Its routers (`openrouter/auto`, `openrouter/fusion`, ...) publish
        // `"-1"`: the price depends on the model the router picks. A negative
        // price therefore means "no fixed rate" for the whole row, never a
        // partial one.
        let parse_price = |value: &Option<String>| -> Result<Option<f64>, CatalogRefreshError> {
            value
                .as_ref()
                .map(|value| {
                    value
                        .trim()
                        .parse::<f64>()
                        .ok()
                        .filter(|value| value.is_finite())
                        .ok_or(CatalogRefreshError::InvalidResponse)
                })
                .transpose()
        };
        let raw = [
            parse_price(&p.prompt)?,
            parse_price(&p.completion)?,
            parse_price(&p.input_cache_read)?,
            parse_price(&p.input_cache_write)?,
        ];
        if raw.iter().flatten().any(|price| *price < 0.0) {
            None
        } else {
            let per_million =
                |price: Option<f64>| price.map(checked_per_token_to_per_million).transpose();
            let cost = ModelsDevCost {
                input: per_million(raw[0])?,
                output: per_million(raw[1])?,
                cache_read: per_million(raw[2])?,
                cache_write: per_million(raw[3])?,
                ..Default::default()
            };
            if !codewhale_config::pricing::catalog_cost_is_valid(&cost) {
                return Err(CatalogRefreshError::InvalidResponse);
            }
            Some(cost)
        }
    } else {
        None
    };

    let reasoning = item.supported_parameters.as_ref().map(|params| {
        params
            .iter()
            .any(|p| p == "reasoning" || p == "include_reasoning" || p.contains("reasoning"))
    });

    let tool_call = item.supported_parameters.as_ref().map(|params| {
        params
            .iter()
            .any(|p| p == "tools" || p == "tool_choice" || p == "functions" || p.contains("tool"))
    });

    let modalities = item.architecture.as_ref().map(|arch| {
        let mut input = arch.input_modalities.clone().unwrap_or_default();
        let mut output = arch.output_modalities.clone().unwrap_or_default();
        if input.is_empty()
            && output.is_empty()
            && let Some((left, right)) = arch
                .modality
                .as_deref()
                .and_then(|value| value.split_once("->"))
        {
            input.extend(
                left.split('+')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            );
            output.extend(
                right
                    .split('+')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
            );
        }
        ModelsDevModalities { input, output }
    });

    Ok(CatalogOffering {
        cost_source: (item.pricing.is_some() && cost.is_none()).then(|| CatalogSource::Live {
            base_url_fingerprint: base_url_fingerprint.to_string(),
            fetched_at,
        }),
        modalities_source: None,
        provider: provider.to_string(),
        wire_model_id: item.id.clone(),
        canonical_model: None,
        endpoint_key: "chat".to_string(),
        default_for_provider: false,
        family: None,
        limit,
        cost,
        modalities,
        attachment: None,
        reasoning,
        tool_call,
        structured_output: None,
        reasoning_options: Vec::new(),
        source: CatalogSource::Live {
            base_url_fingerprint: base_url_fingerprint.to_string(),
            fetched_at,
        },
        ..Default::default()
    })
}

/// The rate a main interactive turn freezes at its dispatch boundary (#6690).
/// An operator-declared `[[custom_models]]` rate wins, exactly as on the
/// background envelope path; `client` must be installed on the endpoint this
/// turn dispatches to, so a declaration never leaks onto another endpoint. A
/// declaration with no rates yields to the catalog price for that endpoint.
pub(crate) fn main_turn_pricing_quote_at(
    client: Option<&CodewhaleClient>,
    provider: ProviderKind,
    provider_identity: &str,
    model: &str,
    endpoint_fingerprint: &str,
    dispatched_at: u64,
) -> Option<crate::provider_catalog_live::ProviderLivePricingQuote> {
    let declared = client
        .filter(|client| {
            crate::cost_status::endpoint_fingerprint(client.base_url()).as_deref()
                == Some(endpoint_fingerprint)
        })
        .and_then(|client| {
            client.configured_pricing_quote_at(provider, provider_identity, model, dispatched_at)
        });
    crate::provider_catalog_live::declared_or_catalog_quote(declared, || {
        crate::provider_catalog_live::fresh_provider_live_pricing_quote_at(
            provider,
            provider_identity,
            model,
            endpoint_fingerprint,
            dispatched_at,
        )
    })
}

pub(super) fn system_to_instructions(system: Option<SystemPrompt>) -> Option<String> {
    match system {
        Some(SystemPrompt::Text(text)) => Some(text),
        Some(SystemPrompt::Blocks(blocks)) => {
            let joined = blocks
                .into_iter()
                .map(|b| b.text)
                .collect::<Vec<_>>()
                .join("\n\n---\n\n");
            if joined.trim().is_empty() {
                None
            } else {
                Some(joined)
            }
        }
        None => None,
    }
}

/// Write DeepSeek's Chat Completions thinking controls from the shared tier
/// table.
///
/// The table (`client::deepseek_effort`) is the only place the tier ladder is
/// written down; this function is just the Chat wire's spelling of it. An
/// effort string the table does not name is not a DeepSeek tier request, so
/// nothing is written rather than guessing a field the user did not ask for.
fn apply_deepseek_chat_reasoning_effort(body: &mut Value, normalized: &str) {
    let Some(tier) = deepseek_effort::deepseek_effort_tier(normalized) else {
        return;
    };
    if let Some(value) = tier.chat_reasoning_effort() {
        body["reasoning_effort"] = json!(value);
    }
    body["thinking"] = json!({
        "type": if tier.chat_thinking_enabled() { "enabled" } else { "disabled" },
    });
}

pub(super) fn apply_reasoning_effort(
    body: &mut Value,
    effort: Option<&str>,
    provider: ProviderKind,
) {
    let Some(effort) = effort else {
        return;
    };
    let normalized = effort.trim().to_ascii_lowercase();
    // DeepSeek's first-party routes read their tier ladder from the one
    // annotated table (`client::deepseek_effort`), shared with the Responses
    // wire, so a documented mapping change is a single edit. Every other
    // provider keeps its own dialect below.
    if matches!(provider, ProviderKind::Deepseek) {
        apply_deepseek_chat_reasoning_effort(body, &normalized);
        return;
    }
    if provider == ProviderKind::Stepfun {
        // Step 3.5 has no documented effort selector. The other coding models
        // are always reasoning-capable; do not invent an Off wire value.
        let model = body.get("model").and_then(Value::as_str).unwrap_or("");
        let tier = match (model, normalized.as_str()) {
            ("step-5-preview" | "step-3.7-flash" | "step-3.5-flash-2603", "minimal" | "low") => {
                Some("low")
            }
            ("step-5-preview" | "step-3.7-flash", "medium") => Some("medium"),
            ("step-3.5-flash-2603", "medium") => Some("high"),
            (
                "step-5-preview" | "step-3.7-flash" | "step-3.5-flash-2603",
                "high" | "max" | "xhigh" | "ultra",
            ) => Some("high"),
            _ => None,
        };
        if let Some(tier) = tier {
            body["reasoning_effort"] = json!(tier);
        }
        return;
    }
    match normalized.as_str() {
        "off" | "disabled" | "none" | "false" => match provider {
            // Handled by the shared DeepSeek table above, before this match.
            ProviderKind::Deepseek => {}
            ProviderKind::Openrouter
            | ProviderKind::Orcarouter
            | ProviderKind::XiaomiMimo
            | ProviderKind::Novita
            | ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Sglang
            | ProviderKind::Volcengine
            | ProviderKind::Deepinfra
            | ProviderKind::Together
            | ProviderKind::Atlascloud
            | ProviderKind::Zai => {
                body["thinking"] = json!({ "type": "disabled" });
            }
            // TelecomJS TokenHub: the gateway's OpenAI Chat Completions API
            // (POST /v1/chat/completions) does not document `reasoning_effort`
            // or `thinking` as supported parameters. The `thinking` field is
            // only available on the Anthropic Messages API (POST /v1/messages)
            // with a different shape ({"type":"enabled","budget_tokens":N}).
            // Since CodeWhale routes TelecomJS through the Chat Completions
            // path, we must NOT inject these fields — the gateway may silently
            // ignore them or reject the request, and not every gateway model
            // (qwen-max, deepseek-chat, gpt-4o, claude, etc.) accepts the same
            // reasoning dialect (#4188 review: verify against actual behavior).
            ProviderKind::Telecomjs => {}
            // Eden AI documents `thinking` only for Anthropic Claude models.
            // This gateway can route unrelated model families, so the generic
            // provider must not inject a model-specific reasoning dialect.
            ProviderKind::Edenai => {}
            ProviderKind::Zenmux => {}
            // CSDN 星图 is OpenAI-compatible but documents no provider-owned
            // reasoning dialect; do not invent one.
            ProviderKind::Csdn => {}
            // The Codewhale API is a passthrough to the account's own
            // connected provider; it documents no Codewhale-owned
            // reasoning-effort translation, so nothing is invented here.
            ProviderKind::Codewhale => {}
            // Concentrate rides the Responses wire (`reasoning.effort`), never
            // these Chat Completions controls.
            ProviderKind::Concentrate => {}
            // Model Studio (DashScope): its top-level controls are route- AND
            // model-specific, so the provider enum alone cannot decide them —
            // a custom `base_url` on the same identity is an arbitrary
            // gateway. `apply_modelstudio_route_reasoning_controls` in
            // client::chat is the sole writer; it strips these fields for all
            // four variants and re-adds them only on a verified Alibaba host.
            // Source: <https://www.alibabacloud.com/help/en/model-studio/deep-thinking>
            ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic => {}
            ProviderKind::OpenaiCodex => {
                // OpenAI Codex uses Responses API — thinking handled differently
            }
            ProviderKind::Fireworks => {}
            // vLLM is an OpenAI-protocol server, not an Anthropic-protocol one.
            // For Qwen3 / DeepSeek-R1 / other reasoning models hosted via vLLM,
            // the canonical OpenAI extension to disable thinking is
            // `chat_template_kwargs.enable_thinking`. The old
            // `thinking: {type: disabled}` field is Anthropic-native and
            // silently ignored by vLLM — the model still emits a full
            // reasoning trace into the `reasoning` field (which this client
            // doesn't surface), causing 10+ seconds of perceived "freeze"
            // before the first content token (PR #1480 by @h3c-hexin).
            ProviderKind::Vllm => {
                body["chat_template_kwargs"] = json!({
                    "enable_thinking": false,
                });
            }
            ProviderKind::Openai
            | ProviderKind::WanjieArk
            | ProviderKind::Qianfan
            | ProviderKind::Arcee
            | ProviderKind::Huggingface
            | ProviderKind::Modelscope
            | ProviderKind::Custom => {}
            ProviderKind::Moonshot => {
                // #3024: Kimi models accept thinking enable/disable.
                body["thinking"] = json!({ "type": "disabled" });
            }
            ProviderKind::Ollama => {
                // #3024: Ollama OpenAI-compat endpoint accepts think param.
                body["think"] = json!(false);
            }
            ProviderKind::OllamaCloud => {
                // Ollama Cloud stays on the documented OpenAI-compatible
                // `/v1/chat/completions` wire. Native `/api/chat` uses
                // `think`; this wire uses `reasoning_effort`.
                body["reasoning_effort"] = json!("none");
            }
            ProviderKind::Anthropic
            | ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::Openmodel => {
                // Thinking shaping happens in the Messages adapter, which
                // applies each provider's supported control fields.
            }
            ProviderKind::NvidiaNim => {
                body["chat_template_kwargs"] = json!({
                    "thinking": false,
                });
            }
            ProviderKind::Minimax => {}
            ProviderKind::Stepfun => {}
            ProviderKind::Sakana => {}
            ProviderKind::LongCat => {}
            ProviderKind::OpencodeGo | ProviderKind::OpencodeZen => {}
            ProviderKind::Meta => {}
            ProviderKind::Xai => {}
            ProviderKind::Mistral => {}
            ProviderKind::Google => {}
            ProviderKind::Antigravity => {}
        },
        "low" | "minimal" | "medium" | "mid" | "high" | "" => match provider {
            // Handled by the shared DeepSeek table above, before this match.
            ProviderKind::Deepseek => {}
            // DeepSeek-compatible hosted routes: low/medium both map to high.
            // Their own wire contracts are not verified here, so the historic
            // collapse stays rather than inventing unsupported wire values.
            ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Sglang
            | ProviderKind::Volcengine
            | ProviderKind::Deepinfra
            | ProviderKind::Atlascloud => {
                body["reasoning_effort"] = json!("high");
                body["thinking"] = json!({ "type": "enabled" });
            }
            // TelecomJS: see comment in the "off" branch above — the gateway's
            // Chat Completions API does not support reasoning_effort or thinking.
            ProviderKind::Telecomjs => {}
            ProviderKind::Edenai => {}
            ProviderKind::Zenmux => {}
            // CSDN 星图 is OpenAI-compatible but documents no provider-owned
            // reasoning dialect; do not invent one.
            ProviderKind::Csdn => {}
            // The Codewhale API is a passthrough to the account's own
            // connected provider; it documents no Codewhale-owned
            // reasoning-effort translation, so nothing is invented here.
            ProviderKind::Codewhale => {}
            // Concentrate rides the Responses wire (`reasoning.effort`), never
            // these Chat Completions controls.
            ProviderKind::Concentrate => {}
            // Model Studio: see the "off" branch — the route- and model-aware
            // shaper in client::chat is the sole writer of these fields.
            ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic => {}
            // OpenRouter/OrcaRouter/Novita/Together: pass through the actual
            // user-chosen value. OpenRouter's unified scale is
            // none/minimal/low/medium/high/xhigh; DeepSeek models hosted there
            // accept those directly.
            ProviderKind::Openrouter
            | ProviderKind::Orcarouter
            | ProviderKind::Novita
            | ProviderKind::Together => {
                let value = match normalized.as_str() {
                    "low" | "minimal" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                body["reasoning_effort"] = json!(value);
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::XiaomiMimo => {
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::Arcee | ProviderKind::Huggingface | ProviderKind::Modelscope => {
                let value = match normalized.as_str() {
                    "minimal" => "minimal",
                    "low" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                body["reasoning_effort"] = json!(value);
            }
            ProviderKind::Fireworks => {
                body["reasoning_effort"] = json!("high");
            }
            ProviderKind::Vllm => {
                body["chat_template_kwargs"] = json!({
                    "enable_thinking": true,
                });
                // vLLM supports low/medium/high natively — pass through the
                // user-chosen value instead of hard-coding "high".
                let value = match normalized.as_str() {
                    "low" | "minimal" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                body["reasoning_effort"] = json!(value);
            }
            ProviderKind::Openai
            | ProviderKind::WanjieArk
            | ProviderKind::Qianfan
            | ProviderKind::OpenaiCodex
            | ProviderKind::Custom => {}
            ProviderKind::Moonshot => {
                // #3024: Kimi models accept thinking enable.
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::Ollama => {
                // #3024: Ollama think param.
                body["think"] = json!(true);
            }
            ProviderKind::OllamaCloud => {
                let value = match normalized.as_str() {
                    "low" | "minimal" => "low",
                    "medium" | "mid" => "medium",
                    _ => "high",
                };
                body["reasoning_effort"] = json!(value);
            }
            ProviderKind::Anthropic
            | ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::Openmodel => {
                // Thinking shaping happens in the Messages adapter, which
                // applies each provider's supported control fields.
            }
            ProviderKind::NvidiaNim => {
                body["chat_template_kwargs"] = json!({
                    "thinking": true,
                    "reasoning_effort": "high",
                });
            }
            ProviderKind::Minimax => {}
            ProviderKind::Zai => {
                body["thinking"] = json!({
                    "type": "enabled",
                    "clear_thinking": false,
                });
            }
            ProviderKind::Stepfun => {}
            ProviderKind::Sakana => {}
            ProviderKind::LongCat => {}
            ProviderKind::OpencodeGo | ProviderKind::OpencodeZen => {}
            ProviderKind::Meta => {}
            ProviderKind::Xai => {}
            ProviderKind::Mistral => {}
            ProviderKind::Google => {}
            ProviderKind::Antigravity => {}
        },
        "xhigh" | "max" | "highest" | "ultra" | "ultracode" => match provider {
            // Handled by the shared DeepSeek table above, before this match.
            ProviderKind::Deepseek => {}
            ProviderKind::Siliconflow
            | ProviderKind::SiliconflowCN
            | ProviderKind::Sglang
            | ProviderKind::Volcengine
            | ProviderKind::Deepinfra
            | ProviderKind::Atlascloud => {
                body["reasoning_effort"] = json!("max");
                body["thinking"] = json!({ "type": "enabled" });
            }
            // TelecomJS: see comment in the "off" branch above — the gateway's
            // Chat Completions API does not support reasoning_effort or thinking.
            ProviderKind::Telecomjs => {}
            ProviderKind::Edenai => {}
            ProviderKind::Zenmux => {}
            // CSDN 星图 is OpenAI-compatible but documents no provider-owned
            // reasoning dialect; do not invent one.
            ProviderKind::Csdn => {}
            // The Codewhale API is a passthrough to the account's own
            // connected provider; it documents no Codewhale-owned
            // reasoning-effort translation, so nothing is invented here.
            ProviderKind::Codewhale => {}
            // Concentrate rides the Responses wire (`reasoning.effort`), never
            // these Chat Completions controls.
            ProviderKind::Concentrate => {}
            // Model Studio: see the "off" branch — the route- and model-aware
            // shaper in client::chat is the sole writer of these fields.
            ProviderKind::ModelstudioTokenPlan
            | ProviderKind::ModelstudioTokenPlanAnthropic
            | ProviderKind::ModelstudioCodingPlan
            | ProviderKind::ModelstudioCodingPlanAnthropic => {}
            ProviderKind::Openrouter
            | ProviderKind::Orcarouter
            | ProviderKind::Novita
            | ProviderKind::Together => {
                body["reasoning_effort"] = json!("xhigh");
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::XiaomiMimo => {
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::Arcee | ProviderKind::Huggingface | ProviderKind::Modelscope => {
                body["reasoning_effort"] = json!("high");
            }
            ProviderKind::Fireworks => {
                body["reasoning_effort"] = json!("max");
            }
            ProviderKind::Vllm => {
                body["chat_template_kwargs"] = json!({
                    "enable_thinking": true,
                });
                // vLLM only supports none/low/medium/high — downgrade
                // "max" to "high" instead of sending an invalid value.
                body["reasoning_effort"] = json!("high");
            }
            ProviderKind::Openai
            | ProviderKind::WanjieArk
            | ProviderKind::Qianfan
            | ProviderKind::OpenaiCodex
            | ProviderKind::Custom => {}
            ProviderKind::Moonshot => {
                // #3024: Kimi models accept thinking enable.
                body["thinking"] = json!({ "type": "enabled" });
            }
            ProviderKind::Ollama => {
                // #3024: Ollama think param.
                body["think"] = json!(true);
            }
            ProviderKind::OllamaCloud => {
                body["reasoning_effort"] = json!("max");
            }
            ProviderKind::Anthropic
            | ProviderKind::DeepseekAnthropic
            | ProviderKind::MinimaxAnthropic
            | ProviderKind::Openmodel => {
                // Thinking shaping happens in the Messages adapter, which
                // applies each provider's supported control fields.
            }
            ProviderKind::NvidiaNim => {
                body["chat_template_kwargs"] = json!({
                    "thinking": true,
                    "reasoning_effort": "max",
                });
            }
            ProviderKind::Minimax => {}
            ProviderKind::Zai => {
                body["thinking"] = json!({
                    "type": "enabled",
                    "clear_thinking": false,
                });
            }
            ProviderKind::Stepfun => {}
            ProviderKind::Sakana => {}
            ProviderKind::LongCat => {}
            ProviderKind::OpencodeGo | ProviderKind::OpencodeZen => {}
            ProviderKind::Meta => {}
            ProviderKind::Xai => {}
            ProviderKind::Mistral => {}
            ProviderKind::Google => {}
            ProviderKind::Antigravity => {}
        },
        _ => {}
    }
}

impl CodewhaleClient {
    pub(crate) fn can_request_fim_completion(&self) -> bool {
        self.api_provider != ProviderKind::OpencodeZen
            && self.wire_format == WireFormat::ChatCompletions
    }

    /// Call the DeepSeek `/beta/completions` FIM endpoint.
    pub async fn fim_completion(
        &self,
        model: &str,
        prompt: &str,
        suffix: &str,
        max_tokens: u32,
    ) -> anyhow::Result<String> {
        let _inference = self.acquire_remote_control_inference_permit().await;
        let _permit = self.acquire_provider_request_permit().await;
        if !self.can_request_fim_completion() {
            bail!(
                "FIM completion is not supported for {} because the route has no proven FIM wire contract ({:?})",
                self.api_provider.provider().display_name(),
                self.wire_format
            );
        }
        let url = api_url_with_suffix(&self.base_url, "beta/completions", None);
        let model = self.wire_model_for_route(model);
        let max_tokens = self.bounded_output_tokens(
            &model,
            self.route_limits_for_model(&model),
            max_tokens.min(self.effective_max_output_tokens(&model)),
            crate::compaction::estimate_text_tokens_conservative(prompt)
                .saturating_add(crate::compaction::estimate_text_tokens_conservative(suffix)),
        )?;
        let body = json!({
            "model": model,
            "prompt": prompt,
            "suffix": suffix,
            "max_tokens": max_tokens,
        });
        let response = self.send_json_with_retry(&url, &body).await?;
        let status = response.status();
        if !status.is_success() {
            let raw_error_text = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            let error_text = sanitize_http_error_body(
                Some(self.api_provider.provider().display_name()),
                status.as_u16(),
                &raw_error_text,
            );
            anyhow::bail!("FIM API error: HTTP {status}: {error_text}");
        }
        let response_text = response
            .text()
            .await
            .context("Failed to read FIM API response body")?;
        let value: serde_json::Value =
            serde_json::from_str(&response_text).context("Failed to parse FIM API response")?;
        let text = value
            .pointer("/choices/0/text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("FIM response missing choices[0].text"))?;
        Ok(text.to_string())
    }
}

mod anthropic;
mod chat;
mod deepseek_effort;
#[cfg(test)]
mod ds4_tests;
mod prepared;
mod provider_native_search;
mod responses;
mod role_placement;
mod stream_entry;
pub(crate) mod system_one;

/// Longest a request may take to open its stream and deliver the first body
/// byte before the client itself times out (#6184): the first-byte bound plus
/// two header waits, because a failed or stalled open on the dual client
/// retries once on the HTTP/1.1 twin under its own header wait. HTTP retries
/// inside one open attempt run within that attempt's header wait. The engine
/// heartbeat uses this as its awaiting-model bound, so it must cover the
/// fallback or an in-progress recovery reads as a stall (#6711).
#[must_use]
pub(crate) fn stream_first_response_bound(open: Duration, idle: Duration) -> Duration {
    open.saturating_mul(2)
        .saturating_add(stream_entry::first_byte_timeout(idle))
}

#[cfg(test)]
pub(crate) use stream_entry::first_byte_timeout as stream_first_byte_timeout;
pub(crate) use stream_entry::{is_stream_open_transport_failure, resolve_stream_open_timeout};
mod wire;

// Retain the crate-visible accounting helpers at the existing client seam.
pub(super) use wire::{parse_usage, saturating_u32};

#[cfg(test)]
pub(crate) fn anthropic_tool_result_content_for_test(
    content: &str,
    content_blocks: Option<&[Value]>,
) -> Value {
    anthropic::anthropic_tool_result_content(content, content_blocks)
}

#[cfg(test)]
pub(crate) fn responses_tool_output_for_test(
    content: &str,
    content_blocks: Option<&[Value]>,
) -> Value {
    responses::responses_tool_output(content, content_blocks)
}

#[cfg(test)]
pub(crate) fn chat_messages_for_test(messages: &[codewhale_models::Message]) -> Vec<Value> {
    chat::build_chat_messages(None, messages, "gpt-4o")
}

pub(crate) use chat::{
    CacheWarmupKey, PromptInspection, PromptLayerInspection, PromptLayerStability,
    ToolResultInspection, TurnMetaInspection,
};
pub(crate) use prepared::{
    CallerStreamMode, EndpointIdentity, PreparedOutboundRequest, RouteShape, WireBodyView,
    WireDialect, canonical_json,
};
pub(crate) use provider_native_search::{ProviderNativeSearchClient, ProviderNativeSearchRequest};

/// Whether a route speaks ordinary `/chat/completions` (not Messages or Responses).
#[must_use]
pub(crate) fn provider_speaks_chat_completions(api_provider: ProviderKind) -> bool {
    provider_default_wire_format(api_provider) == WireFormat::ChatCompletions
}

pub(crate) fn inspect_prompt_for_request(request: &MessageRequest) -> PromptInspection {
    chat::inspect_prompt_for_request(request)
}

pub(crate) fn build_cache_warmup_request(request: &MessageRequest) -> MessageRequest {
    chat::build_cache_warmup_request(request)
}

pub(crate) use chat::CACHE_WARMUP_MAX_TOKENS;
pub(crate) use chat::is_reasoning_replay_placeholder;

#[cfg(test)]
mod tests {
    include!("client/test_cases_01.rs");

    include!("client/test_cases_02.rs");

    include!("client/test_cases_03.rs");

    include!("client/test_cases_04.rs");

    include!("client/test_cases_05.rs");

    include!("client/test_cases_06.rs");

    include!("client/test_cases_07.rs");

    include!("client/test_cases_08.rs");
}

#[cfg(test)]
mod configured_model_client_tests {
    use super::*;
    use crate::config::{ProviderConfig, ProvidersConfig};

    fn config() -> Config {
        Config {
            provider: Some("deepseek".into()),
            providers: Some(ProvidersConfig {
                deepseek: ProviderConfig {
                    api_key: Some("configured-model-local-fixture".into()),
                    base_url: Some("https://api.deepseek.com".into()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            custom_models: Some(vec![
                toml::from_str(
                    r#"
                provider = "deepseek"
                base_url = "https://api.deepseek.com"
                id = "deepseek-v4pro"
                limit = { context = 96000, output = 32 }
                cost = { input = 1.0, output = 2.0 }
            "#,
                )
                .unwrap(),
            ]),
            ..Config::default()
        }
    }

    #[test]
    fn alternate_declared_model_freezes_wire_limits_and_price() {
        let _env = crate::test_support::lock_test_env();
        let mut config = config();
        let mut initial = config.custom_models.as_ref().unwrap()[0].clone();
        initial.id = "initial-model".into();
        initial.tool_call = Some(true);
        config.custom_models.as_mut().unwrap()[0].tool_call = Some(false);
        config.custom_models.as_mut().unwrap().push(initial);
        let client = CodewhaleClient::from_parts(
            "https://api.deepseek.com".into(),
            "initial-model".into(),
            WireFormat::ChatCompletions,
            None,
            &config,
        )
        .unwrap();
        let model = "deepseek-v4pro";
        config.providers.as_mut().unwrap().deepseek.model = Some(model.into());
        crate::config::normalize_model_config_for_test(&mut config);
        assert_eq!(config.default_model(), model);
        assert_eq!(
            config.providers.as_ref().unwrap().deepseek.model.as_deref(),
            Some(model)
        );
        config.custom_models.as_mut().unwrap()[0]
            .limit
            .as_mut()
            .unwrap()
            .output = Some(512);
        config.custom_models.as_mut().unwrap()[0]
            .cost
            .as_mut()
            .unwrap()
            .output = Some(99.0);
        config.custom_models.as_mut().unwrap()[0].tool_call = Some(true);

        assert_eq!(client.effective_max_output_tokens(model), 32);
        let prepared = client
            .prepare_outbound_request(
                translation_message_request("hello", model.into(), "English", 4096),
                false,
            )
            .unwrap();
        assert_eq!(prepared.wire_model, model);
        assert_eq!(prepared.body["model"], model);
        assert_eq!(prepared.body["max_tokens"], 32);
        let mut tool_request = translation_message_request("hello", model.into(), "English", 4096);
        tool_request.tools = Some(vec![codewhale_models::Tool {
            tool_type: None,
            name: "fixture".into(),
            description: "read-only fixture".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            allowed_callers: None,
            defer_loading: Some(false),
            input_examples: None,
            strict: Some(true),
            cache_control: None,
        }]);
        assert!(
            client
                .prepare_outbound_request(tool_request.clone(), false)
                .is_err()
        );
        let mut initial_request = tool_request.clone();
        initial_request.model = "initial-model".into();
        assert!(
            client
                .prepare_outbound_request(initial_request, false)
                .is_ok()
        );
        let rebound = client
            .rebound_for_model_protocol(Some(&config), model)
            .unwrap()
            .unwrap();
        assert_eq!(rebound.default_model, model);
        assert_eq!(rebound.route_limits.unwrap().output_tokens, Some(32));
        assert!(
            rebound
                .prepare_outbound_request(tool_request, false)
                .is_err()
        );
        let envelope = rebound.effective_route_envelope(model, chrono::Utc::now());
        assert_eq!(envelope.model, model);
        let quote = serde_json::to_value(envelope.provider_live_pricing.unwrap()).unwrap();
        assert_eq!(quote["output_per_million"], "2");

        let mut wrong_endpoint = client.clone();
        wrong_endpoint.base_url = "https://other.example.test/v1".into();
        assert_eq!(wrong_endpoint.declared_wire_model(model), None);
        assert_ne!(wrong_endpoint.effective_max_output_tokens(model), 32);
        let mut wrong_identity = client;
        wrong_identity.admitted_identity.key = "other-provider".into();
        assert_eq!(wrong_identity.declared_wire_model(model), None);
    }

    #[test]
    fn declaration_does_not_open_opencode_go_protocol_roster() {
        let _env = crate::test_support::lock_test_env();
        let mut config = config();
        config.provider = Some("opencode-go".into());
        config.providers.as_mut().unwrap().opencode_go = ProviderConfig {
            api_key: Some("configured-model-local-fixture".into()),
            ..ProviderConfig::default()
        };
        let base_url = ProviderKind::OpencodeGo.provider().default_base_url();
        let declaration = &mut config.custom_models.as_mut().unwrap()[0];
        declaration.provider = "opencode-go".into();
        declaration.base_url = base_url.into();
        declaration.id = "claude-sonnet-unproven".into();
        let client = CodewhaleClient::from_parts(
            base_url.into(),
            config.default_model(),
            WireFormat::ChatCompletions,
            None,
            &config,
        )
        .unwrap();
        assert_eq!(client.declared_wire_model("claude-sonnet-unproven"), None);
        assert!(
            client
                .rebound_for_model_protocol(None, "claude-sonnet-unproven")
                .is_err()
        );
        assert!(
            client
                .prepare_outbound_request(
                    translation_message_request(
                        "hello",
                        "claude-sonnet-unproven".into(),
                        "English",
                        64
                    ),
                    false,
                )
                .is_err()
        );
    }
}

#[cfg(test)]
mod openrouter_vendor_tests {
    use super::*;
    use crate::config::{OPENROUTER_QWEN_3_6_FLASH_MODEL, ProviderConfig, ProvidersConfig};
    use futures_util::StreamExt;
    use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};

    fn config(base_url: &str, vendor: Option<&str>) -> Config {
        Config {
            provider: Some("openrouter".into()),
            providers: Some(ProvidersConfig {
                openrouter: ProviderConfig {
                    api_key: Some("vendor-pin-local-fixture".into()),
                    base_url: Some(base_url.into()),
                    model: Some("deepseek/deepseek-v4-pro".into()),
                    vendor: vendor.map(str::to_string),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            }),
            ..Config::default()
        }
    }

    fn request() -> MessageRequest {
        translation_message_request("hello", "deepseek/deepseek-v4-pro".into(), "English", 64)
    }

    #[tokio::test]
    async fn openrouter_vendor_is_serialized_on_stream_blocking_and_translation_requests() {
        let _env = crate::test_support::lock_test_env();
        for streaming in [false, true] {
            let server = MockServer::start().await;
            let response = if streaming {
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string("data: [DONE]\n\n")
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "id": "chatcmpl-vendor-pin", "object": "chat.completion",
                    "model": "deepseek/deepseek-v4-pro",
                    "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
                }))
            };
            Mock::given(method("POST"))
                .respond_with(response)
                .expect(if streaming { 1 } else { 2 })
                .mount(&server)
                .await;
            let client =
                CodewhaleClient::new(&config(&server.uri(), Some("deepinfra/turbo"))).unwrap();
            let mut input = request();
            input.stream = Some(streaming);
            let preview = client
                .prepare_outbound_request(input.clone(), streaming)
                .unwrap();
            if streaming {
                let mut stream = client.create_message_stream(input).await.unwrap();
                while let Some(event) = stream.next().await {
                    event.unwrap();
                }
            } else {
                client.create_message(input).await.unwrap();
                assert_eq!(
                    client
                        .translate("hello", "deepseek/deepseek-v4-pro", "English")
                        .await
                        .unwrap(),
                    "ok"
                );
            }
            let captured = server.received_requests().await.unwrap();
            for outbound in &captured {
                let body: Value = serde_json::from_slice(&outbound.body).unwrap();
                assert_eq!(
                    body["provider"],
                    json!({"order": ["deepinfra/turbo"], "allow_fallbacks": false})
                );
                assert_eq!(body["provider"], preview.body["provider"]);
                assert_eq!(body["model"], "deepseek/deepseek-v4-pro");
            }
        }
    }

    #[test]
    fn openrouter_vendor_freezes_rebinds_and_partitions_cached_requests() {
        let _env = crate::test_support::lock_test_env();
        let initial = config("https://openrouter.ai/api/v1", Some("deepinfra/turbo"));
        let client = CodewhaleClient::new(&initial).unwrap();
        let mut updated = initial.clone();
        updated.providers.as_mut().unwrap().openrouter.vendor =
            Some("another-vendor/region".into());
        let fresh = CodewhaleClient::new(&updated).unwrap();
        let rebound = client
            .rebound_for_model_protocol(Some(&updated), OPENROUTER_QWEN_3_6_FLASH_MODEL)
            .unwrap()
            .unwrap();
        assert_eq!(rebound.openrouter_vendor(), Some("deepinfra/turbo"));
        assert_eq!(client.clone().openrouter_vendor(), Some("deepinfra/turbo"));
        let key = |client: &CodewhaleClient| {
            let body = client
                .prepare_outbound_request(request(), false)
                .unwrap()
                .body;
            crate::llm_response_cache::ResponseCache::make_key(
                "openrouter",
                &client.base_url,
                None,
                &client.api_key,
                &serde_json::to_vec(&body).unwrap(),
            )
        };
        assert_ne!(key(&client), key(&fresh));
        updated.providers.as_mut().unwrap().openrouter.vendor = Some(String::new());
        let cleared = CodewhaleClient::new(&updated).unwrap();
        assert!(
            cleared
                .prepare_outbound_request(request(), false)
                .unwrap()
                .body
                .get("provider")
                .is_none()
        );
        assert_ne!(key(&fresh), key(&cleared));
        assert_eq!(
            client.turn_route_receipt().openrouter_vendor(),
            Some("deepinfra/turbo")
        );
        assert!(
            CodewhaleClient::new(&config("https://openrouter.ai/api/v1", Some("bad vendor")))
                .is_err()
        );
        updated.provider = Some("openai".into());
        updated.providers.as_mut().unwrap().openai = ProviderConfig {
            api_key: Some("other-provider-fixture".into()),
            base_url: Some("https://openrouter.ai/api/v1".into()),
            model: Some("deepseek/deepseek-v4-pro".into()),
            ..ProviderConfig::default()
        };
        let other = CodewhaleClient::new(&updated).unwrap();
        assert!(
            other
                .prepare_outbound_request(request(), false)
                .unwrap()
                .body
                .get("provider")
                .is_none()
        );
    }
}

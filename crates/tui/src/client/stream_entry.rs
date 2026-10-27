//! Shared stream entry seam for Chat Completions / Anthropic Messages / Responses.
//!
//! Scoped consolidation for v0.9.1: wire-protocol adapters stay at the edge
//! (`chat.rs`, `anthropic.rs`, `responses.rs`); this module owns the common
//! open path, HTTP/1.1 fallback policy, and idle-timeout envelope so providers
//! do not re-implement transport differently.
//!
//! Full piagent-style provider collapse is deferred — see
//! `docs/notes/post-0.9.1-thin-tui-and-stream.md`.

use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use reqwest::Client;

use crate::llm_client::LlmError;

/// Default wait for the first body byte after the response headers (#6184).
/// Well under the 900s default inter-chunk idle budget: a provider that has
/// answered the headers and then sends nothing at all — not even an SSE
/// keep-alive — for five minutes has stopped, it is not thinking. Applies only
/// while the idle budget is the default; an explicitly configured
/// `stream_chunk_timeout_secs` is respected for the first byte too, so a user
/// who raised it for long silent reasoning keeps that allowance.
pub(crate) const DEFAULT_STREAM_FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(300);

/// Resolve the first-body-byte bound for a stream whose inter-chunk idle
/// budget is `idle`. `CODEWHALE_STREAM_FIRST_BYTE_TIMEOUT_SECS` overrides it
/// (clamped to 5..=3600).
#[must_use]
pub(crate) fn first_byte_timeout(idle: Duration) -> Duration {
    first_byte_timeout_from_env(
        idle,
        std::env::var("CODEWHALE_STREAM_FIRST_BYTE_TIMEOUT_SECS")
            .ok()
            .as_deref(),
    )
}

pub(crate) fn first_byte_timeout_from_env(idle: Duration, value: Option<&str>) -> Duration {
    if let Some(secs) = value.and_then(|v| v.trim().parse::<u64>().ok()) {
        return Duration::from_secs(secs.clamp(5, 3600));
    }
    let default_idle =
        Duration::from_secs(codewhale_runtime::config::DEFAULT_STREAM_CHUNK_TIMEOUT_SECS);
    if idle == default_idle {
        DEFAULT_STREAM_FIRST_BYTE_TIMEOUT.min(idle)
    } else {
        idle
    }
}

/// Bound for the next body read: the first-byte bound until any byte arrived,
/// the inter-chunk idle budget after.
#[must_use]
pub(crate) fn next_chunk_timeout(
    idle: Duration,
    first_byte: Duration,
    bytes_received: usize,
) -> Duration {
    if bytes_received == 0 {
        first_byte
    } else {
        idle
    }
}

/// Message and stall record for a body read that timed out. A first-byte
/// timeout is a stall worth a `crashes/` record (#6184); a later idle timeout
/// is reported the same way so every silent provider wait leaves a trace.
pub(crate) fn body_timeout_message(
    timeout: Duration,
    bytes_received: usize,
    stream_age: Duration,
    since_last_chunk: Duration,
    provider: &str,
) -> String {
    let message = if bytes_received == 0 {
        format!(
            "SSE stream first-byte timeout after {}s — the provider sent headers but no data \
             (stream_age_ms={})",
            timeout.as_secs(),
            stream_age.as_millis(),
        )
    } else {
        idle_timeout_message(timeout, bytes_received, stream_age, since_last_chunk)
    };
    let phase = if bytes_received == 0 {
        "waiting for the provider's first byte"
    } else {
        "waiting for the next stream chunk"
    };
    codewhale_runtime::stall_report::report_stall(&codewhale_runtime::stall_report::StallReport {
        source: "client",
        phase: format!("while {phase}"),
        detail: Some(provider.to_string()),
        turn_id: None,
        provider_request: None,
        since_progress: since_last_chunk,
        bound: Some(timeout),
    });
    message
}

/// How the shared stream open path should pin HTTP version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamHttpPolicy {
    /// Prefer the dual client (H2 primary, H1 twin for fallback).
    DualWithH1Fallback,
    /// Force HTTP/1.1 only (config/env pin or prior H2 stall).
    Http1Only,
}

/// Inputs shared by every streaming provider adapter at open time.
#[derive(Debug, Clone)]
pub struct StreamOpenRequest {
    pub policy: StreamHttpPolicy,
    pub open_timeout: Duration,
    pub idle_timeout: Duration,
}

impl StreamOpenRequest {
    /// `force_http1` is the client's resolved pin (`Config::force_http1`:
    /// `[stream].force_http1`, legacy `[tui]`, or `CODEWHALE_FORCE_HTTP1`), never re-read here.
    #[must_use]
    pub fn new(force_http1: bool, open_timeout: Duration, idle_timeout: Duration) -> Self {
        Self {
            policy: if force_http1 {
                StreamHttpPolicy::Http1Only
            } else {
                StreamHttpPolicy::DualWithH1Fallback
            },
            open_timeout,
            idle_timeout,
        }
    }

    /// After an H2 stall, retry on the HTTP/1.1 twin.
    #[must_use]
    pub fn with_h1_only(mut self) -> Self {
        self.policy = StreamHttpPolicy::Http1Only;
        self
    }
}

impl super::CodewhaleClient {
    /// The open request every streaming adapter starts from, carrying this
    /// client's resolved HTTP/1.1 pin and timeouts (#6700).
    #[must_use]
    pub(super) fn stream_open_request(&self) -> StreamOpenRequest {
        StreamOpenRequest::new(
            self.force_http1,
            self.stream_open_timeout,
            self.stream_idle_timeout,
        )
    }
}

/// Select the HTTP client for a stream open attempt.
#[must_use]
pub fn client_for_policy<'a>(
    primary: &'a Client,
    http1_fallback: &'a Client,
    policy: StreamHttpPolicy,
) -> &'a Client {
    match policy {
        StreamHttpPolicy::DualWithH1Fallback => primary,
        StreamHttpPolicy::Http1Only => http1_fallback,
    }
}

/// Whether a transport error should trigger H1 fallback retry.
#[must_use]
pub fn should_retry_with_h1(policy: StreamHttpPolicy, err_text: &str) -> bool {
    if policy != StreamHttpPolicy::DualWithH1Fallback {
        return false;
    }
    let lower = err_text.to_ascii_lowercase();
    lower.contains("http2")
        || lower.contains("h2 ")
        || lower.contains("stream closed")
        || lower.contains("connection reset")
        || lower.contains("protocol error")
        || lower.contains("frame size")
}

/// Whether an error raised before response headers should retry through the
/// HTTP/1.1 twin. Prefer typed transport errors, then retain the narrow text
/// classifier for lower-level H2 errors that reqwest exposes only as prose.
#[must_use]
fn should_retry_error_with_h1(policy: StreamHttpPolicy, err: &anyhow::Error) -> bool {
    if policy != StreamHttpPolicy::DualWithH1Fallback {
        return false;
    }

    typed_open_transport_failure(err)
        .unwrap_or_else(|| should_retry_with_h1(policy, &format!("{err:#}")))
}

/// Classify `err` by its types alone, searching the whole context chain: an
/// adapter's `.context("... request failed")` must not hide the transport
/// cause (#6711). `Some(true)` for a typed `LlmError::NetworkError`/`Timeout`
/// or a reqwest connect/timeout/request error that carries no HTTP status;
/// `Some(false)` for any other typed `LlmError` or reqwest error; `None` when
/// the chain holds neither type.
fn typed_open_transport_failure(err: &anyhow::Error) -> Option<bool> {
    err.chain().find_map(|cause| {
        if let Some(llm_error) = cause.downcast_ref::<LlmError>() {
            return Some(matches!(
                llm_error,
                LlmError::NetworkError(_) | LlmError::Timeout(_)
            ));
        }
        cause.downcast_ref::<reqwest::Error>().map(|reqwest_error| {
            reqwest_error.status().is_none()
                && (reqwest_error.is_connect()
                    || reqwest_error.is_timeout()
                    || reqwest_error.is_request())
        })
    })
}

/// Whether a failed stream open is a real transport failure: the request
/// never received response headers (#6699). Decided by type only: an untyped
/// error, such as an adapter's `HTTP 500 ...` rejection whose provider body
/// happens to mention a connection or timeout, is a provider answer and is
/// never classified as a transport failure here.
#[must_use]
pub(crate) fn is_stream_open_transport_failure(err: &anyhow::Error) -> bool {
    typed_open_transport_failure(err).unwrap_or(false)
}

/// Preserve provider-semantic failures returned by the H1 attempt. Only a
/// transport failure should be normalized into the shared retryable network
/// error; otherwise an auth or invalid-request failure could be retried as if
/// switching protocols had failed.
fn h1_fallback_error(err: anyhow::Error) -> anyhow::Error {
    if let Some(llm_error) = err.downcast_ref::<LlmError>()
        && !matches!(llm_error, LlmError::NetworkError(_) | LlmError::Timeout(_))
    {
        return err;
    }

    let detail = format!("{err:#}");
    if let Some(unreachable) = connect_failure_summary(&err) {
        return anyhow::Error::new(LlmError::NetworkError(format!("{unreachable}: {detail}")));
    }
    let mut message = format!("SSE stream request failed after HTTP/1.1 fallback: {detail}.");
    // The HTTP/1.1 hint only helps when the protocol or TLS layer failed; it
    // is noise for a refused connection or an unknown host.
    if has_tls_cause(&err) || is_protocol_or_tls_failure(&detail) {
        message.push_str(
            " `codewhale doctor` can still pass when non-streaming requests work; \
             on Windows or proxy networks, try `CODEWHALE_FORCE_HTTP1=1` and rerun `codewhale`.",
        );
    }
    anyhow::Error::new(LlmError::NetworkError(message))
}

/// `Cannot reach host:port (<why>)` for a failure to open the connection.
/// reqwest also reports TLS handshake and certificate failures as connect
/// errors; those return `None` so they keep the HTTP/1.1 and proxy hint.
fn connect_failure_summary(err: &anyhow::Error) -> Option<String> {
    let reqwest_error = err
        .chain()
        .find_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .filter(|error| error.is_connect())?;
    // Classify on the underlying causes only: reqwest's own message embeds
    // the request URL, whose host or path could contain "dns" or "tls".
    let mut causes = String::new();
    let mut source = std::error::Error::source(reqwest_error);
    while let Some(cause) = source {
        causes.push_str(&cause.to_string().to_ascii_lowercase());
        causes.push('\n');
        source = cause.source();
    }
    if has_tls_cause(err) || is_protocol_or_tls_failure(&causes) {
        return None;
    }
    let target = reqwest_error
        .url()
        .and_then(|url| {
            Some(format!(
                "{}:{}",
                url.host_str()?,
                url.port_or_known_default()?
            ))
        })
        .unwrap_or_else(|| "the provider host".to_string());
    let why = if causes.contains("refused") {
        "connection refused"
    } else if causes.contains("dns") || causes.contains("lookup") {
        "DNS lookup failed"
    } else {
        "connection failed"
    };
    // With a proxy configured, the host that refused may be the proxy.
    let via_proxy = [
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ]
    .iter()
    .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    Some(if via_proxy {
        format!("Cannot reach {target} or the configured proxy ({why})")
    } else {
        format!("Cannot reach {target} ({why})")
    })
}

fn has_tls_cause(err: &anyhow::Error) -> bool {
    err.chain().any(|mut cause| {
        // hyper-rustls wraps tokio-rustls's IO error in another IO error;
        // Error::source skips those inners, so inspect get_ref explicitly.
        while let Some(inner) = cause
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
        {
            cause = inner;
        }
        cause.is::<rustls::Error>()
    })
}

fn is_protocol_or_tls_failure(detail: &str) -> bool {
    let lower = detail.to_ascii_lowercase();
    should_retry_with_h1(StreamHttpPolicy::DualWithH1Fallback, &lower)
        || ["tls", "ssl", "certificate", "handshake", "alpn"]
            .iter()
            .any(|needle| lower.contains(needle))
}

/// Added to a response-header timeout (#6889). A provider that loads a model
/// on its first call can hold the headers back longer than this wait, and the
/// transport advice alone makes that read as a broken connection.
const COLD_MODEL_HINT: &str = "If this model has not been used recently, the provider may \
     still be loading it: wait a minute and send the message again, or allow a longer wait with \
     `codewhale config set stream.open_timeout_secs 180` (up to 300).";

/// Open an SSE response through the shared transport policy.
///
/// `attempt` builds and sends one wire-specific request on the client
/// selected for the given policy (via [`client_for_policy`]); everything
/// transport-shared lives here:
///
/// - the response-header wait is bounded by `open_req.open_timeout`;
/// - a classified transport failure or header stall on the dual client
///   retries exactly once on the HTTP/1.1 twin;
/// - a failure on an already H1-pinned request never retries;
/// - once response headers have been received the seam never retries —
///   body/stream errors belong to the adapter's decode loop.
pub(crate) async fn open_sse_response<F, Fut>(
    open_req: &StreamOpenRequest,
    attempt: F,
) -> Result<reqwest::Response>
where
    F: Fn(StreamHttpPolicy) -> Fut,
    Fut: Future<Output = Result<reqwest::Response>>,
{
    let fallback_reason = match tokio::time::timeout(
        open_req.open_timeout,
        attempt(open_req.policy),
    )
    .await
    {
        Ok(Ok(response)) => return Ok(response),
        Ok(Err(err)) => {
            if !should_retry_error_with_h1(open_req.policy, &err) {
                return Err(err);
            }
            "transport error before response headers"
        }
        Err(_elapsed) => {
            if open_req.policy == StreamHttpPolicy::Http1Only {
                return Err(anyhow::Error::new(LlmError::NetworkError(format!(
                    "SSE stream request did not receive response headers after {}s. \
                         `codewhale doctor` can still pass when non-streaming requests work; \
                         on Windows or proxy networks, try `CODEWHALE_FORCE_HTTP1=1` and rerun `codewhale`. \
                         {COLD_MODEL_HINT}",
                    open_req.open_timeout.as_secs()
                ))));
            }
            "response-header timeout"
        }
    };

    // No response body exists yet, so switching protocols and replaying the
    // request cannot corrupt stream state. It can still bill twice if the
    // provider accepted the first request (see the ambiguous-replay limit on
    // `llm_client::with_retry`). The policy guard above keeps this to exactly
    // one retry.
    let h1_req = open_req.clone().with_h1_only();
    codewhale_runtime::logging::warn(format!(
        "SSE stream {fallback_reason}; retrying once with HTTP/1.1"
    ));
    match tokio::time::timeout(h1_req.open_timeout, attempt(h1_req.policy)).await {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(err)) => Err(h1_fallback_error(err)),
        // Typed, not a bare string: a header stall is a transport
        // failure, and `LlmError::NetworkError` is what the shared
        // retry layer recognizes as retryable. As an untyped anyhow
        // error this killed the whole turn outright.
        Err(_elapsed) => Err(anyhow::Error::new(LlmError::NetworkError(format!(
            "SSE stream request did not receive response headers after {}s \
             (HTTP/2 and HTTP/1.1). `codewhale doctor` can still pass when \
             non-streaming requests work; try `CODEWHALE_FORCE_HTTP1=1` and \
             rerun `codewhale`. {COLD_MODEL_HINT}",
            open_req.open_timeout.as_secs()
        )))),
    }
}

/// Format a stable idle-timeout message shared across adapters.
#[must_use]
pub fn idle_timeout_message(
    idle: Duration,
    bytes_received: usize,
    stream_age: Duration,
    since_last_chunk: Duration,
) -> String {
    format!(
        "SSE stream idle timeout after {}s — no data received \
         (bytes_received={}, stream_age_ms={}, ms_since_last_chunk={})",
        idle.as_secs(),
        bytes_received,
        stream_age.as_millis(),
        since_last_chunk.as_millis(),
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use codewhale_config::{
        MAX_STREAM_OPEN_TIMEOUT_SECS, MIN_STREAM_OPEN_TIMEOUT_SECS, resolve_stream_open_timeout,
        stream_open_timeout_from_env,
    };

    fn open_req(policy: StreamHttpPolicy, open_timeout: Duration) -> StreamOpenRequest {
        StreamOpenRequest {
            policy,
            open_timeout,
            idle_timeout: Duration::from_secs(30),
        }
    }

    async fn ok_server() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;
        server
    }

    #[test]
    fn configured_http1_pin_controls_open_attempts_and_survives_clone() {
        let _env = crate::test_support::lock_test_env();
        let _codewhale = crate::test_support::EnvVarGuard::remove("CODEWHALE_FORCE_HTTP1");
        let _deepseek = crate::test_support::EnvVarGuard::remove("DEEPSEEK_FORCE_HTTP1");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for pinned in [true, false] {
            let config: codewhale_runtime::config::Config = toml::from_str(&format!(
                r#"
provider = "zai"
[providers.zai]
api_key = "stream-open-fixture-key"
[tui]
force_http1 = {pinned}
"#
            ))
            .unwrap();
            let client = super::super::CodewhaleClient::new(&config).unwrap();
            for client in [client.clone(), client] {
                let attempts = AtomicUsize::new(0);
                let result =
                    runtime.block_on(open_sse_response(&client.stream_open_request(), |policy| {
                        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                        async move {
                            if attempt == 0 {
                                // Only a genuinely dual-protocol request may
                                // retry this failure on its HTTP/1.1 twin.
                                return Err(anyhow::Error::new(LlmError::NetworkError(
                                    "connection reset before response headers".into(),
                                )));
                            }
                            assert_eq!(policy, StreamHttpPolicy::Http1Only);
                            Ok(reqwest::Response::from(
                                axum::http::Response::builder().status(200).body("")?,
                            ))
                        }
                    }));
                if pinned {
                    assert!(result.is_err(), "config pin must forbid a second send");
                    assert_eq!(attempts.load(Ordering::SeqCst), 1);
                } else {
                    assert_eq!(result.unwrap().status(), 200);
                    assert_eq!(attempts.load(Ordering::SeqCst), 2);
                }
            }
        }
    }

    #[tokio::test]
    async fn open_returns_first_attempt_response_on_dual_policy() {
        let server = ok_server().await;
        let client = codewhale_release::tls::reqwest_client();
        let attempts = Arc::new(AtomicUsize::new(0));
        let response = open_sse_response(
            &open_req(StreamHttpPolicy::DualWithH1Fallback, Duration::from_secs(5)),
            |policy| {
                assert_eq!(policy, StreamHttpPolicy::DualWithH1Fallback);
                let attempts = Arc::clone(&attempts);
                let client = client.clone();
                let url = server.uri();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Ok(client.post(url).send().await?)
                }
            },
        )
        .await
        .expect("first attempt succeeds");
        assert_eq!(response.status(), 200);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn header_stall_on_dual_policy_retries_exactly_once_on_h1() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let response = open_sse_response(
            &open_req(
                StreamHttpPolicy::DualWithH1Fallback,
                Duration::from_millis(150),
            ),
            |policy| {
                let attempts = Arc::clone(&attempts);
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        // First attempt stalls before response headers.
                        assert_eq!(policy, StreamHttpPolicy::DualWithH1Fallback);
                        std::future::pending::<()>().await;
                    }
                    assert_eq!(policy, StreamHttpPolicy::Http1Only);
                    // This test exercises retry policy, not loopback scheduling.
                    // A ready response keeps the 150 ms first-attempt timeout
                    // from also imposing a network deadline under suite load.
                    Ok(reqwest::Response::from(
                        axum::http::Response::builder().status(200).body("")?,
                    ))
                }
            },
        )
        .await
        .expect("H1 fallback retry succeeds");
        assert_eq!(response.status(), 200);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "exactly one fallback retry"
        );
    }

    #[tokio::test]
    async fn transport_error_before_headers_retries_exactly_once_on_h1() {
        let server = ok_server().await;
        let client = codewhale_release::tls::reqwest_client();
        let attempts = Arc::new(AtomicUsize::new(0));
        let response = open_sse_response(
            &open_req(StreamHttpPolicy::DualWithH1Fallback, Duration::from_secs(5)),
            |policy| {
                let attempts = Arc::clone(&attempts);
                let client = client.clone();
                let url = server.uri();
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        assert_eq!(policy, StreamHttpPolicy::DualWithH1Fallback);
                        return Err(anyhow::Error::new(LlmError::NetworkError(
                            "connection reset before response headers".to_string(),
                        ))
                        .context("Chat API request failed"));
                    }
                    assert_eq!(policy, StreamHttpPolicy::Http1Only);
                    Ok(client.post(url).send().await?)
                }
            },
        )
        .await
        .expect("H1 fallback retry succeeds after a transport error");
        assert_eq!(response.status(), 200);
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "exactly one fallback retry"
        );
    }

    #[tokio::test]
    async fn header_stall_when_h1_pinned_never_retries_and_reports_timeout_text() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let err = open_sse_response(
            &open_req(StreamHttpPolicy::Http1Only, Duration::from_millis(100)),
            |_| {
                let attempts = Arc::clone(&attempts);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<()>().await;
                    unreachable!("stalled attempt never resolves")
                }
            },
        )
        .await
        .expect_err("H1-pinned stall fails without retry");
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "no retry when pinned");
        let text = err.to_string();
        assert!(text.contains("did not receive response headers"), "{text}");
        // The whole point of the typed error: a header stall must reach the
        // shared retry layer as retryable. As an untyped anyhow error it
        // killed the turn outright, so a long root run lost all its work while
        // a sub-agent — which text-matches the same message in its own
        // classifier — would have retried and continued.
        let classified = err
            .downcast_ref::<crate::llm_client::LlmError>()
            .expect("header stall must be a typed LlmError");
        assert!(
            classified.is_retryable(),
            "header stall must be retryable: {classified:?}"
        );
        assert!(text.contains("CODEWHALE_FORCE_HTTP1=1"), "{text}");
        // #6889: a header wait that ran out may be a model still loading.
        assert!(text.contains("may still be loading it"), "{text}");
        assert!(
            !text.contains("HTTP/2 and HTTP/1.1"),
            "single-protocol stall must not claim a dual-protocol attempt: {text}"
        );
    }

    #[tokio::test]
    async fn transport_error_when_h1_pinned_never_retries() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let err = open_sse_response(
            &open_req(StreamHttpPolicy::Http1Only, Duration::from_secs(5)),
            |_| {
                let attempts = Arc::clone(&attempts);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err(anyhow::Error::new(LlmError::NetworkError(
                        "connection reset before response headers".to_string(),
                    )))
                }
            },
        )
        .await
        .expect_err("an H1-pinned transport error must not retry");
        assert_eq!(attempts.load(Ordering::SeqCst), 1, "no retry when pinned");
        assert!(err.to_string().contains("connection reset"), "{err}");
    }

    #[tokio::test]
    async fn refused_connection_names_the_host_and_skips_the_http1_hint() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("addr").port()
        };
        let client = codewhale_release::tls::reqwest_client();
        let url = format!("http://127.0.0.1:{port}/v1/chat");
        let err = open_sse_response(
            &open_req(StreamHttpPolicy::DualWithH1Fallback, Duration::from_secs(5)),
            |_| {
                let request = client.post(&url);
                async move { Ok::<_, anyhow::Error>(request.send().await?) }
            },
        )
        .await
        .expect_err("nothing listens on the port");
        let text = err.to_string();
        assert!(
            text.contains(&format!("Cannot reach 127.0.0.1:{port}"))
                && text.contains("(connection refused)"),
            "{text}"
        );
        assert!(!text.contains("CODEWHALE_FORCE_HTTP1"), "{text}");
        assert!(
            err.downcast_ref::<LlmError>()
                .is_some_and(LlmError::is_retryable),
            "{err:#}"
        );
    }

    #[tokio::test]
    async fn tls_failure_during_connect_keeps_the_http1_hint() {
        // A plain-TCP peer on an https URL fails the TLS handshake, which
        // reqwest reports as a connect error; it must not read as
        // "Cannot reach" and must keep the FORCE_HTTP1/proxy hint.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut socket, _)) = listener.accept().await {
                let _ = socket.write_all(b"HTTP/1.1 400 Not TLS\r\n\r\n").await;
                // Keep reading until the client closes. Dropping a socket with
                // unread ClientHello bytes can send a TCP reset on Windows,
                // hiding the TLS protocol error this fixture is meant to test.
                let _ = tokio::io::copy(&mut socket, &mut tokio::io::sink()).await;
            }
        });
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let client = codewhale_release::tls::reqwest_client();
        let url = format!("https://127.0.0.1:{port}/v1/chat");
        let err = open_sse_response(
            &open_req(StreamHttpPolicy::DualWithH1Fallback, Duration::from_secs(5)),
            |_| {
                let request = client.post(&url);
                async move { Ok::<_, anyhow::Error>(request.send().await?) }
            },
        )
        .await
        .expect_err("the peer does not speak TLS");
        server.abort();
        let text = err.to_string();
        assert!(!text.contains("Cannot reach"), "{text}");
        assert!(text.contains("CODEWHALE_FORCE_HTTP1=1"), "{text}");
    }

    #[test]
    fn h1_fallback_hint_follows_protocol_errors_with_the_full_chain() {
        let protocol = h1_fallback_error(
            anyhow::Error::new(LlmError::NetworkError("http2 protocol error".to_string()))
                .context("sending stream request"),
        );
        let text = protocol.to_string();
        assert!(text.contains("sending stream request: "), "{text}");
        assert!(text.contains("http2 protocol error"), "{text}");
        assert!(text.contains("CODEWHALE_FORCE_HTTP1=1"), "{text}");

        let other = h1_fallback_error(anyhow::Error::new(LlmError::NetworkError(
            "unexpected end of body".to_string(),
        )));
        assert!(
            !other.to_string().contains("CODEWHALE_FORCE_HTTP1"),
            "{other}"
        );
    }

    #[tokio::test]
    async fn provider_error_from_h1_fallback_keeps_its_semantic_type() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let err = open_sse_response(
            &open_req(StreamHttpPolicy::DualWithH1Fallback, Duration::from_secs(5)),
            |policy| {
                let attempts = Arc::clone(&attempts);
                async move {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        assert_eq!(policy, StreamHttpPolicy::DualWithH1Fallback);
                        return Err(anyhow::Error::new(LlmError::NetworkError(
                            "connection reset before response headers".to_string(),
                        )));
                    }
                    assert_eq!(policy, StreamHttpPolicy::Http1Only);
                    Err(anyhow::Error::new(LlmError::InvalidRequest {
                        status: 400,
                        message: "invalid request".to_string(),
                    }))
                }
            },
        )
        .await
        .expect_err("the H1 provider error must be returned");
        assert_eq!(attempts.load(Ordering::SeqCst), 2, "one fallback attempt");
        assert!(
            matches!(
                err.downcast_ref::<LlmError>(),
                Some(LlmError::InvalidRequest { status: 400, .. })
            ),
            "provider error was reclassified: {err:#}"
        );
    }

    #[tokio::test]
    async fn attempt_error_before_headers_is_not_h1_retried() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let err = open_sse_response(
            &open_req(StreamHttpPolicy::DualWithH1Fallback, Duration::from_secs(5)),
            |_| {
                let attempts = Arc::clone(&attempts);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err(anyhow::anyhow!("HTTP 401: invalid api key"))
                }
            },
        )
        .await
        .expect_err("provider error propagates");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "non-stall errors are never H1-retried"
        );
        assert!(err.to_string().contains("HTTP 401"), "{err}");
    }

    #[tokio::test]
    async fn double_stall_reports_both_protocols_in_timeout_text() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let err = open_sse_response(
            &open_req(
                StreamHttpPolicy::DualWithH1Fallback,
                Duration::from_millis(100),
            ),
            |_| {
                let attempts = Arc::clone(&attempts);
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    std::future::pending::<()>().await;
                    unreachable!("stalled attempt never resolves")
                }
            },
        )
        .await
        .expect_err("double stall fails");
        assert_eq!(attempts.load(Ordering::SeqCst), 2, "one fallback, no more");
        let text = err.to_string();
        assert!(text.contains("HTTP/2 and HTTP/1.1"), "{text}");
        // #6889: a header wait that ran out may be a model still loading.
        assert!(text.contains("may still be loading it"), "{text}");
        assert!(text.contains("stream.open_timeout_secs"), "{text}");
    }

    #[test]
    fn h1_retry_only_on_dual_policy() {
        assert!(should_retry_with_h1(
            StreamHttpPolicy::DualWithH1Fallback,
            "http2 protocol error"
        ));
        assert!(!should_retry_with_h1(
            StreamHttpPolicy::Http1Only,
            "http2 protocol error"
        ));
    }

    #[test]
    fn stall_first_byte_timeout_is_well_under_default_idle_budget() {
        let default_idle =
            Duration::from_secs(codewhale_runtime::config::DEFAULT_STREAM_CHUNK_TIMEOUT_SECS);
        let first_byte = first_byte_timeout_from_env(default_idle, None);
        assert_eq!(first_byte, DEFAULT_STREAM_FIRST_BYTE_TIMEOUT);
        assert!(
            first_byte * 3 <= default_idle,
            "{first_byte:?} vs {default_idle:?}"
        );
        // An explicitly configured idle budget is respected for the first byte.
        let custom = Duration::from_secs(1800);
        assert_eq!(first_byte_timeout_from_env(custom, None), custom);
        assert_eq!(
            first_byte_timeout_from_env(Duration::from_secs(60), None),
            Duration::from_secs(60)
        );
        assert_eq!(
            first_byte_timeout_from_env(default_idle, Some("90")),
            Duration::from_secs(90)
        );
        assert_eq!(next_chunk_timeout(default_idle, first_byte, 0), first_byte);
        assert_eq!(
            next_chunk_timeout(default_idle, first_byte, 1),
            default_idle
        );
    }

    #[test]
    fn stream_open_timeout_defaults_and_clamps_env_values() {
        assert_eq!(stream_open_timeout_from_env(None), Duration::from_secs(45));
        assert_eq!(
            stream_open_timeout_from_env(Some("not-a-number")),
            Duration::from_secs(45)
        );
        assert_eq!(
            stream_open_timeout_from_env(Some("1")),
            Duration::from_secs(5)
        );
        assert_eq!(
            stream_open_timeout_from_env(Some("120")),
            Duration::from_secs(120)
        );
        assert_eq!(
            stream_open_timeout_from_env(Some("999")),
            Duration::from_secs(300)
        );
    }

    #[test]
    fn configured_stream_open_timeout_wins_and_clamps() {
        // Positive config values never consult the env, so these are
        // deterministic regardless of the test process environment.
        assert_eq!(
            resolve_stream_open_timeout(Some(90)),
            Duration::from_secs(90)
        );
        assert_eq!(
            resolve_stream_open_timeout(Some(1)),
            Duration::from_secs(MIN_STREAM_OPEN_TIMEOUT_SECS)
        );
        assert_eq!(
            resolve_stream_open_timeout(Some(u64::MAX)),
            Duration::from_secs(MAX_STREAM_OPEN_TIMEOUT_SECS)
        );
    }

    #[test]
    fn idle_message_is_stable() {
        let msg = idle_timeout_message(
            Duration::from_secs(30),
            0,
            Duration::from_secs(30),
            Duration::from_secs(30),
        );
        assert!(msg.contains("idle timeout"));
        assert!(msg.contains("bytes_received=0"));
    }
}

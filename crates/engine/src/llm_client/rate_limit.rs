//! Recovery policy for a refused model operation. Generation time is not a retry budget.

use std::time::Duration;

use super::{LlmError, RateLimitRetryPolicy};

#[derive(Default)]
struct Clock {
    recovery_started: Option<tokio::time::Instant>,
    phase: u64,
    waited: Duration,
    waiting: Option<tokio::time::Instant>,
}

tokio::task_local! {
    static RECOVERY_CLOCK: std::sync::Arc<std::sync::Mutex<Clock>>;
}

struct PauseGuard(std::sync::Arc<std::sync::Mutex<Clock>>);
impl Drop for PauseGuard {
    fn drop(&mut self) {
        let mut clock = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(started) = clock.waiting.take() {
            clock.waited += started.elapsed();
        }
    }
}

/// Establish the transport clock when polled, so an enclosing active deadline
/// and the complete streaming-open retry loop observe the same wait ledger.
pub fn with_recovery_clock<F: std::future::Future>(
    future: F,
) -> std::pin::Pin<Box<impl std::future::Future<Output = F::Output>>> {
    Box::pin(async move {
        let clock = RECOVERY_CLOCK.try_with(Clone::clone).unwrap_or_default();
        RECOVERY_CLOCK.scope(clock, future).await
    })
}

/// Admission and the retry loop share the same transport-local recovery clock.
pub fn note_recovery_started(started: Option<tokio::time::Instant>) {
    let _ = RECOVERY_CLOCK.try_with(|clock| {
        clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .recovery_started = started;
    });
}

pub fn remaining_recovery_budget(policy: &RateLimitRetryPolicy) -> Duration {
    let elapsed = RECOVERY_CLOCK
        .try_with(|clock| {
            clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recovery_started
                .map_or(Duration::ZERO, |started| started.elapsed())
        })
        .unwrap_or_default();
    Duration::from_secs(policy.recovery_budget_secs).saturating_sub(elapsed)
}

pub fn new_attempt() {
    let _ = RECOVERY_CLOCK.try_with(|clock| {
        let mut clock = clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        clock.phase = clock.phase.wrapping_add(1);
    });
}

pub async fn without_active_time<F: std::future::Future>(future: F) -> F::Output {
    let _pause = RECOVERY_CLOCK
        .try_with(|clock| {
            clock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .waiting = Some(tokio::time::Instant::now());
            PauseGuard(clock.clone())
        })
        .ok();
    future.await
}

pub async fn wait(delay: Duration) {
    without_active_time(tokio::time::sleep(delay)).await;
}

/// Bound active work while a provider-directed backoff belongs to its own recovery budget.
pub fn active_timeout<F: std::future::Future>(
    duration: Duration,
    future: F,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<F::Output, ()>>>> {
    active_timeout_policy(duration, future, true)
}

pub fn active_timeout_total<F: std::future::Future>(
    duration: Duration,
    future: F,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<F::Output, ()>>>> {
    active_timeout_policy(duration, future, false)
}

fn active_timeout_policy<F: std::future::Future>(
    duration: Duration,
    future: F,
    reset_on_attempt: bool,
) -> std::pin::Pin<Box<impl std::future::Future<Output = Result<F::Output, ()>>>> {
    let clock = RECOVERY_CLOCK.try_with(Clone::clone).unwrap_or_default();
    Box::pin(RECOVERY_CLOCK.scope(clock.clone(), async move {
        let mut start = tokio::time::Instant::now();
        let mut baseline = clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .waited;
        let mut phase = clock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .phase;
        tokio::pin!(future);
        loop {
            let waiting = {
                let clock = clock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if reset_on_attempt && clock.phase != phase {
                    phase = clock.phase;
                    start = tokio::time::Instant::now();
                    baseline = clock.waited;
                }
                clock.waited.saturating_sub(baseline)
                    + clock.waiting.map_or(Duration::ZERO, |t| t.elapsed())
            };
            let remaining = duration.saturating_sub(start.elapsed().saturating_sub(waiting));
            if remaining.is_zero() {
                return Err(());
            }
            tokio::select! {
                biased;
                result = &mut future => return Ok(result),
                () = tokio::time::sleep(remaining.min(Duration::from_millis(100))) => {}
            }
        }
    }))
}

pub fn server_delay(value: &str) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    if let Ok(seconds) = value.parse::<f64>() {
        return Duration::try_from_secs_f64(seconds).ok();
    }
    chrono::DateTime::parse_from_rfc2822(value)
        .ok()
        .map(|date| {
            (date.with_timezone(&chrono::Utc) - chrono::Utc::now())
                .to_std()
                .unwrap_or_default()
        })
}

/// Some native providers report logical failures in a successful JSON envelope.
/// SSE bodies remain streaming and are owned by their existing recovery path.
#[cfg(test)]
#[allow(
    clippy::result_large_err,
    reason = "Keep the shared transport typed LlmError contract"
)]
async fn inspect_json_envelope(
    provider: &str,
    response: reqwest::Response,
) -> Result<reqwest::Response, LlmError> {
    inspect_json_envelope_with_disclosure(provider, response, None, |status, raw| {
        super::sanitize_http_error_body(Some(provider), status, raw)
    })
    .await
}

#[allow(
    clippy::result_large_err,
    reason = "Keep the shared transport typed LlmError contract"
)]
pub async fn inspect_json_envelope_with_disclosure<F>(
    provider: &str,
    mut response: reqwest::Response,
    body_timeouts: Option<(Duration, Duration)>,
    disclose: F,
) -> Result<reqwest::Response, LlmError>
where
    F: Fn(u16, &str) -> String + Send,
{
    if !response.status().is_success()
        || !matches!(provider, "minimax" | "minimax-anthropic" | "zai")
        || !response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/json"))
            })
    {
        return Ok(response);
    }
    let status = response.status();
    let version = response.version();
    let headers = response.headers().clone();
    let retry_after = super::extract_retry_after(&headers);
    const MAX_ENVELOPE_BYTES: usize = 32 * 1024 * 1024;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ENVELOPE_BYTES as u64)
    {
        return Err(LlmError::ParseError(
            "Provider JSON envelope exceeds 32 MiB".into(),
        ));
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = if let Some((first_byte, idle)) = body_timeouts {
            let timeout = if bytes.is_empty() { first_byte } else { idle };
            tokio::time::timeout(timeout, response.chunk())
                .await
                .map_err(|_| LlmError::Timeout(timeout))?
        } else {
            response.chunk().await
        }
        .map_err(|error| LlmError::from_reqwest(&error))?;
        let Some(chunk) = chunk else { break };
        if chunk.len() > MAX_ENVELOPE_BYTES.saturating_sub(bytes.len()) {
            return Err(LlmError::ParseError(
                "Provider JSON envelope exceeds 32 MiB".into(),
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let raw = String::from_utf8_lossy(&bytes);
    if let Ok(body) = serde_json::from_str::<serde_json::Value>(&raw) {
        let failed = body
            .pointer("/base_resp/status_code")
            .and_then(serde_json::Value::as_i64)
            .is_some_and(|code| code != 0)
            || body.get("error").is_some_and(|error| !error.is_null());
        if failed {
            let safe = disclose(status.as_u16(), &raw);
            return Err(
                provider_error(provider, status.as_u16(), &raw, &safe, retry_after).unwrap_or(
                    LlmError::InvalidRequest {
                        status: status.as_u16(),
                        message: safe,
                    },
                ),
            );
        }
    }
    let mut rebuilt = http::Response::new(reqwest::Body::from(bytes));
    *rebuilt.status_mut() = status;
    *rebuilt.version_mut() = version;
    *rebuilt.headers_mut() = headers;
    Ok(reqwest::Response::from(rebuilt))
}

pub fn body_delay(body: &str) -> Option<Duration> {
    let body: serde_json::Value = serde_json::from_str(body).ok()?;
    let objects = [
        &body,
        body.get("error").unwrap_or(&body),
        body.get("base_resp").unwrap_or(&body),
    ];
    let mut delay = None;
    for object in objects {
        for key in ["retry_after", "retry_after_seconds", "retryDelay"] {
            if let Some(value) = object.get(key) {
                let text = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string());
                delay = delay.max(server_delay(text.trim_end_matches('s')));
            }
        }
        for key in ["next_flush_time", "reset_time", "reset_at"] {
            if let Some(value) = object.get(key) {
                let text = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| value.to_string());
                let reset = text
                    .parse::<i64>()
                    .ok()
                    .and_then(|n| {
                        if n > 10_000_000_000 {
                            chrono::DateTime::from_timestamp_millis(n)
                        } else {
                            chrono::DateTime::from_timestamp(n, 0)
                        }
                    })
                    .or_else(|| {
                        chrono::DateTime::parse_from_rfc3339(&text)
                            .ok()
                            .map(|d| d.with_timezone(&chrono::Utc))
                    });
                delay =
                    delay.max(reset.map(|d| (d - chrono::Utc::now()).to_std().unwrap_or_default()));
            }
        }
    }
    delay
}

fn normalized_error_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn independent_billing_evidence(body: &serde_json::Value, raw: &str, bounded_hint: bool) -> bool {
    let coded = [
        "/error/code",
        "/error/type",
        "/error/error_code",
        "/code",
        "/type",
        "/error_code",
    ]
    .into_iter()
    .filter_map(|p| body.pointer(p).and_then(serde_json::Value::as_str))
    .any(|code| {
        normalized_error_code(code) != "insufficientquota" && super::is_explicit_quota_code(code)
    });
    // Reuse the legacy durable-message contract, excluding only the ambiguous
    // code. Check every supported message field so a gateway's generic class
    // cannot hide an explicit billing detail. Decode JSON Unicode escapes too.
    coded
        || std::iter::once(raw)
            .chain(
                [
                    "/error/message",
                    "/error",
                    "/message",
                    "/detail",
                    "/error_description",
                ]
                .into_iter()
                .filter_map(|p| body.pointer(p).and_then(serde_json::Value::as_str)),
            )
            .any(|message| {
                let message = message.to_ascii_lowercase();
                super::has_explicit_quota_phrase(&message, bounded_hint)
                    || [
                        "billing hard limit reached",
                        "billing limit reached",
                        "billing limit has been reached",
                        "credit balance too low",
                        "insufficient credits",
                        "out of credits",
                        "insufficient balance",
                        "insufficient funds",
                        "payment required",
                        "subscription expired",
                        "subscription has expired",
                        "\u{4f59}\u{989d}\u{4e0d}\u{8db3}",
                        "\u{4f59}\u{989d}\u{5df2}\u{8017}\u{5c3d}",
                        "\u{6b20}\u{8d39}",
                        "\u{8ba2}\u{9605}\u{5df2}\u{8fc7}\u{671f}",
                    ]
                    .iter()
                    .any(|phrase| message.contains(phrase))
            })
}

/// Interpret native codes before sanitization; unknown 429s remain bounded.
pub fn provider_error(
    provider: &str,
    status: u16,
    raw: &str,
    safe: &str,
    hint: Option<Duration>,
) -> Option<LlmError> {
    let hint = hint
        .max(body_delay(raw))
        .max(super::retry_delay_from_error_body(raw));
    let body: serde_json::Value = serde_json::from_str(raw).unwrap_or(serde_json::Value::Null);
    let code = [
        "/error/code",
        "/error/type",
        "/error/error_code",
        "/base_resp/status_code",
        "/code",
        "/type",
        "/error_code",
    ]
    .into_iter()
    .find_map(|p| body.pointer(p));
    let code = code
        .map(|code| {
            code.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| code.to_string())
        })
        .unwrap_or_default();
    let provider = provider.to_ascii_lowercase();
    let code_lower = code.to_ascii_lowercase();
    let window = match provider.as_str() {
        "minimax" | "minimax-anthropic" | "minimax_anthropic" => {
            matches!(code.as_str(), "2045" | "2046" | "2047")
        }
        "glm" | "zai" | "zhipu" => matches!(
            code.as_str(),
            "1308" | "1310" | "1316" | "1317" | "1318" | "1319" | "1320" | "1321"
        ),
        _ => false,
    };
    if window {
        return Some(LlmError::QuotaWindow {
            message: safe.to_owned(),
            retry_after: hint,
            transmission: super::RequestTransmission::Rejected,
        });
    }
    if (matches!(status, 400 | 402 | 403 | 429) || (200..300).contains(&status))
        && independent_billing_evidence(&body, raw, true)
    {
        return Some(LlmError::QuotaExhausted(
            super::QuotaExhaustionError::from_http_message(safe.to_owned()),
        ));
    }
    // This code means billing on OpenAI, but RPM/TPM on other surfaces.
    // An unknown relay must not turn that ambiguous code alone into a
    // permanent account failure. Independent billing evidence still wins.
    let raw_lower = raw.to_ascii_lowercase();
    let marked = raw_lower
        .split_once("provider error code:")
        .map(|(_, tail)| {
            tail.trim_start()
                .split(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-')))
                .next()
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let ambiguous_quota = status == 429
        && !matches!(
            provider.as_str(),
            "openai" | "openai-codex" | "openai_codex"
        )
        && ([
            "/error/code",
            "/error/type",
            "/error/error_code",
            "/code",
            "/type",
            "/error_code",
        ]
        .into_iter()
        .filter_map(|p| body.pointer(p).and_then(serde_json::Value::as_str))
        .any(|code| normalized_error_code(code) == "insufficientquota")
            || normalized_error_code(marked) == "insufficientquota");
    if ambiguous_quota && independent_billing_evidence(&body, raw, hint.is_some()) {
        return Some(LlmError::QuotaExhausted(
            super::QuotaExhaustionError::from_http_message(safe.to_owned()),
        ));
    }
    let transient = match provider.as_str() {
        "qwen"
        | "dashscope"
        | "alibaba"
        | "modelstudio"
        | "modelstudio-token-plan"
        | "modelstudio-token-plan-anthropic"
        | "modelstudio_token_plan"
        | "modelstudio_token_plan_anthropic" => {
            status == 429
                && matches!(
                    code_lower.as_str(),
                    "insufficient_quota" | "throttling" | "throttling.ratequota"
                )
        }
        "minimax" | "minimax-anthropic" | "minimax_anthropic" => {
            matches!(code.as_str(), "1002" | "1041")
        }
        "glm" | "zai" | "zhipu" => matches!(
            code.as_str(),
            "1302" | "1305" | "1308" | "1310" | "1316" | "1317" | "1318" | "1319" | "1320" | "1321"
        ),
        _ => false,
    };
    if transient {
        return Some(LlmError::RateLimited {
            message: safe.to_owned(),
            retry_after: hint,
        });
    }
    let user_action = match provider.as_str() {
        "minimax" | "minimax-anthropic" | "minimax_anthropic" => {
            matches!(code.as_str(), "1008" | "1039")
        }
        "glm" | "zai" | "zhipu" => matches!(code.as_str(), "1113" | "1309"),
        _ => false,
    };
    if user_action && code != "1039" {
        return Some(LlmError::QuotaExhausted(
            super::QuotaExhaustionError::from_http_message(safe.to_owned()),
        ));
    }
    if user_action {
        return Some(LlmError::InvalidRequest {
            status,
            message: safe.to_owned(),
        });
    }
    if ambiguous_quota {
        return Some(LlmError::RateLimited {
            message: safe.to_owned(),
            retry_after: hint,
        });
    }
    None
}

pub fn retry_delay(policy: &RateLimitRetryPolicy, retry: u32) -> Duration {
    let multiplier = 2u32.saturating_pow(retry.min(31));
    let base = Duration::from_secs(policy.initial_delay_secs)
        .saturating_mul(multiplier)
        .min(Duration::from_secs(policy.max_delay_secs));
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let unit = f64::from(u16::from_le_bytes([bytes[0], bytes[1]])) / f64::from(u16::MAX);
    base.mul_f64(0.5 + unit * 0.5)
}

#[cfg(test)]
mod tests;

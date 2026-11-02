pub const DEFAULT_STREAM_CHUNK_TIMEOUT_SECS: u64 = 900;

pub const MIN_STREAM_CHUNK_TIMEOUT_SECS: u64 = 1;

pub const MAX_STREAM_CHUNK_TIMEOUT_SECS: u64 = 3600;

/// Default bounded wait for SSE response headers. Intentionally shorter than
/// the per-chunk idle timeout: it covers connection setup and upstream header
/// return only, never model thinking time after streaming has started.
pub const DEFAULT_STREAM_OPEN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);

/// Accepted response-header wait range, in seconds.
pub const MIN_STREAM_OPEN_TIMEOUT_SECS: u64 = 5;

pub const MAX_STREAM_OPEN_TIMEOUT_SECS: u64 = 300;

/// Resolve the response-header wait shared by every streaming adapter.
///
/// A positive `[stream].open_timeout_secs` (legacy `[tui]` fallback) wins; omitted or `0`
/// falls back to the env override (`CODEWHALE_STREAM_OPEN_TIMEOUT_SECS`,
/// legacy `DEEPSEEK_STREAM_OPEN_TIMEOUT_SECS`), then the 45s default. Every
/// source clamps to `5..=300`.
#[must_use]
pub fn resolve_stream_open_timeout(configured_secs: Option<u64>) -> std::time::Duration {
    match configured_secs {
        Some(secs) if secs > 0 => std::time::Duration::from_secs(
            secs.clamp(MIN_STREAM_OPEN_TIMEOUT_SECS, MAX_STREAM_OPEN_TIMEOUT_SECS),
        ),
        _ => stream_open_timeout_from_env(
            std::env::var("CODEWHALE_STREAM_OPEN_TIMEOUT_SECS")
                .or_else(|_| std::env::var("DEEPSEEK_STREAM_OPEN_TIMEOUT_SECS"))
                .ok()
                .as_deref(),
        ),
    }
}

pub fn stream_open_timeout_from_env(value: Option<&str>) -> std::time::Duration {
    let secs = value
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_STREAM_OPEN_TIMEOUT.as_secs())
        .clamp(MIN_STREAM_OPEN_TIMEOUT_SECS, MAX_STREAM_OPEN_TIMEOUT_SECS);
    std::time::Duration::from_secs(secs)
}

/// Returns true when CODEWHALE_FORCE_HTTP1 (legacy alias: DEEPSEEK_FORCE_HTTP1)
/// is set to a truthy value (`1`, `true`, `yes`, `on`, case-insensitive). Read
/// only by `Config::force_http1`, which ORs it with the selected stream config flag; every
/// client builder and stream open takes that resolved value (#103, #6700). Anything else (unset, `0`,
/// `false`, ...) leaves HTTP/2 on.
#[must_use]
pub fn force_http1_from_env() -> bool {
    std::env::var("CODEWHALE_FORCE_HTTP1")
        .or_else(|_| std::env::var("DEEPSEEK_FORCE_HTTP1"))
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .is_some_and(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
}

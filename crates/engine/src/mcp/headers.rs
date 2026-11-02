use std::collections::HashMap;

use reqwest::header::{ACCEPT, CONTENT_TYPE};

pub(super) const MCP_HTTP_ACCEPT: &str = "application/json, text/event-stream";

pub(super) fn with_default_mcp_http_headers(
    request: reqwest::RequestBuilder,
    json_body: bool,
) -> reqwest::RequestBuilder {
    let request = request.header(ACCEPT, MCP_HTTP_ACCEPT);
    if json_body {
        request.header(CONTENT_TYPE, "application/json")
    } else {
        request
    }
}

/// Predicate for the custom-header pass used by MCP HTTP transports.
///
/// We accept whatever reqwest's `HeaderName::try_from` /
/// `HeaderValue::try_from` would accept, but with three extra rules:
///
/// 1. Reject empty / whitespace-only keys - these would surface as a
///    request-builder error mid-send and abort the whole connection.
/// 2. Reject keys that duplicate the framing we already emit
///    (`Accept`, `Content-Type`). The MCP Streamable HTTP transport
///    relies on those exact values for protocol negotiation; a stray
///    user override could silently break tool discovery.
/// 3. Reject values containing ASCII CR or LF. reqwest already
///    rejects those, but the explicit check makes the failure path
///    visible (a `tracing::warn!` instead of an obscure
///    builder error) and documents the response-splitting
///    defense.
///
/// Returning `false` means "skip this header"; the rest of the
/// request still goes out.
pub fn is_safe_custom_header(key: &str, value: &str) -> bool {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.eq_ignore_ascii_case("accept") || trimmed.eq_ignore_ascii_case("content-type") {
        return false;
    }
    !value.contains('\r') && !value.contains('\n')
}

pub(super) fn apply_safe_custom_headers(
    mut request: reqwest::RequestBuilder,
    headers: &HashMap<String, String>,
) -> reqwest::RequestBuilder {
    for (key, value) in headers {
        if !is_safe_custom_header(key, value) {
            tracing::warn!(
                target: "mcp",
                "skipping unsafe MCP header {:?} (empty/control-char/reserved)",
                key
            );
            continue;
        }
        request = request.header(key.as_str(), value.as_str());
    }
    request
}

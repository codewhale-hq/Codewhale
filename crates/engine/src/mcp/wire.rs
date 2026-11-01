//! MCP wire-format helpers shared by the HTTP, SSE, streamable-HTTP, and
//! stdio transports: frame/response size ceilings, SSE event framing and
//! field parsing, and the error-text classifiers that decide whether a
//! failure is a stale session or a closed connection.
/// Hard ceiling on the SSE frame-assembly buffer. A server that never emits a
/// frame separator would otherwise grow it without bound (OOM DoS).
pub const MAX_SSE_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Hard ceiling on a single MCP HTTP response body / stdio line. A misbehaving
/// or malicious server could otherwise stream an unbounded body (or a
/// newline-free multi-GB "line") and OOM the process at transport-read time,
/// before any transcript-level spillover applies.
pub const MAX_MCP_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

pub fn is_mcp_stale_session_body(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    body.contains("session") && (body.contains("expired") || body.contains("invalid"))
}

/// A transport-level refusal of the session id, raised only where the
/// HTTP layer turned the request away before handing it to the server's
/// method dispatch: a Streamable HTTP stale-session status, or a legacy SSE
/// POST rejected with a stale-session body. A JSON-RPC error response is
/// never this type: it answers the request id, so the server processed it.
#[derive(Debug)]
pub struct McpSessionRejected(pub String);

impl std::fmt::Display for McpSessionRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for McpSessionRejected {}

/// The transport refused the session id, so the server provably did not run
/// the request. This is the only failure after which a non-idempotent
/// `tools/call` may be replayed on a fresh connection. Typed, not matched on
/// text, so a tool error that merely mentions an expired session cannot
/// qualify.
pub(super) fn is_mcp_session_rejected_error(err: &anyhow::Error) -> bool {
    err.downcast_ref::<McpSessionRejected>().is_some()
}

/// The connection is unusable: either the server rejected the session id,
/// or the transport itself is gone (dead pipe/socket) rather than merely
/// idle. The connection must be rebuilt, but a request already written to
/// a transport that then died may have run, so this alone does not make a
/// `tools/call` safe to replay.
pub(super) fn is_mcp_connection_lost_error(err: &anyhow::Error) -> bool {
    if is_mcp_stale_session_error(err) {
        return true;
    }
    let lower = format!("{err:#}").to_ascii_lowercase();
    is_connection_closed_error_text(&lower)
}

pub(super) fn is_mcp_stale_session_error(err: &anyhow::Error) -> bool {
    let err = format!("{err:#}");
    let lower_err = err.to_ascii_lowercase();
    err.contains("MCP Streamable HTTP session expired")
        || err.contains("MCP session expired")
        || err.contains("SSE transport closed")
        // The exact bail text of a stdio transport whose child died (the
        // EOF arm of `StdioTransport::recv`); without this arm a dead-child
        // error missed the drop→reconnect→retry path that SSE closes get.
        || err.contains("Stdio transport closed")
        || (err.contains("MCP SSE POST send failed") && is_connection_closed_error_text(&lower_err))
        || is_mcp_stale_session_body(&err)
}

pub(super) fn is_connection_closed_error_text(err: &str) -> bool {
    err.contains("connection closed")
        || err.contains("connection reset")
        || err.contains("broken pipe")
        || err.contains("unexpected eof")
        || err.contains("forcibly closed")
}

pub(super) fn parse_sse_message_data(body: &str) -> Vec<Vec<u8>> {
    let normalized = body.replace("\r\n", "\n");
    let mut messages = Vec::new();

    for block in normalized.split("\n\n") {
        let mut event_type = "message";
        let mut data = String::new();

        for line in block.lines() {
            if let Some(value) = sse_field_value(line, "event:") {
                event_type = value;
            } else if let Some(value) = sse_field_value(line, "data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(value);
            }
        }

        if event_type != "message" || data.trim().is_empty() {
            continue;
        }

        messages.push(data.trim().as_bytes().to_vec());
    }

    messages
}

// Retained for tests; the SSE transport now uses the byte-oriented twin.
#[cfg(test)]
pub(super) fn find_sse_event_separator(buffer: &str) -> Option<(usize, usize)> {
    match (buffer.find("\n\n"), buffer.find("\r\n\r\n")) {
        (Some(lf), Some(crlf)) if crlf < lf => Some((crlf, 4)),
        (Some(lf), _) => Some((lf, 2)),
        (_, Some(crlf)) => Some((crlf, 4)),
        _ => None,
    }
}

/// Byte-oriented twin of `find_sse_event_separator`. Used by the SSE
/// transport so it can accumulate RAW bytes and decode only complete event
/// blocks — a multi-byte UTF-8 char split across two network reads is never
/// corrupted to U+FFFD (the `\n`/`\r` separators are ASCII and can never fall
/// inside a multi-byte sequence).
pub fn find_sse_event_separator_bytes(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer.windows(2).position(|w| w == b"\n\n");
    let crlf = buffer.windows(4).position(|w| w == b"\r\n\r\n");
    match (lf, crlf) {
        (Some(lf), Some(crlf)) if crlf < lf => Some((crlf, 4)),
        (Some(lf), _) => Some((lf, 2)),
        (_, Some(crlf)) => Some((crlf, 4)),
        _ => None,
    }
}

pub fn sse_field_value<'a>(line: &'a str, field: &str) -> Option<&'a str> {
    let value = line.strip_prefix(field)?;
    Some(value.strip_prefix(' ').unwrap_or(value))
}

pub fn is_json_rpc_error_body(body_excerpt: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body_excerpt.trim())
        .ok()
        .and_then(|value| value.get("error")?.get("code")?.as_i64())
        .is_some()
}

pub fn is_streamable_http_incompatible_status(status: reqwest::StatusCode) -> bool {
    matches!(
        status,
        reqwest::StatusCode::NOT_FOUND
            | reqwest::StatusCode::METHOD_NOT_ALLOWED
            | reqwest::StatusCode::NOT_ACCEPTABLE
            | reqwest::StatusCode::UNSUPPORTED_MEDIA_TYPE
            | reqwest::StatusCode::NOT_IMPLEMENTED
    )
}

pub fn is_streamable_http_stale_session_status(
    status: reqwest::StatusCode,
    body_excerpt: &str,
) -> bool {
    if status == reqwest::StatusCode::NOT_FOUND {
        return true;
    }
    if status != reqwest::StatusCode::BAD_REQUEST && status != reqwest::StatusCode::UNAUTHORIZED {
        return false;
    }
    let body = body_excerpt.to_ascii_lowercase();
    body.contains("session") && (body.contains("expired") || body.contains("invalid"))
}

/// Continue one newline-terminated line in caller-owned `out`, aborting if it
/// exceeds `max` bytes. Cancellation retains consumed bytes; the caller clears
/// the buffer only after receiving a complete frame. Returns the total bytes
/// accumulated; 0 means EOF.
pub async fn read_line_capped<R>(
    reader: &mut R,
    out: &mut Vec<u8>,
    max: usize,
) -> std::io::Result<usize>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    loop {
        let (chunk, consumed, done) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                (Vec::new(), 0usize, true)
            } else if let Some(pos) = available.iter().position(|&b| b == b'\n') {
                (available[..=pos].to_vec(), pos + 1, true)
            } else {
                (available.to_vec(), available.len(), false)
            }
        };
        if consumed > 0 {
            reader.consume(consumed);
        }
        out.extend_from_slice(&chunk);
        if out.len() > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("MCP stdio line exceeded {max} bytes"),
            ));
        }
        if done {
            break;
        }
    }
    Ok(out.len())
}

#[cfg(test)]
mod read_cap_tests {
    use super::read_line_capped;

    #[tokio::test]
    async fn cancelled_partial_read_preserves_next_frame() {
        use futures_util::FutureExt;
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(4096);
        let mut reader = tokio::io::BufReader::new(reader);
        let prefix = br#"{"jsonrpc":"2.0","id":"1","result":"#;
        writer.write_all(prefix).await.unwrap();
        let mut pending = Vec::new();
        // Poll through the consumed prefix to Pending, then drop the future.
        assert!(
            read_line_capped(&mut reader, &mut pending, 1024)
                .now_or_never()
                .is_none()
        );
        assert_eq!(pending, prefix);
        writer.write_all(b"null}\n").await.unwrap();
        read_line_capped(&mut reader, &mut pending, 1024)
            .await
            .unwrap();
        let first: serde_json::Value =
            serde_json::from_slice(&std::mem::take(&mut pending)).unwrap();
        assert_eq!(first["id"], "1");
        writer
            .write_all(b"{\"id\":\"2\",\"result\":true}\n")
            .await
            .unwrap();
        read_line_capped(&mut reader, &mut pending, 1024)
            .await
            .unwrap();
        let second: serde_json::Value = serde_json::from_slice(&pending).unwrap();
        assert_eq!(second["id"], "2");
        assert_eq!(second["result"], true);
    }

    #[tokio::test]
    async fn resumed_frame_still_enforces_cap_at_newline() {
        use futures_util::FutureExt;
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(4096);
        let mut reader = tokio::io::BufReader::new(reader);
        let mut pending = Vec::new();
        writer.write_all(b"1234").await.unwrap();
        assert!(
            read_line_capped(&mut reader, &mut pending, 6)
                .now_or_never()
                .is_none()
        );
        writer.write_all(b"567\n").await.unwrap();
        assert_eq!(
            read_line_capped(&mut reader, &mut pending, 6)
                .await
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
    }

    #[tokio::test]
    async fn reads_a_line_and_reports_eof() {
        let data = b"hello\nworld\n".to_vec();
        let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(data));
        let mut out = Vec::new();
        assert_eq!(
            read_line_capped(&mut reader, &mut out, 1024).await.unwrap(),
            6
        );
        assert_eq!(out, b"hello\n");
        out.clear();
        assert_eq!(
            read_line_capped(&mut reader, &mut out, 1024).await.unwrap(),
            6
        );
        assert_eq!(out, b"world\n");
        out.clear();
        // EOF.
        assert_eq!(
            read_line_capped(&mut reader, &mut out, 1024).await.unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn aborts_on_newline_free_line_over_cap() {
        let data = vec![b'x'; 4096]; // no newline
        let mut reader = tokio::io::BufReader::new(std::io::Cursor::new(data));
        let mut out = Vec::new();
        let err = read_line_capped(&mut reader, &mut out, 1024)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}

pub fn resolve_sse_endpoint_url(base_url: &str, endpoint_url: &str) -> anyhow::Result<String> {
    let base = reqwest::Url::parse(base_url)?;
    let resolved = if endpoint_url.starts_with("http://") || endpoint_url.starts_with("https://") {
        reqwest::Url::parse(endpoint_url)?
    } else {
        base.join(endpoint_url)?
    };
    // reqwest converts userinfo into Basic Authorization while building a
    // request, before the request-time guard can inspect the original URL.
    if !resolved.username().is_empty() || resolved.password().is_some() {
        anyhow::bail!("MCP SSE endpoint must not contain URL credentials");
    }
    // Security: the server-supplied `endpoint` event must stay same-origin
    // as the connect URL. The connect host is vetted by network policy
    // once, but the endpoint host is never re-checked — so an absolute
    // cross-origin endpoint would let a malicious MCP server redirect the
    // client's *authenticated* POSTs (Bearer/OAuth headers attached) to an
    // internal host (169.254.169.254, localhost admin ports, …): an SSRF /
    // policy bypass. Relative endpoints are same-origin by construction.
    if resolved.scheme() != base.scheme()
        || resolved.host_str() != base.host_str()
        || resolved.port_or_known_default() != base.port_or_known_default()
    {
        anyhow::bail!(
            "MCP SSE endpoint {} is not same-origin as {} — refusing to send \
             authenticated requests cross-origin",
            super::mask_url_secrets(resolved.as_str()),
            super::mask_url_secrets(base.as_str()),
        );
    }
    Ok(resolved.to_string())
}

#[cfg(test)]
mod endpoint_tests {
    use super::resolve_sse_endpoint_url;

    #[test]
    fn resolve_endpoint_accepts_relative_and_same_origin() {
        let base = "https://mcp.example.com/v1/sse";
        // Relative path -> same origin.
        assert_eq!(
            resolve_sse_endpoint_url(base, "/messages?sid=1").unwrap(),
            "https://mcp.example.com/messages?sid=1"
        );
        // Absolute but same origin -> allowed.
        assert_eq!(
            resolve_sse_endpoint_url(base, "https://mcp.example.com/messages").unwrap(),
            "https://mcp.example.com/messages"
        );
    }

    #[test]
    fn resolve_endpoint_rejects_cross_origin_ssrf() {
        let base = "https://mcp.example.com/v1/sse";
        // Different host (metadata endpoint) -> rejected.
        assert!(resolve_sse_endpoint_url(base, "http://169.254.169.254/latest").is_err());
        // Different scheme -> rejected.
        assert!(resolve_sse_endpoint_url(base, "http://mcp.example.com/messages").is_err());
        // Different port -> rejected.
        assert!(resolve_sse_endpoint_url(base, "https://mcp.example.com:8443/x").is_err());
        // Same-origin userinfo must not become an implicit Basic credential.
        for endpoint in [
            "https://fixture-user:fixture-password@mcp.example.com/messages",
            "//fixture-user@mcp.example.com/messages",
        ] {
            let error = resolve_sse_endpoint_url(base, endpoint).unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("must not contain URL credentials")
            );
            assert!(!error.to_string().contains("fixture-user"));
            assert!(!error.to_string().contains("fixture-password"));
        }
    }
}

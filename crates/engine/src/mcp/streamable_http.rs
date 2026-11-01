use std::collections::VecDeque;

use anyhow::{Context, Result};
use reqwest::StatusCode;
use reqwest::header::CONTENT_TYPE;

use super::http_client::McpHttpClient;
use super::wire::{
    MAX_MCP_RESPONSE_BYTES, is_streamable_http_incompatible_status,
    is_streamable_http_stale_session_status, parse_sse_message_data,
};
use super::{ERROR_BODY_PREVIEW_BYTES, bounded_body_excerpt, mask_url_secrets};

pub(super) struct StreamableHttpTransport {
    pub(super) client: McpHttpClient,
    pub(super) url: String,
    pending_messages: VecDeque<Vec<u8>>,
    /// Per-spec MCP session identifier returned by the server in the
    /// first response (typically the `initialize` response). Attached
    /// as the `Mcp-Session-Id` header on every subsequent outbound
    /// request so the server can correlate messages within the same
    /// session.
    pub(super) session_id: Option<String>,
    /// Protocol revision negotiated at `initialize`. Attached as the
    /// `MCP-Protocol-Version` header on every subsequent outbound request
    /// per the Streamable HTTP spec (absent means the server assumes
    /// the 2025-03-26 default, so the negotiated value is always sent).
    protocol_version: Option<String>,
}

#[derive(Debug)]
pub(super) enum StreamableSendError {
    Incompatible(String),
    StaleSession(String),
    Other(anyhow::Error),
}

impl StreamableHttpTransport {
    pub(super) fn new(client: McpHttpClient, url: String) -> Self {
        Self {
            client,
            url,
            pending_messages: VecDeque::new(),
            session_id: None,
            protocol_version: None,
        }
    }

    pub(super) fn set_protocol_version(&mut self, version: &str) {
        self.protocol_version = Some(version.to_string());
    }

    pub(super) async fn send(
        &mut self,
        msg: Vec<u8>,
    ) -> std::result::Result<(), StreamableSendError> {
        let mut request = self.client.post(&self.url).body(msg);
        if let Some(ref sid) = self.session_id {
            request = request.header("Mcp-Session-Id", sid.as_str());
        }
        if let Some(ref version) = self.protocol_version {
            request = request.header("MCP-Protocol-Version", version.as_str());
        }
        let client = self.client.clone();
        let response = client
            .send_mcp_request(
                request,
                true,
                false,
                true,
                || Ok(()),
                |response| {
                    if let Some(sid) = response
                        .headers()
                        .get("Mcp-Session-Id")
                        .and_then(|v| v.to_str().ok())
                    {
                        self.session_id = Some(sid.to_string());
                    }
                    Ok(())
                },
            )
            .await
            .map_err(StreamableSendError::Other)?;
        let status = response.status();
        if status == StatusCode::ACCEPTED || status == StatusCode::NO_CONTENT {
            return Ok(());
        }
        if !status.is_success() {
            let body_excerpt = bounded_body_excerpt(response, ERROR_BODY_PREVIEW_BYTES).await;
            let stale_session = self.session_id.is_some()
                && is_streamable_http_stale_session_status(status, &body_excerpt);
            let body_excerpt = self.client.server_error_preview(&body_excerpt);
            if stale_session {
                return Err(StreamableSendError::StaleSession(format!(
                    "status={status} body={body_excerpt}"
                )));
            }
            if is_streamable_http_incompatible_status(status) {
                return Err(StreamableSendError::Incompatible(format!(
                    "status={status} body={body_excerpt}"
                )));
            }
            return Err(StreamableSendError::Other(anyhow::anyhow!(
                "MCP Streamable HTTP rejected (transport=http url={} status={}): {}",
                mask_url_secrets(&self.url),
                status,
                body_excerpt,
            )));
        }

        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        // Reject an over-large declared body before reading anything (fast
        // path), then bound the read itself so chunked / length-less
        // responses cannot OOM us either — Content-Length alone does not
        // protect against a server that streams without declaring a length.
        if let Some(len) = response.content_length()
            && len > MAX_MCP_RESPONSE_BYTES as u64
        {
            return Err(StreamableSendError::Other(anyhow::anyhow!(
                "MCP response Content-Length {len} exceeds {} bytes — aborting",
                MAX_MCP_RESPONSE_BYTES
            )));
        }
        let body = read_body_capped(response, MAX_MCP_RESPONSE_BYTES)
            .await
            .map_err(StreamableSendError::Other)?;
        self.store_response_body(content_type.as_deref(), &body)
            .map_err(StreamableSendError::Other)
    }

    pub(super) async fn recv(&mut self) -> Result<Vec<u8>> {
        self.pending_messages
            .pop_front()
            .context("MCP Streamable HTTP response queue is empty")
    }

    fn store_response_body(&mut self, content_type: Option<&str>, body: &str) -> Result<()> {
        if body.trim().is_empty() {
            return Ok(());
        }

        let is_event_stream = content_type
            .map(|value| value.to_ascii_lowercase().contains("text/event-stream"))
            .unwrap_or(false)
            || body.trim_start().starts_with("event:")
            || body.trim_start().starts_with("data:");

        if is_event_stream {
            for msg in parse_sse_message_data(body) {
                self.pending_messages.push_back(msg);
            }
            return Ok(());
        }

        self.pending_messages.push_back(body.as_bytes().to_vec());
        Ok(())
    }
}

/// Read a response body through the byte stream, failing as soon as it
/// exceeds `max_bytes`. This bounds chunked and missing-Content-Length
/// responses exactly like declared ones (the declared-length fast path in
/// `send` only covers servers honest enough to announce their size).
/// MCP bodies are JSON or SSE, so lossy UTF-8 matches `.text()` behavior.
pub(super) async fn read_body_capped(
    response: reqwest::Response,
    max_bytes: usize,
) -> Result<String> {
    let buf = codewhale_util::utils::read_response_body_capped(response, max_bytes)
        .await
        .map_err(|error| anyhow::anyhow!("MCP {error:#}"))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// TUI recovery for a rejected OAuth session. Settings recovery and the
/// Streamable HTTP path share the oauth helpers so `/mcp login <name>` stays
/// the only advertised command; `/mcp auth` is not a command.
#[cfg(test)]
fn oauth_refresh_failed_hint() -> &'static str {
    super::oauth::tui_reauth_refresh_failed_hint()
}

/// TUI recovery for a rejected OAuth session. `oauth_configured` is the
/// server's configured auth path ([`McpHttpClient::oauth_configured`]), not the
/// presence of a cached token, so a first-run OAuth server — a 401 with
/// nothing stored yet — is still pointed at `/mcp login <name>` rather than at
/// a bearer token it never had (#6030). Servers where a bearer credential is
/// genuinely configured (or that are plugin-contributed, where OAuth login is
/// disabled) keep the bearer-token copy.
#[cfg(test)]
fn unauthorized_session_hint(oauth_configured: bool) -> &'static str {
    if oauth_configured {
        super::oauth::tui_reauth_hint()
    } else {
        "Check the configured bearer token (or its environment variable)."
    }
}

#[cfg(test)]
mod tests {
    use super::{oauth_refresh_failed_hint, unauthorized_session_hint};
    use crate::mcp::http_client::McpHttpAuth;
    use codewhale_mcp::config::McpServerConfig;

    fn server_config(json: serde_json::Value) -> McpServerConfig {
        serde_json::from_value(json).expect("MCP server config fixture")
    }

    #[test]
    fn oauth_configured_server_without_a_cached_token_names_login() {
        // The OAuth fields are optional in MCP config, so a URL-based server
        // with no manual bearer configuration is OAuth's to claim — including
        // before the first login, when there is no runtime to observe.
        let auth = McpHttpAuth::from_config(
            "remote",
            &server_config(serde_json::json!({ "url": "https://example.invalid/mcp" })),
            None,
        );
        assert!(auth.oauth.is_none(), "precondition: no cached credential");
        assert!(auth.oauth_configured, "a URL server is OAuth-servable");
        assert!(
            unauthorized_session_hint(auth.oauth_configured).contains("/mcp login <name>"),
            "a first-run OAuth 401 must name the login command, not a bearer token"
        );

        // A server whose bearer token is genuinely expected keeps that copy.
        let bearer = McpHttpAuth::from_config(
            "remote",
            &server_config(serde_json::json!({
                "url": "https://example.invalid/mcp",
                "bearer_token_env_var": "EXAMPLE_MCP_TOKEN",
            })),
            None,
        );
        assert!(!bearer.oauth_configured);
        assert!(unauthorized_session_hint(bearer.oauth_configured).contains("bearer token"));
    }

    #[test]
    fn unauthorized_oauth_hints_name_the_login_command() {
        for hint in [oauth_refresh_failed_hint(), unauthorized_session_hint(true)] {
            assert!(
                hint.contains("/mcp login <name>"),
                "OAuth recovery must name the implemented command"
            );
            assert!(
                !hint.contains("/mcp auth"),
                "OAuth recovery must not advertise a missing /mcp auth command"
            );
        }
        assert!(
            !unauthorized_session_hint(false).contains("/mcp"),
            "bearer-token recovery should not send the user to OAuth login"
        );
    }
}

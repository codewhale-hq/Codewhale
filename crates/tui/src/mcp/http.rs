//! HTTP MCP transport.
//!
//! Speaks Streamable HTTP first and falls back to the legacy SSE endpoint
//! when the server rejects the newer protocol. Request-time authentication
//! and egress authority belong to the shared McpHttpClient session.

use std::time::Duration;

use anyhow::Result;

use super::McpTransport;
use super::http_client::McpHttpClient;
use super::sse::SseTransport;
use super::streamable_http::{StreamableHttpTransport, StreamableSendError};
use super::wire::McpSessionRejected;
pub(super) struct HttpTransport {
    mode: HttpTransportMode,
    client: McpHttpClient,
    base_url: String,
    cancel_token: tokio_util::sync::CancellationToken,
    endpoint_timeout: Duration,
}

enum HttpTransportMode {
    Streamable(StreamableHttpTransport),
    Sse(SseTransport),
}

impl HttpTransport {
    pub(super) fn new(
        client: McpHttpClient,
        url: String,
        cancel_token: tokio_util::sync::CancellationToken,
        endpoint_timeout: Duration,
    ) -> Self {
        Self {
            mode: HttpTransportMode::Streamable(StreamableHttpTransport::new(
                client.clone(),
                url.clone(),
            )),
            client,
            base_url: url,
            cancel_token,
            endpoint_timeout,
        }
    }

    async fn switch_to_sse_and_send(&mut self, msg: Vec<u8>) -> Result<()> {
        let mut sse = SseTransport::connect(
            self.client.clone(),
            self.base_url.clone(),
            self.cancel_token.clone(),
            self.endpoint_timeout,
        )
        .await?;
        sse.send(msg).await?;
        self.mode = HttpTransportMode::Sse(sse);
        Ok(())
    }

    /// Best-effort session-establishment GET preflight.
    ///
    /// Per the Streamable HTTP spec, the server may return an
    /// `Mcp-Session-Id` header on the `initialize` response (the normal
    /// path handled inside [`StreamableHttpTransport::send`] above).
    /// However some servers (e.g. Hindsight, #1629) **require** a session
    /// ID on every POST including `initialize`, creating a chicken-and-egg
    /// problem. For those servers we send a short-lived GET before the
    /// first POST: if the server returns a session ID in the GET response
    /// it will be captured by the header-reading code in
    /// [`StreamableHttpTransport::send`] just as if it came from a POST
    /// response.
    ///
    /// This is intentionally best-effort:
    /// * The GET uses a tight per-request inner timeout so it never
    ///   blocks connection startup for long.
    /// * If the server doesn't support GET (405, 404, …) we log a debug
    ///   line and move on — the `initialize` POST will proceed without a
    ///   session ID.
    /// * If the server opens an SSE stream in response (the GET from old
    ///   SSE transport), we read only the headers, then discard the body
    ///   so the SSE stream is torn down. The actual SSE path uses a
    ///   dedicated `SseTransport` and is triggered by the incompatible-
    ///   status fallback in [`HttpTransport::send`].
    pub(super) async fn try_establish_session(&mut self) -> Result<()> {
        let cancel = self.cancel_token.clone();
        let transport = match &mut self.mode {
            HttpTransportMode::Streamable(t) => t,
            // Already on SSE — session is implicit via the long-lived GET.
            HttpTransportMode::Sse(_) => return Ok(()),
        };

        let request = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                anyhow::bail!("MCP session preflight cancelled after plugin authority changed")
            }
            request = transport.client.prepare_mcp_request(
                transport.client.get(&transport.url), false,
            ) => request?,
        };
        let response = tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                anyhow::bail!("MCP session preflight cancelled after plugin authority changed")
            }
            response = tokio::time::timeout(Duration::from_secs(5), transport.client.send(request)) => {
                response
                    .map_err(|_| anyhow::anyhow!("GET timeout"))?
                    .map_err(|e| anyhow::anyhow!("GET error: {e}"))?
            }
        };

        // Capture session ID from the GET response so subsequent POSTs
        // (including `initialize`) can include it. This is the same
        // header-reading logic that would be hit inside
        // `StreamableHttpTransport::send` for POST responses, but since
        // the GET is sent before any POST we do it here directly.
        if let Some(sid) = response
            .headers()
            .get("Mcp-Session-Id")
            .and_then(|v| v.to_str().ok())
            && transport.session_id.as_deref() != Some(sid)
        {
            let session_ref = codewhale_util::utils::redacted_identifier_for_log(sid);
            tracing::debug!(target: "mcp", session = %session_ref, "captured MCP session ID via GET preflight");
            transport.session_id = Some(sid.to_string());
        }

        // We only care about the response headers — discard the body.
        // If the server opened an SSE stream in response (some servers
        // do this on GET), it will be torn down when response is dropped.
        drop(response);

        Ok(())
    }
}

#[async_trait::async_trait]
impl McpTransport for HttpTransport {
    fn set_protocol_version(&mut self, version: &str) {
        // Only Streamable HTTP carries the MCP-Protocol-Version header; the
        // legacy SSE transport predates it and ignores the negotiation result.
        if let HttpTransportMode::Streamable(transport) = &mut self.mode {
            transport.set_protocol_version(version);
        }
    }

    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        match &mut self.mode {
            HttpTransportMode::Streamable(transport) => match transport.send(msg.clone()).await {
                Ok(()) => Ok(()),
                Err(StreamableSendError::Incompatible(detail)) => {
                    tracing::debug!(
                        "MCP Streamable HTTP unavailable; falling back to SSE endpoint discovery: {}",
                        detail
                    );
                    self.switch_to_sse_and_send(msg).await
                }
                Err(StreamableSendError::StaleSession(detail)) => {
                    if let HttpTransportMode::Streamable(transport) = &mut self.mode {
                        tracing::debug!(
                            target: "mcp",
                            error = %detail,
                            "MCP Streamable HTTP session expired; clearing cached session ID"
                        );
                        transport.session_id = None;
                    }
                    Err(McpSessionRejected(format!(
                        "MCP Streamable HTTP session expired; retry with a new session required ({detail})"
                    ))
                    .into())
                }
                Err(StreamableSendError::Other(err)) => Err(err),
            },
            HttpTransportMode::Sse(transport) => transport.send(msg).await,
        }
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        match &mut self.mode {
            HttpTransportMode::Streamable(transport) => transport.recv().await,
            HttpTransportMode::Sse(transport) => transport.recv().await,
        }
    }

    fn probe_dead(&self) -> bool {
        match &self.mode {
            HttpTransportMode::Streamable(_) => self.cancel_token.is_cancelled(),
            HttpTransportMode::Sse(transport) => transport.probe_dead(),
        }
    }

    async fn shutdown(&mut self) {
        if let HttpTransportMode::Sse(transport) = &mut self.mode {
            transport.shutdown().await;
        }
    }
}

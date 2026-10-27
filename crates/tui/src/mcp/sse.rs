use std::time::Duration;

use anyhow::{Context, Result};

use super::http_client::McpHttpClient;
use super::wire::{
    MAX_SSE_FRAME_BYTES, McpSessionRejected, find_sse_event_separator_bytes,
    is_mcp_stale_session_body, resolve_sse_endpoint_url, sse_field_value,
};
use super::{ERROR_BODY_PREVIEW_BYTES, McpTransport, bounded_body_excerpt, mask_url_secrets};

const SSE_INBOUND_CHANNEL_CAPACITY: usize = 4;

pub(crate) struct SseTransport {
    pub(super) client: McpHttpClient,
    pub(super) base_url: String,
    pub(super) endpoint_url: Option<String>,
    pub(super) receiver: tokio::sync::mpsc::Receiver<SseInbound>,
    pub(super) sse_task: tokio::task::JoinHandle<()>,
}

pub(super) enum SseInbound {
    Endpoint(String),
    Message(Vec<u8>),
}

impl SseTransport {
    pub(super) async fn connect(
        client: McpHttpClient,
        url: String,
        cancel_token: tokio_util::sync::CancellationToken,
        endpoint_timeout: Duration,
    ) -> Result<Self> {
        let (tx, rx) = tokio::sync::mpsc::channel(SSE_INBOUND_CHANNEL_CAPACITY);
        let client_clone = client.clone();
        let url_clone = url.clone();
        let wait_cancel_token = cancel_token.clone();

        let sse_task = tokio::spawn(async move {
            if cancel_token.is_cancelled() {
                return;
            }
            use futures_util::FutureExt;
            let result = std::panic::AssertUnwindSafe(Self::run_sse_loop(
                client_clone,
                url_clone,
                tx,
                cancel_token,
            ))
            .catch_unwind()
            .await;
            match result {
                Ok(res) => {
                    if let Err(e) = res {
                        tracing::error!("SSE loop error: {}", e);
                    }
                }
                Err(panic_err) => {
                    if let Some(msg) = panic_err.downcast_ref::<&str>() {
                        tracing::error!("SSE loop panicked: {}", msg);
                    } else if let Some(msg) = panic_err.downcast_ref::<String>() {
                        tracing::error!("SSE loop panicked: {}", msg);
                    } else {
                        tracing::error!("SSE loop panicked with unknown error");
                    }
                }
            }
        });

        let mut transport = Self {
            client,
            base_url: url,
            endpoint_url: None,
            receiver: rx,
            sse_task,
        };
        transport
            .wait_for_endpoint(&wait_cancel_token, endpoint_timeout)
            .await?;
        Ok(transport)
    }

    async fn run_sse_loop(
        client: McpHttpClient,
        url: String,
        tx: tokio::sync::mpsc::Sender<SseInbound>,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<()> {
        let request = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                anyhow::bail!("MCP SSE connect cancelled before authentication completed")
            }
            request = client.prepare_mcp_request(client.get(&url), false) => request?,
        };
        let response = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                anyhow::bail!("MCP SSE connect cancelled before the request completed")
            }
            response = client.send_event_stream(request) => response.with_context(|| {
                format!(
                    "MCP SSE connect failed (transport=http url={})",
                    mask_url_secrets(&url),
                )
            })?,
        };
        let status = response.status();
        if !status.is_success() {
            let body_excerpt = bounded_body_excerpt(response, ERROR_BODY_PREVIEW_BYTES).await;
            let body_excerpt = client.server_error_preview(&body_excerpt);
            anyhow::bail!(
                "MCP SSE rejected (transport=http url={} status={}): {}",
                mask_url_secrets(&url),
                status,
                body_excerpt,
            );
        }

        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;
        // Raw byte buffer so a multi-byte UTF-8 char split across reads is not
        // corrupted, and bounded so a separator-less server cannot OOM us.
        let mut buffer: Vec<u8> = Vec::new();

        loop {
            if cancel_token.is_cancelled() {
                tracing::debug!("SSE loop cancelled");
                break;
            }
            let item = tokio::select! {
                _ = cancel_token.cancelled() => {
                    tracing::debug!("SSE loop shutting down");
                    break;
                }
                item = stream.next() => {
                    match item {
                        Some(i) => i,
                        None => break,
                    }
                }
            };
            let chunk = item?;
            buffer.extend_from_slice(&chunk);
            if buffer.len() > MAX_SSE_FRAME_BYTES {
                anyhow::bail!(
                    "MCP SSE frame exceeded {} bytes without a separator — aborting",
                    MAX_SSE_FRAME_BYTES
                );
            }

            while let Some((pos, separator_len)) = find_sse_event_separator_bytes(&buffer) {
                // Complete block: decoding cannot split a multi-byte char.
                let event_block = String::from_utf8_lossy(&buffer[..pos]).into_owned();
                buffer.drain(..pos + separator_len);

                let mut event_type = "message";
                let mut data = String::new();

                for line in event_block.lines() {
                    if let Some(value) = sse_field_value(line, "event:") {
                        event_type = value;
                    } else if let Some(value) = sse_field_value(line, "data:") {
                        if !data.is_empty() {
                            data.push('\n');
                        }
                        data.push_str(value);
                    }
                }

                let inbound = match event_type {
                    "endpoint" => Some(SseInbound::Endpoint(data)),
                    "message" if !data.trim().is_empty() => {
                        Some(SseInbound::Message(data.into_bytes()))
                    }
                    _ => None,
                };
                if let Some(inbound) = inbound {
                    let sent = tokio::select! {
                        biased;
                        _ = cancel_token.cancelled() => return Ok(()),
                        sent = tx.send(inbound) => sent,
                    };
                    if sent.is_err() {
                        return Ok(());
                    }
                }
            }
        }
        Ok(())
    }

    async fn wait_for_endpoint(
        &mut self,
        cancel_token: &tokio_util::sync::CancellationToken,
        endpoint_timeout: Duration,
    ) -> Result<()> {
        let timeout = tokio::time::sleep(endpoint_timeout);
        tokio::pin!(timeout);

        let msg = tokio::select! {
            _ = cancel_token.cancelled() => {
                anyhow::bail!("SSE transport cancelled before endpoint was discovered");
            }
            _ = &mut timeout => {
                anyhow::bail!(
                    "SSE endpoint not received within {}ms",
                    endpoint_timeout.as_millis()
                );
            }
            msg = self.receiver.recv() => {
                msg.context("SSE transport closed before endpoint was discovered")?
            }
        };

        match msg {
            SseInbound::Endpoint(endpoint) => self.store_endpoint(&endpoint),
            SseInbound::Message(_) => {
                anyhow::bail!("MCP SSE server sent a message before declaring its endpoint");
            }
        }
    }

    fn store_endpoint(&mut self, endpoint: &str) -> Result<()> {
        self.endpoint_url = Some(resolve_sse_endpoint_url(&self.base_url, endpoint)?);
        Ok(())
    }
}

#[async_trait::async_trait]
impl McpTransport for SseTransport {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        let endpoint = self
            .endpoint_url
            .as_ref()
            .context("SSE endpoint not yet discovered")?
            .clone();
        let request = self
            .client
            .prepare_mcp_request(self.client.post(&endpoint), true)
            .await?
            .body(msg);
        let response = self.client.send(request).await.with_context(|| {
            format!(
                "MCP SSE POST send failed (transport=sse endpoint={})",
                mask_url_secrets(&endpoint)
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            let body_excerpt = bounded_body_excerpt(response, ERROR_BODY_PREVIEW_BYTES).await;
            let stale_session = is_mcp_stale_session_body(&body_excerpt);
            let body_excerpt = self.client.server_error_preview(&body_excerpt);
            if stale_session {
                return Err(McpSessionRejected(format!(
                    "MCP session expired (transport=sse endpoint={} status={}): {}",
                    mask_url_secrets(&endpoint),
                    status,
                    body_excerpt
                ))
                .into());
            }
            anyhow::bail!(
                "MCP SSE POST rejected (transport=sse endpoint={} status={}): {}",
                mask_url_secrets(&endpoint),
                status,
                body_excerpt
            );
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        loop {
            match self.receiver.recv().await.context("SSE transport closed")? {
                SseInbound::Endpoint(endpoint) => {
                    self.store_endpoint(&endpoint)?;
                }
                SseInbound::Message(msg) => return Ok(msg),
            }
        }
    }

    /// The event stream is the only inbound channel: once its task has
    /// ended (server closed it, network error, oversize frame), POSTs may
    /// still be accepted while every reply is lost. Report it dead so the
    /// pool reconnects before dispatching instead of losing a tool result.
    fn probe_dead(&self) -> bool {
        self.sse_task.is_finished()
    }

    async fn shutdown(&mut self) {
        self.sse_task.abort();
    }
}

impl Drop for SseTransport {
    fn drop(&mut self) {
        // Dropping a JoinHandle detaches its task. Abort explicitly so a
        // cancelled connection cannot leave an auth refresh, connect, or SSE
        // body stream running without an authority owner.
        self.sse_task.abort();
    }
}

#[cfg(test)]
mod endpoint_tests {
    use std::time::Duration;

    use super::{McpHttpClient, SseInbound, SseTransport};

    #[tokio::test]
    async fn message_before_endpoint_is_rejected_instead_of_buffered() {
        // Building a reqwest client needs the process-wide rustls provider;
        // production installs it at startup, and this test must not depend
        // on another test in the same process having done so first.
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(SseInbound::Message(br#"{"jsonrpc":"2.0"}"#.to_vec()))
            .await
            .unwrap();
        let mut transport = SseTransport {
            client: McpHttpClient::new(
                "https://example.invalid/sse",
                false,
                false,
                false,
                None,
                Duration::from_secs(10),
                Duration::from_secs(120),
            )
            .unwrap(),
            base_url: "https://example.invalid/sse".to_string(),
            endpoint_url: None,
            receiver: rx,
            sse_task: tokio::spawn(async {}),
        };

        let error = transport
            .wait_for_endpoint(
                &tokio_util::sync::CancellationToken::new(),
                Duration::from_secs(1),
            )
            .await
            .expect_err("pre-endpoint message must fail closed");
        assert!(error.to_string().contains("before declaring its endpoint"));
    }

    /// Serve one legacy SSE stream: the endpoint event at once, then each
    /// `(delay, frame)` in order, then close the stream or hold it open.
    async fn serve_sse_stream(frames: Vec<(Duration, &'static [u8])>, close: bool) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/sse", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0, "client closed before sending its request");
                request.extend_from_slice(&buf[..n]);
            }
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\nevent: endpoint\ndata: /messages\n\n")
                .await
                .unwrap();
            for (delay, frame) in frames {
                tokio::time::sleep(delay).await;
                if socket.write_all(frame).await.is_err() {
                    return;
                }
            }
            if !close {
                std::future::pending::<()>().await;
            }
        });
        url
    }

    async fn connect_with_read_timeout(url: String, read_timeout: Duration) -> SseTransport {
        codewhale_release::tls::ensure_rustls_crypto_provider();
        let client = McpHttpClient::new(
            &url,
            false,
            false,
            false,
            None,
            Duration::from_secs(5),
            read_timeout,
        )
        .unwrap();
        SseTransport::connect(
            client,
            url,
            tokio_util::sync::CancellationToken::new(),
            Duration::from_secs(5),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn event_stream_outlives_the_request_read_timeout() {
        use crate::mcp::McpTransport as _;
        let _env = crate::test_support::lock_test_env();
        let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*,127.0.0.1");
        // A quiet stream for longer than read_timeout is healthy, not dead.
        let url = serve_sse_stream(
            vec![(
                Duration::from_millis(900),
                b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1}\n\n",
            )],
            false,
        )
        .await;
        let mut transport = connect_with_read_timeout(url, Duration::from_millis(300)).await;
        let message = tokio::time::timeout(Duration::from_secs(5), transport.recv())
            .await
            .expect("stream message within the test bound")
            .expect("stream still open after read_timeout");
        assert_eq!(message, br#"{"jsonrpc":"2.0","id":1}"#);
        assert!(!transport.probe_dead());
    }

    #[tokio::test]
    async fn closed_event_stream_reads_as_dead() {
        use crate::mcp::McpTransport as _;
        let _env = crate::test_support::lock_test_env();
        let _no_proxy = crate::test_support::EnvVarGuard::set("NO_PROXY", "*,127.0.0.1");
        let url = serve_sse_stream(Vec::new(), true).await;
        let transport = connect_with_read_timeout(url, Duration::from_secs(5)).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !transport.probe_dead() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "a closed SSE stream must stop reading as alive"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

//! MCP newline framing over the broker-owned reviewed stdio process.
use anyhow::Result;
use tokio::process::ChildStdout;

use super::McpTransport;
use super::process_broker::BrokerSession;
use super::wire::{MAX_MCP_RESPONSE_BYTES, read_line_capped};
use codewhale_mcp::config::McpServerConfig;

pub(super) struct StdioTransport {
    pub(super) session: BrokerSession,
    reader: tokio::io::BufReader<ChildStdout>,
    /// Partial frame bytes survive cancellation of the receive future.
    pub(super) pending_line: Vec<u8>,
}

impl StdioTransport {
    pub(super) fn spawn(
        server_name: &str,
        command: &str,
        config: &McpServerConfig,
        cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<Self> {
        let (session, stdout) = BrokerSession::spawn(server_name, command, config, cancel_token)?;
        Ok(Self {
            session,
            reader: tokio::io::BufReader::new(stdout),
            pending_line: Vec::new(),
        })
    }
}

#[async_trait::async_trait]
impl McpTransport for StdioTransport {
    async fn last_stderr_line(&self) -> Option<String> {
        self.session.last_stderr_line().await
    }

    async fn send(&mut self, mut msg: Vec<u8>) -> Result<()> {
        msg.push(b'\n');
        self.session.write(&msg).await
    }

    /// Non-blocking liveness probe: a reaped child means the transport is
    /// dead even though the `Ready` flag is still set (#6187). The sync
    /// trait contract forbids awaiting the lock, so a contended lock reads
    /// as alive — the read side observes the death on the next call.
    fn probe_dead(&self) -> bool {
        self.session.probe_dead()
    }

    async fn recv(&mut self) -> Result<Vec<u8>> {
        loop {
            // Bounded read: a server emitting a newline-free multi-GB "line"
            // must not OOM us (read_line is unbounded).
            let bytes = match read_line_capped(
                &mut self.reader,
                &mut self.pending_line,
                MAX_MCP_RESPONSE_BYTES,
            )
            .await
            {
                Ok(b) => b,
                Err(err) => {
                    if let Some(stderr) = self.session.stderr_context().await {
                        anyhow::bail!("Stdio transport read error: {err}\n{stderr}");
                    }
                    return Err(err.into());
                }
            };
            if bytes == 0 {
                // Let the stderr drain task catch up before snapshotting, and
                // name the exit status: a reviewed plugin's stderr is never
                // retained, so the status is the only reason the operator
                // gets when the child dies before the handshake (#5916).
                tokio::task::yield_now().await;
                let exit = self.session.exit_status().await;
                let exit = exit.map_or_else(String::new, |status| format!(" ({status})"));
                if let Some(stderr) = self.session.stderr_context().await {
                    anyhow::bail!("Stdio transport closed{exit}\n{stderr}");
                }
                anyhow::bail!("Stdio transport closed{exit}");
            }

            let line_bytes = std::mem::take(&mut self.pending_line);
            let line = String::from_utf8_lossy(&line_bytes);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            return Ok(trimmed.as_bytes().to_vec());
        }
    }

    /// Send SIGTERM and wait up to `STDIO_SHUTDOWN_GRACE` for graceful exit,
    /// then force termination and reap the child as the backstop.
    async fn shutdown(&mut self) {
        self.session.shutdown().await;
    }
}

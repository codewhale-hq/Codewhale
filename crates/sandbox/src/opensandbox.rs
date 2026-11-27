//! Alibaba OpenSandbox backend adapter.
//!
//! Sends shell commands to an OpenSandbox-compatible HTTP API for remote
//! execution.  The API endpoint is `POST {base_url}/v1/sandbox/run` with
//! JSON body `{"cmd": "...", "env": {...}}` and expects a JSON response
//! `{"stdout": "...", "stderr": "...", "exit_code": 0}`.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;

use super::backend::{SandboxBackend, SandboxKind, SandboxOutput};

const RESPONSE_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Request body sent to the OpenSandbox `/v1/sandbox/run` endpoint.
#[derive(Debug, Serialize)]
struct SandboxRunRequest {
    /// Full shell command to execute.
    cmd: String,
    /// Environment variables to set in the sandbox.
    env: HashMap<String, String>,
}

/// Response body from the OpenSandbox `/v1/sandbox/run` endpoint.
#[derive(Debug, Deserialize)]
struct SandboxRunResponse {
    /// Standard output from the command.
    stdout: String,
    /// Standard error from the command.
    stderr: String,
    /// Exit code (0 for success).
    exit_code: i32,
}

/// An OpenSandbox-compatible remote execution backend.
///
/// Constructed with a base URL (e.g. `"http://localhost:8080"`), an optional
/// API key sent as a `Bearer` token, and a timeout in seconds.
pub struct OpenSandboxBackend {
    base_url: String,
    api_key: Option<String>,
    client: reqwest::Client,
}

impl OpenSandboxBackend {
    /// Create a new OpenSandbox backend.
    ///
    /// `base_url` should be the root of the OpenSandbox API (e.g.
    /// `"http://localhost:8080"`). `api_key` is optional and sent as
    /// `Authorization: Bearer <key>` when set. `timeout_secs` controls the
    /// HTTP request timeout.
    pub fn new(base_url: String, api_key: Option<String>, timeout_secs: u64) -> Result<Self> {
        let client = codewhale_release::tls::reqwest_client_builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .context("failed to construct HTTP client for OpenSandbox backend")?;

        Ok(Self {
            base_url,
            api_key,
            client,
        })
    }

    /// Build the full URL for the sandbox run endpoint.
    fn run_url(&self) -> String {
        format!("{}/v1/sandbox/run", self.base_url.trim_end_matches('/'))
    }
}

#[async_trait]
impl SandboxBackend for OpenSandboxBackend {
    fn kind(&self) -> SandboxKind {
        SandboxKind::OpenSandbox
    }

    async fn exec(&self, cmd: &str, env: &HashMap<String, String>) -> Result<SandboxOutput> {
        let request_body = SandboxRunRequest {
            cmd: cmd.to_string(),
            env: env.clone(),
        };

        let mut req = self.client.post(self.run_url()).json(&request_body);

        if let Some(ref api_key) = self.api_key {
            req = req.bearer_auth(api_key);
        }

        let response = req
            .send()
            .await
            .context("Failed to reach OpenSandbox endpoint")?;

        let status = response.status();
        let body = codewhale_release::read_http_response(response, RESPONSE_MAX_BYTES)
            .await
            .context("Failed to read OpenSandbox response")?;
        if !status.is_success() {
            anyhow::bail!(
                "OpenSandbox returned HTTP {}: {}",
                status.as_u16(),
                String::from_utf8_lossy(&body)
            );
        }

        let parsed: SandboxRunResponse = serde_json::from_slice(&body).map_err(|error| {
            anyhow::anyhow!(
                "Failed to parse OpenSandbox response at line {}, column {}: {error}",
                error.line(),
                error.column()
            )
        })?;

        Ok(SandboxOutput {
            stdout: parsed.stdout,
            stderr: parsed.stderr,
            exit_code: parsed.exit_code,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn exec_parse_error_preserves_serde_cause() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            while let Ok(n) = socket.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
                if req.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let body = r#"{"stdout":"ok"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        });

        let backend = OpenSandboxBackend::new(format!("http://{addr}"), None, 5).unwrap();
        let err = backend.exec("echo ok", &HashMap::new()).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("Failed to parse OpenSandbox response at line"),
            "unexpected error: {msg}"
        );
        assert!(
            msg.contains("missing field") && msg.contains("stderr"),
            "expected serde cause in error, got: {msg}"
        );
        let _ = server.await;
    }
}

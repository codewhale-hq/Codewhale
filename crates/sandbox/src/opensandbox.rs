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
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenSandbox returned HTTP {}: {}", status.as_u16(), body);
        }

        let parsed: SandboxRunResponse = response
            .json()
            .await
            .context("Failed to parse OpenSandbox response")?;

        Ok(SandboxOutput {
            stdout: parsed.stdout,
            stderr: parsed.stderr,
            exit_code: parsed.exit_code,
        })
    }
}

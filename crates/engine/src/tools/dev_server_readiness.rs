//! Local dev-server readiness tool.
//!
//! This intentionally covers only the narrow "is my localhost dev server ready
//! yet?" primitive. It is not process supervision and it rejects non-loopback
//! targets so agents do not turn it into a general network probe.

use super::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
    optional_str, optional_u64, required_u64,
};
use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};
use std::future::Future;
use std::net::IpAddr;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::{Instant, sleep, timeout};

const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const HARD_MAX_TIMEOUT_MS: u64 = 120_000;
const DEFAULT_POLL_INTERVAL_MS: u64 = 250;
const MIN_POLL_INTERVAL_MS: u64 = 10;
const MAX_POLL_INTERVAL_MS: u64 = 5_000;
const TCP_CONNECT_ATTEMPT_TIMEOUT_MS: u64 = 2_000;
const HTTP_HEALTHCHECK_ATTEMPT_TIMEOUT_MS: u64 = 10_000;

pub struct WaitForDevServerTool;

#[derive(Debug, Clone)]
struct ReadinessRequest {
    host: String,
    port: u16,
    url: Option<reqwest::Url>,
    timeout: Duration,
    poll_interval: Duration,
}

#[derive(Debug, Serialize)]
struct ReadinessOutput {
    ready: bool,
    phase: &'static str,
    target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    elapsed_ms: u64,
    timed_out: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_status: Option<u16>,
}

#[async_trait]
impl ToolSpec for WaitForDevServerTool {
    fn name(&self) -> &'static str {
        "wait_for_dev_server"
    }

    fn model_visible(&self) -> bool {
        false
    }

    fn description(&self) -> &'static str {
        "Wait for a local dev server to become ready. Polls a loopback TCP port, optionally then an HTTP(S) health URL on the same port, with bounded timeout and structured success/failure output."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "host": {
                    "type": "string",
                    "description": "Loopback host to poll (default 127.0.0.1). Allowed: localhost, 127.0.0.1, ::1, or another loopback IP."
                },
                "port": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 65535,
                    "description": "TCP port to wait for."
                },
                "url": {
                    "type": "string",
                    "description": "Optional HTTP/HTTPS loopback healthcheck URL on the same port. 2xx and 3xx statuses count as ready."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Maximum time to wait in milliseconds (default 30000; hard max 120000)."
                },
                "poll_interval_ms": {
                    "type": "integer",
                    "description": "Delay between probes in milliseconds (default 250; clamped to 10..5000)."
                }
            },
            "required": ["port"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly, ToolCapability::Network]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    async fn execute(&self, input: Value, context: &ToolContext) -> Result<ToolResult, ToolError> {
        let request = parse_request(&input)?;
        let output = wait_for_readiness(request, context).await?;
        readiness_result(output)
    }
}

fn parse_request(input: &Value) -> Result<ReadinessRequest, ToolError> {
    let host = normalize_loopback_host(optional_str(input, "host")?.unwrap_or(DEFAULT_HOST))?;
    let port = parse_port(input)?;
    let url = parse_healthcheck_url(input, port)?;
    let timeout = Duration::from_millis(
        optional_u64(input, "timeout_ms", DEFAULT_TIMEOUT_MS)?.min(HARD_MAX_TIMEOUT_MS),
    );
    let poll_interval = Duration::from_millis(
        optional_u64(input, "poll_interval_ms", DEFAULT_POLL_INTERVAL_MS)?
            .clamp(MIN_POLL_INTERVAL_MS, MAX_POLL_INTERVAL_MS),
    );

    Ok(ReadinessRequest {
        host,
        port,
        url,
        timeout,
        poll_interval,
    })
}

fn parse_port(input: &Value) -> Result<u16, ToolError> {
    let raw = required_u64(input, "port")?;
    if raw == 0 || raw > u16::MAX as u64 {
        return Err(ToolError::invalid_input(
            "`port` must be between 1 and 65535",
        ));
    }
    Ok(raw as u16)
}

fn normalize_loopback_host(host: &str) -> Result<String, ToolError> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err(ToolError::invalid_input("`host` cannot be empty"));
    }
    let unbracketed = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(trimmed);
    let lowered = unbracketed.to_ascii_lowercase();
    if lowered == "localhost" {
        return Ok(DEFAULT_HOST.to_string());
    }
    let ip = lowered.parse::<IpAddr>().map_err(|_| {
        ToolError::invalid_input("`host` must be localhost or a loopback IP address")
    })?;
    if !ip.is_loopback() {
        return Err(ToolError::invalid_input(
            "`host` must be localhost or a loopback IP address",
        ));
    }
    Ok(ip.to_string())
}

fn parse_healthcheck_url(input: &Value, port: u16) -> Result<Option<reqwest::Url>, ToolError> {
    let Some(url) = optional_str(input, "url")?
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return Ok(None);
    };
    let mut parsed = reqwest::Url::parse(url)
        .map_err(|err| ToolError::invalid_input(format!("invalid `url`: {err}")))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(ToolError::invalid_input(
            "`url` must use http:// or https://",
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(ToolError::invalid_input(
            "`url` must not include credentials",
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| ToolError::invalid_input("`url` must include a host"))?;
    let normalized_host = normalize_loopback_host(host).map_err(|_| {
        ToolError::invalid_input("`url` host must be localhost or a loopback IP address")
    })?;
    let url_port = parsed
        .port_or_known_default()
        .ok_or_else(|| ToolError::invalid_input("`url` must include or imply a port"))?;
    if url_port != port {
        return Err(ToolError::invalid_input(
            "`url` port must match the `port` readiness target",
        ));
    }
    parsed
        .set_host(Some(&normalized_host))
        .map_err(|_| ToolError::invalid_input("`url` host must be a valid loopback target"))?;
    Ok(Some(parsed))
}

async fn wait_for_readiness(
    request: ReadinessRequest,
    context: &ToolContext,
) -> Result<ReadinessOutput, ToolError> {
    let started = Instant::now();
    let deadline = started + request.timeout;
    let target = target_label(&request.host, request.port);

    if let Some(timeout) = wait_for_tcp(&request, &target, started, deadline, context).await? {
        return Ok(timeout);
    }

    let Some(url) = request.url.clone() else {
        return Ok(ReadinessOutput {
            ready: true,
            phase: "ready",
            target,
            url: None,
            elapsed_ms: elapsed_ms(started),
            timed_out: false,
            last_error: None,
            last_status: None,
        });
    };

    wait_for_http(&request, url, &target, started, deadline, context).await
}

async fn wait_for_tcp(
    request: &ReadinessRequest,
    target: &str,
    started: Instant,
    deadline: Instant,
    context: &ToolContext,
) -> Result<Option<ReadinessOutput>, ToolError> {
    let mut last_error = None;

    loop {
        check_cancelled(context)?;
        match run_until_deadline(
            deadline,
            Duration::from_millis(TCP_CONNECT_ATTEMPT_TIMEOUT_MS),
            TcpStream::connect((request.host.as_str(), request.port)),
        )
        .await
        {
            Ok(Ok(_stream)) => break,
            Ok(Err(err)) => last_error = Some(err.to_string()),
            Err(()) if last_error.is_none() => {
                last_error = Some("connection attempt timed out".to_string());
            }
            Err(()) => {}
        }

        if Instant::now() >= deadline {
            return Ok(Some(ReadinessOutput {
                ready: false,
                phase: "tcp",
                target: target.to_string(),
                url: request.url.as_ref().map(ToString::to_string),
                elapsed_ms: elapsed_ms(started),
                timed_out: true,
                last_error,
                last_status: None,
            }));
        }

        sleep_until_next_poll(deadline, request.poll_interval, context).await?;
    }

    Ok(None)
}

async fn wait_for_http(
    request: &ReadinessRequest,
    url: reqwest::Url,
    target: &str,
    started: Instant,
    deadline: Instant,
    context: &ToolContext,
) -> Result<ReadinessOutput, ToolError> {
    let client = codewhale_release::tls::reqwest_client_builder()
        .timeout(request.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|err| {
            ToolError::execution_failed(format!("failed to build HTTP client: {err}"))
        })?;
    let mut last_status = None;
    let mut last_error = None;

    loop {
        check_cancelled(context)?;
        match run_until_deadline(
            deadline,
            Duration::from_millis(HTTP_HEALTHCHECK_ATTEMPT_TIMEOUT_MS),
            client.get(url.clone()).send(),
        )
        .await
        {
            Ok(Ok(response)) => {
                let status = response.status();
                last_status = Some(status.as_u16());
                last_error = None;
                if status.is_success() || status.is_redirection() {
                    return Ok(ReadinessOutput {
                        ready: true,
                        phase: "ready",
                        target: target.to_string(),
                        url: Some(url.to_string()),
                        elapsed_ms: elapsed_ms(started),
                        timed_out: false,
                        last_error: None,
                        last_status,
                    });
                }
            }
            Ok(Err(err)) => {
                last_error = Some(if err.is_timeout() {
                    "healthcheck request timed out".to_string()
                } else {
                    err.to_string()
                });
            }
            Err(()) if last_error.is_none() && last_status.is_none() => {
                last_error = Some("healthcheck request timed out".to_string());
            }
            Err(()) => {}
        }

        if Instant::now() >= deadline {
            return Ok(ReadinessOutput {
                ready: false,
                phase: "http",
                target: target.to_string(),
                url: Some(url.to_string()),
                elapsed_ms: elapsed_ms(started),
                timed_out: true,
                last_error,
                last_status,
            });
        }

        sleep_until_next_poll(deadline, request.poll_interval, context).await?;
    }
}

async fn run_until_deadline<T, F>(
    deadline: Instant,
    attempt_timeout: Duration,
    future: F,
) -> Result<T, ()>
where
    F: Future<Output = T>,
{
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(());
    }
    timeout(remaining.min(attempt_timeout), future)
        .await
        .map_err(|_| ())
}

async fn sleep_until_next_poll(
    deadline: Instant,
    poll_interval: Duration,
    context: &ToolContext,
) -> Result<(), ToolError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Ok(());
    }
    let delay = remaining.min(poll_interval);
    if let Some(token) = context.cancel_token.as_ref() {
        tokio::select! {
            () = token.cancelled() => Err(ToolError::cancelled("wait_for_dev_server cancelled")),
            () = sleep(delay) => Ok(()),
        }
    } else {
        sleep(delay).await;
        Ok(())
    }
}

fn check_cancelled(context: &ToolContext) -> Result<(), ToolError> {
    if context
        .cancel_token
        .as_ref()
        .is_some_and(tokio_util::sync::CancellationToken::is_cancelled)
    {
        return Err(ToolError::cancelled("wait_for_dev_server cancelled"));
    }
    Ok(())
}

fn target_label(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn readiness_result(output: ReadinessOutput) -> Result<ToolResult, ToolError> {
    let success = output.ready;
    let metadata = json!({
        "ready": output.ready,
        "phase": output.phase,
        "target": output.target,
        "url": output.url,
        "elapsed_ms": output.elapsed_ms,
        "timed_out": output.timed_out,
        "last_error": output.last_error,
        "last_status": output.last_status,
    });
    let content = serde_json::to_string_pretty(&output).map_err(|err| {
        ToolError::execution_failed(format!("failed to serialize readiness result: {err}"))
    })?;
    Ok(ToolResult {
        content,
        success,
        metadata: Some(metadata),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::{ToolContext, ToolResult, ToolSpec};
    use serde_json::{Value, json};
    use std::path::PathBuf;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpSocket};
    use tokio::task::JoinHandle;

    fn ctx() -> ToolContext {
        ToolContext::new(PathBuf::from("."))
    }

    async fn run_tool(input: Value) -> (ToolResult, Value) {
        let tool = WaitForDevServerTool;
        let result = tool.execute(input, &ctx()).await.expect("tool result");
        let payload = serde_json::from_str(&result.content).expect("json result");
        (result, payload)
    }

    async fn bind_tcp_listener() -> (TcpListener, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        (listener, port)
    }

    fn reserve_unlistened_tcp_port() -> (TcpSocket, u16) {
        let socket = TcpSocket::new_v4().unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = socket.local_addr().unwrap().port();
        (socket, port)
    }

    fn spawn_http_server(status: &'static str) -> (u16, JoinHandle<()>) {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = std_listener.local_addr().unwrap().port();
        std_listener.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(std_listener).unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _addr)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(async move {
                    let mut buf = [0_u8; 512];
                    let _ = stream.read(&mut buf).await;
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (port, handle)
    }

    fn spawn_hanging_http_server() -> (u16, JoinHandle<()>) {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = std_listener.local_addr().unwrap().port();
        std_listener.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(std_listener).unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _addr)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(async move {
                    let _stream = stream;
                    sleep(Duration::from_secs(60)).await;
                });
            }
        });
        (port, handle)
    }

    fn spawn_delayed_http_server(delay: Duration) -> (u16, JoinHandle<()>) {
        let std_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = std_listener.local_addr().unwrap().port();
        std_listener.set_nonblocking(true).unwrap();
        let listener = TcpListener::from_std(std_listener).unwrap();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _addr)) = listener.accept().await else {
                    continue;
                };
                tokio::spawn(async move {
                    let mut buf = [0_u8; 512];
                    let _ = stream.read(&mut buf).await;
                    sleep(delay).await;
                    let response =
                        "HTTP/1.1 204 No Content\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });
        (port, handle)
    }

    #[tokio::test]
    async fn waits_until_tcp_port_accepts_connections() {
        let (listener, port) = bind_tcp_listener().await;
        let accept = tokio::spawn(async move {
            let _ = listener.accept().await;
        });

        let (result, payload) = run_tool(json!({
            "host": "127.0.0.1",
            "port": port,
            "timeout_ms": 1_000,
            "poll_interval_ms": 10
        }))
        .await;

        assert!(result.success);
        assert_eq!(payload["ready"], true);
        assert_eq!(payload["phase"], "ready");
        assert_eq!(payload["target"], format!("127.0.0.1:{port}"));
        assert!(payload["elapsed_ms"].as_u64().is_some());
        let _ = accept.await;
    }

    #[tokio::test]
    async fn reports_timeout_for_refused_tcp_port() {
        // Keep the port reserved without listening. Dropping a listener first
        // lets a parallel test or process claim the port before this probe.
        let (_reservation, port) = reserve_unlistened_tcp_port();

        let (result, payload) = run_tool(json!({
            "host": "127.0.0.1",
            "port": port,
            "timeout_ms": 80,
            "poll_interval_ms": 10
        }))
        .await;

        assert!(!result.success);
        assert_eq!(payload["ready"], false);
        assert_eq!(payload["phase"], "tcp");
        assert_eq!(payload["timed_out"], true);
        assert_eq!(payload["target"], format!("127.0.0.1:{port}"));
        assert!(payload["elapsed_ms"].as_u64().is_some());
        assert!(
            payload["last_error"]
                .as_str()
                .is_some_and(|message| !message.is_empty())
        );
    }

    #[tokio::test]
    async fn waits_for_http_success_status_after_tcp_ready() {
        let (port, server) = spawn_http_server("204 No Content");

        let (result, payload) = run_tool(json!({
            "host": "127.0.0.1",
            "port": port,
            "url": format!("http://127.0.0.1:{port}/health"),
            "timeout_ms": 1_000,
            "poll_interval_ms": 10
        }))
        .await;

        assert!(result.success);
        assert_eq!(payload["ready"], true);
        assert_eq!(payload["phase"], "ready");
        assert_eq!(payload["last_status"], 204);
        server.abort();
    }

    #[tokio::test]
    async fn reports_last_http_status_when_healthcheck_never_succeeds() {
        let (port, server) = spawn_http_server("503 Service Unavailable");

        let (result, payload) = run_tool(json!({
            "host": "127.0.0.1",
            "port": port,
            "url": format!("http://127.0.0.1:{port}/health"),
            "timeout_ms": 120,
            "poll_interval_ms": 10
        }))
        .await;

        assert!(!result.success);
        assert_eq!(payload["ready"], false);
        assert_eq!(payload["phase"], "http");
        assert_eq!(payload["timed_out"], true);
        assert_eq!(payload["last_status"], 503);
        server.abort();
    }

    #[tokio::test]
    async fn reports_http_timeout_when_healthcheck_hangs() {
        let (port, server) = spawn_hanging_http_server();

        let (result, payload) = run_tool(json!({
            "host": "127.0.0.1",
            "port": port,
            "url": format!("http://127.0.0.1:{port}/health"),
            "timeout_ms": 80,
            "poll_interval_ms": 10
        }))
        .await;

        assert!(!result.success);
        assert_eq!(payload["ready"], false);
        assert_eq!(payload["phase"], "http");
        assert_eq!(payload["timed_out"], true);
        assert!(payload["last_status"].is_null());
        assert_eq!(
            payload["last_error"].as_str(),
            Some("healthcheck request timed out")
        );
        server.abort();
    }

    #[tokio::test]
    async fn slow_healthcheck_can_complete_across_short_poll_intervals() {
        let (port, server) = spawn_delayed_http_server(Duration::from_millis(600));

        let (result, payload) = run_tool(json!({
            "host": "127.0.0.1",
            "port": port,
            "url": format!("http://127.0.0.1:{port}/health"),
            "timeout_ms": 2_000,
            "poll_interval_ms": 50
        }))
        .await;

        assert!(result.success);
        assert_eq!(payload["ready"], true);
        assert_eq!(payload["phase"], "ready");
        assert_eq!(payload["last_status"], 204);
        server.abort();
    }

    #[test]
    fn canonicalizes_localhost_to_loopback_literals() {
        let request = parse_request(&json!({
            "host": "localhost",
            "port": 8080,
            "url": "http://localhost:8080/health"
        }))
        .unwrap();

        assert_eq!(request.host, "127.0.0.1");
        let url = request.url.unwrap();
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.as_str(), "http://127.0.0.1:8080/health");
    }

    #[tokio::test]
    async fn rejects_non_loopback_targets() {
        let tool = WaitForDevServerTool;

        let err = tool
            .execute(
                json!({
                    "host": "example.com",
                    "port": 80,
                    "timeout_ms": 10
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("loopback"));

        let err = tool
            .execute(
                json!({
                    "host": "127.0.0.1",
                    "port": 8080,
                    "url": "https://example.com/health",
                    "timeout_ms": 10
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("loopback"));
    }

    #[tokio::test]
    async fn rejects_healthcheck_url_credentials() {
        let tool = WaitForDevServerTool;

        let err = tool
            .execute(
                json!({
                    "host": "127.0.0.1",
                    "port": 8080,
                    "url": "http://user:secret@127.0.0.1:8080/health",
                    "timeout_ms": 10
                }),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("credentials"));
    }
}

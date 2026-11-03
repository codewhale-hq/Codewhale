//! Thin JSON-RPC over stdio client for LSP servers.
//!
//! We deliberately do **not** depend on `tower-lsp` — it is a server-side
//! framework and dragging it in here would add hundreds of unnecessary
//! transitive dependencies and slow down `cargo build` for every contributor.
//! The LSP wire protocol is small enough that handling it ourselves is a
//! self-contained ~400 LOC and lets us keep total control of the spawn
//! lifecycle, timeouts, and the async surface.
//!
//! Architecture:
//!
//! - [`LspTransport`] is the trait the [`super::LspManager`] talks to. The
//!   real implementation is [`StdioLspTransport`] (forks an LSP server with
//!   `tokio::process::Command`); tests use `super::tests::FakeTransport`.
//! - [`StdioLspTransport`] runs three tokio tasks: a reader, a writer, and
//!   the public API. Communication uses tokio mpsc channels.
//! - We parse `Content-Length`-framed JSON-RPC and route inbound messages
//!   either to a per-request response slot (for replies) or to the
//!   diagnostics queue (for `textDocument/publishDiagnostics` notifications).
//!
//! The transport is one-shot per file in MVP form: the manager spawns a
//! transport on demand for a language and reuses it. We do not implement
//! workspace sync beyond didOpen/didChange because the goal is "post-edit
//! diagnostics," not full IDE smartness.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use super::diagnostics::{Diagnostic, Severity};
use codewhale_util::utils::spawn_supervised;

const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LSP_HEADER_BYTES: usize = 8 * 1024;
const MAX_LSP_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// A publication retains the server's document version instead of pretending
/// that a matching URI alone proves which text was checked.
#[derive(Debug)]
pub struct DiagnosticPublication {
    pub items: Vec<Diagnostic>,
    pub document_version: Option<i64>,
    pub diagnostic_version: Option<i64>,
}

impl DiagnosticPublication {
    #[must_use]
    pub fn freshness(&self) -> &'static str {
        if self.document_version.is_some() && self.document_version == self.diagnostic_version {
            "verified"
        } else {
            "unverified"
        }
    }
}

// Diagnostic-only transports have no document-version proof. Existing callers
// may still use their results, but must not claim freshness from an empty list.
impl From<Vec<Diagnostic>> for DiagnosticPublication {
    fn from(items: Vec<Diagnostic>) -> Self {
        Self {
            items,
            document_version: None,
            diagnostic_version: None,
        }
    }
}

/// Source synchronization proof for a semantic reply. Legacy transports keep
/// the raw result but cannot assert which document version served it.
#[derive(Debug)]
pub struct SemanticReply {
    pub result: Value,
    pub document_version: Option<i64>,
}

/// Trait the LSP manager talks to. A real LSP server speaks this via stdio;
/// tests use an in-process fake.
#[async_trait]
pub trait LspTransport: Send + Sync {
    /// Notify the server that a file was opened or its contents updated, then
    /// wait up to `wait` for a `publishDiagnostics` notification for that
    /// file. Returns the diagnostics list (possibly empty). Implementations
    /// must NOT block past `wait`.
    async fn diagnostics_for(
        &self,
        path: &Path,
        text: &str,
        wait: Duration,
    ) -> Result<DiagnosticPublication>;

    /// Send a JSON-RPC request and wait up to `wait` for the reply.
    ///
    /// Default returns "unsupported" so diagnostic-only fakes keep working.
    /// Real transports implement this for go-to-definition, symbols, and
    /// references without spawning a second server lifecycle.
    async fn request(&self, _method: &str, _params: Value, _wait: Duration) -> Result<Value> {
        Err(anyhow!("LSP request not supported by this transport"))
    }

    /// Synchronize and query one document atomically when the transport can
    /// prove that ordering. Diagnostic-only/legacy transports stay unverified.
    async fn request_for_document(
        &self,
        path: &Path,
        text: &str,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<SemanticReply> {
        timeout(wait, async {
            self.ensure_open(path, text).await?;
            Ok(SemanticReply {
                result: self.request(method, params, wait).await?,
                document_version: None,
            })
        })
        .await
        .map_err(|_| anyhow!("LSP semantic request timed out"))?
    }

    /// Ensure `path` is open with `text` (didOpen/didChange) so position-based
    /// requests can target it. Default is a no-op; real transports track opens.
    async fn ensure_open(&self, _path: &Path, _text: &str) -> Result<()> {
        Ok(())
    }

    /// A closed transport is never valid cache evidence. Diagnostic-only
    /// in-process implementations remain usable until their owner removes them.
    fn is_alive(&self) -> bool {
        true
    }

    /// Best-effort shutdown. Called via `LspManager::shutdown_all`.
    async fn shutdown(&self);
}

type DiagnosticMessage = (PathBuf, Option<i64>, Vec<Diagnostic>);

/// Stdio-backed transport. Spawns the LSP server as a child process and
/// pipes JSON-RPC over stdin/stdout. Stderr is drained without retaining or
/// exposing arbitrary server output.
pub struct StdioLspTransport {
    /// JoinHandle for the running server. Held so the child stays alive for
    /// the transport's lifetime; consumed during `shutdown`.
    child: Arc<AsyncMutex<Option<Child>>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    /// Outgoing message sender to the writer task.
    tx_outbound: mpsc::Sender<Vec<u8>>,
    /// Inbound diagnostics queue. We push every `publishDiagnostics`
    /// notification into here and the public API drains the relevant entries.
    diagnostics_gate: AsyncMutex<()>,
    diagnostics_rx: AsyncMutex<mpsc::Receiver<DiagnosticMessage>>,
    /// Map of in-flight request id -> reply slot for model-facing intelligence
    /// requests (definition, references, symbols).
    pending: Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Value>>>>,
    /// Monotonic request id counter for JSON-RPC request/reply methods.
    next_id: AsyncMutex<i64>,
    /// Language id passed in `textDocument/didOpen` (e.g. "rust").
    language_id: String,
    /// Track which files we have opened so the second touch sends
    /// `didChange` instead of `didOpen`.
    opened: AsyncMutex<HashMap<PathBuf, i64>>,
}

impl StdioLspTransport {
    /// Spawn `command args…` and run the LSP `initialize` handshake. Returns
    /// `Err` immediately if the binary is not on PATH or `initialize` fails.
    pub async fn spawn(
        command: &str,
        args: &[String],
        language_id: &str,
        workspace: PathBuf,
    ) -> Result<Self> {
        Self::spawn_with_timeout(command, args, language_id, workspace, INITIALIZE_TIMEOUT).await
    }

    async fn spawn_with_timeout(
        command: &str,
        args: &[String],
        language_id: &str,
        workspace: PathBuf,
        initialize_wait: Duration,
    ) -> Result<Self> {
        let mut cmd = Command::new(command);
        cmd.args(args);
        // Language servers execute workspace code (rust-analyzer runs build
        // scripts and proc-macros, tsserver loads tsconfig plugins), so they
        // start from the sanitized child environment like `exec_shell`.
        codewhale_runtime::child_env::apply_to_tokio_command(
            &mut cmd,
            std::iter::empty::<(&str, &str)>(),
        );
        cmd.stdin(Stdio::piped());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .with_context(|| format!("failed to spawn LSP server `{command}`"))?;

        let stdin = child
            .stdin
            .take()
            .context("LSP child has no stdin handle")?;
        let stdout = child
            .stdout
            .take()
            .context("LSP child has no stdout handle")?;

        let mut stderr = child
            .stderr
            .take()
            .context("LSP child has no stderr handle")?;
        let stderr_task =
            spawn_supervised("lsp-stderr", std::panic::Location::caller(), async move {
                // Drain bytes, not lines: even a single unbounded log line must
                // neither block the child nor accumulate in host memory.
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            });

        let (tx_outbound, rx_outbound) = mpsc::channel::<Vec<u8>>(64);
        let (tx_inbound, rx_inbound) = mpsc::channel::<Value>(64);
        let (tx_diag, rx_diag) = mpsc::channel::<DiagnosticMessage>(64);

        // Writer task: drain outbound channel, frame with Content-Length, write to stdin.
        let writer_task = spawn_supervised(
            "lsp-writer",
            std::panic::Location::caller(),
            writer_task(stdin, rx_outbound),
        );
        // Reader task: parse Content-Length frames from stdout, push to inbound queue.
        let reader_task = spawn_supervised(
            "lsp-reader",
            std::panic::Location::caller(),
            reader_task(stdout, tx_inbound),
        );
        // Inbound dispatcher: routes notifications to `tx_diag`, replies to a
        // pending map. We keep the pending map for completeness even though
        // diagnostics polling itself does not reuse it.
        let pending: Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Value>>>> =
            Arc::new(AsyncMutex::new(HashMap::new()));
        let child = Arc::new(AsyncMutex::new(Some(child)));
        let dispatcher_child = child.clone();
        let dispatcher_pending = pending.clone();
        let dispatcher_task = spawn_supervised(
            "lsp-dispatcher",
            std::panic::Location::caller(),
            async move {
                dispatcher_task(rx_inbound, tx_diag, dispatcher_pending).await;
                // EOF, malformed frames, or an overflowing diagnostics queue
                // terminate the producer too, so its pipes cannot stay stuck.
                if let Some(mut child) = dispatcher_child.lock().await.take() {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                }
            },
        );

        let transport = Self {
            child,
            tasks: vec![stderr_task, writer_task, reader_task, dispatcher_task],
            tx_outbound,
            diagnostics_gate: AsyncMutex::new(()),
            diagnostics_rx: AsyncMutex::new(rx_diag),
            pending,
            next_id: AsyncMutex::new(1),
            language_id: language_id.to_string(),
            opened: AsyncMutex::new(HashMap::new()),
        };
        let result = transport.request("initialize", json!({
            "processId": std::process::id(),
            "rootUri": uri_from_path(&workspace),
            "capabilities": {
                "general": { "positionEncodings": ["utf-16"] },
                "textDocument": {
                    "publishDiagnostics": { "relatedInformation": false, "versionSupport": true }
                }
            },
            "workspaceFolders": [{"uri": uri_from_path(&workspace), "name": "workspace"}]
        }), initialize_wait).await.context("LSP initialization failed")?;
        if !result.get("capabilities").is_some_and(Value::is_object) {
            return Err(anyhow!(
                "LSP initialize response is missing server capabilities"
            ));
        }
        if result
            .pointer("/capabilities/positionEncoding")
            .is_some_and(|encoding| encoding.as_str() != Some("utf-16"))
        {
            return Err(anyhow!("LSP server must use UTF-16 positions"));
        }
        timeout(
            initialize_wait,
            send_message(
                &transport.tx_outbound,
                &json!({
                    "jsonrpc": "2.0", "method": "initialized", "params": {}
                }),
            ),
        )
        .await
        .map_err(|_| anyhow!("LSP initialized notification timed out"))??;
        Ok(transport)
    }
}

impl Drop for StdioLspTransport {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        if let Ok(mut child) = self.child.try_lock()
            && let Some(child) = child.as_mut()
        {
            let _ = child.start_kill();
        }
        // If shutdown/dispatcher currently owns the child, abort releases
        // its local Child and kill_on_drop remains the final fallback.
    }
}

impl StdioLspTransport {
    async fn open_or_change(&self, path: &Path, text: &str) -> Result<(String, i64)> {
        let path_buf = path.to_path_buf();
        let uri = uri_from_path(&path_buf);
        let mut opened = self.opened.lock().await;
        let is_new = !opened.contains_key(&path_buf);
        let new_version = opened.get(&path_buf).copied().unwrap_or(0) + 1;

        let payload = if is_new {
            let language_id = if self.language_id == "typescript"
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("tsx"))
            {
                "typescriptreact"
            } else {
                &self.language_id
            };
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": uri.clone(),
                        "languageId": language_id,
                        "version": new_version,
                        "text": text
                    }
                }
            })
        } else {
            json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": {
                    "textDocument": {
                        "uri": uri.clone(),
                        "version": new_version
                    },
                    "contentChanges": [{ "text": text }]
                }
            })
        };
        send_message(&self.tx_outbound, &payload).await?;
        opened.insert(path_buf, new_version);
        Ok((uri, new_version))
    }
}

#[async_trait]
impl LspTransport for StdioLspTransport {
    fn is_alive(&self) -> bool {
        // stderr may close independently. The writer, reader and dispatcher
        // are the protocol lifetime; none may have exited or been aborted.
        !self.tx_outbound.is_closed() && self.tasks.iter().skip(1).all(|task| !task.is_finished())
    }

    async fn diagnostics_for(
        &self,
        path: &Path,
        text: &str,
        wait: Duration,
    ) -> Result<DiagnosticPublication> {
        // One receiver cannot serve concurrent polling safely: serialize the
        // open/version/send/wait transaction, including semantic ensure_open.
        let deadline = tokio::time::Instant::now() + wait;
        let _gate = timeout(wait, self.diagnostics_gate.lock())
            .await
            .map_err(|_| anyhow!("LSP diagnostics timed out waiting for another document"))?;
        let path_buf = path.to_path_buf();
        let (_, version) = timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            self.open_or_change(path, text),
        )
        .await
        .map_err(|_| anyhow!("LSP diagnostics timed out sending document"))??;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(anyhow!(
                    "LSP diagnostics timed out before a current publication"
                ));
            }
            let mut rx = self.diagnostics_rx.lock().await;
            let (file, published_version, items) = match timeout(remaining, rx.recv()).await {
                Ok(Some(item)) => item,
                Ok(None) => {
                    return Err(anyhow!(
                        "LSP diagnostics channel closed before publishDiagnostics"
                    ));
                }
                Err(_) => {
                    return Err(anyhow!(
                        "LSP diagnostics timed out before a current publication"
                    ));
                }
            };
            if file != path_buf || published_version.is_some_and(|published| published != version) {
                continue;
            }
            return Ok(DiagnosticPublication {
                items,
                document_version: Some(version),
                diagnostic_version: published_version,
            });
        }
    }

    async fn request_for_document(
        &self,
        path: &Path,
        text: &str,
        method: &str,
        params: Value,
        wait: Duration,
    ) -> Result<SemanticReply> {
        let deadline = tokio::time::Instant::now() + wait;
        let _gate = timeout(wait, self.diagnostics_gate.lock())
            .await
            .map_err(|_| anyhow!("LSP semantic request timed out waiting for another document"))?;
        let (_, version) = timeout(
            deadline.saturating_duration_since(tokio::time::Instant::now()),
            self.open_or_change(path, text),
        )
        .await
        .map_err(|_| anyhow!("LSP semantic request timed out sending document"))??;
        let result = self
            .request(
                method,
                params,
                deadline.saturating_duration_since(tokio::time::Instant::now()),
            )
            .await?;
        Ok(SemanticReply {
            result,
            document_version: Some(version),
        })
    }

    async fn ensure_open(&self, path: &Path, text: &str) -> Result<()> {
        let _gate = self.diagnostics_gate.lock().await;
        self.open_or_change(path, text).await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value, wait: Duration) -> Result<Value> {
        let id = {
            let mut next = self.next_id.lock().await;
            let id = *next;
            *next = next.saturating_add(1);
            id
        };
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }
        let payload = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        // The deadline includes queue backpressure, not just the reply.
        let response = timeout(wait, async {
            send_message(&self.tx_outbound, &payload).await?;
            rx.await.map_err(|_| anyhow!("LSP request channel closed"))
        })
        .await;
        self.pending.lock().await.remove(&id);
        match response {
            Ok(Ok(reply)) => {
                if let Some(error) = reply.get("error") {
                    let message = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("LSP request failed");
                    return Err(anyhow!("{message}"));
                }
                reply
                    .get("result")
                    .cloned()
                    .ok_or_else(|| anyhow!("LSP response has no result"))
            }
            Ok(Err(error)) => Err(error),
            Err(_) => Err(anyhow!("LSP request timed out for {method}")),
        }
    }

    async fn shutdown(&self) {
        let mut child = self.child.lock().await;
        if let Some(mut c) = child.take() {
            let _ = c.start_kill();
            let _ = c.wait().await;
        }
        for task in &self.tasks {
            task.abort();
        }
        self.pending.lock().await.clear();
    }
}

/// Send a JSON value as one Content-Length-framed JSON-RPC message.
async fn send_message(tx: &mpsc::Sender<Vec<u8>>, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value).context("serialize LSP message")?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    let mut frame = Vec::with_capacity(header.len() + body.len());
    frame.extend_from_slice(header.as_bytes());
    frame.extend_from_slice(&body);
    tx.send(frame)
        .await
        .map_err(|_| anyhow!("LSP outbound channel closed"))?;
    Ok(())
}

/// Background task that drains the outbound queue and writes each frame to
/// the LSP server's stdin. Exits cleanly when the channel closes.
async fn writer_task(mut stdin: tokio::process::ChildStdin, mut rx: mpsc::Receiver<Vec<u8>>) {
    while let Some(frame) = rx.recv().await {
        if stdin.write_all(&frame).await.is_err() {
            break;
        }
        if stdin.flush().await.is_err() {
            break;
        }
    }
}

/// Background task that parses `Content-Length`-framed JSON-RPC frames from
/// the LSP server's stdout. Pushes each parsed JSON value to `tx`. Exits
/// when stdout closes or a frame is malformed (we choose to fail closed
/// rather than risk hanging).
async fn reader_task(mut stdout: impl AsyncRead + Unpin, tx: mpsc::Sender<Value>) {
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut tmp = [0u8; 4096];
    loop {
        let n = match stdout.read(&mut tmp).await {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };
        buf.extend_from_slice(&tmp[..n]);
        loop {
            let (header_end, content_length) = match parse_header(&buf) {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(_) => return,
            };
            // Both operands are bounded by parse_header.
            let frame_end = header_end + content_length;
            if buf.len() < frame_end {
                break;
            }
            let value = match serde_json::from_slice::<Value>(&buf[header_end..frame_end]) {
                Ok(value) => value,
                Err(_) => return,
            };
            buf.drain(..frame_end);
            if tx.send(value).await.is_err() {
                return;
            }
        }
    }
}

/// Distinguish incomplete headers from malformed or oversized frames so a
/// broken server cannot cause an indefinitely growing input buffer.
fn parse_header(buf: &[u8]) -> Result<Option<(usize, usize)>> {
    let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") else {
        if buf.len() > MAX_LSP_HEADER_BYTES {
            return Err(anyhow!("LSP header exceeds size limit"));
        }
        return Ok(None);
    };
    if pos + 4 > MAX_LSP_HEADER_BYTES {
        return Err(anyhow!("LSP header exceeds size limit"));
    }
    let header = std::str::from_utf8(&buf[..pos]).context("invalid LSP header encoding")?;
    let mut content_length = None;
    for line in header.split("\r\n") {
        let (name, value) = line.split_once(':').context("malformed LSP header")?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if content_length.is_some() {
                return Err(anyhow!("duplicate LSP Content-Length"));
            }
            let length = value
                .trim()
                .parse::<usize>()
                .context("invalid LSP Content-Length")?;
            if length == 0 || length > MAX_LSP_FRAME_BYTES {
                return Err(anyhow!("LSP frame exceeds size limit or is empty"));
            }
            content_length = Some(length);
        }
    }
    Ok(Some((
        pos + 4,
        content_length.context("missing LSP Content-Length")?,
    )))
}

/// Background task that consumes inbound JSON values, classifies them as
/// notifications/responses, and routes accordingly.
async fn dispatcher_task(
    mut rx: mpsc::Receiver<Value>,
    tx_diag: mpsc::Sender<DiagnosticMessage>,
    pending: Arc<AsyncMutex<HashMap<i64, oneshot::Sender<Value>>>>,
) {
    while let Some(value) = rx.recv().await {
        // Notifications have a `method` and no `id`.
        let method = value.get("method").and_then(|v| v.as_str());
        if method == Some("textDocument/publishDiagnostics") {
            if let Some(publication) = parse_publish_diagnostics(&value) {
                // Do not let an unconsumed diagnostics burst prevent reply
                // delivery or EOF cleanup. Overflow closes this transport's
                // dispatcher, giving callers an explicit channel error.
                if tx_diag.try_send(publication).is_err() {
                    break;
                }
            }
            continue;
        }
        // Replies have an `id` and a `result` or `error`.
        if let Some(id) = value.get("id").and_then(|v| v.as_i64()) {
            let mut map = pending.lock().await;
            if let Some(slot) = map.remove(&id) {
                let _ = slot.send(value);
            }
        }
    }
    // Reader EOF/malformed frames and queue overflow wake every pending
    // request immediately instead of leaving reply slots until their timeout.
    pending.lock().await.clear();
}

/// Decode a `textDocument/publishDiagnostics` notification.
fn parse_publish_diagnostics(value: &Value) -> Option<DiagnosticMessage> {
    let params = value.get("params")?;
    let uri = params.get("uri")?.as_str()?;
    let path = path_from_uri(uri)?;
    let version = match params.get("version") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_i64()?),
    };
    let raw = params.get("diagnostics")?.as_array()?;
    let mut out = Vec::with_capacity(raw.len());
    for d in raw {
        let range = d.get("range")?;
        let start = range.get("start")?;
        let line = start.get("line")?.as_u64()? as u32 + 1;
        let column = start.get("character")?.as_u64()? as u32 + 1;
        let severity = Severity::from_lsp(d.get("severity").and_then(|v| v.as_i64()))
            .unwrap_or(Severity::Error);
        let message = d
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        out.push(Diagnostic {
            line,
            column,
            severity,
            message,
        });
    }
    Some((path, version, out))
}

/// Encode an absolute filesystem path without following its links.
pub fn uri_from_path(path: &Path) -> String {
    reqwest::Url::from_file_path(path)
        .map(|url| url.to_string())
        .unwrap_or_default()
}

/// File URLs only: no network authority, query or fragment. Decoding happens
/// before the workspace no-follow opener validates the target path.
pub(super) fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let url = reqwest::Url::parse(uri).ok()?;
    if url.scheme() != "file"
        || url.host_str().is_some_and(|host| !host.is_empty())
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    url.to_file_path().ok()
}

#[cfg(any(test, feature = "test-support"))]
#[cfg_attr(not(test), allow(dead_code, unused_imports))]
pub(super) mod tests {
    use super::*;

    fn fixture_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join("codewhale-lsp-fixture")
            .join(name)
    }

    #[test]
    fn parses_lsp_header() {
        let frame = b"Content-Length: 5\r\n\r\nhello";
        let (end, len) = parse_header(frame)
            .expect("valid header")
            .expect("header parses");
        assert_eq!(end, 21);
        assert_eq!(len, 5);
    }

    #[test]
    fn parse_header_returns_none_when_truncated() {
        let frame = b"Content-Length: 5\r\nMissingTerm";
        assert!(parse_header(frame).unwrap().is_none());
    }

    #[test]
    fn parses_publish_diagnostics_payload() {
        let payload = json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": uri_from_path(&fixture_path("foo.rs")),
                "diagnostics": [
                    {
                        "range": {
                            "start": { "line": 11, "character": 7 },
                            "end":   { "line": 11, "character": 8 }
                        },
                        "severity": 1,
                        "message": "missing semicolon"
                    }
                ]
            }
        });
        let (path, version, diags) = parse_publish_diagnostics(&payload).expect("parses");
        assert_eq!(path, fixture_path("foo.rs"));
        assert_eq!(version, None);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 12);
        assert_eq!(diags[0].column, 8);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].message, "missing semicolon");
    }

    #[test]
    fn round_trips_uri_path() {
        let path = fixture_path("example/foo.rs");
        let uri = uri_from_path(&path);
        assert_eq!(path_from_uri(&uri), Some(path));
    }

    #[tokio::test]
    async fn closed_diagnostics_channel_is_an_error_not_an_empty_result() {
        let (tx_outbound, _rx_outbound) = mpsc::channel(1);
        let (tx_diag, rx_diag) = mpsc::channel(1);
        drop(tx_diag);
        let transport = StdioLspTransport {
            child: Arc::new(AsyncMutex::new(None)),
            tasks: Vec::new(),
            tx_outbound,
            diagnostics_gate: AsyncMutex::new(()),
            diagnostics_rx: AsyncMutex::new(rx_diag),
            pending: Arc::new(AsyncMutex::new(HashMap::new())),
            next_id: AsyncMutex::new(1),
            language_id: "rust".to_string(),
            opened: AsyncMutex::new(HashMap::new()),
        };

        let error = transport
            .diagnostics_for(
                &fixture_path("closed-channel.rs"),
                "fn main() {}\n",
                Duration::from_millis(10),
            )
            .await
            .expect_err("a closed transport must not look like an empty lint result");

        assert!(
            error.to_string().contains("diagnostics channel closed"),
            "unexpected error: {error}"
        );
    }

    fn diagnostic_fixture() -> (
        StdioLspTransport,
        mpsc::Receiver<Vec<u8>>,
        mpsc::Sender<DiagnosticMessage>,
    ) {
        let (tx_outbound, rx_outbound) = mpsc::channel(8);
        let (tx_diag, rx_diag) = mpsc::channel(8);
        (
            StdioLspTransport {
                child: Arc::new(AsyncMutex::new(None)),
                tasks: Vec::new(),
                tx_outbound,
                diagnostics_gate: AsyncMutex::new(()),
                diagnostics_rx: AsyncMutex::new(rx_diag),
                pending: Arc::new(AsyncMutex::new(HashMap::new())),
                next_id: AsyncMutex::new(1),
                language_id: "rust".into(),
                opened: AsyncMutex::new(HashMap::new()),
            },
            rx_outbound,
            tx_diag,
        )
    }

    async fn next_document(rx: &mut mpsc::Receiver<Vec<u8>>) -> Value {
        let frame = rx.recv().await.unwrap();
        let (start, _) = parse_header(&frame).unwrap().unwrap();
        serde_json::from_slice::<Value>(&frame[start..]).unwrap()
    }

    fn diagnostic(line: u32) -> Diagnostic {
        Diagnostic {
            line,
            column: 1,
            severity: Severity::Error,
            message: "fixture".into(),
        }
    }

    #[tokio::test]
    async fn diagnostic_freshness_rejects_old_version_and_accepts_current_empty_publication() {
        let (transport, mut outbound, diag) = diagnostic_fixture();
        let server = tokio::spawn(async move {
            let first = next_document(&mut outbound).await;
            assert_eq!(first["method"], "textDocument/didOpen");
            assert_eq!(first["params"]["textDocument"]["version"], 1);
            let path =
                path_from_uri(first["params"]["textDocument"]["uri"].as_str().unwrap()).unwrap();
            diag.send((path.clone(), Some(1), vec![diagnostic(1)]))
                .await
                .unwrap();
            let second = next_document(&mut outbound).await;
            assert_eq!(second["method"], "textDocument/didChange");
            assert_eq!(second["params"]["textDocument"]["version"], 2);
            assert_eq!(second["params"]["contentChanges"][0]["text"], "new 🐋 text");
            diag.send((path.clone(), Some(1), vec![diagnostic(99)]))
                .await
                .unwrap();
            diag.send((path, Some(2), vec![])).await.unwrap();
        });
        let path = &fixture_path("freshness.rs");
        let first = transport
            .diagnostics_for(path, "old text", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(first.freshness(), "verified");
        assert_eq!(first.items[0].line, 1);
        let second = transport
            .diagnostics_for(path, "new 🐋 text", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(second.freshness(), "verified");
        assert_eq!(second.document_version, Some(2));
        assert_eq!(second.diagnostic_version, Some(2));
        assert!(
            second.items.is_empty(),
            "old error must not apply to the new text"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn diagnostic_freshness_serializes_concurrent_file_requests() {
        let (transport, mut outbound, diag) = diagnostic_fixture();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let request = next_document(&mut outbound).await;
                assert!(
                    matches!(outbound.try_recv(), Err(mpsc::error::TryRecvError::Empty)),
                    "second file must not advance before this publication"
                );
                let path =
                    path_from_uri(request["params"]["textDocument"]["uri"].as_str().unwrap())
                        .unwrap();
                let line = if path.ends_with("one.rs") { 1 } else { 2 };
                diag.send((fixture_path("unrelated.rs"), Some(1), vec![diagnostic(99)]))
                    .await
                    .unwrap();
                diag.send((path, Some(1), vec![diagnostic(line)]))
                    .await
                    .unwrap();
            }
        });
        let one_path = fixture_path("one.rs");
        let two_path = fixture_path("two.rs");
        let (one, two) = tokio::join!(
            transport.diagnostics_for(&one_path, "one", Duration::from_secs(1)),
            transport.diagnostics_for(&two_path, "two", Duration::from_secs(1)),
        );
        assert_eq!(one.unwrap().items[0].line, 1);
        assert_eq!(two.unwrap().items[0].line, 2);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn diagnostic_freshness_unversioned_is_unverified_and_silence_is_error() {
        let (transport, mut outbound, diag) = diagnostic_fixture();
        let server = tokio::spawn(async move {
            let request = next_document(&mut outbound).await;
            let path =
                path_from_uri(request["params"]["textDocument"]["uri"].as_str().unwrap()).unwrap();
            diag.send((path, None, vec![])).await.unwrap();
            // Keep the channels alive while the second request times out.
            let _ = next_document(&mut outbound).await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });
        let response = transport
            .diagnostics_for(
                &fixture_path("unversioned.rs"),
                "text",
                Duration::from_secs(1),
            )
            .await
            .unwrap();
        assert_eq!(response.freshness(), "unverified");
        assert_eq!(response.diagnostic_version, None);
        assert!(
            transport
                .diagnostics_for(
                    &fixture_path("silent.rs"),
                    "text",
                    Duration::from_millis(10)
                )
                .await
                .unwrap_err()
                .to_string()
                .contains("timed out")
        );
        server.abort();
    }

    #[test]
    fn diagnostic_freshness_parser_retains_version_and_rejects_malformed_version() {
        let mut payload = json!({"params":{"uri":uri_from_path(&fixture_path("version.rs")),"version":4,"diagnostics":[]}});
        assert_eq!(parse_publish_diagnostics(&payload).unwrap().1, Some(4));
        payload["params"]["version"] = json!("4");
        assert!(parse_publish_diagnostics(&payload).is_none());
    }
    #[cfg(unix)]
    pub const STDIO_FIXTURE: &str = r#"
import json, os, select, sys, time
mode, pid_path = sys.argv[1:]
input_stream = os.fdopen(0, 'rb', buffering=0)
with open(pid_path, 'a' if mode == 'cache' else 'w') as f:
    f.write(str(os.getpid()) + ('\n' if mode == 'cache' else ''))
def read():
    headers = {}
    while True:
        line = input_stream.readline()
        if not line:
            raise SystemExit(0)
        if line == b'\r\n':
            break
        key, value = line.decode().split(':', 1)
        headers[key.lower()] = value.strip()
    body = b''
    length = int(headers['content-length'])
    while len(body) < length:
        chunk = input_stream.read(length - len(body))
        if not chunk:
            raise SystemExit(0)
        body += chunk
    return json.loads(body)
def send(value):
    body = json.dumps(value).encode()
    sys.stdout.buffer.write(('Content-Length: %d\r\n\r\n' % len(body)).encode() + body)
    sys.stdout.buffer.flush()
request = read()
assert request['method'] == 'initialize'
if mode == 'eof':
    raise SystemExit(0)
if mode == 'silence':
    time.sleep(60)
if mode == 'error':
    send({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32002,'message':'fixture rejected initialization'}})
    time.sleep(60)
if mode == 'delayed' and select.select([input_stream], [], [], 0.1)[0]:
    send({'jsonrpc':'2.0','id':request['id'],'error':{'code':-32002,'message':'notification arrived before initialize reply'}})
    raise SystemExit(1)
if mode == 'stderr':
    for _ in range(64):
        os.write(2, b'x' * 32768)
send({'jsonrpc':'2.0','id':request['id'],'result':{'capabilities':{}}})
assert read()['method'] == 'initialized'
while True:
    request = read()
    if request.get('method') == 'fixture/exit':
        raise SystemExit(0)
    if request.get('method') == 'fixture/overflow':
        for _ in range(80):
            send({'jsonrpc':'2.0','method':'textDocument/publishDiagnostics','params':{'uri':'file:///tmp/overflow.rs','version':1,'diagnostics':[]}})
        time.sleep(60)
    elif 'id' in request:
        send({'jsonrpc':'2.0','id':request['id'],'result':{'ready':True}})
"#;

    #[cfg(unix)]
    async fn spawn_stdio_fixture(
        mode: &str,
        root: &Path,
        wait: Duration,
    ) -> Result<StdioLspTransport> {
        StdioLspTransport::spawn_with_timeout(
            "python3",
            &[
                "-u".into(),
                "-c".into(),
                STDIO_FIXTURE.into(),
                mode.into(),
                root.join("pid").to_string_lossy().into_owned(),
            ],
            "rust",
            root.to_path_buf(),
            wait,
        )
        .await
    }

    #[cfg(unix)]
    async fn assert_fixture_exited(root: &Path) {
        let pid = std::fs::read_to_string(root.join("pid"))
            .expect("fixture started")
            .parse::<i32>()
            .unwrap();
        for _ in 0..100 {
            // Signal zero probes only this fixture PID; it never sends a signal.
            if unsafe { libc::kill(pid, 0) } == -1 {
                assert_eq!(
                    std::io::Error::last_os_error().raw_os_error(),
                    Some(libc::ESRCH)
                );
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("fixture child remained alive after transport termination");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_startup_waits_for_initialize_and_drains_stderr_pressure() {
        for mode in ["delayed", "stderr"] {
            let root = tempfile::tempdir().unwrap();
            let transport = spawn_stdio_fixture(mode, root.path(), Duration::from_secs(3))
                .await
                .unwrap();
            let result = transport
                .request("fixture/ready", json!({}), Duration::from_secs(1))
                .await
                .unwrap();
            assert_eq!(result["ready"], true, "{mode}");
            transport.shutdown().await;
            assert_fixture_exited(root.path()).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_startup_error_eof_and_silence_fail_and_terminate_child() {
        for (mode, expected) in [
            ("error", "fixture rejected initialization"),
            ("eof", "channel closed"),
            ("silence", "timed out"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let error = match timeout(
                Duration::from_secs(3),
                spawn_stdio_fixture(mode, root.path(), Duration::from_millis(500)),
            )
            .await
            .unwrap()
            {
                Ok(_) => panic!("{mode} unexpectedly initialized"),
                Err(error) => error,
            };
            assert!(format!("{error:#}").contains(expected), "{mode}: {error:#}");
            assert_fixture_exited(root.path()).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_diagnostic_overflow_fails_pending_request_and_terminates_child() {
        let root = tempfile::tempdir().unwrap();
        let transport = spawn_stdio_fixture("ready", root.path(), Duration::from_secs(3))
            .await
            .unwrap();
        let error = transport
            .request("fixture/overflow", json!({}), Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("channel closed"), "{error:#}");
        assert!(transport.pending.lock().await.is_empty());
        assert_fixture_exited(root.path()).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn language_server_does_not_inherit_parent_secret_env() {
        use codewhale_runtime::test_support::{EnvVarGuard, lock_test_env};
        let _env_lock = lock_test_env();
        let _secret = EnvVarGuard::set("CODEWHALE_TEST_LSP_SECRET", "lsp-secret-value");
        let root = tempfile::tempdir().unwrap();
        let report = root.path().join("env");
        // A fake server that records what it can see and exits before
        // answering `initialize`.
        let script = format!(
            "printf 'secret=%s\\n' \"${{CODEWHALE_TEST_LSP_SECRET-unset}}\" > '{}'; \
             [ -n \"$PATH\" ] && printf 'path-ok\\n' >> '{}'",
            report.display(),
            report.display()
        );
        let result = StdioLspTransport::spawn_with_timeout(
            "/bin/sh",
            &["-c".into(), script],
            "rust",
            root.path().to_path_buf(),
            Duration::from_secs(3),
        )
        .await;
        assert!(result.is_err(), "fake server exits without initializing");
        let seen = std::fs::read_to_string(&report).expect("fake server ran");
        assert!(seen.contains("secret=unset"), "{seen}");
        assert!(!seen.contains("lsp-secret-value"), "{seen}");
        assert!(seen.contains("path-ok"), "{seen}");
    }

    #[tokio::test]
    async fn semantic_request_holds_document_gate_until_reply_before_diagnostics() {
        timeout(Duration::from_secs(2), async {
            let (transport, mut outbound, diag) = diagnostic_fixture();
            let transport = Arc::new(transport);
            let semantic = {
                let transport = transport.clone();
                tokio::spawn(async move {
                    transport
                        .request_for_document(
                            &fixture_path("semantic.rs"),
                            "before",
                            "textDocument/definition",
                            json!({}),
                            Duration::from_secs(1),
                        )
                        .await
                })
            };
            let open = next_document(&mut outbound).await;
            assert_eq!(open["method"], "textDocument/didOpen");
            let request = next_document(&mut outbound).await;
            assert_eq!(request["method"], "textDocument/definition");
            let diagnostics = {
                let transport = transport.clone();
                tokio::spawn(async move {
                    transport
                        .diagnostics_for(
                            &fixture_path("semantic.rs"),
                            "after",
                            Duration::from_secs(1),
                        )
                        .await
                })
            };
            assert!(
                timeout(Duration::from_millis(20), outbound.recv())
                    .await
                    .is_err()
            );
            transport
                .pending
                .lock()
                .await
                .remove(&request["id"].as_i64().unwrap())
                .unwrap()
                .send(json!({"result":[]}))
                .unwrap();
            assert_eq!(semantic.await.unwrap().unwrap().document_version, Some(1));
            let change = next_document(&mut outbound).await;
            assert_eq!(change["params"]["textDocument"]["version"], 2);
            diag.send((fixture_path("semantic.rs"), Some(2), vec![]))
                .await
                .unwrap();
            assert_eq!(
                diagnostics.await.unwrap().unwrap().document_version,
                Some(2)
            );
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn semantic_deadline_includes_gate_wait_and_cleans_pending_request() {
        let (transport, _outbound, _diag) = diagnostic_fixture();
        let gate = transport.diagnostics_gate.lock().await;
        let result = timeout(
            Duration::from_millis(250),
            transport.request_for_document(
                &fixture_path("a.rs"),
                "a",
                "textDocument/definition",
                json!({}),
                Duration::from_millis(20),
            ),
        )
        .await
        .unwrap();
        assert!(result.unwrap_err().to_string().contains("timed out"));
        drop(gate);
        assert!(transport.pending.lock().await.is_empty());
        let result = transport
            .request_for_document(
                &fixture_path("a.rs"),
                "a",
                "textDocument/definition",
                json!({}),
                Duration::from_millis(20),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("timed out"));
        assert!(transport.pending.lock().await.is_empty());
    }

    #[test]
    fn semantic_file_uris_decode_spaces_and_reject_authority_and_query() {
        let path = &fixture_path("a b#🐋.rs");
        assert_eq!(
            path_from_uri(&uri_from_path(path)).as_deref(),
            Some(path.as_path())
        );
        for uri in [
            "https://example.test/a.rs",
            "file://remote/tmp/a.rs",
            "file:///tmp/a.rs?q=1",
            "file:///tmp/a.rs#fragment",
        ] {
            assert!(path_from_uri(uri).is_none(), "{uri}");
        }
    }

    #[tokio::test]
    async fn stdio_request_deadline_includes_full_outbound_queue() {
        let (transport, _outbound, _diag) = diagnostic_fixture();
        for _ in 0..8 {
            transport.tx_outbound.try_send(vec![]).unwrap();
        }
        let error = timeout(
            Duration::from_millis(250),
            transport.request("fixture/blocked", json!({}), Duration::from_millis(20)),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(transport.pending.lock().await.is_empty());
    }

    #[tokio::test]
    async fn stdio_reader_bounds_and_malformed_frames_close_pending_replies() {
        let frames = [
            vec![b'x'; MAX_LSP_HEADER_BYTES + 1],
            format!("Content-Length: {}\r\n\r\n", MAX_LSP_FRAME_BYTES + 1).into_bytes(),
            b"Content-Length: 999999999999999999999999999999999\r\n\r\n".to_vec(),
            b"Content-Length: 1\r\nContent-Length: 1\r\n\r\nx".to_vec(),
            b"Missing-Length: 1\r\n\r\nx".to_vec(),
            b"Content-Length: 1\r\n\r\n{".to_vec(),
        ];
        for frame in frames {
            let (mut producer, reader) = tokio::io::duplex(MAX_LSP_HEADER_BYTES * 2);
            let (tx, rx) = mpsc::channel(8);
            let (diag, _diagnostics) = mpsc::channel(8);
            let pending = Arc::new(AsyncMutex::new(HashMap::new()));
            let (reply, receiver) = oneshot::channel();
            pending.lock().await.insert(1, reply);
            let read_task = tokio::spawn(reader_task(reader, tx));
            let dispatch = tokio::spawn(dispatcher_task(rx, diag, pending.clone()));
            producer.write_all(&frame).await.unwrap();
            assert!(
                timeout(Duration::from_secs(1), receiver)
                    .await
                    .unwrap()
                    .is_err()
            );
            read_task.await.unwrap();
            dispatch.await.unwrap();
            assert!(pending.lock().await.is_empty());
        }
    }

    #[tokio::test]
    async fn stdio_reader_preserves_fragmented_and_coalesced_valid_frames() {
        let (mut producer, reader) = tokio::io::duplex(128);
        let (tx, mut rx) = mpsc::channel(8);
        let task = tokio::spawn(reader_task(reader, tx));
        producer.write_all(b"content-length: 2\r\n").await.unwrap();
        producer
            .write_all(b"\r\n{}Content-Length: 2\r\n\r\n[]")
            .await
            .unwrap();
        assert_eq!(
            timeout(Duration::from_secs(1), rx.recv()).await.unwrap(),
            Some(json!({}))
        );
        assert_eq!(
            timeout(Duration::from_secs(1), rx.recv()).await.unwrap(),
            Some(json!([]))
        );
        drop(producer);
        task.await.unwrap();
        assert!(rx.recv().await.is_none());
    }
}

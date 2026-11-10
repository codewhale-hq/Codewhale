//! The Engine's one Agent Client Protocol client, over a fleet worker's duplex
//! stdio.
//!
//! The fleet host owns the process: groups, parent-death, stop, restart and
//! logs. This module owns only the wire: newline-framed JSON-RPC, one request
//! at a time, and the client side of the agent's requests. A caller supplies
//! an [`AcpHandler`] that sees every `session/update` and answers every
//! `session/request_permission`; [`PermissionPolicy`] is the fixed-answer
//! handler for callers with no person in the loop.
//!
//! Deliberately absent: filesystem and terminal client methods (advertised
//! false and answered method-not-found), elicitation (declined), `authenticate`
//! and any credential handling. The agent runs in its own process with its
//! own login and tools, and its stderr stays in the worker log. Messages from
//! another session id are ignored, and a permission request for one is
//! answered `cancelled`. A line longer than [`MAX_MESSAGE_BYTES`] ends the
//! connection rather than being parsed.

use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::host::FleetWorkerIo;

const CLIENT_NAME: &str = "codewhale";
const METHOD_NOT_FOUND: i64 = -32601;
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
pub const AUTH_REQUIRED: i64 = -32000;
const ABORT_POLL: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionPolicy {
    /// Choose the first option the agent labels `allow_once`.
    AllowOnce,
    /// Choose the first option the agent labels `reject_once`.
    RejectOnce,
    /// Answer `cancelled`, the protocol's "no decision" outcome.
    Cancel,
}

impl PermissionPolicy {
    fn option_kind(self) -> &'static str {
        match self {
            PermissionPolicy::AllowOnce => "allow_once",
            PermissionPolicy::RejectOnce => "reject_once",
            PermissionPolicy::Cancel => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionAnswer {
    Selected(String),
    Cancelled,
}

impl PermissionAnswer {
    fn outcome(&self) -> Value {
        match self {
            PermissionAnswer::Selected(option_id) => {
                json!({"outcome": "selected", "optionId": option_id})
            }
            PermissionAnswer::Cancelled => json!({"outcome": "cancelled"}),
        }
    }
}

pub trait AcpHandler {
    fn update(&mut self, update: &Value);
    fn permission(&mut self, params: &Value) -> PermissionAnswer;
}

#[derive(Debug)]
pub struct AcpError {
    message: String,
    code: Option<i64>,
}

impl AcpError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }

    fn timeout(method: &str) -> Self {
        Self::new(format!("{method} did not answer before the deadline"))
    }

    pub fn code(&self) -> Option<i64> {
        self.code
    }

    pub fn is_auth_required(&self) -> bool {
        self.code == Some(AUTH_REQUIRED) || self.message.to_ascii_lowercase().contains("auth")
    }
}

impl std::fmt::Display for AcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AcpError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    pub name: String,
    pub version: String,
    pub protocol_version: u64,
    pub load_session: bool,
    pub resume_session: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptOutcome {
    pub stop_reason: String,
    pub text: String,
    pub tool_calls: Vec<String>,
    pub decisions: Vec<String>,
}

struct PolicyHandler<'a> {
    policy: PermissionPolicy,
    outcome: &'a mut PromptOutcome,
}

impl AcpHandler for PolicyHandler<'_> {
    fn update(&mut self, update: &Value) {
        match update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or("")
        {
            "agent_message_chunk" => {
                if let Some(chunk) = update
                    .get("content")
                    .and_then(|content| content.get("text"))
                    .and_then(Value::as_str)
                {
                    self.outcome.text.push_str(chunk);
                }
            }
            "tool_call" => {
                let title = update
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let kind = update
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("other");
                self.outcome.tool_calls.push(format!("{kind}:{title}"));
            }
            _ => {}
        }
    }

    fn permission(&mut self, params: &Value) -> PermissionAnswer {
        let answer = if self.policy == PermissionPolicy::Cancel {
            PermissionAnswer::Cancelled
        } else {
            AcpClient::permission_option(params, self.policy)
                .map_or(PermissionAnswer::Cancelled, PermissionAnswer::Selected)
        };
        if let PermissionAnswer::Selected(option) = &answer {
            self.outcome.decisions.push(option.clone());
        }
        answer
    }
}

struct DiscardHandler;

impl AcpHandler for DiscardHandler {
    fn update(&mut self, _update: &Value) {}

    fn permission(&mut self, _params: &Value) -> PermissionAnswer {
        PermissionAnswer::Cancelled
    }
}

type Line = Result<String, String>;

/// One live ACP conversation over a worker's pipes.
pub struct AcpClient {
    stdin: Arc<Mutex<std::process::ChildStdin>>,
    lines: Receiver<Line>,
    next_id: u64,
    abort: Option<CancellationToken>,
}

/// A write-only handle that can send `session/cancel` while a prompt blocks
/// the client that owns the reader.
#[derive(Clone)]
pub struct AcpCanceller {
    stdin: Arc<Mutex<std::process::ChildStdin>>,
    session: String,
}

impl AcpCanceller {
    pub fn cancel(&self) -> Result<(), AcpError> {
        write_line(
            &self.stdin,
            &json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": self.session}}),
        )
    }
}

impl std::fmt::Debug for AcpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpClient")
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

fn write_line(stdin: &Mutex<std::process::ChildStdin>, message: &Value) -> Result<(), AcpError> {
    write_line_unless_cancelled(stdin, message, None)
}

fn write_line_unless_cancelled(
    stdin: &Mutex<std::process::ChildStdin>,
    message: &Value,
    cancel: Option<&CancellationToken>,
) -> Result<(), AcpError> {
    let mut line = serde_json::to_string(message)
        .map_err(|err| AcpError::new(format!("encoding a message failed: {err}")))?;
    line.push('\n');
    let mut stdin = stdin
        .lock()
        .map_err(|_| AcpError::new("the agent's stdin lock was poisoned"))?;
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(AcpError::new(
            "session/prompt was cancelled before dispatch",
        ));
    }
    stdin
        .write_all(line.as_bytes())
        .and_then(|()| stdin.flush())
        .map_err(|err| AcpError::new(format!("the agent's stdin closed: {err}")))
}

fn read_bounded_line(
    stdout: &mut std::io::BufReader<std::process::ChildStdout>,
    line: &mut Vec<u8>,
) -> std::io::Result<usize> {
    line.clear();
    (&mut *stdout)
        .take(MAX_MESSAGE_BYTES as u64 + 1)
        .read_until(b'\n', line)
}

impl AcpClient {
    pub fn new(io: FleetWorkerIo) -> Self {
        let FleetWorkerIo { stdin, stdout } = io;
        let (tx, lines) = sync_channel::<Line>(2);
        std::thread::spawn(move || {
            let mut stdout = stdout;
            let mut line = Vec::new();
            loop {
                match read_bounded_line(&mut stdout, &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if line.len() > MAX_MESSAGE_BYTES {
                            let _ = tx.send(Err(format!(
                                "the agent sent a message larger than {MAX_MESSAGE_BYTES} bytes"
                            )));
                            break;
                        }
                        let text = match std::str::from_utf8(&line) {
                            Ok(text) => text.trim_end().to_string(),
                            Err(_) => {
                                let _ = tx.send(Err("the agent sent invalid UTF-8".to_string()));
                                break;
                            }
                        };
                        if text.is_empty() {
                            continue;
                        }
                        if tx.send(Ok(text)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            stdin: Arc::new(Mutex::new(stdin)),
            lines,
            next_id: 1,
            abort: None,
        }
    }

    pub fn abort_requests_on(&mut self, abort: Option<CancellationToken>) {
        self.abort = abort;
    }

    fn write(&self, message: &Value) -> Result<(), AcpError> {
        write_line(&self.stdin, message)
    }

    /// A handle that can settle a blocked prompt from another thread.
    pub fn canceller(&self, session: &str) -> AcpCanceller {
        AcpCanceller {
            stdin: Arc::clone(&self.stdin),
            session: session.to_string(),
        }
    }

    fn next(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn read(&mut self, deadline: Instant, method: &str) -> Result<Value, AcpError> {
        let line = loop {
            if self
                .abort
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return Err(AcpError::new(format!(
                    "{method} was cancelled before the agent answered"
                )));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(AcpError::timeout(method));
            }
            let wait = if self.abort.is_some() {
                left.min(ABORT_POLL)
            } else {
                left
            };
            match self.lines.recv_timeout(wait) {
                Ok(Ok(line)) => break line,
                Ok(Err(reason)) => return Err(AcpError::new(reason)),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(AcpError::new(format!(
                        "the agent exited before answering {method}"
                    )));
                }
            }
        };
        let message: Value = serde_json::from_str(&line).map_err(|err| {
            AcpError::new(format!("the agent sent a line that is not JSON: {err}"))
        })?;
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(AcpError::new("the agent sent an invalid JSON-RPC envelope"));
        }
        Ok(message)
    }

    fn request(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
        deadline: Instant,
        handler: &mut dyn AcpHandler,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, AcpError> {
        let id = self.next();
        write_line_unless_cancelled(
            &self.stdin,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
            cancel,
        )?;
        loop {
            let message = self.read(deadline, method)?;
            let incoming = message.get("method").and_then(Value::as_str);
            if incoming.is_none() && message.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = message.get("error") {
                    let text = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the agent refused");
                    return Err(AcpError {
                        message: format!("{method} failed: {text}"),
                        code: error.get("code").and_then(Value::as_i64),
                    });
                }
                return message.get("result").cloned().ok_or_else(|| {
                    AcpError::new("the agent sent a response without a result or error")
                });
            }
            let Some(incoming) = incoming else {
                continue;
            };
            let incoming_id = message.get("id").cloned();
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            let ours = session.is_some_and(|session| {
                params.get("sessionId").and_then(Value::as_str) == Some(session)
            });
            match (incoming, incoming_id) {
                ("session/update", None) => {
                    if ours && let Some(update) = params.get("update") {
                        handler.update(update);
                    }
                }
                ("session/request_permission", Some(request_id)) => {
                    let answer = if ours {
                        handler.permission(&params)
                    } else {
                        PermissionAnswer::Cancelled
                    };
                    write_line_unless_cancelled(
                        &self.stdin,
                        &json!({"jsonrpc": "2.0", "id": request_id, "result": {"outcome": answer.outcome()}}),
                        cancel,
                    )?;
                }
                ("elicitation/create", Some(request_id)) => {
                    self.write(&json!({"jsonrpc": "2.0", "id": request_id, "result": {"action": "decline"}}))?;
                }
                (other, Some(request_id)) => {
                    self.write(&json!({"jsonrpc": "2.0", "id": request_id, "error": {"code": METHOD_NOT_FOUND, "message": format!("{other} is not supported by this client")}}))?;
                }
                (_, None) => {}
            }
        }
    }

    pub fn initialize(&mut self, timeout: Duration) -> Result<AgentIdentity, AcpError> {
        let deadline = Instant::now() + timeout;
        let result = self.request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
                "clientInfo": {"name": CLIENT_NAME, "version": env!("CARGO_PKG_VERSION")}
            }),
            None,
            deadline,
            &mut DiscardHandler,
            None,
        )?;
        let info = result.get("agentInfo").cloned().unwrap_or(Value::Null);
        let capabilities = result
            .get("agentCapabilities")
            .cloned()
            .unwrap_or(Value::Null);
        let protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .filter(|version| *version == 1)
            .ok_or_else(|| AcpError::new("the agent did not negotiate ACP protocol version 1"))?;
        Ok(AgentIdentity {
            name: info
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("agent")
                .to_string(),
            version: info
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or("0")
                .to_string(),
            protocol_version,
            load_session: capabilities
                .get("loadSession")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            resume_session: capabilities
                .pointer("/sessionCapabilities/resume")
                .is_some_and(|value| !value.is_null() && value != &Value::Bool(false)),
        })
    }

    pub fn new_session(&mut self, cwd: &Path, timeout: Duration) -> Result<String, AcpError> {
        let deadline = Instant::now() + timeout;
        let result = self.request(
            "session/new",
            json!({"cwd": cwd.display().to_string(), "mcpServers": []}),
            None,
            deadline,
            &mut DiscardHandler,
            None,
        )?;
        result
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AcpError::new("session/new did not name a session"))
    }

    pub fn reopen_session(
        &mut self,
        identity: &AgentIdentity,
        session: &str,
        cwd: &Path,
        timeout: Duration,
    ) -> Result<(), AcpError> {
        let method = if identity.resume_session {
            "session/resume"
        } else if identity.load_session {
            "session/load"
        } else {
            return Err(AcpError::new(
                "the agent cannot reopen its stored session; start a new session",
            ));
        };
        let deadline = Instant::now() + timeout;
        self.request(
            method,
            json!({"sessionId": session, "cwd": cwd.display().to_string(), "mcpServers": []}),
            Some(session),
            deadline,
            &mut DiscardHandler,
            None,
        )
        .map(|_| ())
    }

    pub fn set_config_option(
        &mut self,
        session: &str,
        config_id: &str,
        value: &str,
        timeout: Duration,
    ) -> Result<(), AcpError> {
        let deadline = Instant::now() + timeout;
        self.request(
            "session/set_config_option",
            json!({"sessionId": session, "configId": config_id, "value": value}),
            Some(session),
            deadline,
            &mut DiscardHandler,
            None,
        )
        .map(|_| ())
    }

    pub fn prompt_with(
        &mut self,
        session: &str,
        text: &str,
        timeout: Duration,
        handler: &mut dyn AcpHandler,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, AcpError> {
        let deadline = Instant::now() + timeout;
        let result = self.request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
            Some(session),
            deadline,
            handler,
            cancel,
        )?;
        result
            .get("stopReason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
            .map(str::to_string)
            .ok_or_else(|| AcpError::new("session/prompt did not report a stop reason"))
    }

    pub fn prompt(
        &mut self,
        session: &str,
        text: &str,
        policy: PermissionPolicy,
        timeout: Duration,
    ) -> Result<PromptOutcome, AcpError> {
        let mut outcome = PromptOutcome::default();
        let stop_reason = self.prompt_with(
            session,
            text,
            timeout,
            &mut PolicyHandler {
                policy,
                outcome: &mut outcome,
            },
            None,
        )?;
        outcome.stop_reason = stop_reason;
        Ok(outcome)
    }

    pub fn cancel(&self, session: &str) -> Result<(), AcpError> {
        self.write(&json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}}))
    }

    pub fn permission_option(params: &Value, policy: PermissionPolicy) -> Option<String> {
        let wanted = policy.option_kind();
        params
            .get("options")
            .and_then(Value::as_array)
            .and_then(|options| {
                options
                    .iter()
                    .find(|option| option.get("kind").and_then(Value::as_str) == Some(wanted))
            })
            .and_then(|option| option.get("optionId").and_then(Value::as_str))
            .map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fleet::host::{
        FleetHostAdapter, FleetHostWorkerState, FleetWorkerCommand, FleetWorkerStartRequest,
        FleetWorkerTransport, LocalProcessFleetHostAdapter,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp-agent.mjs")
    }

    fn node() -> Option<String> {
        let output = std::process::Command::new("node")
            .arg("--version")
            .output()
            .ok()?;
        output.status.success().then(|| "node".to_string())
    }

    fn json_rpc_request(
        env: BTreeMap<String, String>,
        allow: BTreeSet<String>,
    ) -> FleetWorkerStartRequest {
        let mut request = FleetWorkerStartRequest::new(
            "w-acp",
            FleetWorkerCommand::new("node", [fixture().display().to_string()]),
        );
        request.transport = FleetWorkerTransport::JsonRpc;
        request.env = env;
        request.env_allowlist = allow;
        request
    }

    #[test]
    fn a_json_rpc_worker_speaks_acp_end_to_end() {
        let Some(_node) = node() else {
            return;
        };
        let workspace = tempfile::tempdir().unwrap();
        let mut host = LocalProcessFleetHostAdapter::new(workspace.path());
        let mut request = json_rpc_request(
            BTreeMap::from([(
                "CODEWHALE_TEST_SECRET".to_string(),
                "must-not-travel".to_string(),
            )]),
            BTreeSet::from(["PATH".to_string()]),
        );
        request.cwd = Some(workspace.path().to_path_buf());
        host.start_worker(request).unwrap();

        let io = host
            .take_worker_io("w-acp")
            .expect("a JsonRpc worker yields its pipes");
        assert!(
            host.take_worker_io("w-acp").is_none(),
            "the stream is taken exactly once"
        );
        let mut client = AcpClient::new(io);

        let identity = client.initialize(Duration::from_secs(20)).unwrap();
        assert_eq!(identity.name, "fixture-agent");
        assert_eq!(identity.protocol_version, 1);
        assert!(identity.load_session);

        let session = client
            .new_session(workspace.path(), Duration::from_secs(20))
            .unwrap();
        let allowed = client
            .prompt(
                &session,
                "hello fleet",
                PermissionPolicy::AllowOnce,
                Duration::from_secs(20),
            )
            .unwrap();
        assert_eq!(allowed.stop_reason, "end_turn");
        assert!(
            allowed.text.contains("hello fleet"),
            "streamed text: {:?}",
            allowed.text
        );
        assert!(
            allowed
                .tool_calls
                .iter()
                .any(|call| call == "edit:Write fixture.txt"),
            "tool calls: {:?}",
            allowed.tool_calls
        );
        assert!(
            workspace.path().join("fixture.txt").exists(),
            "an allowed edit wrote its file"
        );

        std::fs::remove_file(workspace.path().join("fixture.txt")).unwrap();
        let rejected = client
            .prompt(
                &session,
                "hello again",
                PermissionPolicy::RejectOnce,
                Duration::from_secs(20),
            )
            .unwrap();
        assert_eq!(rejected.stop_reason, "end_turn");
        assert!(
            !workspace.path().join("fixture.txt").exists(),
            "a rejected edit wrote nothing"
        );

        client
            .reopen_session(
                &identity,
                &session,
                workspace.path(),
                Duration::from_secs(20),
            )
            .unwrap();

        let dumped = std::fs::read_to_string(workspace.path().join("fixture-env.json")).unwrap();
        assert!(
            !dumped.contains("CODEWHALE_TEST_SECRET"),
            "an unlisted variable never reached the agent"
        );

        let status = host.stop_worker("w-acp").unwrap();
        assert_ne!(status.state, FleetHostWorkerState::Running);
    }

    #[test]
    fn invalid_acp_envelopes_and_protocol_versions_are_refused() {
        if node().is_none() {
            return;
        }
        for response in [
            "({ id: request.id, result: { protocolVersion: 1 } })",
            "({ jsonrpc: '2.0', id: request.id, result: { protocolVersion: 2 } })",
            "({ jsonrpc: '2.0', id: request.id })",
        ] {
            let workspace = tempfile::tempdir().unwrap();
            let program = format!(
                "require('node:readline').createInterface({{input:process.stdin}}).once('line', line => {{ const request=JSON.parse(line); console.log(JSON.stringify({response})); }});"
            );
            let mut host = LocalProcessFleetHostAdapter::new(workspace.path());
            let mut request = FleetWorkerStartRequest::new(
                "invalid-acp",
                FleetWorkerCommand::new("node", ["-e".to_string(), program]),
            );
            request.transport = FleetWorkerTransport::JsonRpc;
            request.cwd = Some(workspace.path().to_path_buf());
            host.start_worker(request).unwrap();
            let io = host.take_worker_io("invalid-acp").unwrap();
            let mut client = AcpClient::new(io);
            let error = client.initialize(Duration::from_secs(20)).unwrap_err();
            assert!(
                error.to_string().contains("invalid JSON-RPC")
                    || error.to_string().contains("protocol version")
                    || error.to_string().contains("without a result"),
                "{error}"
            );
            host.stop_worker("invalid-acp").unwrap();
        }
    }

    #[test]
    fn a_logs_worker_keeps_its_log_and_has_no_duplex_io() {
        let workspace = tempfile::tempdir().unwrap();
        let mut host = LocalProcessFleetHostAdapter::new(workspace.path());
        let mut request = FleetWorkerStartRequest::new(
            "w-logs",
            FleetWorkerCommand::new("sh", ["-c", "echo logged-line"]),
        );
        request.cwd = Some(workspace.path().to_path_buf());
        let handle = host.start_worker(request).unwrap();
        assert!(
            host.take_worker_io("w-logs").is_none(),
            "a Logs worker has no duplex io"
        );

        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut log = String::new();
        while std::time::Instant::now() < deadline {
            log = host.read_logs("w-logs", 4096).unwrap();
            if log.contains("logged-line") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(log.contains("logged-line"), "log was {log:?}");
        assert!(handle.log_path.exists());
    }

    #[test]
    fn a_cancel_settles_a_prompt_that_never_answers() {
        let Some(_node) = node() else {
            return;
        };
        let workspace = tempfile::tempdir().unwrap();
        let mut host = LocalProcessFleetHostAdapter::new(workspace.path());
        let mut request = json_rpc_request(
            BTreeMap::from([("HANG".to_string(), "1".to_string())]),
            BTreeSet::from(["HANG".to_string()]),
        );
        request.cwd = Some(workspace.path().to_path_buf());
        host.start_worker(request).unwrap();

        let io = host.take_worker_io("w-acp").expect("duplex io");
        let mut client = AcpClient::new(io);
        client.initialize(Duration::from_secs(20)).unwrap();
        let session = client
            .new_session(workspace.path(), Duration::from_secs(20))
            .unwrap();
        let canceller = client.canceller(&session);
        let prompt_session = session.clone();
        let blocked = std::thread::spawn(move || {
            client.prompt(
                &prompt_session,
                "hang",
                PermissionPolicy::AllowOnce,
                Duration::from_secs(30),
            )
        });
        std::thread::sleep(Duration::from_millis(400));
        canceller.cancel().unwrap();
        let outcome = blocked.join().unwrap().unwrap();
        assert_eq!(outcome.stop_reason, "cancelled");
        host.stop_worker("w-acp").unwrap();
    }
}

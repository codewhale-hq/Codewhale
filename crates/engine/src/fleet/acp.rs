//! A narrow Agent Client Protocol client over a fleet worker's duplex stdio.
//!
//! The fleet host owns the process: groups, parent-death, stop, restart and
//! logs. This module owns only the wire: newline-framed JSON-RPC, one request
//! at a time, and the one client-side request the protocol needs in this slice
//! (`session/request_permission`), answered by a fixed policy rather than a
//! person.
//!
//! Deliberately absent: filesystem and terminal client methods (advertised
//! false), elicitation, session load/fork, and any credential handling. The
//! agent runs in its own process with its own tools, and its stderr stays in
//! the worker log.

use std::io::Write;
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::host::FleetWorkerIo;

const CLIENT_NAME: &str = "codewhale";

/// How the client answers `session/request_permission`.
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

#[derive(Debug)]
pub struct AcpError {
    message: String,
}

impl AcpError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn timeout(method: &str) -> Self {
        Self::new(format!("{method} did not answer before the deadline"))
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
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptOutcome {
    pub stop_reason: String,
    pub text: String,
    pub tool_calls: Vec<String>,
    pub decisions: Vec<String>,
}

/// One live ACP conversation over a worker's pipes.
pub struct AcpClient {
    stdin: Arc<Mutex<std::process::ChildStdin>>,
    lines: Receiver<String>,
    next_id: u64,
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
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| AcpError::new("the agent's stdin lock was poisoned"))?;
        let line = serde_json::to_string(&json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": self.session}}))
            .map_err(|err| AcpError::new(format!("encoding a cancel failed: {err}")))?;
        stdin
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|err| AcpError::new(format!("the agent's stdin closed: {err}")))
    }
}

impl std::fmt::Debug for AcpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpClient")
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

impl AcpClient {
    pub fn new(io: FleetWorkerIo) -> Self {
        let FleetWorkerIo { stdin, stdout } = io;
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            use std::io::BufRead as _;
            let mut stdout = stdout;
            let mut line = String::new();
            loop {
                line.clear();
                match stdout.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end().to_string();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if tx.send(trimmed).is_err() {
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
        }
    }

    fn write(&self, message: &Value) -> Result<(), AcpError> {
        let mut line = serde_json::to_string(message)
            .map_err(|err| AcpError::new(format!("encoding a request failed: {err}")))?;
        line.push('\n');
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| AcpError::new("the agent's stdin lock was poisoned"))?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|err| AcpError::new(format!("the agent's stdin closed: {err}")))
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
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(AcpError::timeout(method));
        }
        let line = self
            .lines
            .recv_timeout(left)
            .map_err(|_| AcpError::timeout(method))?;
        serde_json::from_str(&line)
            .map_err(|err| AcpError::new(format!("the agent sent a line that is not JSON: {err}")))
    }

    /// Send a request and wait for its response, answering the agent's
    /// permission requests with `policy` and reporting every other
    /// notification to `on_update`.
    fn request<F>(
        &mut self,
        method: &str,
        params: Value,
        policy: PermissionPolicy,
        deadline: Instant,
        mut on_update: F,
    ) -> Result<Value, AcpError>
    where
        F: FnMut(&Value),
    {
        let id = self.next();
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let message = self.read(deadline, method)?;
            if message.get("id").and_then(Value::as_u64) == Some(id)
                && (message.get("result").is_some() || message.get("error").is_some())
            {
                if let Some(error) = message.get("error") {
                    let text = error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("the agent refused");
                    return Err(AcpError::new(format!("{method} failed: {text}")));
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
            let Some(incoming) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            let incoming_id = message.get("id").cloned();
            let incoming_params = message.get("params").cloned().unwrap_or(Value::Null);
            if incoming == "session/request_permission" {
                self.answer_permission(incoming_id, &incoming_params, policy)?;
            } else {
                on_update(&json!({"method": incoming, "params": incoming_params}));
            }
        }
    }

    fn answer_permission(
        &mut self,
        id: Option<Value>,
        params: &Value,
        policy: PermissionPolicy,
    ) -> Result<(), AcpError> {
        let Some(id) = id else { return Ok(()) };
        let outcome = if policy == PermissionPolicy::Cancel {
            json!({"outcome": "cancelled"})
        } else {
            let wanted = policy.option_kind();
            let chosen = params
                .get("options")
                .and_then(Value::as_array)
                .and_then(|options| {
                    options
                        .iter()
                        .find(|option| option.get("kind").and_then(Value::as_str) == Some(wanted))
                })
                .and_then(|option| option.get("optionId").and_then(Value::as_str));
            match chosen {
                Some(option_id) => json!({"outcome": "selected", "optionId": option_id}),
                None => json!({"outcome": "cancelled"}),
            }
        };
        self.write(&json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": outcome}}))
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
            PermissionPolicy::Cancel,
            deadline,
            |_| {},
        )?;
        let info = result.get("agentInfo").cloned().unwrap_or(Value::Null);
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
            protocol_version: result
                .get("protocolVersion")
                .and_then(Value::as_u64)
                .unwrap_or(1),
        })
    }

    pub fn new_session(&mut self, cwd: &Path, timeout: Duration) -> Result<String, AcpError> {
        let deadline = Instant::now() + timeout;
        let result = self.request(
            "session/new",
            json!({"cwd": cwd.display().to_string(), "mcpServers": []}),
            PermissionPolicy::Cancel,
            deadline,
            |_| {},
        )?;
        result
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AcpError::new("session/new did not name a session"))
    }

    /// One prompt. Streams are collected into the outcome, which is enough for
    /// a caller that has not yet mapped ACP updates onto journal items.
    pub fn prompt(
        &mut self,
        session: &str,
        text: &str,
        policy: PermissionPolicy,
        timeout: Duration,
    ) -> Result<PromptOutcome, AcpError> {
        let deadline = Instant::now() + timeout;
        let mut outcome = PromptOutcome::default();
        let result = self.request(
            "session/prompt",
            json!({"sessionId": session, "prompt": [{"type": "text", "text": text}]}),
            policy,
            deadline,
            |message| {
                if message.get("method").and_then(Value::as_str) != Some("session/update") {
                    return;
                }
                let Some(update) = message
                    .get("params")
                    .and_then(|params| params.get("update"))
                else {
                    return;
                };
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
                            outcome.text.push_str(chunk);
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
                        outcome.tool_calls.push(format!("{kind}:{title}"));
                    }
                    _ => {}
                }
            },
        )?;
        outcome.stop_reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("end_turn")
            .to_string();
        Ok(outcome)
    }

    pub fn cancel(&self, session: &str) -> Result<(), AcpError> {
        self.write(&json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}}))
    }

    /// Record a decision the caller made on the agent's behalf. The client
    /// answers permissions inline, so this only exists for callers that want
    /// the option id in their own record.
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

        let dumped = std::fs::read_to_string(workspace.path().join("fixture-env.json")).unwrap();
        assert!(
            !dumped.contains("CODEWHALE_TEST_SECRET"),
            "an unlisted variable never reached the agent"
        );

        let status = host.stop_worker("w-acp").unwrap();
        assert_ne!(status.state, FleetHostWorkerState::Running);
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

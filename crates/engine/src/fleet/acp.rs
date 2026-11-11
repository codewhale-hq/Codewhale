//! The Engine's one Agent Client Protocol client, over a fleet worker's duplex
//! stdio.
//!
//! The fleet host owns the process: groups, parent-death, stop, restart and
//! logs. This module owns only the wire, and the wire is the official
//! `agent-client-protocol` SDK: one connection thread per client drives the
//! SDK over the worker's pipes, and the caller's thread sends one request at a
//! time and answers the agent's requests. What we send is built from the
//! official v1 types; what an agent sends is read leniently as JSON. A caller
//! supplies an [`AcpHandler`] that sees every `session/update` and answers
//! every `session/request_permission`; [`PermissionPolicy`] is the
//! fixed-answer handler for callers with no person in the loop.
//!
//! Deliberately absent: filesystem and terminal client methods (advertised
//! false and answered method-not-found), elicitation (declined), `authenticate`
//! and any credential handling. The agent runs in its own process with its
//! own login and tools, and its stderr stays in the worker log. Messages from
//! another session id are ignored, and a permission request for one is
//! answered `cancelled`. A line longer than [`MAX_MESSAGE_BYTES`] ends the
//! connection rather than being parsed.
//!
//! Known limits: each message is bounded but the number of queued messages is
//! not, so an agent that floods small frames grows memory until the caller
//! drains them. A permission request the agent withdraws with
//! `$/cancel_request` is answered `cancelled`, but only once the handler
//! returns; the handler is not interrupted.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema::{
    CancelNotification, ClientCapabilities, ContentBlock, CreateElicitationResponse,
    ElicitationAction, Implementation, InitializeRequest, LoadSessionRequest, NewSessionRequest,
    PromptRequest, RequestPermissionOutcome, RequestPermissionResponse, ResumeSessionRequest,
    SelectedPermissionOutcome, SetSessionConfigOptionRequest, SetSessionModeRequest,
};
use agent_client_protocol::{Client, ErrorCode, Lines, UntypedMessage};
use codewhale_util::bounded_lines::{BoundedLines, MAX_FRAME_BYTES, is_frame_refusal};
use futures_util::{Sink, Stream};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

use super::host::FleetWorkerIo;

const CLIENT_NAME: &str = "codewhale";
pub const MAX_MESSAGE_BYTES: usize = MAX_FRAME_BYTES;
pub const AUTH_REQUIRED: i64 = -32000;
const ABORT_POLL: Duration = Duration::from_millis(25);
pub const MAX_CONTROLS: usize = 32;
pub const MAX_CHOICES: usize = 256;
pub const MAX_VALUE_BYTES: usize = 128;
const MAX_NAME_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 400;

pub mod refusal {
    pub const CONTROL_NOT_OFFERED: &str = "control_not_offered";
    pub const CONTROL_UNAVAILABLE: &str = "control_unavailable";
    pub const CHOICE_NOT_OFFERED: &str = "choice_not_offered";
    pub const CHOICE_REJECTED: &str = "choice_rejected";
    pub const UNSUPPORTED_CONTROL: &str = "unsupported_control";
    pub const CONTROL_TYPE_UNSUPPORTED: &str = "control_type_unsupported";
    pub const CONTROL_HAS_NO_CHOICES: &str = "control_has_no_choices";
}

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
    fn outcome(&self) -> RequestPermissionOutcome {
        match self {
            PermissionAnswer::Selected(option_id) => RequestPermissionOutcome::Selected(
                SelectedPermissionOutcome::new(option_id.clone()),
            ),
            PermissionAnswer::Cancelled => RequestPermissionOutcome::Cancelled,
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

    fn from_agent(method: &str, error: &SdkError) -> Self {
        Self {
            message: format!("{method} failed: {}", describe(error)),
            code: Some(i64::from(i32::from(error.code))),
        }
    }

    pub fn is_auth_required(&self) -> bool {
        self.code == Some(AUTH_REQUIRED)
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
    pub capabilities: AgentCapabilities,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AgentCapabilities {
    pub load_session: bool,
    pub resume_session: bool,
    pub fork_session: bool,
    pub list_sessions: bool,
    pub additional_directories: bool,
    pub prompt_image: bool,
    pub prompt_audio: bool,
    pub prompt_embedded_context: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Choice {
    pub value: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    Model,
    Mode,
    Config,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "method")]
pub enum ControlWrite {
    #[serde(rename = "session/set_config_option")]
    SetConfigOption { config_id: String },
    #[serde(rename = "session/set_model")]
    SetModel,
    #[serde(rename = "session/set_mode")]
    SetMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ControlRefusal {
    pub code: &'static str,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Control {
    pub id: String,
    pub kind: ControlKind,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    pub choices: Vec<Choice>,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write: Option<ControlWrite>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<ControlRefusal>,
}

impl Control {
    pub fn offers(&self, value: &str) -> bool {
        self.unavailable.is_none() && self.choices.iter().any(|choice| choice.value == value)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SessionOffer {
    pub controls: Vec<Control>,
    pub malformed_controls: usize,
}

impl SessionOffer {
    pub fn control(&self, id: &str) -> Option<&Control> {
        self.controls.iter().find(|control| control.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActionSupport {
    pub action: &'static str,
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_reports: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OptionsAgent {
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol_version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentOptions {
    pub agent: OptionsAgent,
    pub cwd: String,
    pub capabilities: AgentCapabilities,
    pub controls: Vec<Control>,
    pub malformed_controls: usize,
    pub actions: Vec<ActionSupport>,
}

impl AgentOptions {
    pub fn new(identity: &AgentIdentity, id: &str, cwd: &Path, offer: SessionOffer) -> Self {
        let actions = action_support(&identity.capabilities, &offer);
        Self {
            agent: OptionsAgent {
                id: id.to_string(),
                name: identity.name.clone(),
                version: identity.version.clone(),
                protocol_version: identity.protocol_version,
            },
            cwd: cwd.display().to_string(),
            capabilities: identity.capabilities.clone(),
            controls: offer.controls,
            malformed_controls: offer.malformed_controls,
            actions,
        }
    }
}

fn action_support(capabilities: &AgentCapabilities, offer: &SessionOffer) -> Vec<ActionSupport> {
    let offered = |kind: ControlKind| {
        offer
            .controls
            .iter()
            .any(|control| control.kind == kind && control.unavailable.is_none())
    };
    let choice = |action, kind| {
        let supported = offered(kind);
        ActionSupport {
            action,
            supported,
            code: (!supported).then_some(refusal::CONTROL_NOT_OFFERED),
            reason: (!supported).then_some("The agent did not offer this control."),
            agent_reports: None,
        }
    };
    let refused = |action, reason, agent_reports| ActionSupport {
        action,
        supported: false,
        code: Some(refusal::UNSUPPORTED_CONTROL),
        reason: Some(reason),
        agent_reports,
    };
    vec![
        choice("model_choice", ControlKind::Model),
        choice("mode_choice", ControlKind::Mode),
        choice("config_choice", ControlKind::Config),
        refused(
            "steering",
            "Agent Client Protocol has no way to steer a running turn; interrupt it and send a new message.",
            None,
        ),
        refused(
            "compaction",
            "Codewhale cannot compact another agent's context.",
            None,
        ),
        refused(
            "fork",
            "Codewhale does not fork another agent's session.",
            Some(capabilities.fork_session),
        ),
        refused(
            "images",
            "Codewhale sends text prompts to another agent only.",
            Some(capabilities.prompt_image),
        ),
        refused(
            "dynamic_tools",
            "The agent uses its own tools; Codewhale tools cannot be added.",
            None,
        ),
        refused(
            "output_limit",
            "Codewhale cannot cap another agent's output tokens.",
            None,
        ),
        refused(
            "provider_override",
            "Codewhale providers do not apply; choose the agent's own model.",
            None,
        ),
        refused(
            "remote_placement",
            "The agent runs on this computer only.",
            None,
        ),
    ]
}

pub struct NewSession {
    pub session_id: String,
    pub offer: SessionOffer,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AgentTokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub thought_tokens: Option<u64>,
    pub cached_read_tokens: Option<u64>,
    pub cached_write_tokens: Option<u64>,
}

impl AgentTokenUsage {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn from_result(result: &Value) -> Option<Self> {
        let usage = result.get("usage")?.as_object()?;
        let field = |name: &str| usage.get(name).and_then(Value::as_u64);
        let parsed = Self {
            input_tokens: field("inputTokens"),
            output_tokens: field("outputTokens"),
            total_tokens: field("totalTokens"),
            thought_tokens: field("thoughtTokens"),
            cached_read_tokens: field("cachedReadTokens"),
            cached_write_tokens: field("cachedWriteTokens"),
        };
        (!parsed.is_empty()).then_some(parsed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptReply {
    pub stop_reason: String,
    pub usage: Option<AgentTokenUsage>,
}

fn bounded_text(value: Option<&Value>, max_chars: usize) -> Option<String> {
    let text: String = value?
        .as_str()?
        .chars()
        .filter(|ch| !ch.is_control())
        .take(max_chars)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

fn offered_value(value: Option<&Value>) -> Option<String> {
    let value = value?.as_str()?;
    (!value.is_empty() && value.len() <= MAX_VALUE_BYTES && !value.chars().any(char::is_control))
        .then(|| value.to_string())
}

fn push_choice(
    choices: &mut Vec<Choice>,
    truncated: &mut bool,
    value: Option<String>,
    name: Option<&Value>,
    description: Option<&Value>,
) {
    let Some(value) = value else {
        *truncated = true;
        return;
    };
    if choices.iter().any(|choice| choice.value == value) {
        return;
    }
    if choices.len() >= MAX_CHOICES {
        *truncated = true;
        return;
    }
    choices.push(Choice {
        name: bounded_text(name, MAX_NAME_CHARS).unwrap_or_else(|| value.clone()),
        description: bounded_text(description, MAX_DESCRIPTION_CHARS),
        value,
    });
}

fn config_choices(options: &[Value]) -> (Vec<Choice>, bool) {
    let mut choices = Vec::new();
    let mut truncated = false;
    for option in options {
        if let Some(group) = option.get("options").and_then(Value::as_array) {
            for grouped in group {
                push_choice(
                    &mut choices,
                    &mut truncated,
                    offered_value(grouped.get("value")),
                    grouped.get("name"),
                    grouped.get("description"),
                );
            }
        } else {
            push_choice(
                &mut choices,
                &mut truncated,
                offered_value(option.get("value")),
                option.get("name"),
                option.get("description"),
            );
        }
    }
    (choices, truncated)
}

impl SessionOffer {
    pub fn from_session_result(result: &Value) -> Self {
        let mut offer = Self::default();
        let mut model_claimed = false;
        let mut mode_claimed = false;
        let options = result
            .get("configOptions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        for option in options {
            if offer.controls.len() >= MAX_CONTROLS {
                offer.malformed_controls += 1;
                continue;
            }
            let Some(config_id) = offered_value(option.get("id")) else {
                offer.malformed_controls += 1;
                continue;
            };
            let category = bounded_text(option.get("category"), 64);
            let kind = if !model_claimed
                && (config_id == "model" || category.as_deref() == Some("model"))
            {
                model_claimed = true;
                ControlKind::Model
            } else if !mode_claimed && (config_id == "mode" || category.as_deref() == Some("mode"))
            {
                mode_claimed = true;
                ControlKind::Mode
            } else {
                ControlKind::Config
            };
            let id = match kind {
                ControlKind::Model => "model".to_string(),
                ControlKind::Mode => "mode".to_string(),
                ControlKind::Config => format!("config:{config_id}"),
            };
            if offer.control(&id).is_some() {
                offer.malformed_controls += 1;
                continue;
            }
            let is_select = option.get("type").and_then(Value::as_str) == Some("select");
            let (choices, truncated) = if is_select {
                config_choices(
                    option
                        .get("options")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or_default(),
                )
            } else {
                (Vec::new(), false)
            };
            let unavailable = if !is_select {
                Some(ControlRefusal {
                    code: refusal::CONTROL_TYPE_UNSUPPORTED,
                    reason: "Codewhale can only write select-type agent options.".to_string(),
                })
            } else if choices.is_empty() {
                Some(ControlRefusal {
                    code: refusal::CONTROL_HAS_NO_CHOICES,
                    reason: "The agent offered this option with no choices.".to_string(),
                })
            } else {
                None
            };
            offer.controls.push(Control {
                name: bounded_text(option.get("name"), MAX_NAME_CHARS)
                    .unwrap_or_else(|| config_id.clone()),
                description: bounded_text(option.get("description"), MAX_DESCRIPTION_CHARS),
                category,
                current: offered_value(option.get("currentValue")),
                write: unavailable
                    .is_none()
                    .then_some(ControlWrite::SetConfigOption { config_id }),
                id,
                kind,
                choices,
                truncated,
                unavailable,
            });
        }
        if !model_claimed
            && let Some(models) = result.get("models").filter(|value| value.is_object())
        {
            offer.push_legacy(
                ControlKind::Model,
                models.get("currentModelId"),
                models.get("availableModels"),
                "modelId",
                ControlWrite::SetModel,
            );
        }
        if !mode_claimed && let Some(modes) = result.get("modes").filter(|value| value.is_object())
        {
            offer.push_legacy(
                ControlKind::Mode,
                modes.get("currentModeId"),
                modes.get("availableModes"),
                "id",
                ControlWrite::SetMode,
            );
        }
        offer
    }

    fn push_legacy(
        &mut self,
        kind: ControlKind,
        current: Option<&Value>,
        available: Option<&Value>,
        value_key: &str,
        write: ControlWrite,
    ) {
        let mut choices = Vec::new();
        let mut truncated = false;
        for entry in available
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            push_choice(
                &mut choices,
                &mut truncated,
                offered_value(entry.get(value_key)),
                entry.get("name"),
                entry.get("description"),
            );
        }
        if choices.is_empty() {
            self.malformed_controls += 1;
            return;
        }
        let (id, name) = match kind {
            ControlKind::Model => ("model", "Model"),
            _ => ("mode", "Mode"),
        };
        self.controls.push(Control {
            id: id.to_string(),
            kind,
            name: name.to_string(),
            description: None,
            category: None,
            current: offered_value(current),
            choices,
            truncated,
            write: Some(write),
            unavailable: None,
        });
    }
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

type SdkError = agent_client_protocol::Error;

enum Command {
    Request {
        method: &'static str,
        params: Value,
        token: u64,
        cancel: Option<CancellationToken>,
    },
    Notify(Value),
}

enum Event {
    Update(Value),
    Request {
        method: String,
        params: Value,
        responder: Box<agent_client_protocol::v1::Responder<Value>>,
    },
    Response {
        token: u64,
        result: Result<Value, AcpError>,
    },
    Closed(Option<String>),
}

/// One live ACP conversation over a worker's pipes.
pub struct AcpClient {
    commands: UnboundedSender<Command>,
    events: Receiver<Event>,
    closed: Arc<Mutex<Option<String>>>,
    next_token: u64,
    abort: Option<CancellationToken>,
}

/// A write-only handle that can send `session/cancel` while a prompt blocks
/// the client that owns the event queue.
#[derive(Clone)]
pub struct AcpCanceller {
    commands: UnboundedSender<Command>,
    session: String,
}

impl AcpCanceller {
    pub fn cancel(&self) -> Result<(), AcpError> {
        send_cancel(&self.commands, &self.session)
    }
}

impl std::fmt::Debug for AcpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpClient")
            .field("next_token", &self.next_token)
            .finish_non_exhaustive()
    }
}

fn encode(message: impl Serialize) -> Result<Value, AcpError> {
    serde_json::to_value(message)
        .map_err(|err| AcpError::new(format!("encoding a message failed: {err}")))
}

fn describe(error: &SdkError) -> String {
    let message = if error.message.is_empty() {
        "the agent refused"
    } else {
        error.message.as_str()
    };
    match bounded_text(error.data.as_ref(), MAX_DESCRIPTION_CHARS) {
        Some(detail) => format!("{message}: {detail}"),
        None => message.to_string(),
    }
}

fn send_cancel(commands: &UnboundedSender<Command>, session: &str) -> Result<(), AcpError> {
    let params = encode(CancelNotification::new(session.to_string()))?;
    commands
        .send(Command::Notify(params))
        .map_err(|_| AcpError::new("the agent's connection has ended"))
}

fn wire_path(path: &Path) -> PathBuf {
    PathBuf::from(path.display().to_string())
}

fn advertised_dirs(identity: &AgentIdentity, add_dirs: &[PathBuf]) -> Vec<PathBuf> {
    if identity.capabilities.additional_directories {
        add_dirs.iter().map(|dir| wire_path(dir)).collect()
    } else {
        Vec::new()
    }
}

fn bounded_lines<R>(
    reader: R,
    closed: Arc<Mutex<Option<String>>>,
) -> impl Stream<Item = std::io::Result<String>> + Send + 'static
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    futures_util::stream::unfold(
        (BoundedLines::new(reader), closed),
        |(mut lines, closed)| async move {
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) if line.trim().is_empty() => continue,
                    Ok(Some(line)) => return Some((Ok(line), (lines, closed))),
                    Ok(None) => return None,
                    Err(error) => {
                        if is_frame_refusal(&error)
                            && let Ok(mut slot) = closed.lock()
                        {
                            slot.get_or_insert_with(|| {
                                format!("the agent's message was refused: {error}")
                            });
                        }
                        return Some((Err(error), (lines, closed)));
                    }
                }
            }
        },
    )
}

fn line_sink<W>(writer: W) -> impl Sink<String, Error = std::io::Error> + Send + 'static
where
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    futures_util::sink::unfold(writer, |mut writer, line: String| async move {
        writer.write_all(line.as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await?;
        Ok::<_, std::io::Error>(writer)
    })
}

async fn drive(
    io: FleetWorkerIo,
    mut commands: UnboundedReceiver<Command>,
    events: Sender<Event>,
    closed: Arc<Mutex<Option<String>>>,
) -> Result<(), SdkError> {
    let stdin =
        tokio::process::ChildStdin::from_std(io.stdin).map_err(SdkError::into_internal_error)?;
    let stdout = tokio::process::ChildStdout::from_std(io.stdout.into_inner())
        .map_err(SdkError::into_internal_error)?;
    let transport = Lines::new(
        line_sink(stdin),
        bounded_lines(tokio::io::BufReader::new(stdout), closed),
    );
    let requests = events.clone();
    let updates = events.clone();
    Client
        .v1()
        .on_receive_request(
            async move |request: UntypedMessage, responder, _cx| {
                let (method, params) = request.into_parts();
                let _ = requests.send(Event::Request {
                    method,
                    params,
                    responder: Box::new(responder),
                });
                Ok(())
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_notification(
            async move |notification: UntypedMessage, _cx| {
                let (method, params) = notification.into_parts();
                if method == "session/update" {
                    let _ = updates.send(Event::Update(params));
                }
                Ok(())
            },
            agent_client_protocol::on_receive_notification!(),
        )
        .connect_with(transport, async move |cx| {
            loop {
                let command = tokio::select! {
                    command = commands.recv() => command,
                    () = cx.incoming_closed() => None,
                };
                match command {
                    Some(Command::Request {
                        method,
                        params,
                        token,
                        cancel,
                    }) => {
                        let answered = events.clone();
                        let sent = if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                            Err(AcpError::new(format!(
                                "{method} was cancelled before dispatch"
                            )))
                        } else {
                            UntypedMessage::new(method, params)
                                .and_then(|message| {
                                    cx.send_request(message).on_receiving_result(
                                        async move |result| {
                                            let _ = answered.send(Event::Response {
                                                token,
                                                result: result.map_err(|error| {
                                                    AcpError::from_agent(method, &error)
                                                }),
                                            });
                                            Ok(())
                                        },
                                    )
                                })
                                .map_err(|error| AcpError::from_agent(method, &error))
                        };
                        if let Err(error) = sent {
                            let _ = events.send(Event::Response {
                                token,
                                result: Err(error),
                            });
                        }
                    }
                    Some(Command::Notify(params)) => {
                        if let Err(error) = UntypedMessage::new("session/cancel", params)
                            .and_then(|message| cx.send_notification(message))
                        {
                            tracing::warn!(error = %describe(&error), "sending session/cancel failed");
                        }
                    }
                    None => return Ok(()),
                }
            }
        })
        .await
}

impl AcpClient {
    pub fn new(io: FleetWorkerIo) -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (event_tx, events) = channel();
        let closed = Arc::new(Mutex::new(None));
        let stream_closed = Arc::clone(&closed);
        let spawned = std::thread::Builder::new()
            .name("acp-conn".to_string())
            .spawn(move || {
                let ended = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => {
                        let ended = runtime.block_on(drive(
                            io,
                            command_rx,
                            event_tx.clone(),
                            stream_closed,
                        ));
                        runtime.shutdown_background();
                        ended
                    }
                    Err(error) => Err(SdkError::into_internal_error(error)),
                };
                let reason = match ended {
                    Ok(()) => None,
                    Err(error) if agent_client_protocol::is_incoming_transport_closed(&error) => {
                        None
                    }
                    Err(error) => {
                        Some(format!("the agent connection failed: {}", describe(&error)))
                    }
                };
                let _ = event_tx.send(Event::Closed(reason));
            });
        if let Err(error) = spawned
            && let Ok(mut slot) = closed.lock()
        {
            *slot = Some(format!("the agent connection could not start: {error}"));
        }
        Self {
            commands,
            events,
            closed,
            next_token: 1,
            abort: None,
        }
    }

    pub fn abort_requests_on(&mut self, abort: Option<CancellationToken>) {
        self.abort = abort;
    }

    /// A handle that can settle a blocked prompt from another thread.
    pub fn canceller(&self, session: &str) -> AcpCanceller {
        AcpCanceller {
            commands: self.commands.clone(),
            session: session.to_string(),
        }
    }

    fn ended(&self, method: &str, reason: Option<String>) -> AcpError {
        let refused = self.closed.lock().ok().and_then(|slot| slot.clone());
        AcpError::new(
            refused
                .or(reason)
                .unwrap_or_else(|| format!("the agent exited before answering {method}")),
        )
    }

    fn read(&mut self, deadline: Instant, method: &str) -> Result<Event, AcpError> {
        loop {
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
            match self.events.recv_timeout(wait) {
                Ok(event) => return Ok(event),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(self.ended(method, None)),
            }
        }
    }

    fn request(
        &mut self,
        method: &'static str,
        params: Value,
        session: Option<&str>,
        deadline: Instant,
        handler: &mut dyn AcpHandler,
        cancel: Option<&CancellationToken>,
    ) -> Result<Value, AcpError> {
        let token = self.next_token;
        self.next_token += 1;
        self.commands
            .send(Command::Request {
                method,
                params,
                token,
                cancel: cancel.cloned(),
            })
            .map_err(|_| self.ended(method, None))?;
        let ours = |params: &Value| {
            session.is_some_and(|session| {
                params.get("sessionId").and_then(Value::as_str) == Some(session)
            })
        };
        loop {
            match self.read(deadline, method)? {
                Event::Response {
                    token: answered,
                    result,
                } => {
                    if answered == token {
                        return result;
                    }
                }
                Event::Update(params) => {
                    if ours(&params)
                        && let Some(update) = params.get("update")
                    {
                        handler.update(update);
                    }
                }
                Event::Request {
                    method: incoming,
                    params,
                    responder,
                } => {
                    let answered = match incoming.as_str() {
                        "session/request_permission" => {
                            let answer = if ours(&params) {
                                handler.permission(&params)
                            } else {
                                PermissionAnswer::Cancelled
                            };
                            let answer = if cancel.is_some_and(CancellationToken::is_cancelled)
                                || responder.cancellation().is_cancelled()
                            {
                                PermissionAnswer::Cancelled
                            } else {
                                answer
                            };
                            responder
                                .respond(encode(RequestPermissionResponse::new(answer.outcome()))?)
                        }
                        "elicitation/create" => responder.respond(encode(
                            CreateElicitationResponse::new(ElicitationAction::Decline),
                        )?),
                        other => responder.respond_with_error(SdkError::new(
                            ErrorCode::METHOD_NOT_FOUND,
                            format!("{other} is not supported by this client"),
                        )),
                    };
                    answered.map_err(|error| {
                        AcpError::new(format!("answering the agent failed: {}", describe(&error)))
                    })?;
                }
                Event::Closed(reason) => return Err(self.ended(method, reason)),
            }
        }
    }

    pub fn initialize(&mut self, timeout: Duration) -> Result<AgentIdentity, AcpError> {
        let deadline = Instant::now() + timeout;
        let result = self.request(
            "initialize",
            encode(
                InitializeRequest::new(ProtocolVersion::V1)
                    .client_capabilities(ClientCapabilities::new())
                    .client_info(Implementation::new(CLIENT_NAME, env!("CARGO_PKG_VERSION"))),
            )?,
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
        let flag = |pointer: &str| {
            capabilities
                .pointer(pointer)
                .is_some_and(|value| !value.is_null() && value != &Value::Bool(false))
        };
        let capability_report = AgentCapabilities {
            load_session: capabilities
                .get("loadSession")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            resume_session: flag("/sessionCapabilities/resume"),
            fork_session: flag("/sessionCapabilities/fork"),
            list_sessions: flag("/sessionCapabilities/list"),
            additional_directories: flag("/sessionCapabilities/additionalDirectories"),
            prompt_image: flag("/promptCapabilities/image"),
            prompt_audio: flag("/promptCapabilities/audio"),
            prompt_embedded_context: flag("/promptCapabilities/embeddedContext"),
        };
        Ok(AgentIdentity {
            capabilities: capability_report,
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

    pub fn new_session(
        &mut self,
        identity: &AgentIdentity,
        cwd: &Path,
        add_dirs: &[PathBuf],
        timeout: Duration,
    ) -> Result<NewSession, AcpError> {
        let deadline = Instant::now() + timeout;
        let result = self.request(
            "session/new",
            encode(
                NewSessionRequest::new(wire_path(cwd))
                    .additional_directories(advertised_dirs(identity, add_dirs)),
            )?,
            None,
            deadline,
            &mut DiscardHandler,
            None,
        )?;
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| AcpError::new("session/new did not name a session"))?;
        Ok(NewSession {
            session_id,
            offer: SessionOffer::from_session_result(&result),
        })
    }

    pub fn reopen_session(
        &mut self,
        identity: &AgentIdentity,
        session: &str,
        cwd: &Path,
        add_dirs: &[PathBuf],
        timeout: Duration,
    ) -> Result<SessionOffer, AcpError> {
        let dirs = advertised_dirs(identity, add_dirs);
        let (method, params) = if identity.resume_session {
            (
                "session/resume",
                encode(
                    ResumeSessionRequest::new(session.to_string(), wire_path(cwd))
                        .additional_directories(dirs),
                )?,
            )
        } else if identity.load_session {
            (
                "session/load",
                encode(
                    LoadSessionRequest::new(session.to_string(), wire_path(cwd))
                        .additional_directories(dirs),
                )?,
            )
        } else {
            return Err(AcpError::new(
                "the agent cannot reopen its stored session; start a new session",
            ));
        };
        let deadline = Instant::now() + timeout;
        self.request(
            method,
            params,
            Some(session),
            deadline,
            &mut DiscardHandler,
            None,
        )
        .map(|result| SessionOffer::from_session_result(&result))
    }

    pub fn apply_control(
        &mut self,
        session: &str,
        control: &Control,
        value: &str,
        timeout: Duration,
    ) -> Result<(), AcpError> {
        let deadline = Instant::now() + timeout;
        let (method, params) = match control.write.as_ref() {
            Some(ControlWrite::SetConfigOption { config_id }) => (
                "session/set_config_option",
                encode(SetSessionConfigOptionRequest::new(
                    session.to_string(),
                    config_id.clone(),
                    value,
                ))?,
            ),
            Some(ControlWrite::SetModel) => (
                "session/set_model",
                json!({"sessionId": session, "modelId": value}),
            ),
            Some(ControlWrite::SetMode) => (
                "session/set_mode",
                encode(SetSessionModeRequest::new(
                    session.to_string(),
                    value.to_string(),
                ))?,
            ),
            None => {
                return Err(AcpError::new(
                    "the agent did not offer this control for writing",
                ));
            }
        };
        self.request(
            method,
            params,
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
            encode(SetSessionConfigOptionRequest::new(
                session.to_string(),
                config_id.to_string(),
                value,
            ))?,
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
    ) -> Result<PromptReply, AcpError> {
        let deadline = Instant::now() + timeout;
        let result = self.request(
            "session/prompt",
            encode(PromptRequest::new(
                session.to_string(),
                vec![ContentBlock::from(text)],
            ))?,
            Some(session),
            deadline,
            handler,
            cancel,
        )?;
        let stop_reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .filter(|reason| !reason.is_empty())
            .map(str::to_string)
            .ok_or_else(|| AcpError::new("session/prompt did not report a stop reason"))?;
        Ok(PromptReply {
            stop_reason,
            usage: AgentTokenUsage::from_result(&result),
        })
    }

    pub fn prompt(
        &mut self,
        session: &str,
        text: &str,
        policy: PermissionPolicy,
        timeout: Duration,
    ) -> Result<PromptOutcome, AcpError> {
        let mut outcome = PromptOutcome::default();
        let reply = self.prompt_with(
            session,
            text,
            timeout,
            &mut PolicyHandler {
                policy,
                outcome: &mut outcome,
            },
            None,
        )?;
        outcome.stop_reason = reply.stop_reason;
        Ok(outcome)
    }

    pub fn cancel(&self, session: &str) -> Result<(), AcpError> {
        send_cancel(&self.commands, session)
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
    fn a_session_offer_is_parsed_bounded_and_never_widened() {
        let many: Vec<Value> = (0..(MAX_CHOICES + 40))
            .map(|index| json!({"value": format!("v{index}"), "name": "x".repeat(900)}))
            .collect();
        let offer = SessionOffer::from_session_result(&json!({
            "configOptions": [
                {"id": "model", "name": "Model", "category": "model", "type": "select",
                 "currentValue": "a",
                 "options": [{"group": "g", "name": "G", "options": [
                    {"value": "a", "name": "A"}, {"value": "a", "name": "dup"}, {"value": "b", "name": "B", "description": "d"}]},
                    {"value": "c", "name": "C"}]},
                {"id": "flag", "name": "Flag", "type": "boolean", "currentValue": true},
                {"id": "none", "name": "None", "type": "select", "options": []},
                {"id": "", "name": "No id", "type": "select", "options": []},
                {"id": "big", "name": "Big", "type": "select", "currentValue": "v0", "options": many},
                {"id": "bad\nid", "name": "Control char", "type": "select", "options": [{"value": "x"}]}
            ],
            "modes": {"currentModeId": "m1", "availableModes": [{"id": "m1", "name": "M1"}, {"id": "", "name": "bad"}]},
            "models": {"currentModelId": "legacy", "availableModels": [{"modelId": "legacy", "name": "Legacy"}]}
        }));
        let ids: Vec<&str> = offer.controls.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            ["model", "config:flag", "config:none", "config:big", "mode"]
        );
        assert_eq!(offer.malformed_controls, 2);
        let model = offer.control("model").unwrap();
        let values: Vec<&str> = model.choices.iter().map(|c| c.value.as_str()).collect();
        assert_eq!(
            values,
            ["a", "b", "c"],
            "groups flatten and duplicates collapse"
        );
        assert_eq!(model.choices[1].description.as_deref(), Some("d"));
        assert!(model.offers("c") && !model.offers("legacy") && !model.offers(""));
        assert_eq!(
            offer
                .control("config:flag")
                .unwrap()
                .unavailable
                .as_ref()
                .unwrap()
                .code,
            refusal::CONTROL_TYPE_UNSUPPORTED
        );
        assert!(!offer.control("config:flag").unwrap().offers("true"));
        assert_eq!(
            offer
                .control("config:none")
                .unwrap()
                .unavailable
                .as_ref()
                .unwrap()
                .code,
            refusal::CONTROL_HAS_NO_CHOICES
        );
        let big = offer.control("config:big").unwrap();
        assert_eq!(big.choices.len(), MAX_CHOICES);
        assert!(big.truncated);
        assert!(big.choices[0].name.chars().count() <= MAX_NAME_CHARS);
        assert!(!big.offers(&format!("v{}", MAX_CHOICES + 1)));
        let mode = offer.control("mode").unwrap();
        assert_eq!(mode.write, Some(ControlWrite::SetMode));
        assert_eq!(mode.choices.len(), 1);
        assert!(
            mode.truncated,
            "a dropped malformed choice marks the list incomplete"
        );

        let legacy = SessionOffer::from_session_result(&json!({
            "models": {"currentModelId": "legacy", "availableModels": [{"modelId": "legacy", "name": "Legacy"}]}
        }));
        assert_eq!(
            legacy.control("model").unwrap().write,
            Some(ControlWrite::SetModel)
        );
        assert!(
            SessionOffer::from_session_result(&json!({}))
                .controls
                .is_empty()
        );
    }

    #[test]
    fn prompt_usage_keeps_only_what_the_agent_reported() {
        let usage = AgentTokenUsage::from_result(&json!({
            "stopReason": "end_turn",
            "usage": {"inputTokens": 5, "outputTokens": "many", "totalTokens": 9}
        }))
        .unwrap();
        assert_eq!(usage.input_tokens, Some(5));
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.total_tokens, Some(9));
        assert!(AgentTokenUsage::from_result(&json!({"stopReason": "end_turn"})).is_none());
        assert!(AgentTokenUsage::from_result(&json!({"usage": {}})).is_none());
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
            .new_session(&identity, workspace.path(), &[], Duration::from_secs(20))
            .unwrap()
            .session_id;
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
                &[],
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
        for (response, timeout, expected) in [
            (
                "({ id: request.id, result: { protocolVersion: 1 } })",
                Duration::from_secs(3),
                "deadline",
            ),
            (
                "({ jsonrpc: '2.0', id: request.id, result: { protocolVersion: 2 } })",
                Duration::from_secs(20),
                "protocol version",
            ),
            (
                "({ jsonrpc: '2.0', id: request.id })",
                Duration::from_secs(3),
                "deadline",
            ),
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
            let error = client.initialize(timeout).unwrap_err();
            assert!(error.to_string().contains(expected), "{response}: {error}");
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
        let identity = client.initialize(Duration::from_secs(20)).unwrap();
        let session = client
            .new_session(&identity, workspace.path(), &[], Duration::from_secs(20))
            .unwrap()
            .session_id;
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

    const WIRE_AGENT: &str = r#"
const fs = require('fs');
const send = (message) => process.stdout.write(JSON.stringify(message) + '\n');
let pending = null;
require('readline').createInterface({ input: process.stdin }).on('line', (line) => {
  fs.appendFileSync('received.jsonl', line + '\n');
  const message = JSON.parse(line);
  if (message.method === 'initialize') {
    send({ jsonrpc: '2.0', id: message.id, result: { protocolVersion: 1, agentCapabilities: {}, agentInfo: { name: 'wire-agent', version: '0' } } });
  } else if (message.method === 'session/new') {
    send({ jsonrpc: '2.0', id: message.id, result: { sessionId: 's1' } });
  } else if (message.method === 'session/set_config_option') {
    send({ jsonrpc: '2.0', id: message.id, result: { configOptions: [] } });
  } else if (message.method === 'session/prompt') {
    const text = message.params.prompt[0].text;
    const options = [{ optionId: 'allow', name: 'Allow', kind: 'allow_once' }];
    if (text === 'hang') {
      pending = message.id;
      send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: 's1', update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'mine' } } } });
      send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: 'other', update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'theirs' } } } });
      send({ jsonrpc: '2.0', id: 'p-other', method: 'session/request_permission', params: { sessionId: 'other', toolCall: { toolCallId: 't1' }, options } });
    } else if (text === 'withdraw') {
      pending = message.id;
      send({ jsonrpc: '2.0', id: 'p-withdrawn', method: 'session/request_permission', params: { sessionId: 's1', toolCall: { toolCallId: 't2' }, options } });
      send({ jsonrpc: '2.0', method: '$/cancel_request', params: { requestId: 'p-withdrawn' } });
    } else if (text === 'flood') {
      process.stdout.write('"' + 'x'.repeat(8 * 1024 * 1024 + 16) + '"\n');
    }
  } else if (message.method === 'session/cancel' && pending !== null) {
    send({ jsonrpc: '2.0', id: pending, result: { stopReason: 'cancelled' } });
    pending = null;
  } else if (message.id === 'p-withdrawn' && pending !== null) {
    send({ jsonrpc: '2.0', id: pending, result: { stopReason: 'end_turn' } });
    pending = null;
  }
});
"#;

    fn wire_client(workspace: &Path) -> (LocalProcessFleetHostAdapter, AcpClient, String) {
        let mut host = LocalProcessFleetHostAdapter::new(workspace);
        let mut request = FleetWorkerStartRequest::new(
            "w-wire",
            FleetWorkerCommand::new("node", ["-e".to_string(), WIRE_AGENT.to_string()]),
        );
        request.transport = FleetWorkerTransport::JsonRpc;
        request.cwd = Some(workspace.to_path_buf());
        host.start_worker(request).unwrap();
        let mut client = AcpClient::new(host.take_worker_io("w-wire").unwrap());
        let identity = client.initialize(Duration::from_secs(20)).unwrap();
        assert_eq!(identity.name, "wire-agent");
        let session = client
            .new_session(&identity, workspace, &[], Duration::from_secs(20))
            .unwrap()
            .session_id;
        (host, client, session)
    }

    struct SlowAllow {
        texts: Vec<String>,
    }

    impl AcpHandler for SlowAllow {
        fn update(&mut self, update: &Value) {
            if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                self.texts.push(text.to_string());
            }
        }

        fn permission(&mut self, _params: &Value) -> PermissionAnswer {
            std::thread::sleep(Duration::from_millis(750));
            PermissionAnswer::Selected("allow".to_string())
        }
    }

    #[test]
    fn foreign_sessions_withdrawn_requests_and_missed_deadlines_keep_their_rules() {
        if node().is_none() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let (mut host, mut client, session) = wire_client(workspace.path());

        let canceller = client.canceller(&session);
        let prompt_session = session.clone();
        let blocked = std::thread::spawn(move || {
            let mut handler = SlowAllow { texts: Vec::new() };
            let reply = client.prompt_with(
                &prompt_session,
                "hang",
                Duration::from_secs(30),
                &mut handler,
                None,
            );
            (client, reply, handler.texts)
        });
        std::thread::sleep(Duration::from_millis(400));
        canceller.cancel().unwrap();
        let (mut client, reply, texts) = blocked.join().unwrap();
        assert_eq!(reply.unwrap().stop_reason, "cancelled");
        assert_eq!(texts, ["mine"], "another session's update was ignored");

        let mut handler = SlowAllow { texts: Vec::new() };
        let withdrawn = client
            .prompt_with(
                &session,
                "withdraw",
                Duration::from_secs(20),
                &mut handler,
                None,
            )
            .unwrap();
        assert_eq!(withdrawn.stop_reason, "end_turn");

        let silent = client
            .prompt(
                &session,
                "silent",
                PermissionPolicy::AllowOnce,
                Duration::from_millis(500),
            )
            .unwrap_err();
        assert!(silent.to_string().contains("deadline"), "{silent}");
        client
            .set_config_option(&session, "model", "any", Duration::from_secs(20))
            .unwrap();

        let received: Vec<Value> = std::fs::read_to_string(workspace.path().join("received.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let answer = |id: &str| {
            received
                .iter()
                .find(|message| message["id"] == id)
                .map(|message| message["result"]["outcome"].clone())
        };
        assert!(
            received
                .iter()
                .any(|message| message["method"] == "session/cancel"
                    && message["params"]["sessionId"] == session.as_str()),
            "{received:?}"
        );
        assert_eq!(
            answer("p-other"),
            Some(json!({"outcome": "cancelled"})),
            "a foreign permission request is answered cancelled: {received:?}"
        );
        assert_eq!(
            answer("p-withdrawn"),
            Some(json!({"outcome": "cancelled"})),
            "a withdrawn permission request is never answered with a selection: {received:?}"
        );
        assert!(
            !received
                .iter()
                .any(|message| message["method"] == "$/cancel_request"),
            "a missed deadline sends nothing to the agent: {received:?}"
        );
        let requests: Vec<&Value> = received
            .iter()
            .filter(|message| message.get("method").is_some() && message.get("id").is_some())
            .collect();
        assert_eq!(requests.len(), 6, "{received:?}");
        assert!(
            requests.iter().all(|message| message["id"].is_string()),
            "request ids are strings: {received:?}"
        );
        host.stop_worker("w-wire").unwrap();
    }

    #[test]
    fn a_line_over_the_limit_ends_the_connection_unparsed() {
        if node().is_none() {
            return;
        }
        let workspace = tempfile::tempdir().unwrap();
        let (mut host, mut client, session) = wire_client(workspace.path());
        let flooded = client
            .prompt(
                &session,
                "flood",
                PermissionPolicy::AllowOnce,
                Duration::from_secs(20),
            )
            .unwrap_err();
        assert!(flooded.to_string().contains("8 MiB"), "{flooded}");
        let after = client
            .set_config_option(&session, "model", "any", Duration::from_secs(5))
            .unwrap_err();
        assert!(
            after.to_string().contains("8 MiB"),
            "the connection stays ended: {after}"
        );
        host.stop_worker("w-wire").unwrap();
    }
}

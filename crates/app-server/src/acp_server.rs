//! ACP stdio projection of the existing Runtime/Engine owner.
//! History, provider requests, tools, approval and cancellation are Core facts.
//! This module keeps only connection configuration and canonical bindings; the
//! official ACP SDK owns framing, request ids and correlation. Secure
//! cross-process attachment is not yet qualified.
//!
//! Known limits: a slow editor no longer slows a turn, because updates queue
//! (SDK queue, then 256 frames of up to 8 MiB each) and memory is bounded only
//! by what one turn emits; `$/cancel_request` for a running `session/prompt` is
//! not honored (only `session/cancel` is).
use agent_client_protocol::Error as SdkError;
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema as acp;
use anyhow::{Result, anyhow};
use codewhale_config::AppMode;
use codewhale_config::ProviderKind;
use codewhale_engine::core::authority::{SandboxNetworkAccess, sandbox_policy_for_turn};
#[cfg(test)]
use codewhale_engine::runtime_threads::manager::RuntimeThreadManagerConfig;
use codewhale_engine::runtime_threads::{
    manager::CreateThreadRequest, manager::RuntimeThreadManager, manager::UpdateThreadRequest,
};
use codewhale_execpolicy::ApprovalMode;
use codewhale_runtime::config::Config;
use codewhale_runtime::features::Feature;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncBufRead, AsyncWrite};
#[cfg(test)]
use tokio::io::{AsyncWriteExt, BufReader};
mod connection;
mod runtime;
use connection::serve_connection;
const MAX_ACP_SESSIONS: usize = 64;
const TOOL_CALL_CONTENT_PREVIEW_CHARS: usize = 4_000;

pub async fn run_acp_server(
    config: Config,
    model: String,
    default_cwd: PathBuf,
    plugins: Arc<codewhale_plugins::PluginDiscoveryContext>,
    config_path: Option<PathBuf>,
    config_profile: Option<String>,
) -> Result<()> {
    ensure_acp_approval_posture(&config)?;
    eprintln!("{}", acp_startup_posture(&config, &default_cwd));
    crate::runtime_api::run_http_server(
        config,
        default_cwd,
        plugins,
        crate::runtime_api::RuntimeApiOptions {
            port: 0,
            config_path,
            config_profile,
            control_frontend: Some(
                codewhale_runtime::runtime_control::RuntimeControlFrontend::Acp { model },
            ),
            ..Default::default()
        },
    )
    .await
}

pub struct CapturedAcpFrontend {
    config: Config,
    model: String,
    cwd: PathBuf,
    manager: Arc<RuntimeThreadManager>,
    sessions_dir: PathBuf,
    config_path: Option<PathBuf>,
    config_profile: Option<String>,
}
pub fn capture_frontend(
    config: Config,
    model: String,
    cwd: PathBuf,
    manager: Arc<RuntimeThreadManager>,
    sessions_dir: PathBuf,
    config_path: Option<PathBuf>,
    config_profile: Option<String>,
) -> Result<Arc<CapturedAcpFrontend>> {
    ensure_acp_approval_posture(&config)?;
    Ok(Arc::new(CapturedAcpFrontend {
        config,
        model,
        cwd,
        manager,
        sessions_dir,
        config_path,
        config_profile,
    }))
}
impl CapturedAcpFrontend {
    pub fn serve<'a>(
        &'a self,
        input: Box<dyn AsyncBufRead + Send + Unpin>,
        output: Box<dyn AsyncWrite + Send + Unpin + 'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let server = AcpServer::new(
                self.config.clone(),
                self.model.clone(),
                self.cwd.clone(),
                self.manager.clone(),
                self.sessions_dir.clone(),
                self.config_path.clone(),
                self.config_profile.clone(),
            );
            serve_connection(server, input, output).await.map(drop)
        })
    }
}

#[cfg(test)]
impl crate::RuntimeOwnerFrontend for CapturedAcpFrontend {
    fn execute(
        &self,
        _request: crate::RuntimeOwnerRequest,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = anyhow::Result<serde_json::Value>> + Send + '_>,
    > {
        Box::pin(async {
            anyhow::bail!("this captured frontend admits no ordinary native service")
        })
    }
    fn events(
        &self,
        _thread_id: String,
        _since_seq: u64,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = anyhow::Result<crate::RuntimeOwnerEventStream>>
                + Send
                + '_,
        >,
    > {
        Box::pin(async {
            anyhow::bail!("this captured frontend admits no ordinary native observation")
        })
    }
    fn validate_selection<'a>(
        &'a self,
        selection: &'a crate::RuntimeOwnerFrontendSelection,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            anyhow::ensure!(
                matches!(selection, crate::RuntimeOwnerFrontendSelection::Acp { .. }),
                "ACP fixture does not capture HTTP services"
            );
            Ok(())
        })
    }
    fn serve<'a>(
        &'a self,
        selection: crate::RuntimeOwnerFrontendSelection,
        _compatibility: crate::AppState,
        input: Box<dyn AsyncBufRead + Send + Unpin>,
        output: Box<dyn AsyncWrite + Send + Unpin + 'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            anyhow::ensure!(
                matches!(selection, crate::RuntimeOwnerFrontendSelection::Acp { .. }),
                "ACP fixture does not capture HTTP services"
            );
            CapturedAcpFrontend::serve(self, input, output).await
        })
    }
}

struct AcpServer {
    config: Config,
    model: String,
    default_cwd: PathBuf,
    runtime: Arc<RuntimeThreadManager>,
    sessions_dir: PathBuf,
    config_path: Option<PathBuf>,
    config_profile: Option<String>,
    sessions: HashMap<String, AcpSession>,
    insertion_order: VecDeque<String>,
    client_supports_terminal: bool,
}
/// A transport binding, never another conversation or executable registry.
struct AcpSession {
    thread_id: String,
}
struct PendingToolCall {
    execution_id: String,
    name: String,
    input: Value,
}
#[derive(Debug)]
struct AcpError {
    code: i32,
    message: String,
}
enum Outgoing {
    Update(acp::SessionNotification),
    Permission {
        key: String,
        request: acp::RequestPermissionRequest,
    },
}
enum Inbound {
    Cancel { session: String },
    Permission { key: String, allow: bool },
    Eof,
}
type Emit<'a> = &'a mut (dyn FnMut(Outgoing) -> Result<()> + Send);
impl AcpServer {
    fn new(
        config: Config,
        model: String,
        default_cwd: PathBuf,
        runtime: Arc<RuntimeThreadManager>,
        sessions_dir: PathBuf,
        config_path: Option<PathBuf>,
        config_profile: Option<String>,
    ) -> Self {
        Self {
            config,
            model,
            default_cwd,
            runtime,
            sessions_dir,
            config_path,
            config_profile,
            sessions: HashMap::new(),
            insertion_order: VecDeque::new(),
            client_supports_terminal: false,
        }
    }
    fn prompt_run(&self) -> runtime::PromptRun {
        runtime::PromptRun {
            runtime: Arc::clone(&self.runtime),
            sessions_dir: self.sessions_dir.clone(),
        }
    }
    async fn handle_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> std::result::Result<Value, SdkError> {
        match method {
            "initialize" => {
                if let Some(terminal) = params
                    .pointer("/clientCapabilities/terminal")
                    .and_then(Value::as_bool)
                {
                    self.client_supports_terminal = terminal;
                }
                let terminal_auth = params
                    .pointer("/clientCapabilities/auth/terminal")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                Ok(to_wire(&initialize_result(&self.config, terminal_auth))?)
            }
            "session/new" => Ok(self.new_session(params).await?),
            "session/list" => Ok(self.list_sessions(params)?),
            "session/load" => Ok(self.load_session(params).await?),
            "session/listProviders" => Ok(self.list_providers()),
            "session/currentModel" => Ok(self.current_model()),
            "session/selectModel" => Ok(self.select_model(params)?),
            "session/set_config_option" => Ok(self.set_session_config(params).await?),
            "session/set_mode" | "session/set_model" => {
                let (config_id, field) = if method == "session/set_mode" {
                    ("mode", "modeId")
                } else {
                    ("model", "modelId")
                };
                self.set_session_config(json!({
                    "sessionId": params.get("sessionId"),
                    "configId": config_id,
                    "value": params.get(field),
                }))
                .await?;
                Ok(to_wire(&acp::SetSessionModeResponse::new())?)
            }
            _ => Err(SdkError::method_not_found().data(Value::String(method.to_string()))),
        }
    }

    fn validate_prompt(&self, params: &Value) -> std::result::Result<(String, String), AcpError> {
        let id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::invalid_params("sessionId is required"))?;
        if !self.sessions.contains_key(id) {
            return Err(AcpError::invalid_params("unknown sessionId"));
        }
        if let Some(kind) = unsupported_prompt_block(params.get("prompt")) {
            return Err(AcpError::invalid_params(format!(
                "unsupported prompt content block `{kind}`: this agent accepts text, resource and resource_link blocks only"
            )));
        }
        let prompt = extract_prompt_text(params.get("prompt"))
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| AcpError::invalid_params("prompt must include text content"))?;
        Ok((id.to_string(), prompt))
    }
    fn shell_allowed(&self) -> bool {
        self.client_supports_terminal && self.config.allow_shell()
    }
    fn remember(&mut self, id: String, thread_id: String) {
        if self.sessions.contains_key(&id) {
            return;
        }
        if self.sessions.len() >= MAX_ACP_SESSIONS
            && let Some(old) = self.insertion_order.pop_front()
        {
            self.sessions.remove(&old);
        }
        self.insertion_order.push_back(id.clone());
        self.sessions.insert(id, AcpSession { thread_id });
    }
    async fn new_session(&mut self, params: Value) -> std::result::Result<Value, AcpError> {
        validate_mcp_servers(&params)?;
        let cwd = absolute_cwd(&params, "session/new")?.unwrap_or_else(|| self.default_cwd.clone());
        let identity = self
            .config
            .active_provider_identity()
            .map_err(|error| AcpError::internal(error.to_string()))?;
        let thread = self
            .runtime
            .create_thread_with_shell_policy(
                CreateThreadRequest {
                    model: Some(self.model.clone()),
                    model_provider: Some(identity.persisted_kind().to_string()),
                    model_provider_id: identity.persisted_id().map(str::to_string),
                    workspace: Some(cwd),
                    mode: Some(
                        if acp_mode(&self.config) == AppMode::Plan {
                            "plan"
                        } else {
                            "agent"
                        }
                        .to_string(),
                    ),
                    permission_posture: Some(self.permission_value().to_string()),
                    allow_shell: Some(self.shell_allowed()),
                    ..Default::default()
                },
                self.config_path.as_deref(),
                self.config_profile.as_deref(),
            )
            .await
            .map_err(|error| AcpError::internal(error.to_string()))?;
        let id = uuid::Uuid::new_v4().to_string();
        crate::runtime_api::sessions::initialize_empty_session(
            &self.runtime,
            &self.sessions_dir,
            &thread.id,
            &id,
        )
        .await
        .map_err(|error| AcpError::internal(error.message))?;
        self.remember(id.clone(), thread.id);
        self.session_configuration(&id).await
    }
    /// Durable Codewhale sessions an ACP client can resume (#5864).
    ///
    /// ACP transport bindings are in-memory and capped; Core sessions are the
    /// durable record, and an IDE that offers "resume" means those. A store
    /// that cannot be read is an empty list, not a failed request: enumeration
    /// is discovery, and a client asking what exists should not be broken by a
    /// missing sessions directory.
    fn list_sessions(&self, params: Value) -> std::result::Result<Value, AcpError> {
        let cwd = match params.get("cwd") {
            None | Some(Value::Null) => None,
            Some(Value::String(path)) if std::path::Path::new(path).is_absolute() => {
                Some(PathBuf::from(path))
            }
            Some(_) => {
                return Err(AcpError::invalid_params(
                    "session/list cwd must be an absolute path",
                ));
            }
        };
        let sessions =
            codewhale_engine::session_manager::SessionManager::new(self.sessions_dir.clone())
                .ok()
                .map(|manager| manager.list_sessions_lenient())
                .unwrap_or_default();
        let sessions: Vec<acp::SessionInfo> = sessions
            .into_iter()
            .filter(|meta| {
                cwd.as_ref().is_none_or(|cwd| {
                    codewhale_engine::session_manager::paths_equivalent(&meta.workspace, cwd)
                })
            })
            .map(|meta| {
                acp::SessionInfo::new(
                    meta.id,
                    PathBuf::from(meta.workspace.to_string_lossy().into_owned()),
                )
                .title(meta.title)
                .updated_at(meta.updated_at.to_rfc3339())
                .meta(codewhale_meta(json!({
                    "createdAt": meta.created_at.to_rfc3339(),
                    "messageCount": meta.message_count,
                })))
            })
            .collect();
        to_wire(&acp::ListSessionsResponse::new(sessions))
    }

    async fn load_session(&mut self, params: Value) -> std::result::Result<Value, AcpError> {
        validate_mcp_servers(&params)?;
        absolute_cwd(&params, "session/load")?;
        let requested = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::invalid_params("session/load requires sessionId"))?;
        if self.sessions.contains_key(requested) {
            return self.session_configuration(requested).await;
        }
        let store =
            codewhale_engine::session_manager::SessionManager::new(self.sessions_dir.clone())
                .map_err(|error| AcpError::internal(error.to_string()))?;
        let saved = store
            .resume_session_by_prefix(requested)
            .map_err(|error| {
                AcpError::invalid_params(format!("could not load session {requested}: {error}"))
            })?
            .session;
        let id = saved.metadata.id;
        if self.sessions.contains_key(&id) {
            return self.session_configuration(&id).await;
        }
        let (_, axum::Json(resumed)) = crate::runtime_api::sessions::resume_session_in_runtime(
            &self.runtime,
            &self.sessions_dir,
            &id,
            crate::runtime_api::sessions::ResumeSessionRequest {
                model: None,
                mode: (acp_mode(&self.config) == AppMode::Plan).then(|| "plan".to_string()),
            },
            (self.config_path.as_deref(), self.config_profile.as_deref()),
            Some(self.shell_allowed()),
        )
        .await
        .map_err(|error| AcpError::internal(error.message))?;
        // A saved posture cannot widen the server, and connection terminal
        // negotiation only narrows the canonical thread's shell ceiling.
        self.runtime
            .update_thread_with_shell_policy(
                &resumed.thread_id,
                UpdateThreadRequest {
                    permission_posture: Some(self.permission_value().to_string()),
                    allow_shell: Some(self.shell_allowed()),
                    ..Default::default()
                },
                self.config_path.as_deref(),
                self.config_profile.as_deref(),
            )
            .await
            .map_err(|error| AcpError::internal(error.to_string()))?;
        self.remember(id.clone(), resumed.thread_id);
        self.session_configuration(&id).await
    }
    async fn history_updates(&self, session_id: &str) -> Result<Vec<acp::SessionUpdate>> {
        use codewhale_models::ContentBlock;
        let saved =
            codewhale_engine::session_manager::SessionManager::new(self.sessions_dir.clone())?
                .load_session(session_id)?;
        let secrets =
            codewhale_engine::runtime_threads::manager::receipt_secret_values(self.config.clone())
                .await?;
        let mut calls = HashMap::new();
        let mut updates = Vec::new();
        for message in &saved.messages {
            let chunk: Option<fn(acp::ContentChunk) -> acp::SessionUpdate> =
                match message.role.as_str() {
                    "user" => Some(acp::SessionUpdate::UserMessageChunk),
                    "assistant" => Some(acp::SessionUpdate::AgentMessageChunk),
                    _ => None,
                };
            if let Some(chunk) = chunk
                && !codewhale_engine::runtime_handoff::is_internal_runtime_handoff(message)
                && codewhale_models::turn_classify::restored_subagent_checkpoint_display(message)
                    .is_none()
            {
                let mut chunks = Vec::<String>::new();
                let mut coalesce = false;
                for (index, block) in message.content.iter().enumerate() {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            if codewhale_engine::runtime_handoff::is_turn_metadata_block(
                                message, index, text,
                            ) {
                                continue;
                            }
                            if text.starts_with("[tool_history_repair]")
                                || (message.role == "assistant"
                                    && codewhale_engine::runtime_handoff::archived_context_parts(
                                        text,
                                    )
                                    .is_some())
                            {
                                coalesce = false;
                                continue;
                            }
                            if coalesce {
                                if let Some(chunk) = chunks.last_mut() {
                                    if !chunk.is_empty() {
                                        chunk.push('\n');
                                    }
                                    chunk.push_str(text);
                                }
                            } else {
                                chunks.push(text.clone());
                            }
                            coalesce = true;
                        }
                        ContentBlock::Thinking { thinking, .. }
                            if thinking != "(reasoning omitted)" =>
                        {
                            coalesce = false;
                        }
                        ContentBlock::ToolUse { name, .. } if name == "update_plan" => {
                            coalesce = false;
                        }
                        _ => {}
                    }
                }
                updates.extend(
                    chunks
                        .into_iter()
                        .map(|text| chunk(acp::ContentChunk::new(acp::ContentBlock::from(text)))),
                );
            }
            for block in &message.content {
                let Some(key) = block.tool_call_key().map(|key| key.as_str().to_string()) else {
                    continue;
                };
                match block {
                    ContentBlock::ToolUse { name, input, .. } => {
                        let call = PendingToolCall {
                            execution_id: key.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        };
                        updates.push(acp::SessionUpdate::ToolCall(tool_call_start(&call)));
                        calls.insert(key, call);
                    }
                    ContentBlock::ToolResult {
                        content, is_error, ..
                    } => {
                        if let Some(call) = calls.remove(&key) {
                            let status = if is_error.unwrap_or(false) {
                                acp::ToolCallStatus::Failed
                            } else {
                                acp::ToolCallStatus::Completed
                            };
                            let detail =
                                codewhale_engine::runtime_threads::manager::tool_receipt_detail(
                                    &call.name, content, &secrets,
                                );
                            updates.push(acp::SessionUpdate::ToolCallUpdate(tool_call_update(
                                &call,
                                status,
                                Some(&detail),
                                &[],
                            )));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(updates)
    }
    fn permission_value(&self) -> &'static str {
        acp_permission_value(&self.config)
    }
    async fn session_configuration(
        &self,
        session_id: &str,
    ) -> std::result::Result<Value, AcpError> {
        to_wire(&self.session_view(session_id).await?)
    }
    async fn session_view(
        &self,
        session_id: &str,
    ) -> std::result::Result<acp::NewSessionResponse, AcpError> {
        use codewhale_localization::{MessageId, resolve_locale, tr};
        let settings = codewhale_runtime::settings::Settings::load().unwrap_or_default();
        let locale = resolve_locale(&settings.locale);
        let binding = self
            .sessions
            .get(session_id)
            .ok_or_else(|| AcpError::invalid_params("unknown sessionId"))?;
        let thread = self
            .runtime
            .get_thread(&binding.thread_id)
            .await
            .map_err(|error| AcpError::internal(error.to_string()))?;
        let identity = self
            .config
            .resolve_persisted_provider_identity(
                thread.model_provider.as_deref(),
                thread.model_provider_id.as_deref(),
            )
            .map_err(AcpError::invalid_params)?;
        let mut models =
            codewhale_engine::provider_lake::models_for_provider(&self.config, &identity);
        if !models.contains(&thread.model) {
            models.push(thread.model.clone());
        }
        let mut modes = vec![
            acp::SessionMode::new("plan", tr(locale, MessageId::AppModePlan).to_string())
                .description(tr(locale, MessageId::AppModePlanHint).to_string()),
        ];
        // #6310: the permission posture is server-owned (a client can never
        // relax it), but it must be discoverable. Work under Full Access must
        // not claim that edits ask for approval, and the posture is surfaced
        // below as a read-only select that names how Full Access is enabled.
        let posture = acp_approval_mode(&self.config);
        let agent_hint = if posture == ApprovalMode::Bypass {
            tr(locale, MessageId::HomeYoloModeTip)
        } else {
            tr(locale, MessageId::AppModeAgentHint)
        };
        if acp_mode(&self.config) != AppMode::Plan {
            modes.insert(
                0,
                acp::SessionMode::new("agent", tr(locale, MessageId::AppModeAgent).to_string())
                    .description(agent_hint.to_string()),
            );
        }
        let current_mode = if thread.mode == "plan" {
            "plan"
        } else {
            "agent"
        };
        let (posture_value, posture_name, posture_description) = match posture {
            ApprovalMode::Bypass => (
                "full-access",
                MessageId::ConfigChoiceFullAccess,
                MessageId::PermissionsPostureBypass,
            ),
            ApprovalMode::Auto => (
                "auto-review",
                MessageId::ConfigChoiceAutoReview,
                MessageId::PermissionsPostureAuto,
            ),
            ApprovalMode::Never => (
                "never",
                MessageId::ConfigChoiceNever,
                MessageId::PermissionsPostureNever,
            ),
            ApprovalMode::Suggest | ApprovalMode::AcceptEdits => (
                "ask",
                MessageId::ConfigChoiceAsk,
                MessageId::PermissionsPostureAsk,
            ),
        };
        let mode_choices: Vec<acp::SessionConfigSelectOption> = modes
            .iter()
            .map(|mode| {
                acp::SessionConfigSelectOption::new(mode.id.0.clone(), mode.name.clone())
                    .description(mode.description.clone())
            })
            .collect();
        let model_choices: Vec<acp::SessionConfigSelectOption> = models
            .iter()
            .map(|model| acp::SessionConfigSelectOption::new(model.clone(), model.clone()))
            .collect();
        let options = vec![
            acp::SessionConfigOption::select(
                "mode",
                tr(locale, MessageId::SettingSubjectMode).to_string(),
                current_mode,
                mode_choices,
            )
            .category(acp::SessionConfigOptionCategory::Mode),
            acp::SessionConfigOption::select(
                "model",
                tr(locale, MessageId::SettingSubjectModel).to_string(),
                thread.model.clone(),
                model_choices,
            )
            .category(acp::SessionConfigOptionCategory::Model),
            // Exactly one option: the posture the server was started
            // with. Offering a looser value here would let a client relax
            // the operator's floor.
            acp::SessionConfigOption::select(
                "permission",
                tr(locale, MessageId::SettingSubjectPermissions).to_string(),
                posture_value,
                vec![
                    acp::SessionConfigSelectOption::new(
                        posture_value,
                        tr(locale, posture_name).to_string(),
                    )
                    .description(tr(locale, posture_description).to_string()),
                ],
            )
            .category(acp::SessionConfigOptionCategory::Other(
                "_permission".to_string(),
            ))
            .meta(codewhale_meta(json!({
                "readOnly": true,
                "fullAccess": posture == ApprovalMode::Bypass,
                "enableFullAccess": ACP_FULL_ACCESS_HINT,
            }))),
        ];
        Ok(acp::NewSessionResponse::new(session_id.to_string())
            .modes(acp::SessionModeState::new(current_mode, modes))
            .config_options(options))
    }

    async fn set_session_config(&mut self, params: Value) -> std::result::Result<Value, AcpError> {
        let session_id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::invalid_params("sessionId is required"))?;
        let config_id = params
            .get("configId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::invalid_params("configId is required"))?;
        let value = params
            .get("value")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::invalid_params("value must be an offered string option"))?;
        if !self.sessions.contains_key(session_id) {
            return Err(AcpError::invalid_params("unknown sessionId"));
        }
        let options = self
            .session_view(session_id)
            .await?
            .config_options
            .unwrap_or_default();
        let offered = options.iter().any(|option| {
            &*option.id.0 == config_id
                && match &option.kind {
                    acp::SessionConfigKind::Select(select) => match &select.options {
                        acp::SessionConfigSelectOptions::Ungrouped(choices) => {
                            choices.iter().any(|choice| &*choice.value.0 == value)
                        }
                        _ => false,
                    },
                    _ => false,
                }
        });
        if !offered {
            return Err(AcpError::invalid_params(
                "unknown configuration option or value",
            ));
        }
        let binding = &self.sessions[session_id];
        let request = match config_id {
            "model" => UpdateThreadRequest {
                model: Some(value.to_string()),
                ..Default::default()
            },
            "mode" => UpdateThreadRequest {
                mode: Some(value.to_string()),
                allow_shell: Some(value != "plan" && self.shell_allowed()),
                ..Default::default()
            },
            "permission" => {
                return to_wire(&acp::SetSessionConfigOptionResponse::new(options));
            }
            _ => unreachable!("validated offered option"),
        };
        self.runtime
            .update_thread_with_shell_policy(
                &binding.thread_id,
                request,
                self.config_path.as_deref(),
                self.config_profile.as_deref(),
            )
            .await
            .map_err(|error| AcpError::internal(error.to_string()))?;
        let options = self
            .session_view(session_id)
            .await?
            .config_options
            .unwrap_or_default();
        to_wire(&acp::SetSessionConfigOptionResponse::new(options))
    }

    fn list_providers(&self) -> Value {
        let mut providers = self.config.provider_identities().into_iter().filter(|identity| identity.provider != ProviderKind::Antigravity).map(|identity| {
            json!({"id": identity.key, "displayName": identity.compatibility().map(|row| row.label).unwrap_or(identity.key.as_str()), "defaultModel": codewhale_engine::model_inventory::provider_default_model(&self.config, &identity)})
        }).collect::<Vec<_>>();
        providers.extend(self.config.unadmitted_provider_keys().into_iter().map(|key| json!({"id": key, "displayName": format!("{key} (unavailable)"), "defaultModel": "", "available": false, "reason": "provider_identity_unavailable"})));
        json!({"providers": providers})
    }

    fn current_model(&self) -> Value {
        // Prefer the raw configured provider key so a custom `[providers.<name>]`
        // entry round-trips through ACP instead of canonicalizing to "custom".
        let provider = self
            .config
            .active_provider_identity()
            .ok()
            .map(|identity| identity.key.to_string())
            .or_else(|| self.config.provider.clone())
            .unwrap_or_else(|| "unavailable".to_string());
        json!({
            "provider": provider,
            "model": self.model.as_str()
        })
    }

    fn select_model(&mut self, params: Value) -> std::result::Result<Value, AcpError> {
        let model = params
            .get("model")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::invalid_params("model is required"))?
            .to_string();

        if let Some(provider_value) = params.get("provider") {
            let provider_name = provider_value
                .as_str()
                .ok_or_else(|| AcpError::invalid_params("provider must be a string"))?;
            let identity = self
                .config
                .resolve_provider_selection_identity(provider_name)
                .map_err(AcpError::invalid_params)?;
            self.config
                .scope_to_provider_identity(&identity)
                .map_err(AcpError::invalid_params)?;
        }

        self.model = model;
        Ok(self.current_model())
    }
}
const ACP_FULL_ACCESS_HINT: &str = "Start the server with `codewhale serve --acp --full-access` (alias `--yolo`), or set `yolo = true` in config.toml. Full Access also turns off Codewhale's own sandbox unless sandbox_mode tightens it; Plan stays read-only.";

fn acp_permission_value(config: &Config) -> &'static str {
    match acp_approval_mode(config) {
        ApprovalMode::Bypass => "full-access",
        ApprovalMode::Auto => "auto-review",
        ApprovalMode::Never => "never",
        ApprovalMode::Suggest | ApprovalMode::AcceptEdits => "ask",
    }
}

fn acp_startup_posture(config: &Config, workspace: &Path) -> String {
    let approval = acp_approval_mode(config);
    let sandbox = sandbox_policy_for_turn(
        acp_mode(config),
        approval,
        config.sandbox_mode.as_deref(),
        workspace,
        SandboxNetworkAccess::from_config(config.sandbox_network_access),
    );
    let mut line = format!(
        "ACP permission: {}; sandbox: {}.",
        acp_permission_value(config),
        sandbox.config_value()
    );
    if approval != ApprovalMode::Bypass {
        line.push_str(
            " For Full Access start with `codewhale serve --acp --full-access` or set `yolo = true`; sandbox_mode alone never turns it on.",
        );
    }
    line
}

fn acp_mode(config: &Config) -> AppMode {
    if config.sandbox_mode.as_deref() == Some("read-only") {
        AppMode::Plan
    } else {
        AppMode::Agent
    }
}

/// Approval posture for ACP turns, derived from server config instead of
/// hardcoded: `--yolo` resolves to Bypass so an unattended headless session
/// actually executes tools (#6337); otherwise the configured approval policy,
/// else the Suggest default. Plan mode still pins read-only downstream
/// regardless of posture.
fn acp_approval_mode(config: &Config) -> ApprovalMode {
    if config.yolo.unwrap_or(false) && !config.approval_policy_is_requirements_managed() {
        ApprovalMode::Bypass
    } else {
        match config
            .approval_policy
            .as_deref()
            .and_then(ApprovalMode::from_config_value)
            .unwrap_or_default()
        {
            ApprovalMode::AcceptEdits => ApprovalMode::Suggest,
            mode => mode,
        }
    }
}

fn ensure_acp_approval_posture(config: &Config) -> Result<()> {
    anyhow::ensure!(
        acp_approval_mode(config) != ApprovalMode::Never,
        "approval_policy \"never\" is not supported by `codewhale serve --acp`: ACP sessions run under Ask, Auto-Review or Full Access. Set approval_policy = \"on-request\" for this server, or start it with --full-access for Full Access."
    );
    Ok(())
}

/// ACP `kind` hint for a tool call, used by the client to pick an icon/label.
/// Falls back to `"other"` for tools without an obvious category.
///
/// `File` is a single canonical tool covering read/list/search/write/edit/
/// patch (#4625), so its kind depends on the `action` argument rather than
/// the tool name alone.
fn tool_call_kind(call: &PendingToolCall) -> acp::ToolKind {
    match call.name.as_str() {
        "File" => match call.input.get("action").and_then(Value::as_str) {
            Some("write" | "edit" | "patch") => acp::ToolKind::Edit,
            _ => acp::ToolKind::Read,
        },
        "apply_patch" | "write" | "edit" | "write_file" | "edit_file" => acp::ToolKind::Edit,
        "read" | "read_file" => acp::ToolKind::Read,
        "Git" => acp::ToolKind::Read,
        "bash" | "Bash" | "terminal/run" | "terminal/send" | "terminal/wait"
        | "terminal/cancel" | "terminal/reset" => acp::ToolKind::Execute,
        _ => acp::ToolKind::Other,
    }
}

/// Human-readable title for a tool call: the tool name plus its primary
/// argument (path/command/pattern) when present, so the client's tool-call
/// card is legible without expanding raw input.
fn tool_call_title(call: &PendingToolCall) -> String {
    let detail = call
        .input
        .get("path")
        .or_else(|| call.input.get("command"))
        .or_else(|| call.input.get("pattern"))
        .or_else(|| call.input.get("task_id"))
        .and_then(Value::as_str);
    match detail {
        Some(detail) => format!("{}: {}", call.name, detail),
        None => call.name.clone(),
    }
}

fn truncate_for_acp(content: &str) -> String {
    if content.chars().count() <= TOOL_CALL_CONTENT_PREVIEW_CHARS {
        return content.to_string();
    }
    let truncated: String = content
        .chars()
        .take(TOOL_CALL_CONTENT_PREVIEW_CHARS)
        .collect();
    format!("{truncated}\n… [truncated for display; the full result was sent to the model]")
}

fn tool_call_start(call: &PendingToolCall) -> acp::ToolCall {
    acp::ToolCall::new(call.execution_id.clone(), tool_call_title(call))
        .kind(tool_call_kind(call))
        .status(acp::ToolCallStatus::Pending)
        .raw_input((!call.input.is_null()).then(|| call.input.clone()))
}

fn tool_call_update(
    call: &PendingToolCall,
    status: acp::ToolCallStatus,
    content: Option<&str>,
    rich_blocks: &[codewhale_tools::ToolResultContentBlock],
) -> acp::ToolCallUpdate {
    let mut fields = acp::ToolCallUpdateFields::new().status(status);
    if content.is_some() || !rich_blocks.is_empty() {
        let mut blocks = Vec::with_capacity(rich_blocks.len() + usize::from(content.is_some()));
        if let Some(content) = content {
            blocks.push(acp::ToolCallContent::from(truncate_for_acp(content)));
        }
        blocks.extend(rich_blocks.iter().map(|block| match block {
            codewhale_tools::ToolResultContentBlock::Image { mime_type, data } => {
                acp::ToolCallContent::from(acp::ContentBlock::Image(acp::ImageContent::new(
                    data.clone(),
                    mime_type.clone(),
                )))
            }
        }));
        fields = fields.content(blocks);
    }
    acp::ToolCallUpdate::new(call.execution_id.clone(), fields)
}

fn agent_text(text: String) -> acp::SessionUpdate {
    acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk::new(acp::ContentBlock::from(text)))
}

impl AcpError {
    fn invalid_params(message: impl Into<String>) -> Self {
        Self {
            code: -32602,
            message: message.into(),
        }
    }

    /// JSON-RPC internal error: the request was well-formed and the agent
    /// could not serve it.
    fn internal(message: impl Into<String>) -> Self {
        Self {
            code: -32603,
            message: message.into(),
        }
    }
}

impl From<AcpError> for SdkError {
    fn from(error: AcpError) -> Self {
        SdkError::new(error.code, error.message)
    }
}

fn to_wire<T: serde::Serialize>(value: &T) -> std::result::Result<Value, AcpError> {
    serde_json::to_value(value).map_err(|error| AcpError::internal(error.to_string()))
}

fn codewhale_meta(fields: Value) -> acp::Meta {
    let mut meta = acp::Meta::new();
    meta.insert("codewhale".to_string(), fields);
    meta
}

fn validate_mcp_servers(params: &Value) -> std::result::Result<(), AcpError> {
    match params.get("mcpServers") {
        None => Ok(()),
        Some(Value::Array(servers)) if servers.is_empty() => Ok(()),
        Some(Value::Array(_)) => Err(AcpError::invalid_params(
            "Client-supplied MCP servers are unsupported by Codewhale's ACP adapter. Use an empty mcpServers list; sessions use the MCP servers and plugins configured in Codewhale itself.",
        )),
        Some(_) => Err(AcpError::invalid_params("mcpServers must be an array")),
    }
}

fn initialize_result(config: &Config, terminal_auth: bool) -> acp::InitializeResponse {
    let features = config.features();
    // ACP `SessionCapabilities` fields are objects, never booleans:
    // `{}` means "supported", absent/null means "not supported"
    // (#5969 — a boolean here made JetBrains' strictly-typed client
    // fail the handshake and kill the agent). `session/load` support
    // is advertised by the top-level `loadSession`; it is not a
    // field of `sessionCapabilities`. We only claim `list` because
    // `session/list` is the only one of the optional session methods
    // this server dispatches.
    let capabilities = acp::AgentCapabilities::new()
        .load_session(true)
        .prompt_capabilities(acp::PromptCapabilities::new().embedded_context(true))
        .mcp_capabilities(acp::McpCapabilities::new())
        .session_capabilities(
            acp::SessionCapabilities::new().list(acp::SessionListCapabilities::new()),
        )
        .meta(codewhale_meta(json!({
            "plugins": features.enabled(Feature::ExtensionHost)
                && codewhale_plugins::activation::extension_host_policy_enabled(),
            "configuredMcpServers": features.enabled(Feature::Mcp),
            "clientMcpServers": false
        })));
    acp::InitializeResponse::new(ProtocolVersion::V1)
        .agent_capabilities(capabilities)
        .agent_info(
            acp::Implementation::new("codewhale", env!("CARGO_PKG_VERSION")).title("codewhale"),
        )
        .auth_methods(acp_auth_methods(config, terminal_auth))
}

fn acp_auth_methods(config: &Config, terminal_auth: bool) -> Vec<acp::AuthMethod> {
    let Some(identity) = terminal_auth
        .then(|| config.active_provider_identity().ok())
        .flatten()
        .filter(|identity| {
            identity.provider != ProviderKind::Custom
                && identity.provider != ProviderKind::Antigravity
        })
    else {
        return Vec::new();
    };
    let provider = identity.provider.as_str();
    vec![acp::AuthMethod::Terminal(
        acp::AuthMethodTerminal::new("codewhale-terminal-auth", "Set Codewhale API key")
            .description(format!(
                "Run Codewhale's terminal credential setup for the {provider} provider."
            ))
            .args(
                ["auth", "set", "--provider", provider]
                    .map(str::to_string)
                    .to_vec(),
            ),
    )]
}

fn extract_prompt_text(prompt: Option<&Value>) -> Option<String> {
    match prompt? {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => {
            let parts = blocks
                .iter()
                .filter_map(content_block_text)
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        _ => None,
    }
}

fn unsupported_prompt_block(prompt: Option<&Value>) -> Option<String> {
    let Value::Array(blocks) = prompt? else {
        return None;
    };
    blocks.iter().find_map(|block| {
        let kind = block
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        content_block_text(block)
            .is_none()
            .then(|| kind.to_string())
    })
}

fn absolute_cwd(params: &Value, method: &str) -> std::result::Result<Option<PathBuf>, AcpError> {
    match params.get("cwd") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(path)) if Path::new(path).is_absolute() => Ok(Some(PathBuf::from(path))),
        Some(_) => Err(AcpError::invalid_params(format!(
            "{method} cwd must be an absolute path"
        ))),
    }
}

fn content_block_text(block: &Value) -> Option<String> {
    match block.get("type").and_then(Value::as_str)? {
        "text" => block
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_string),
        "resource" => resource_text(block),
        "resource_link" | "resourceLink" => resource_link_text(block),
        _ => None,
    }
}

fn resource_text(block: &Value) -> Option<String> {
    let resource = block.get("resource").unwrap_or(block);
    if let Some(text) = resource.get("text").and_then(Value::as_str) {
        return Some(text.to_string());
    }
    resource_link_text(resource)
}

fn resource_link_text(block: &Value) -> Option<String> {
    let uri = block
        .get("uri")
        .or_else(|| block.pointer("/resource/uri"))
        .and_then(Value::as_str)?;
    Some(format!("@{uri}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_engine::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Role};
    use codewhale_util::bounded_lines::MAX_FRAME_BYTES;
    use std::time::Duration;

    fn replayed(updates: Vec<acp::SessionUpdate>) -> Vec<Value> {
        updates
            .into_iter()
            .map(|update| json!({"params":{"update":serde_json::to_value(update).unwrap()}}))
            .collect()
    }
    pub(super) fn fixture_config() -> Config {
        let mut config = {
            let __cw_config_init_0_0 = Some("acp-fixture".into());
            let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
                custom: HashMap::from([(
                    "acp-fixture".into(),
                    codewhale_runtime::config::ProviderConfig {
                        kind: Some("openai-compatible".into()),
                        base_url: Some("http://127.0.0.1:18181/v1".into()),
                        model: Some("fixture-model".into()),
                        api_key: Some("local-test-key".into()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            });
            let __cw_config_init_0_2 = Some(codewhale_runtime::config::SnapshotsConfig {
                enabled: false,
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value.snapshots = __cw_config_init_0_2;
            __cw_config_init_0_value
        };
        config.set_feature("mcp", false).unwrap();
        config.set_feature("subagents", false).unwrap();
        config
    }
    pub(super) struct Editor {
        lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
        pub(super) write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        pub(super) next_id: u64,
        pub(super) seen: Vec<Value>,
    }
    impl Editor {
        pub(super) async fn send(&mut self, message: Value) -> Result<()> {
            let mut line = serde_json::to_vec(&message)?;
            line.push(b'\n');
            self.write.write_all(&line).await?;
            self.write.flush().await?;
            Ok(())
        }
        pub(super) async fn send_raw(&mut self, line: &str) -> Result<()> {
            self.write.write_all(line.as_bytes()).await?;
            self.write.write_all(b"\n").await?;
            self.write.flush().await?;
            Ok(())
        }
        pub(super) async fn recv(&mut self) -> Result<Value> {
            let line = tokio::time::timeout(Duration::from_secs(30), self.lines.next_line())
                .await
                .map_err(|_| anyhow!("no frame within 30s"))??
                .ok_or_else(|| anyhow!("the agent closed the transport"))?;
            let frame: Value = serde_json::from_str(&line)?;
            self.seen.push(frame.clone());
            Ok(frame)
        }
        pub(super) async fn until_response(&mut self, id: &Value) -> Result<(Value, Vec<Value>)> {
            let mut frames = Vec::new();
            loop {
                let frame = self.recv().await?;
                if frame["id"] == *id && frame.get("method").is_none() {
                    return Ok((frame, frames));
                }
                frames.push(frame);
            }
        }
        pub(super) async fn request(
            &mut self,
            method: &str,
            params: Value,
        ) -> Result<(Value, Vec<Value>)> {
            self.next_id += 1;
            let id = json!(self.next_id);
            self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .await?;
            self.until_response(&id).await
        }
        pub(super) async fn until_permission(&mut self) -> Result<Value> {
            loop {
                let frame = self.recv().await?;
                if frame["method"] == "session/request_permission" {
                    return Ok(frame);
                }
            }
        }
    }
    pub(super) struct Rig {
        pub(super) server: AcpServer,
        mock: Arc<MockLlmClient>,
        pub(super) workspace: PathBuf,
        _home: codewhale_engine::test_support::SealedHome,
        _dir: tempfile::TempDir,
    }
    impl Rig {
        pub(super) fn new(
            config: Config,
            turns: Vec<codewhale_engine::llm_client::mock::CannedTurn>,
        ) -> Result<Self> {
            Self::with_base(config, turns, true)
        }
        fn with_base(
            config: Config,
            turns: Vec<codewhale_engine::llm_client::mock::CannedTurn>,
            acp: bool,
        ) -> Result<Self> {
            let dir = tempfile::tempdir()?;
            let home = codewhale_engine::test_support::SealedHome::at(dir.path());
            let workspace = dir.path().join("workspace");
            std::fs::create_dir(&workspace)?;
            let runtime_dir = dir.path().join("runtime");
            let manager_config = RuntimeThreadManagerConfig {
                mcp_auth_context: Default::default(),
                data_dir: runtime_dir.clone(),
                task_data_dir: runtime_dir,
                sessions_dir: None,
                max_active_threads: 4,
            };
            let plugins = Arc::new(codewhale_plugins::PluginRegistry::empty(&workspace));
            let manager = Arc::new(if acp {
                RuntimeThreadManager::open_host(
                    config.clone(),
                    workspace.clone(),
                    manager_config,
                    plugins,
                    codewhale_engine::core::engine::EngineHostProfile::Acp,
                    None,
                    None,
                )?
            } else {
                RuntimeThreadManager::open_with_plugin_registry(
                    config.clone(),
                    workspace.clone(),
                    manager_config,
                    plugins,
                )?
            });
            let mock = Arc::new(MockLlmClient::new(turns));
            manager.set_test_model_client(mock.clone());
            let sessions = codewhale_engine::session_manager::default_sessions_dir()?;
            Ok(Self {
                server: AcpServer::new(
                    config,
                    "fixture-model".into(),
                    workspace.clone(),
                    manager,
                    sessions,
                    None,
                    None,
                ),
                mock,
                workspace,
                _home: home,
                _dir: dir,
            })
        }
        pub(super) async fn new_session(&mut self) -> String {
            self.server.new_session(json!({})).await.unwrap()["sessionId"]
                .as_str()
                .unwrap()
                .to_string()
        }
        fn placeholder(&self) -> AcpServer {
            AcpServer::new(
                self.server.config.clone(),
                self.server.model.clone(),
                self.server.default_cwd.clone(),
                Arc::clone(&self.server.runtime),
                self.server.sessions_dir.clone(),
                None,
                None,
            )
        }
        pub(super) async fn drive<T>(
            &mut self,
            script: impl AsyncFnOnce(&mut Editor) -> Result<T>,
        ) -> Result<(T, Vec<Value>)> {
            let (editor_end, agent_end) = tokio::io::duplex(1 << 20);
            let (agent_read, agent_write) = tokio::io::split(agent_end);
            let (read, write) = tokio::io::split(editor_end);
            let mut editor = Editor {
                lines: tokio::io::AsyncBufReadExt::lines(BufReader::new(read)),
                write,
                next_id: 0,
                seen: Vec::new(),
            };
            let placeholder = self.placeholder();
            let server = std::mem::replace(&mut self.server, placeholder);
            let serving = super::connection::serve_connection(
                server,
                Box::new(BufReader::new(agent_read)),
                Box::new(agent_write),
            );
            let scripted = async {
                let result = script(&mut editor).await;
                let _ = editor.write.shutdown().await;
                result
            };
            let (served, scripted) = tokio::time::timeout(
                Duration::from_secs(if cfg!(windows) { 120 } else { 20 }),
                async { tokio::join!(serving, scripted) },
            )
            .await?;
            let shared = served?;
            self.server = Arc::try_unwrap(shared)
                .map_err(|_| anyhow!("the connection kept a server reference"))?
                .into_server();
            while let Some(line) = editor.lines.next_line().await? {
                editor.seen.push(serde_json::from_str(&line)?);
            }
            Ok((scripted?, editor.seen))
        }
        async fn prompt(&mut self, session: &str, text: &str) -> Result<(String, Vec<Value>)> {
            let (response, frames) = self
                .drive(async |editor| {
                    let (response, _) = editor
                        .request("session/prompt", json!({"sessionId":session,"prompt":text}))
                        .await?;
                    Ok(response)
                })
                .await?;
            if let Some(error) = response.get("error") {
                anyhow::bail!("{error}");
            }
            let reason = response["result"]["stopReason"]
                .as_str()
                .ok_or_else(|| anyhow!("{response}"))?
                .to_string();
            let updates = frames
                .into_iter()
                .filter(|frame| frame["method"] == "session/update")
                .collect();
            Ok((reason, updates))
        }
        async fn history(&self, session: &str) -> Vec<codewhale_models::Message> {
            let thread = &self.server.sessions[session].thread_id;
            self.server
                .runtime
                .get_engine(thread)
                .await
                .unwrap()
                .get_session_snapshot()
                .await
                .unwrap()
                .messages
        }
        pub(super) async fn close(&self) {
            self.server.runtime.shutdown_and_wait().await.unwrap();
        }
    }
    #[cfg(any(unix, windows))]
    #[tokio::test(flavor = "current_thread")]
    async fn authenticated_acp_owner_projects_actual_engine_and_guest_eof_preserves_lease()
    -> Result<()> {
        authenticated_owner_profile_case(true).await
    }
    #[cfg(any(unix, windows))]
    #[tokio::test(flavor = "current_thread")]
    async fn normal_owner_admits_authenticated_acp_then_ordinary_successor_without_profile_leak()
    -> Result<()> {
        authenticated_owner_profile_case(false).await
    }
    #[cfg(any(unix, windows))]
    async fn authenticated_owner_profile_case(base_acp: bool) -> Result<()> {
        struct AbortOnDrop(tokio::task::AbortHandle);
        impl Drop for AbortOnDrop {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        async fn correlated(
            guest: &mut crate::daemon_client::OwnerClient,
            id: Value,
        ) -> Result<Value> {
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let frame = guest
                        .recv()
                        .await?
                        .ok_or_else(|| anyhow!("ACP owner closed before reply"))?;
                    if frame["id"] == id {
                        return Ok(frame);
                    }
                }
            })
            .await?
        }
        let config = fixture_config();
        let rig = Rig::with_base(
            config.clone(),
            vec![canned::simple_text_turn("same owner answer")],
            base_acp,
        )?;
        let manager = rig.server.runtime.clone();
        let capture = manager.clone();
        let (binding, generation) =
            codewhale_runtime::runtime_control::owner_work(move || capture.capture_control_owner())
                .await?;
        #[cfg(unix)]
        let socket_directory = rig._dir.path().canonicalize()?.join("endpoint");
        #[cfg(unix)]
        let _socket_directory =
            codewhale_config::private_directory::PrivateDirectory::open(&socket_directory)?;
        #[cfg(unix)]
        let socket_path = socket_directory.join("acp-owner.sock");
        #[cfg(windows)]
        let socket_path = PathBuf::from(format!(
            r"\\.\pipe\codewhale-acp-test-{}",
            uuid::Uuid::new_v4()
        ));
        #[cfg(unix)]
        let principal =
            codewhale_config::private_directory::PrivateDirectory::current_user_id().to_string();
        #[cfg(windows)]
        let principal = codewhale_runtime::runtime_control::owner_work(|| {
            codewhale_config::windows_identity::CurrentWindowsUser::open()?.sid_string()
        })
        .await?;
        let receipt = codewhale_protocol::RuntimeOwnerReceipt {
            version: 1,
            data_dir: binding.data_dir,
            execution_scope: binding.execution_scope,
            lease_generation: generation,
            pid: std::process::id(),
            process_start: crate::daemon_socket::capture_process_start(std::process::id()).await?,
            principal,
            socket_path: socket_path.clone(),
            config_path: None,
        };
        let frontend = capture_frontend(
            config,
            "fixture-model".into(),
            rig.workspace.clone(),
            manager.clone(),
            rig.server.sessions_dir.clone(),
            None,
            None,
        )?;
        // ACP calls the held manager directly through its captured projection;
        // this closed endpoint has no fake HTTP service and is never used.
        let (daemon, _state) = crate::bind_runtime_frontends(
            None,
            None,
            receipt.clone(),
            crate::RuntimeOwnerRouting {
                workers: None,
                workspace: Some(rig.workspace.clone()),
                endpoint: "127.0.0.1:1".parse()?,
                mobile: false,
                web: false,
                acp: true,
                acp_only: base_acp,
            },
            Some(frontend),
            None,
        )
        .await?;
        // ACP publications are discovered through this socket, so an
        // unbound one is a failure here rather than a degraded start.
        let daemon = daemon.ok_or_else(|| {
            anyhow!("this filesystem cannot hold the private runtime endpoint directory")
        })?;
        let shutdown = daemon.shutdown_handle();
        let owner = tokio::spawn(daemon.serve());
        let _abort = AbortOnDrop(owner.abort_handle());
        let mut guest =
            crate::daemon_client::connect_acp_if_published(None, Some(socket_path.clone()))
                .await?
                .ok_or_else(|| anyhow!("published ACP owner unavailable"))?;
        assert_eq!(guest.receipt(), &receipt);
        assert!(guest.routing().is_some_and(|routing| routing.acp));
        guest
            .send(
                json!(1),
                "initialize",
                json!({"protocolVersion":1,"clientCapabilities":{}}),
            )
            .await?;
        assert!(correlated(&mut guest, json!(1)).await?["error"].is_null());
        guest
            .send(json!(2), "session/new", json!({"cwd":rig.workspace}))
            .await?;
        let created = correlated(&mut guest, json!(2)).await?;
        let session = created["result"]["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("{created}"))?
            .to_string();
        guest
            .send(
                json!(3),
                "session/prompt",
                json!({"sessionId":session,"prompt":"one actual turn"}),
            )
            .await?;
        let completed = correlated(&mut guest, json!(3)).await?;
        assert_eq!(completed["result"]["stopReason"], "end_turn", "{completed}");
        tokio::time::timeout(
            Duration::from_secs(5),
            guest.forward(tokio::io::empty(), tokio::io::sink()),
        )
        .await??;
        let mut shutdown_guest =
            crate::daemon_client::connect_acp_if_published(None, Some(socket_path.clone()))
                .await?
                .ok_or_else(|| anyhow!("owner ended after guest EOF"))?;
        shutdown_guest.send(json!(4), "shutdown", json!({})).await?;
        let closed = correlated(&mut shutdown_guest, json!(4)).await?;
        assert!(closed["error"].is_null());
        assert!(
            tokio::time::timeout(Duration::from_secs(5), shutdown_guest.recv())
                .await??
                .is_none(),
            "ACP shutdown closes only this connection"
        );
        let rows = manager
            .list_threads(
                codewhale_engine::runtime_threads::manager::ThreadListFilter::IncludeArchived,
                None,
            )
            .await?;
        assert_eq!(rows.len(), 1);
        let detail = manager.get_thread_detail(&rows[0].id).await?;
        assert_eq!(detail.turns.len(), 1);
        assert_eq!(rig.mock.call_count(), 1);
        if !base_acp {
            rig.mock
                .push_turn(canned::simple_text_turn("ordinary successor"));
            let ordinary = manager
                .start_turn(
                    &rows[0].id,
                    codewhale_engine::runtime_threads::manager::StartTurnRequest {
                        prompt: "ordinary successor".into(),
                        ..Default::default()
                    },
                )
                .await?;
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let detail = manager.get_thread_detail(&rows[0].id).await?;
                    if detail.turns.iter().any(|t| {
                        t.id == ordinary.id
                            && t.status
                                == codewhale_engine::runtime_threads::RuntimeTurnStatus::Completed
                    }) {
                        return Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await??;
            let requests = rig.mock.captured_requests();
            assert_eq!(requests.len(), 2);
            let acp = requests[0]
                .tools
                .as_ref()
                .ok_or_else(|| anyhow!("ACP tools missing"))?;
            assert!(acp.iter().all(|tool| !matches!(
                tool.name.as_str(),
                "update_goal"
                    | "create_goal"
                    | "request_user_input"
                    | "tool_search"
                    | "spawn_agent"
            )));
            let normal = requests[1]
                .tools
                .as_ref()
                .ok_or_else(|| anyhow!("ordinary tools missing"))?;
            assert_ne!(
                acp.iter().map(|t| &t.name).collect::<Vec<_>>(),
                normal.iter().map(|t| &t.name).collect::<Vec<_>>(),
                "ordinary successor must rebuild its ordinary catalog"
            );
            assert_eq!(manager.get_thread_detail(&rows[0].id).await?.turns.len(), 2);
        }
        let capture = manager.clone();
        let (after, generation) =
            codewhale_runtime::runtime_control::owner_work(move || capture.capture_control_owner())
                .await?;
        assert_eq!(after.data_dir, receipt.data_dir);
        assert_eq!(
            generation, receipt.lease_generation,
            "guest EOF cannot release the host lease"
        );
        shutdown.trigger();
        tokio::time::timeout(Duration::from_secs(5), owner).await???;
        assert!(
            crate::daemon_client::connect_acp_if_published(None, Some(socket_path))
                .await?
                .is_none(),
            "shutdown withdraws the authenticated owner receipt instead of advertising a guest"
        );
        rig.close().await;
        Ok(())
    }

    #[test]
    fn acp_approval_mode_derives_from_server_config() {
        // #6337: `--yolo --danger-full-access` must not silently run as Ask.
        let yolo = {
            let __cw_config_init_1_0 = Some(true);
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.yolo = __cw_config_init_1_0;
            __cw_config_init_1_value
        };
        assert_eq!(acp_approval_mode(&yolo), ApprovalMode::Bypass);
        let requirements = tempfile::tempdir().expect("requirements directory");
        let requirements_path = requirements.path().join("requirements.toml");
        std::fs::write(
            &requirements_path,
            "allowed_approval_policies = ['on-request']\n",
        )
        .expect("requirements file");
        let mut managed_yolo = Config::default();
        managed_yolo.requirements_path = Some(requirements_path.display().to_string());
        managed_yolo.yolo = Some(true);
        assert_eq!(
            acp_approval_mode(&managed_yolo),
            ApprovalMode::Suggest,
            "requirements must outrank the CLI Full Access override"
        );
        let policy = {
            let __cw_config_init_2_0 = Some("never".into());
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.approval_policy = __cw_config_init_2_0;
            __cw_config_init_2_value
        };
        assert_eq!(acp_approval_mode(&policy), ApprovalMode::Never);
        assert_eq!(acp_approval_mode(&Config::default()), ApprovalMode::Suggest);
    }

    #[test]
    fn initialize_advertises_baseline_acp_agent() {
        let result = to_wire(&initialize_result(&Config::default(), true)).unwrap();

        assert_eq!(result["protocolVersion"], 1);
        assert_eq!(result["agentInfo"]["name"], "codewhale");
        // #5864: enumerating and resuming durable Codewhale sessions is now
        // served, so the capability says so rather than declining it.
        // #5969: this test used to assert `list == true` and `load == true`,
        // certifying the wire format that broke every strictly-typed client.
        // `loadSession` is the top-level boolean that advertises
        // `session/load`; inside `sessionCapabilities` every field is a
        // capability *object*, and `load` is not a field at all. Comparing the
        // whole object pins both halves: a boolean `list` or a resurrected
        // `load` key fails here.
        assert_eq!(result["agentCapabilities"]["loadSession"], true);
        assert_eq!(
            result["agentCapabilities"]["sessionCapabilities"],
            json!({"list": {}})
        );
        assert!(
            result["agentCapabilities"]["sessionCapabilities"]["list"].is_object(),
            "sessionCapabilities.list must be a SessionListCapabilities object, got {}",
            result["agentCapabilities"]["sessionCapabilities"]["list"]
        );
        assert_eq!(
            result["agentCapabilities"]["promptCapabilities"]["embeddedContext"],
            true
        );
        assert_eq!(result["authMethods"][0]["type"], "terminal");
        assert_eq!(
            result["authMethods"][0]["args"],
            json!(["auth", "set", "--provider", "deepseek"])
        );
    }

    #[test]
    fn initialize_stays_inside_the_official_capability_shape() {
        let result = to_wire(&initialize_result(&Config::default(), false)).unwrap();

        assert!(result["agentCapabilities"].get("modelSelection").is_none());
        assert_eq!(result["authMethods"], json!([]));
        assert_eq!(result["agentCapabilities"]["auth"], json!({}));
    }

    #[test]
    fn initialize_advertises_configured_plugins_and_mcp_but_not_client_servers() {
        let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(true);
        let mut config = Config::default();
        config.set_feature("extension_host", true).unwrap();
        config.set_feature("mcp", true).unwrap();
        let result = to_wire(&initialize_result(&config, false)).unwrap();
        let meta = &result["agentCapabilities"]["_meta"]["codewhale"];

        assert_eq!(meta["plugins"], true);
        assert_eq!(meta["configuredMcpServers"], true);
        assert_eq!(meta["clientMcpServers"], false);
        assert_eq!(
            result["agentCapabilities"]["mcpCapabilities"]["http"],
            false
        );

        config.set_feature("extension_host", false).unwrap();
        config.set_feature("mcp", false).unwrap();
        let off = to_wire(&initialize_result(&config, false)).unwrap();
        assert_eq!(
            off["agentCapabilities"]["_meta"]["codewhale"]["plugins"],
            false
        );
        assert_eq!(
            off["agentCapabilities"]["_meta"]["codewhale"]["configuredMcpServers"],
            false
        );
    }

    #[test]
    fn extract_prompt_text_accepts_text_and_resource_blocks() {
        let prompt = json!([
            { "type": "text", "text": "Review this file" },
            {
                "type": "resource",
                "resource": {
                    "uri": "file:///tmp/app.rs",
                    "mimeType": "text/rust",
                    "text": "fn main() {}"
                }
            },
            { "type": "resource_link", "uri": "file:///tmp/lib.rs" }
        ]);

        let text = extract_prompt_text(Some(&prompt)).expect("prompt text");

        assert!(text.contains("Review this file"));
        assert!(text.contains("fn main() {}"));
        assert!(text.contains("@file:///tmp/lib.rs"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_connection_echoes_ids_and_answers_only_requests() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        let (_, frames) = rig
            .drive(async |editor| {
                for info in [json!({}), json!({"clientInfo":{"name":"zed","version":"1"}})] {
                    let mut params = json!({"protocolVersion":1,"clientCapabilities":{}});
                    params.as_object_mut().unwrap().extend(info.as_object().unwrap().clone());
                    let (reply, _) = editor.request("initialize", params).await?;
                    assert!(reply["id"].is_number(), "{reply}");
                }
                editor
                    .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":{"requestId":999}}))
                    .await?;
                editor
                    .send(json!({"jsonrpc":"2.0","method":"session/not_a_notification","params":{}}))
                    .await?;
                editor.send_raw("not json").await?;
                let parse = editor.recv().await?;
                assert_eq!(parse["id"], Value::Null);
                assert_eq!(parse["error"]["code"], -32700);
                let (unknown, _) = editor.request("session/resume", json!({})).await?;
                assert_eq!(unknown["error"]["code"], -32601);
                Ok(())
            })
            .await?;
        assert!(
            frames
                .iter()
                .all(|frame| frame["id"].is_number() || frame["id"].is_null()),
            "{frames:?}"
        );
        assert_eq!(
            frames.iter().filter(|frame| frame["id"].is_null()).count(),
            1,
            "a notification is never answered: {frames:?}"
        );
        rig.close().await;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_oversize_line_is_refused_and_the_connection_continues() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        let (_, _) = rig
            .drive(async |editor| {
                editor.send_raw(&"a".repeat(MAX_FRAME_BYTES + 16)).await?;
                let refused = editor.recv().await?;
                assert_eq!(refused["id"], Value::Null);
                assert_eq!(refused["error"]["code"], -32700);
                let (initialized, _) = editor
                    .request("initialize", json!({"protocolVersion":1}))
                    .await?;
                assert_eq!(initialized["result"]["protocolVersion"], 1);
                Ok(())
            })
            .await?;
        rig.close().await;
        Ok(())
    }

    #[test]
    fn an_oversize_response_becomes_an_error_and_anything_else_ends_the_connection() {
        let big = "x".repeat(MAX_FRAME_BYTES);
        let response = json!({"jsonrpc":"2.0","id":7,"result":{"big":big}}).to_string();
        let replaced: Value =
            serde_json::from_str(&connection::bound_outgoing(response).unwrap()).unwrap();
        assert_eq!(replaced["id"], 7);
        assert_eq!(replaced["error"]["code"], -32603);
        let update =
            json!({"jsonrpc":"2.0","method":"session/update","params":{"big":big}}).to_string();
        let error = connection::bound_outgoing(update).unwrap_err();
        assert!(crate::stdio::is_frame_refusal(&error));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_session_cap_evicts_the_oldest_binding() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        for index in 0..=MAX_ACP_SESSIONS {
            rig.server
                .remember(format!("session-{index}"), format!("thread-{index}"));
        }
        assert_eq!(rig.server.sessions.len(), MAX_ACP_SESSIONS);
        assert!(!rig.server.sessions.contains_key("session-0"));
        assert!(
            rig.server
                .sessions
                .contains_key(&format!("session-{MAX_ACP_SESSIONS}"))
        );
        rig.close().await;
        Ok(())
    }

    #[test]
    fn tool_update_emits_typed_acp_image_content() {
        let call = PendingToolCall {
            execution_id: uuid::Uuid::new_v4().to_string(),
            name: "read".to_string(),
            input: json!({"path": "shot.png"}),
        };
        let update = to_wire(&tool_call_update(
            &call,
            acp::ToolCallStatus::Completed,
            Some("screenshot captured"),
            &[codewhale_tools::ToolResultContentBlock::Image {
                mime_type: "image/png".to_string(),
                data: "QUJD".to_string(),
            }],
        ))
        .expect("ACP update");
        let content = update["content"].as_array().expect("ACP content blocks");
        assert_eq!(content[0]["content"]["type"], "text");
        assert_eq!(content[1]["content"]["type"], "image");
        assert_eq!(content[1]["content"]["mimeType"], "image/png");
        assert_eq!(content[1]["content"]["data"], "QUJD");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_session_returns_durable_bare_uuid_without_provider_or_history() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        for servers in [
            json!([{"name":"fixture","command":"must-not-execute","args":[],"env":[]}]),
            json!({}),
            Value::Null,
        ] {
            let error = rig
                .server
                .new_session(json!({"mcpServers":servers}))
                .await
                .unwrap_err();
            assert_eq!(error.code, -32602);
            assert!(rig.server.sessions.is_empty());
            assert!(rig.server.insertion_order.is_empty());
        }
        let id = rig.new_session().await;
        uuid::Uuid::parse_str(&id)?;
        let store = codewhale_engine::session_manager::SessionManager::new(
            rig.server.sessions_dir.clone(),
        )?;
        let saved = store.load_session(&id)?;
        assert!(saved.messages.is_empty());
        assert_eq!(
            saved.metadata.runtime_store.as_ref(),
            Some(&rig.server.runtime.session_store_binding())
        );
        let thread = rig.server.sessions[&id].thread_id.clone();
        assert_eq!(
            rig.server
                .runtime
                .get_thread(&thread)
                .await?
                .session_id
                .as_deref(),
            Some(id.as_str())
        );
        assert_eq!(rig.mock.call_count(), 0);
        for requested in [id.as_str(), &id[..8]] {
            let error = rig.server.load_session(json!({"sessionId":requested,"mcpServers":[{"name":"fixture","command":"must-not-execute","args":[],"env":[]}]})).await.unwrap_err();
            assert_eq!(error.code, -32602);
            assert_eq!(rig.server.sessions.len(), 1);
            assert_eq!(rig.mock.call_count(), 0);
        }
        let loaded = rig
            .server
            .load_session(json!({"sessionId":&id[..8]}))
            .await
            .unwrap();
        assert_eq!(loaded["sessionId"], id);
        assert_eq!(rig.server.sessions.len(), 1);
        assert_eq!(rig.server.insertion_order.len(), 1);
        assert!(
            rig.server
                .load_session(json!({"sessionId":uuid::Uuid::new_v4().to_string()}))
                .await
                .is_err()
        );
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn canonical_sessions_keep_independent_history_and_scoped_configuration() -> Result<()> {
        let mut rig = Rig::new(
            fixture_config(),
            vec![
                canned::simple_text_turn("A answer"),
                canned::simple_text_turn("B answer"),
            ],
        )?;
        let a = rig.new_session().await;
        let b = rig.new_session().await;
        rig.server
            .set_session_config(json!({"sessionId":a,"configId":"mode","value":"plan"}))
            .await
            .unwrap();
        assert_eq!(
            rig.server.session_configuration(&a).await.unwrap()["modes"]["currentModeId"],
            "plan"
        );
        assert_eq!(
            rig.server.session_configuration(&b).await.unwrap()["modes"]["currentModeId"],
            "agent"
        );
        assert!(
            rig.server
                .set_session_config(
                    json!({"sessionId":a,"configId":"permission","value":"full-access"})
                )
                .await
                .is_err()
        );
        assert!(
            rig.server
                .set_session_config(json!({"sessionId":a,"configId":"mode","value":"fabricated"}))
                .await
                .is_err()
        );
        assert_eq!(rig.prompt(&a, "A prompt").await?.0, "end_turn");
        assert_eq!(rig.prompt(&b, "B prompt").await?.0, "end_turn");
        let ah = serde_json::to_string(&rig.history(&a).await)?;
        let bh = serde_json::to_string(&rig.history(&b).await)?;
        assert!(ah.contains("A prompt") && ah.contains("A answer") && !ah.contains("B prompt"));
        assert!(bh.contains("B prompt") && bh.contains("B answer") && !bh.contains("A prompt"));
        for (session, prompt, answer) in
            [(&a, "A prompt", "A answer"), (&b, "B prompt", "B answer")]
        {
            let updates = replayed(rig.server.history_updates(session).await?);
            let text: Vec<_> = updates
                .iter()
                .map(|update| {
                    (
                        update["params"]["update"]["sessionUpdate"]
                            .as_str()
                            .unwrap(),
                        update["params"]["update"]["content"]["text"]
                            .as_str()
                            .unwrap(),
                    )
                })
                .collect();
            assert_eq!(
                text,
                vec![
                    ("user_message_chunk", prompt),
                    ("agent_message_chunk", answer)
                ]
            );
        }
        let listed = rig
            .server
            .list_sessions(json!({"cwd":rig.workspace}))
            .unwrap();
        assert_eq!(listed["sessions"].as_array().unwrap().len(), 2);
        let store = codewhale_engine::session_manager::SessionManager::new(
            rig.server.sessions_dir.clone(),
        )?;
        let mut saved = store.load_session(&a)?;
        saved.messages = serde_json::from_value(json!([
            {"role":"user","content":[
                {"type":"text","text":""},
                {"type":"text","text":"alpha"},
                {"type":"text","text":"<turn_meta>hidden middle</turn_meta>"},
                {"type":"text","text":"beta"},
                {"type":"thinking","thinking":"(reasoning omitted)"},
                {"type":"text","text":"gamma"},
                {"type":"thinking","thinking":"private reasoning"},
                {"type":"text","text":"delta"},
                {"type":"tool_use","id":"saved-plan","name":"update_plan","input":{}},
                {"type":"text","text":"epsilon"},
                {"type":"text","text":"[tool_history_repair] hidden repair"},
                {"type":"text","text":"zeta"},
                {"type":"text","text":"<turn_meta>hidden tail</turn_meta>"}
            ]},
            {"role":"assistant","content":[
                {"type":"text","text":"before archive"},
                {"type":"text","text":"<archived_context level=\"1\">summary</archived_context>"},
                {"type":"text","text":"after archive"},
                {"type":"text","text":"<archived_context</archived_context>"},
                {"type":"tool_use","id":"saved-read","name":"read","input":{}},
                {"type":"text","text":"tail"}
            ]},
            {"role":"user","content":[
                {"type":"text","text":"<turn_meta>literal example</turn_meta>"}
            ]},
            {"role":"assistant_interrupted","content":[
                {"type":"text","text":"interrupted answer"}
            ]}
        ]))?;
        saved
            .messages
            .push(codewhale_engine::runtime_handoff::operate_contract_runtime_message());
        saved.messages.extend(
            codewhale_engine::runtime_handoff::project_owned_messages_for_restore(vec![
                codewhale_engine::runtime_handoff::subagent_completion_runtime_message("{}"),
            ]),
        );
        store.save_session(&saved)?;
        let updates = replayed(rig.server.history_updates(&a).await?);
        let text: Vec<_> = updates
            .iter()
            .filter_map(|update| {
                let kind = update["params"]["update"]["sessionUpdate"].as_str()?;
                matches!(kind, "user_message_chunk" | "agent_message_chunk").then(|| {
                    (
                        kind,
                        update["params"]["update"]["content"]["text"]
                            .as_str()
                            .unwrap(),
                    )
                })
            })
            .collect();
        assert_eq!(
            text,
            vec![
                ("user_message_chunk", "alpha\nbeta\ngamma"),
                ("user_message_chunk", "delta"),
                ("user_message_chunk", "epsilon"),
                ("user_message_chunk", "zeta"),
                ("agent_message_chunk", "before archive"),
                (
                    "agent_message_chunk",
                    "after archive\n<archived_context</archived_context>\ntail"
                ),
                (
                    "user_message_chunk",
                    "<turn_meta>literal example</turn_meta>"
                ),
            ]
        );
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn agentic_turn_chains_real_write_read_and_retains_full_core_pairs() -> Result<()> {
        let mut config = fixture_config();
        config.yolo = Some(true);
        let mut rig = Rig::new(
            config,
            vec![
                canned::tool_call_turn(
                    "provider-write",
                    "write",
                    r#"{"path":"receipt.txt","content":"full receipt"}"#,
                ),
                canned::tool_call_turn("provider-read", "read", r#"{"path":"receipt.txt"}"#),
                canned::simple_text_turn("done"),
            ],
        )?;
        let id = rig.new_session().await;
        let (reason, wire) = rig.prompt(&id, "write then read").await?;
        assert_eq!(reason, "end_turn");
        assert_eq!(
            std::fs::read_to_string(rig.workspace.join("receipt.txt"))?,
            "full receipt"
        );
        assert!(
            !wire
                .iter()
                .any(|v| v["method"] == "session/request_permission")
        );
        let statuses: Vec<_> = wire
            .iter()
            .filter_map(|v| {
                v.pointer("/params/update/status")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        (v.pointer("/params/update/sessionUpdate") == Some(&json!("tool_call")))
                            .then_some("pending")
                    })
            })
            .collect();
        assert_eq!(
            statuses,
            vec![
                "pending",
                "in_progress",
                "completed",
                "pending",
                "in_progress",
                "completed"
            ]
        );
        let history = rig.history(&id).await;
        let uses = history
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
            .count();
        let results = history
            .iter()
            .flat_map(|m| &m.content)
            .filter(|b| matches!(b, ContentBlock::ToolResult { .. }))
            .count();
        assert_eq!(uses, 2);
        assert_eq!(results, 2);
        let saved = codewhale_engine::session_manager::SessionManager::new(
            rig.server.sessions_dir.clone(),
        )?
        .load_session(&id)?;
        assert_eq!(
            saved.messages, history,
            "checkpoint is the full Engine snapshot, not display chunks"
        );
        let request = rig.mock.captured_requests();
        assert_eq!(request.len(), 3);
        assert!(
            request.iter().all(
                |r| r.tools.as_ref().is_some_and(|t| t.iter().all(|t| !matches!(
                    t.name.as_str(),
                    "code_execution"
                        | "js_execution"
                        | "execute_tools"
                        | "tool_search"
                        | "task"
                        | "subagent"
                )))
            )
        );
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn agentic_turn_preserves_core_partial_write_when_later_provider_fails() -> Result<()> {
        let mut config = fixture_config();
        config.yolo = Some(true);
        let mut rig = Rig::new(
            config,
            vec![canned::tool_call_turn(
                "partial",
                "write",
                r#"{"path":"partial.txt","content":"completed effect"}"#,
            )],
        )?;
        rig.mock.push_error("fixture terminal provider refusal");
        let id = rig.new_session().await;
        let error = rig.prompt(&id, "write then fail").await.unwrap_err();
        assert!(!error.to_string().is_empty());
        assert_eq!(
            std::fs::read_to_string(rig.workspace.join("partial.txt"))?,
            "completed effect"
        );
        let history = rig.history(&id).await;
        assert!(
            history
                .iter()
                .flat_map(|m| &m.content)
                .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
        );
        let saved = codewhale_engine::session_manager::SessionManager::new(
            rig.server.sessions_dir.clone(),
        )?
        .load_session(&id)?;
        assert_eq!(saved.messages, history);
        let detail = rig
            .server
            .runtime
            .get_thread_detail(&rig.server.sessions[&id].thread_id)
            .await?;
        assert_eq!(
            detail.turns.last().unwrap().status,
            codewhale_engine::runtime_threads::RuntimeTurnStatus::Failed
        );
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn core_prompt_is_stable_after_instruction_write_and_streams_each_text_delta()
    -> Result<()> {
        let mut config = fixture_config();
        config.yolo = Some(true);
        let final_turn = vec![
            canned::message_start("final"),
            canned::text_block_start(0),
            canned::text_delta(0, "hello"),
            canned::text_delta(0, " world"),
            canned::block_stop(0),
            canned::message_delta("end_turn", None),
            canned::message_stop(),
        ];
        let mut rig = Rig::new(
            config,
            vec![
                canned::tool_call_turn("law-read", "read", r#"{"path":"AGENTS.md"}"#),
                canned::tool_call_turn(
                    "law-write",
                    "write",
                    r#"{"path":"AGENTS.md","content":"new-self-authored-law"}"#,
                ),
                final_turn,
            ],
        )?;
        std::fs::write(rig.workspace.join("AGENTS.md"), "initial-project-law")?;
        let id = rig.new_session().await;
        let (reason, wire) = rig.prompt(&id, "update instructions then answer").await?;
        assert_eq!(reason, "end_turn");
        assert_eq!(
            std::fs::read_to_string(rig.workspace.join("AGENTS.md"))?,
            "new-self-authored-law"
        );
        let requests = rig.mock.captured_requests();
        assert_eq!(requests.len(), 3);
        assert_eq!(
            serde_json::to_vec(&requests[0].system)?,
            serde_json::to_vec(&requests[2].system)?
        );
        let prompt = codewhale_engine::prompts::system_prompt_flat_text(
            requests[0].system.as_ref().unwrap(),
        );
        assert!(prompt.contains(codewhale_engine::prompts::text::BASE_PROMPT.trim()));
        assert!(!prompt.contains("initial-project-law"));
        assert!(!prompt.contains("new-self-authored-law"));
        assert!(serde_json::to_string(&requests[0].messages)?.contains("initial-project-law"));
        let chunks: Vec<_> = wire
            .iter()
            .filter(|v| {
                v.pointer("/params/update/sessionUpdate") == Some(&json!("agent_message_chunk"))
            })
            .filter_map(|v| {
                v.pointer("/params/update/content/text")
                    .and_then(Value::as_str)
            })
            .collect();
        // Runtime may coalesce provider frames. ACP must project its durable
        // deltas once each, rather than inventing a second streaming source.
        let events = rig
            .server
            .runtime
            .events_since(&rig.server.sessions[&id].thread_id, None)?;
        let deltas: Vec<_> = events
            .iter()
            .filter(|event| {
                event.event == "item.delta"
                    && event.payload["kind"].as_str() == Some("agent_message")
            })
            .collect();
        let canonical_chunks: Vec<_> = deltas
            .iter()
            .map(|event| event.payload["delta"].as_str().unwrap())
            .collect();
        assert!(!chunks.is_empty());
        assert_eq!(chunks, canonical_chunks);
        assert_eq!(chunks.concat(), "hello world");
        let completed = events
            .iter()
            .find(|event| event.event == "turn.completed")
            .unwrap();
        assert!(deltas.iter().all(|event| event.seq < completed.seq));
        let history = serde_json::to_string(&rig.history(&id).await)?;
        assert!(history.contains("hello world"));
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn core_file_batch_orders_real_results_and_returns_missing_path_to_model() -> Result<()> {
        let mut events = vec![canned::message_start("file-batch")];
        for (index, id, name, input) in [
            (0, "list", "File", r#"{"action":"list","path":"."}"#),
            (1, "read", "read", r#"{"path":"present.txt"}"#),
            (2, "missing", "read", r#"{"path":"missing.txt"}"#),
        ] {
            events.extend([
                canned::tool_use_block_start(index, id, name),
                canned::tool_input_delta(index, input),
                canned::block_stop(index),
            ]);
        }
        events.extend([
            canned::message_delta("tool_use", None),
            canned::message_stop(),
        ]);
        let mut rig = Rig::new(
            fixture_config(),
            vec![events, canned::simple_text_turn("handled file failure")],
        )?;
        std::fs::write(rig.workspace.join("present.txt"), "real-file-marker")?;
        let id = rig.new_session().await;
        let (reason, wire) = rig
            .prompt(&id, "list then read present and missing")
            .await?;
        assert_eq!(reason, "end_turn");
        let statuses: Vec<_> = wire
            .iter()
            .filter_map(|v| {
                v.pointer("/params/update/status")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        (v.pointer("/params/update/sessionUpdate") == Some(&json!("tool_call")))
                            .then_some("pending")
                    })
            })
            .collect();
        assert_eq!(
            statuses,
            vec![
                "pending",
                "pending",
                "pending",
                "in_progress",
                "completed",
                "in_progress",
                "completed",
                "in_progress",
                "failed"
            ]
        );
        let requests = rig.mock.captured_requests();
        assert_eq!(requests.len(), 2);
        let feedback = serde_json::to_string(&requests[1].messages)?;
        assert!(
            feedback.contains("present.txt")
                && feedback.contains("real-file-marker")
                && feedback.contains("missing.txt")
        );
        assert!(
            requests[1]
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .any(|b| matches!(
                    b,
                    ContentBlock::ToolResult {
                        is_error: Some(true),
                        ..
                    }
                ))
        );
        rig.close().await;
        Ok(())
    }
    #[derive(Clone, Copy)]
    enum PermissionReply {
        AllowAfterWrong,
        ForeignCancelAndBusy,
        Reject,
        CancelLateAllow,
        Eof,
    }
    async fn permission_case(reply: PermissionReply) -> Result<()> {
        permission_case_with_base(reply, true).await
    }
    async fn permission_case_with_base(reply: PermissionReply, base_acp: bool) -> Result<()> {
        let mut rig = Rig::with_base(
            fixture_config(),
            vec![
                canned::tool_call_turn(
                    "provider-id-not-authority",
                    "write",
                    r#"{"path":"ask.txt","content":"allowed"}"#,
                ),
                canned::simple_text_turn("settled"),
            ],
            base_acp,
        )?;
        let id = rig.new_session().await;
        let thread = rig.server.sessions[&id].thread_id.clone();
        let manager = Arc::clone(&rig.server.runtime);
        let target = rig.workspace.join("ask.txt");
        let sid = id.clone();
        let target_in = target.clone();
        let (outcome, wire) = rig
            .drive(async |editor| {
                editor.next_id += 1;
                let prompt_id = json!(editor.next_id);
                editor
                    .send(json!({"jsonrpc":"2.0","id":prompt_id,"method":"session/prompt","params":{"sessionId":sid,"prompt":"write under Ask"}}))
                    .await?;
                let v = editor.until_permission().await?;
                assert!(
                    v.pointer("/params/toolCall/content/0/content/text")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
                );
                let detail = manager.get_thread_detail(&thread).await?;
                let pending = detail
                    .pending_approvals
                    .first()
                    .ok_or_else(|| anyhow!("actual pending approval missing"))?;
                assert_eq!(
                    pending.tool_call_id.as_deref(),
                    v.pointer("/params/toolCall/toolCallId")
                        .and_then(Value::as_str)
                );
                assert!(!target_in.exists());
                assert!(
                    !editor
                        .seen
                        .iter()
                        .any(|v| v.pointer("/params/update/status") == Some(&json!("in_progress"))),
                    "no dispatch before the real decision"
                );
                let allow = json!({"outcome":{"outcome":"selected","optionId":"allow-once"}});
                match reply {
                    PermissionReply::AllowAfterWrong => {
                        editor.send(json!({"jsonrpc":"2.0","id":"wrong-id","result":allow})).await?;
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        assert!(!target_in.exists());
                        assert_eq!(manager.get_thread_detail(&thread).await?.pending_approvals.len(), 1);
                        editor.send(json!({"jsonrpc":"2.0","id":v["id"],"result":allow})).await?;
                    }
                    PermissionReply::ForeignCancelAndBusy => {
                        editor.send(json!({"jsonrpc":"2.0","id":41,"method":"session/cancel","params":{"sessionId":"foreign-session"}})).await?;
                        editor.send(json!({"jsonrpc":"2.0","id":42,"method":"session/prompt","params":{"sessionId":sid,"prompt":"must not run"}})).await?;
                        for expected in [41, 42] {
                            let (response, _) = editor.until_response(&json!(expected)).await?;
                            if expected == 41 {
                                assert_eq!(response["result"], Value::Null);
                            } else {
                                assert_eq!(response["error"]["code"], -32603);
                            }
                        }
                        assert!(!target_in.exists());
                        assert_eq!(manager.get_thread_detail(&thread).await?.pending_approvals.len(), 1);
                        editor.send(json!({"jsonrpc":"2.0","id":v["id"],"result":allow})).await?;
                    }
                    PermissionReply::Reject => {
                        editor.send(json!({"jsonrpc":"2.0","id":v["id"],"result":{"outcome":{"outcome":"selected","optionId":"fabricated"}}})).await?;
                    }
                    PermissionReply::Eof => {
                        editor.write.shutdown().await?;
                    }
                    PermissionReply::CancelLateAllow => {
                        editor.send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}})).await?;
                        editor.send(json!({"jsonrpc":"2.0","id":v["id"],"result":allow})).await?;
                    }
                }
                if !matches!(reply, PermissionReply::Eof) {
                    editor.until_response(&prompt_id).await?;
                }
                Ok(prompt_id)
            })
            .await?;
        let response = wire
            .iter()
            .find(|frame| frame["id"] == outcome && frame.get("method").is_none())
            .ok_or_else(|| anyhow!("the prompt was never answered: {wire:?}"))?;
        let result = response["result"]["stopReason"]
            .as_str()
            .ok_or_else(|| anyhow!("{response}"));
        match reply {
            PermissionReply::AllowAfterWrong | PermissionReply::ForeignCancelAndBusy => {
                assert_eq!(result?, "end_turn");
                assert_eq!(std::fs::read_to_string(&target)?, "allowed");
                assert_eq!(
                    wire.iter()
                        .filter(
                            |v| v.pointer("/params/update/status") == Some(&json!("in_progress"))
                        )
                        .count(),
                    1
                );
            }
            PermissionReply::Reject => {
                assert_eq!(result?, "end_turn");
                assert!(!target.exists());
            }
            PermissionReply::CancelLateAllow | PermissionReply::Eof => {
                assert_eq!(result?, "cancelled");
                assert!(!target.exists());
            }
        }
        let saved = codewhale_engine::session_manager::SessionManager::new(
            rig.server.sessions_dir.clone(),
        )?
        .load_session(&id)?;
        assert!(saved.messages.iter().any(|m| m.role == Role::User));
        if !base_acp {
            let thread = rig.server.sessions[&id].thread_id.clone();
            let successor = rig
                .server
                .runtime
                .start_turn(
                    &thread,
                    codewhale_engine::runtime_threads::manager::StartTurnRequest {
                        prompt: "ordinary after cancelled ACP".into(),
                        ..Default::default()
                    },
                )
                .await?;
            wait_real_turn(&rig.server.runtime, &thread, &successor.id).await?;
            let requests = rig.mock.captured_requests();
            assert_eq!(
                requests.len(),
                2,
                "cancelled approval cannot replay the original model request"
            );
            assert_ne!(
                requests[0].tools, requests[1].tools,
                "cancelled ACP narrowing must not leak into its ordinary successor"
            );
            assert!(
                !target.exists(),
                "late allow cannot execute after cancellation"
            );
        }
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn normal_owner_acp_cancel_then_ordinary_successor_keeps_effects_and_profile_scoped()
    -> Result<()> {
        permission_case_with_base(PermissionReply::CancelLateAllow, false).await
    }

    async fn wait_real_turn(
        manager: &RuntimeThreadManager,
        thread: &str,
        turn: &str,
    ) -> Result<codewhale_engine::runtime_threads::TurnRecord> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let detail = manager.get_thread_detail(thread).await?;
                if let Some(record) = detail.turns.iter().find(|record| {
                    record.id == turn
                        && !matches!(
                            record.status,
                            codewhale_engine::runtime_threads::RuntimeTurnStatus::InProgress
                                | codewhale_engine::runtime_threads::RuntimeTurnStatus::Queued
                        )
                }) && manager
                    .events_since_async(thread, None)
                    .await?
                    .iter()
                    .any(|event| {
                        event.event == "turn.completed" && event.turn_id.as_deref() == Some(turn)
                    })
                {
                    return Ok::<_, anyhow::Error>(record.clone());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await?
    }

    #[tokio::test(flavor = "current_thread")]
    async fn real_normal_engine_acp_operation_replay_is_profile_bound_and_successor_is_ordinary()
    -> Result<()> {
        let mut rig = Rig::with_base(
            fixture_config(),
            vec![canned::simple_text_turn("ACP initial")],
            false,
        )?;
        let session = rig.new_session().await;
        let thread = rig.server.sessions[&session].thread_id.clone();
        let request = codewhale_engine::runtime_threads::manager::StartTurnRequest {
            prompt: "same semantic request".into(),
            operation_key: Some("acp-operation".into()),
            ..Default::default()
        };
        let first = rig
            .server
            .runtime
            .start_acp_turn(&thread, request.clone())
            .await?;
        let replay = rig
            .server
            .runtime
            .start_acp_turn(&thread, request.clone())
            .await?;
        assert_eq!(replay.id, first.id);
        assert!(
            rig.server
                .runtime
                .start_turn(&thread, request.clone())
                .await
                .is_err(),
            "ordinary admission cannot redeem an ACP operation binding"
        );
        assert_eq!(
            wait_real_turn(&rig.server.runtime, &thread, &first.id)
                .await?
                .status,
            codewhale_engine::runtime_threads::RuntimeTurnStatus::Completed
        );
        assert_eq!(
            rig.mock.call_count(),
            1,
            "exact replay executes the Engine once"
        );
        rig.mock
            .push_turn(canned::simple_text_turn("ordinary initial"));
        let ordinary_request = codewhale_engine::runtime_threads::manager::StartTurnRequest {
            operation_key: Some("ordinary-operation".into()),
            ..request
        };
        let ordinary = rig
            .server
            .runtime
            .start_turn(&thread, ordinary_request.clone())
            .await?;
        assert!(
            rig.server
                .runtime
                .start_acp_turn(&thread, ordinary_request.clone())
                .await
                .is_err(),
            "ACP admission cannot redeem an ordinary historical binding"
        );
        assert_eq!(
            wait_real_turn(&rig.server.runtime, &thread, &ordinary.id)
                .await?
                .status,
            codewhale_engine::runtime_threads::RuntimeTurnStatus::Completed
        );
        assert_eq!(
            rig.server
                .runtime
                .start_turn(&thread, ordinary_request)
                .await?
                .id,
            ordinary.id
        );
        assert_eq!(rig.mock.call_count(), 2);
        assert_eq!(
            rig.server
                .runtime
                .get_thread_detail(&thread)
                .await?
                .turns
                .len(),
            2
        );
        let requests = rig.mock.captured_requests();
        assert_ne!(
            requests[0].tools, requests[1].tools,
            "ordinary successor must retain its own catalog"
        );
        rig.close().await;
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn read_and_config_requests_are_served_while_a_turn_awaits_approval() -> Result<()> {
        let mut rig = Rig::new(
            fixture_config(),
            vec![
                canned::tool_call_turn("w", "write", r#"{"path":"busy.txt","content":"x"}"#),
                canned::simple_text_turn("done"),
            ],
        )?;
        let id = rig.new_session().await;
        let (_, wire) = rig
            .drive(async |editor| {
                editor.next_id += 1;
                let prompt_id = json!(editor.next_id);
                editor
                    .send(json!({"jsonrpc":"2.0","id":prompt_id,"method":"session/prompt","params":{"sessionId":id,"prompt":"write"}}))
                    .await?;
                let permission = editor.until_permission().await?;
                let (listed, _) = editor.request("session/list", json!({})).await?;
                assert!(listed["result"]["sessions"].is_array(), "{listed}");
                let (model, _) = editor
                    .request("session/set_config_option", json!({"sessionId":id,"configId":"model","value":"fixture-model"}))
                    .await?;
                assert!(model["result"]["configOptions"].is_array(), "{model}");
                let (created, _) = editor.request("session/new", json!({})).await?;
                assert_eq!(created["error"]["code"], -32603);
                editor
                    .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":id}}))
                    .await?;
                editor
                    .send(json!({"jsonrpc":"2.0","id":permission["id"],"result":{"outcome":{"outcome":"cancelled"}}}))
                    .await?;
                let (done, _) = editor.until_response(&prompt_id).await?;
                assert_eq!(done["result"]["stopReason"], "cancelled");
                Ok(())
            })
            .await?;
        assert!(!rig.workspace.join("busy.txt").exists());
        assert!(!wire.is_empty());
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_permission_allow_after_wrong_id_executes_once() -> Result<()> {
        permission_case(PermissionReply::AllowAfterWrong).await
    }
    #[tokio::test(flavor = "current_thread")]
    async fn foreign_cancel_and_busy_request_leave_the_same_core_approval_pending() -> Result<()> {
        permission_case(PermissionReply::ForeignCancelAndBusy).await
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_permission_invalid_option_refuses_without_effect() -> Result<()> {
        permission_case(PermissionReply::Reject).await
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_permission_cancel_ignores_late_allow_and_never_runs_tool() -> Result<()> {
        permission_case(PermissionReply::CancelLateAllow).await
    }
    #[tokio::test(flavor = "current_thread")]
    async fn plan_and_operator_floor_cannot_be_relaxed_by_full_access_or_client() -> Result<()> {
        let mut config = fixture_config();
        config.yolo = Some(true);
        config.sandbox_mode = Some("read-only".into());
        let mut rig = Rig::new(
            config,
            vec![
                canned::tool_call_turn(
                    "write-denied",
                    "write",
                    r#"{"path":"denied.txt","content":"forbidden"}"#,
                ),
                canned::simple_text_turn("read only"),
            ],
        )?;
        let id = rig.new_session().await;
        assert!(
            rig.server
                .set_session_config(json!({"sessionId":id,"configId":"mode","value":"agent"}))
                .await
                .is_err()
        );
        let view = rig.server.session_configuration(&id).await.unwrap();
        assert_eq!(view["modes"]["currentModeId"], "plan");
        assert_eq!(
            view["configOptions"][2]["options"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let (_, wire) = rig.prompt(&id, "try write").await?;
        assert!(!rig.workspace.join("denied.txt").exists());
        assert!(
            !wire
                .iter()
                .any(|v| v["method"] == "session/request_permission")
        );
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_full_access_keeps_repo_law_without_permission_modal() -> Result<()> {
        let mut config = fixture_config();
        config.approval_policy = Some("full-access".into());
        config.allow_shell = Some(true);
        let mut rig = Rig::new(
            config,
            vec![
                canned::tool_call_turn(
                    "ordinary-write",
                    "write",
                    r#"{"path":"ordinary.txt","content":"allowed"}"#,
                ),
                // ACP reviews even foreground bash as Headless. Full Access
                // skips the deterministic floor, so the harmless fixture
                // writes its own sentinel instead of holding.
                canned::tool_call_turn(
                    "floor-run",
                    "bash",
                    r#"{"command":"echo \"rm -rf /\" > floor-sentinel.txt"}"#,
                ),
                canned::tool_call_turn(
                    "law-held",
                    "write",
                    r#"{"path":"CHANGELOG.md","content":"must not be written"}"#,
                ),
                canned::simple_text_turn("ordinary write completed; protected changes refused"),
            ],
        )?;
        std::fs::write(rig.workspace.join("floor-sentinel.txt"), "guarded\n")?;
        std::fs::create_dir_all(rig.workspace.join(".codewhale"))?;
        std::fs::write(
            rig.workspace.join(".codewhale/constitution.json"),
            r#"{"protected_invariants":[{"text":"Release notes need human review","paths":["CHANGELOG.md"]}]}"#,
        )?;
        let config_path = rig._dir.path().join("operator.toml");
        std::fs::write(
            &config_path,
            "allow_shell = true\napproval_policy = \"full-access\"\n",
        )?;
        rig.server.config_path = Some(config_path);
        rig.server
            .handle_request(
                "initialize",
                json!({"protocolVersion":1,"clientCapabilities":{"terminal":true}}),
            )
            .await
            .unwrap();
        let session = rig.new_session().await;
        let (reason, wire) = rig
            .prompt(&session, "write ordinary and protected files")
            .await?;
        assert_eq!(reason, "end_turn");
        assert_eq!(
            std::fs::read_to_string(rig.workspace.join("ordinary.txt"))?,
            "allowed"
        );
        let sentinel = rig.workspace.join("floor-sentinel.txt");
        for _ in 0..180 {
            if codewhale_engine::test_support::read_shell_sentinel(&sentinel) == "rm -rf /" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        assert_eq!(
            codewhale_engine::test_support::read_shell_sentinel(&sentinel),
            "rm -rf /"
        );
        assert!(!rig.workspace.join("CHANGELOG.md").exists());
        assert!(
            !wire
                .iter()
                .any(|frame| frame["method"] == "session/request_permission")
        );
        let statuses: Vec<_> = wire
            .iter()
            .filter_map(|frame| {
                frame
                    .pointer("/params/update/status")
                    .and_then(Value::as_str)
            })
            .collect();
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == "in_progress")
                .count(),
            2
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == "completed")
                .count(),
            2
        );
        assert_eq!(
            statuses
                .iter()
                .filter(|status| **status == "failed")
                .count(),
            1
        );
        let requests = rig.mock.captured_requests();
        assert_eq!(requests.len(), 4, "no guardian or provider replay");
        assert!(
            requests[0]
                .tools
                .iter()
                .flatten()
                .any(|tool| tool.name == "bash"),
            "the floor fixture must exercise an admitted shell tool"
        );
        let history = rig.history(&session).await;
        for (request, call, error, refusal_reason) in [
            (1, "ordinary-write", false, None),
            (2, "floor-run", false, None),
            (3, "law-held", true, Some("Release notes need human review")),
        ] {
            let results: Vec<_> = history
                .iter()
                .flat_map(|message| &message.content)
                .filter_map(|block| match block {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        ..
                    } if tool_use_id == call => Some((content.as_str(), *is_error)),
                    _ => None,
                })
                .collect();
            assert_eq!(results.len(), 1, "one paired result for {call}");
            assert_eq!(results[0].1.unwrap_or(false), error, "{call}: {results:?}");
            assert!(!results[0].0.is_empty(), "{call}");
            let provider_results: Vec<_> = requests[request]
                .messages
                .iter()
                .flat_map(|message| &message.content)
                .filter_map(|block| match block {
                    ContentBlock::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                        ..
                    } if tool_use_id == call => Some((content.as_str(), *is_error)),
                    _ => None,
                })
                .collect();
            assert_eq!(
                provider_results, results,
                "the next provider request must receive the exact paired Core result for {call}"
            );
            if let Some(reason) = refusal_reason {
                assert!(
                    provider_results[0].0.contains(reason),
                    "{call} must report its actual policy hold: {provider_results:?}"
                );
            }
        }
        let saved = codewhale_engine::session_manager::SessionManager::new(
            rig.server.sessions_dir.clone(),
        )?
        .load_session(&session)?;
        assert_eq!(
            saved.messages, history,
            "ACP persists the full paired Core history"
        );
        let detail = rig
            .server
            .runtime
            .get_thread_detail(&rig.server.sessions[&session].thread_id)
            .await?;
        assert!(detail.pending_approvals.is_empty());
        assert_eq!(detail.turns.len(), 1);
        assert_eq!(
            detail.turns[0].status,
            codewhale_engine::runtime_threads::RuntimeTurnStatus::Completed
        );
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn foreign_canonical_store_refuses_before_allocating_or_copying_history() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        let id = rig.new_session().await;
        let store = codewhale_engine::session_manager::SessionManager::new(
            rig.server.sessions_dir.clone(),
        )?;
        let mut saved = store.load_session(&id)?;
        let path = store.save_session(&saved)?;
        let original = std::fs::read(&path)?;
        saved.metadata.runtime_store.as_mut().unwrap().data_dir =
            rig.workspace.join("foreign-owner");
        let write_error = store.save_session(&saved).unwrap_err();
        assert_eq!(write_error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(&path)?, original);
        // A normal writer rejects this binding. Inject a synthetic stale
        // document only into this private fixture to exercise load refusal.
        std::fs::write(&path, serde_json::to_vec_pretty(&saved)?)?;
        rig.server.sessions.clear();
        rig.server.insertion_order.clear();
        let before = rig
            .server
            .runtime
            .list_threads(
                codewhale_engine::runtime_threads::manager::ThreadListFilter::IncludeArchived,
                None,
            )
            .await?
            .len();
        let error = rig
            .server
            .load_session(json!({"sessionId":id}))
            .await
            .unwrap_err();
        assert!(error.message.contains("secure owner attachment"));
        assert_eq!(
            rig.server
                .runtime
                .list_threads(
                    codewhale_engine::runtime_threads::manager::ThreadListFilter::IncludeArchived,
                    None
                )
                .await?
                .len(),
            before
        );
        assert_eq!(rig.mock.call_count(), 0);
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn transport_eof_cancels_actual_turn_and_retains_checkpoint() -> Result<()> {
        permission_case(PermissionReply::Eof).await
    }
    #[tokio::test(flavor = "current_thread")]
    async fn real_image_tool_result_projects_typed_blocks_without_losing_core_history() -> Result<()>
    {
        use base64::Engine as _;
        let mut rig = Rig::new(
            fixture_config(),
            vec![
                canned::tool_call_turn("image", "read", r#"{"path":"shot.png"}"#),
                canned::simple_text_turn("image read"),
            ],
        )?;
        let image = codewhale_engine::image_attach::test_support::runtime_image_fixture(21);
        std::fs::write(
            rig.workspace.join("shot.png"),
            base64::engine::general_purpose::STANDARD.decode(image.data_base64)?,
        )?;
        let id = rig.new_session().await;
        let (_, wire) = rig.prompt(&id, "read image").await?;
        assert!(
            wire.iter()
                .filter_map(|v| v
                    .pointer("/params/update/content")
                    .and_then(Value::as_array))
                .flatten()
                .any(|block| block.pointer("/content/type") == Some(&json!("image")))
        );
        assert!(rig.history(&id).await.iter().flat_map(|m|&m.content).any(|b|matches!(b,ContentBlock::ToolResult{content_blocks:Some(blocks),..} if !blocks.is_empty())));
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn finite_engine_step_budget_is_a_typed_acp_stop_reason() -> Result<()> {
        finite_engine_step_budget_case(true).await
    }
    #[tokio::test(flavor = "current_thread")]
    async fn normal_owner_acp_step_stop_preserves_ordinary_successor() -> Result<()> {
        finite_engine_step_budget_case(false).await
    }
    async fn finite_engine_step_budget_case(base_acp: bool) -> Result<()> {
        let mut config = fixture_config();
        config.tui = Some(codewhale_runtime::config::TuiConfig {
            max_model_steps: Some(1),
            ..Default::default()
        });
        let mut rig = Rig::with_base(
            config,
            vec![
                canned::tool_call_turn("budget", "read", r#"{"path":"one.txt"}"#),
                canned::simple_text_turn("bounded final report"),
            ],
            base_acp,
        )?;
        std::fs::write(rig.workspace.join("one.txt"), "one")?;
        let id = rig.new_session().await;
        assert_eq!(
            rig.prompt(&id, "read until budget").await?.0,
            "max_turn_requests"
        );
        let detail = rig
            .server
            .runtime
            .get_thread_detail(&rig.server.sessions[&id].thread_id)
            .await?;
        assert_eq!(
            detail
                .turns
                .last()
                .unwrap()
                .model_request_diagnostics
                .as_ref()
                .unwrap()
                .stop_reason,
            Some(codewhale_engine::runtime_threads::RuntimeTurnStopReason::StepBudgetExhausted)
        );
        assert_eq!(
            rig.mock.call_count(),
            2,
            "one admitted model step plus existing Core final-report step"
        );
        if !base_acp {
            rig.mock
                .push_turn(canned::simple_text_turn("ordinary after ACP step stop"));
            let thread = rig.server.sessions[&id].thread_id.clone();
            let successor = rig
                .server
                .runtime
                .start_turn(
                    &thread,
                    codewhale_engine::runtime_threads::manager::StartTurnRequest {
                        prompt: "ordinary after step stop".into(),
                        ..Default::default()
                    },
                )
                .await?;
            assert_eq!(
                wait_real_turn(&rig.server.runtime, &thread, &successor.id)
                    .await?
                    .status,
                codewhale_engine::runtime_threads::RuntimeTurnStatus::Completed
            );
            let requests = rig.mock.captured_requests();
            assert_eq!(requests.len(), 3);
            assert_ne!(requests[0].tools, requests[2].tools);
        }
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn invalid_prompt_and_rpc_shapes_refuse_before_core_admission() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        let id = rig.new_session().await;
        let (_, wire) = rig
            .drive(async |editor| {
                for (id, method, params) in [
                    (3, "session/prompt", json!({"sessionId":id,"prompt":[]})),
                    (
                        4,
                        "session/prompt",
                        json!({"sessionId":"unknown","prompt":"x"}),
                    ),
                    (5, "fabricated", json!({})),
                    (6, "shutdown", json!({})),
                ] {
                    editor
                        .send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                        .await?;
                }
                Ok(())
            })
            .await?;
        assert_eq!(wire[0]["error"]["code"], -32602);
        assert_eq!(wire[1]["error"]["code"], -32602);
        assert_eq!(wire[2]["error"]["code"], -32601);
        assert_eq!(wire[0]["id"], 3);
        assert_eq!(rig.mock.call_count(), 0);
        assert!(
            rig.server
                .runtime
                .get_thread_detail(&rig.server.sessions[&id].thread_id)
                .await?
                .turns
                .is_empty()
        );
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn canonical_thread_shell_ceiling_requires_operator_and_client_opt_in() -> Result<()> {
        for (operator, client, expected) in [
            (true, false, false),
            (false, true, false),
            (true, true, true),
        ] {
            let mut config = fixture_config();
            config.allow_shell = Some(operator);
            let operator_config = format!("allow_shell = {operator}\n");
            let mut rig = Rig::new(config, vec![])?;
            let config_path = rig._dir.path().join("operator.toml");
            std::fs::write(&config_path, operator_config)?;
            rig.server.config_path = Some(config_path);
            rig.server.client_supports_terminal = client;
            let id = rig.new_session().await;
            let thread = rig
                .server
                .runtime
                .get_thread(&rig.server.sessions[&id].thread_id)
                .await?;
            assert_eq!(thread.allow_shell, expected);
            assert_eq!(rig.mock.call_count(), 0);
            rig.close().await;
        }
        let mut config = fixture_config();
        config.allow_shell = Some(true);
        let mut rig = Rig::new(config, vec![])?;
        rig.server.client_supports_terminal = true;
        let error = rig.server.new_session(json!({})).await.unwrap_err();
        assert!(error.message.contains("unresolved configuration source"));
        assert!(rig.server.sessions.is_empty());
        assert!(
            rig.server
                .runtime
                .list_threads(
                    codewhale_engine::runtime_threads::manager::ThreadListFilter::IncludeArchived,
                    None,
                )
                .await?
                .is_empty()
        );
        assert_eq!(rig.mock.call_count(), 0);
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn model_discovery_and_selection_keep_custom_provider_identity_and_refuse_invalid_values()
    -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        assert_eq!(rig.server.current_model()["provider"], "acp-fixture");
        assert_eq!(rig.server.current_model()["model"], "fixture-model");
        assert!(
            rig.server.list_providers()["providers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["id"] == "acp-fixture")
        );
        assert!(
            rig.server
                .select_model(json!({"provider":"not-a-provider","model":"x"}))
                .is_err()
        );
        assert!(
            rig.server
                .select_model(json!({"provider":"acp-fixture"}))
                .is_err()
        );
        let selected = rig
            .server
            .select_model(json!({"provider":"acp-fixture","model":"fixture-model"}))
            .unwrap();
        assert_eq!(selected["provider"], "acp-fixture");
        assert_eq!(rig.mock.call_count(), 0);
        rig.close().await;
        Ok(())
    }
    struct FailingInput(tokio::sync::mpsc::UnboundedReceiver<std::io::Result<Vec<u8>>>);
    impl tokio::io::AsyncRead for FailingInput {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            match self.get_mut().0.poll_recv(cx) {
                std::task::Poll::Ready(Some(Ok(bytes))) => {
                    buf.put_slice(&bytes);
                    std::task::Poll::Ready(Ok(()))
                }
                std::task::Poll::Ready(Some(Err(error))) => std::task::Poll::Ready(Err(error)),
                std::task::Poll::Ready(None) => std::task::Poll::Ready(Ok(())),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        }
    }
    struct FailingOutput {
        fail_on: Option<&'static str>,
    }
    impl AsyncWrite for FailingOutput {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if self
                .fail_on
                .is_some_and(|marker| String::from_utf8_lossy(bytes).contains(marker))
            {
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "the editor stopped reading",
                )));
            }
            std::task::Poll::Ready(Ok(bytes.len()))
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }
    #[derive(Clone, Copy, PartialEq)]
    enum Failure {
        OutputAtPermission,
        InputError,
        OutputAfterWrite,
    }
    async fn transport_failure_case(mode: Failure) -> Result<()> {
        let mut config = fixture_config();
        config.yolo = (mode == Failure::OutputAfterWrite).then_some(true);
        let mut rig = Rig::new(
            config,
            vec![
                canned::tool_call_turn(
                    "review-probe",
                    "write",
                    r#"{"path":"writer-failure.txt","content":"completed"}"#,
                ),
                canned::simple_text_turn("settled"),
            ],
        )?;
        let id = rig.new_session().await;
        let thread = rig.server.sessions[&id].thread_id.clone();
        let manager = Arc::clone(&rig.server.runtime);
        let (lines, input) = tokio::sync::mpsc::unbounded_channel::<std::io::Result<Vec<u8>>>();
        let mut prompt = serde_json::to_vec(
            &json!({"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"sessionId":id,"prompt":"write under Ask"}}),
        )?;
        prompt.push(b'\n');
        lines
            .send(Ok(prompt))
            .map_err(|_| anyhow!("review probe input closed early"))?;
        let placeholder = rig.placeholder();
        let server = std::mem::replace(&mut rig.server, placeholder);
        let serving = tokio::time::timeout(
            Duration::from_secs(60),
            super::connection::serve_connection(
                server,
                Box::new(BufReader::new(FailingInput(input))),
                Box::new(FailingOutput {
                    fail_on: match mode {
                        Failure::OutputAtPermission => Some("session/request_permission"),
                        Failure::OutputAfterWrite => Some("agent_message_chunk"),
                        Failure::InputError => None,
                    },
                }),
            ),
        );
        let breaking = async {
            if mode == Failure::InputError {
                let waited = tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        if !manager
                            .get_thread_detail(&thread)
                            .await?
                            .pending_approvals
                            .is_empty()
                        {
                            return Ok::<_, anyhow::Error>(());
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await;
                let _ = lines.send(Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "the editor's socket was reset",
                )));
                waited??;
            }
            Ok::<_, anyhow::Error>(())
        };
        let (served, broke) = tokio::join!(serving, breaking);
        broke?;
        let served = served.map_err(|_| anyhow!("serve did not return in 60s"))?;
        let ended_with_error = served.is_err();
        drop(served);
        let terminal = tokio::time::timeout(Duration::from_secs(40), async {
            loop {
                let detail = manager.get_thread_detail(&thread).await?;
                if detail.turns.last().is_some_and(|turn| {
                    !matches!(
                        turn.status,
                        codewhale_engine::runtime_threads::RuntimeTurnStatus::Queued
                            | codewhale_engine::runtime_threads::RuntimeTurnStatus::InProgress
                    )
                }) {
                    return Ok::<_, anyhow::Error>(format!(
                        "{:?}",
                        detail.turns.last().map(|turn| &turn.status)
                    ));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .map_err(|_| anyhow!("the Core turn never settled"))??;
        tokio::time::sleep(Duration::from_millis(500)).await;
        let saved_user_messages = codewhale_engine::session_manager::SessionManager::new(
            rig.server.sessions_dir.clone(),
        )?
        .load_session(&id)
        .map(|saved| {
            saved
                .messages
                .iter()
                .filter(|m| m.role == Role::User)
                .count()
        });
        drop(lines);
        rig.close().await;
        assert!(
            ended_with_error,
            "a failed transport must end serve with an error"
        );
        assert!(
            matches!(saved_user_messages, Ok(count) if count > 0),
            "no checkpoint after a mid-turn transport failure ({}); Core turn ended {terminal}; saved user messages: {saved_user_messages:?}",
            mode as u8
        );
        if mode == Failure::OutputAfterWrite {
            assert_eq!(
                std::fs::read_to_string(rig.workspace.join("writer-failure.txt"))?,
                "completed"
            );
        }
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_output_write_mid_turn_keeps_the_checkpoint() -> Result<()> {
        transport_failure_case(Failure::OutputAtPermission).await
    }
    #[tokio::test(flavor = "current_thread")]
    async fn a_failed_input_read_mid_turn_keeps_the_checkpoint() -> Result<()> {
        transport_failure_case(Failure::InputError).await
    }
    #[tokio::test(flavor = "current_thread")]
    async fn failed_transport_after_completed_write_retains_actual_core_terminal_and_checkpoint()
    -> Result<()> {
        transport_failure_case(Failure::OutputAfterWrite).await
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_oversize_update_fails_the_connection_without_dropping_the_turn() -> Result<()> {
        use futures_util::SinkExt;
        let rig = Rig::new(fixture_config(), vec![])?;
        let shared = Arc::new(super::connection::Shared::new(rig.placeholder()));
        let (frames, mut queued) = tokio::sync::mpsc::channel(4);
        let mut sink = Box::pin(super::connection::outgoing_lines(
            frames,
            Arc::clone(&shared),
        ));
        let update = json!({"jsonrpc":"2.0","method":"session/update","params":{"big":"x".repeat(MAX_FRAME_BYTES)}})
            .to_string();
        sink.send(update).await?;
        assert!(queued.try_recv().is_err());
        assert!(shared.take_failure().is_some());
        rig.close().await;
        Ok(())
    }
}

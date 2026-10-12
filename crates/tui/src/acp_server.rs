//! ACP stdio projection of the existing Runtime/Engine owner.
//! History, provider requests, tools, approval and cancellation are Core facts.
//! This module keeps only connection configuration, canonical bindings and
//! JSON-RPC correlation. Secure cross-process attachment is not yet qualified.
use crate::config::{Config, ProviderKind};
#[cfg(test)]
use crate::runtime_threads::RuntimeThreadManagerConfig;
use crate::runtime_threads::{CreateThreadRequest, RuntimeThreadManager, UpdateThreadRequest};
use anyhow::{Result, anyhow};
use codewhale_config::AppMode;
use codewhale_execpolicy::ApprovalMode;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(test)]
use tokio::io::BufReader;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};
mod runtime;
const ACP_PROTOCOL_VERSION: u64 = 1;
const MAX_ACP_SESSIONS: usize = 64;
const TOOL_CALL_CONTENT_PREVIEW_CHARS: usize = 4_000;
static NEXT_ACP_PERMISSION_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub async fn run_acp_server(
    config: Config,
    model: String,
    default_cwd: PathBuf,
    plugins: Arc<crate::plugins::PluginDiscoveryContext>,
    config_path: Option<PathBuf>,
    config_profile: Option<String>,
) -> Result<()> {
    crate::runtime_api::run_http_server(
        config,
        default_cwd,
        plugins,
        crate::runtime_api::RuntimeApiOptions {
            port: 0,
            config_path,
            config_profile,
            control_frontend: Some(codewhale_app_server::RuntimeControlFrontend::Acp { model }),
            ..Default::default()
        },
    )
    .await
}

pub(crate) struct CapturedAcpFrontend {
    config: Config,
    model: String,
    cwd: PathBuf,
    manager: Arc<RuntimeThreadManager>,
    sessions_dir: PathBuf,
    config_path: Option<PathBuf>,
    config_profile: Option<String>,
}
pub(crate) fn capture_frontend(
    config: Config,
    model: String,
    cwd: PathBuf,
    manager: Arc<RuntimeThreadManager>,
    sessions_dir: PathBuf,
    config_path: Option<PathBuf>,
    config_profile: Option<String>,
) -> Result<Arc<CapturedAcpFrontend>> {
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
    pub(crate) fn serve(
        &self,
        input: Box<dyn AsyncBufRead + Send + Unpin>,
        mut output: Box<dyn AsyncWrite + Send + Unpin>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let mut server = AcpServer::new(
                self.config.clone(),
                self.model.clone(),
                self.cwd.clone(),
                self.manager.clone(),
                self.sessions_dir.clone(),
                self.config_path.clone(),
                self.config_profile.clone(),
            );
            let mut input = codewhale_app_server::BoundedLines::new(input);
            server.serve(&mut input, &mut output).await
        })
    }
}

#[cfg(test)]
impl codewhale_app_server::RuntimeOwnerFrontend for CapturedAcpFrontend {
    fn validate_selection<'a>(
        &'a self,
        selection: &'a codewhale_app_server::RuntimeOwnerFrontendSelection,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            anyhow::ensure!(
                matches!(
                    selection,
                    codewhale_app_server::RuntimeOwnerFrontendSelection::Acp { .. }
                ),
                "ACP fixture does not capture HTTP services"
            );
            Ok(())
        })
    }
    fn serve(
        &self,
        selection: codewhale_app_server::RuntimeOwnerFrontendSelection,
        _compatibility: codewhale_app_server::AppState,
        input: Box<dyn AsyncBufRead + Send + Unpin>,
        output: Box<dyn AsyncWrite + Send + Unpin>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            anyhow::ensure!(
                matches!(
                    selection,
                    codewhale_app_server::RuntimeOwnerFrontendSelection::Acp { .. }
                ),
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
    response_id_policy: JsonRpcResponseIdPolicy,
}
/// A transport binding, never another conversation or executable registry.
struct AcpSession {
    thread_id: String,
    cursor: u64,
}
struct PendingToolCall {
    execution_id: String,
    name: String,
    input: Value,
}
enum AcpDispatch {
    Response(Value),
    Shutdown,
}
#[derive(Debug)]
struct AcpError {
    code: i32,
    message: String,
}
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
            response_id_policy: JsonRpcResponseIdPolicy::Preserve,
        }
    }
    async fn serve<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(
        &mut self,
        reader: &mut codewhale_app_server::BoundedLines<R>,
        writer: &mut W,
    ) -> Result<()> {
        while let Some(line) = reader.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(error) => {
                    write_jsonrpc_error(writer, None, -32700, format!("invalid json: {error}"))
                        .await?;
                    continue;
                }
            };
            if codewhale_app_server::is_control_input_closed(&message) {
                break;
            }
            let response_id = message
                .get("id")
                .cloned()
                .map(|id| self.response_id_policy.response_id(id));
            if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                write_jsonrpc_error(writer, response_id, -32600, "jsonrpc version must be 2.0")
                    .await?;
                continue;
            }
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                if is_jsonrpc_response(&message) {
                    continue;
                }
                write_jsonrpc_error(writer, response_id, -32600, "missing method").await?;
                continue;
            };
            let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
            if method == "session/prompt" {
                if let Err(error) = self.validate_prompt(&params) {
                    write_jsonrpc_error(writer, response_id, error.code, error.message).await?;
                    continue;
                }
                let result = self.drive_prompt(params, reader, writer).await;
                match result {
                    Ok(reason) => {
                        if let Some(id) = response_id {
                            write_jsonrpc_result(writer, id, json!({"stopReason":reason})).await?;
                        }
                    }
                    Err(error) => {
                        write_jsonrpc_error(
                            writer,
                            response_id,
                            -32603,
                            crate::client::redact_model_bound_text(&error.to_string(), &[]),
                        )
                        .await?
                    }
                }
                continue;
            }
            match self.handle_request(method, params).await {
                Ok(AcpDispatch::Response(result)) => {
                    if let Some(id) = response_id {
                        write_jsonrpc_result(writer, id, result).await?;
                    }
                }
                Ok(AcpDispatch::Shutdown) => {
                    if let Some(id) = response_id {
                        write_jsonrpc_result(writer, id, json!(null)).await?;
                    }
                    break;
                }
                Err(error) => {
                    write_jsonrpc_error(writer, response_id, error.code, error.message).await?
                }
            }
        }
        Ok(())
    }
    // `session/prompt` is handled in the main loop (it needs to run concurrently
    // with the reader for cancellation); every other method is request/response.
    async fn handle_request(
        &mut self,
        method: &str,
        params: Value,
    ) -> std::result::Result<AcpDispatch, AcpError> {
        match method {
            "initialize" => {
                if let Some(terminal) = params
                    .pointer("/clientCapabilities/terminal")
                    .and_then(Value::as_bool)
                {
                    self.client_supports_terminal = terminal;
                }
                self.response_id_policy = JsonRpcResponseIdPolicy::from_initialize_params(&params);
                Ok(AcpDispatch::Response(initialize_result(
                    params.get("protocolVersion").and_then(Value::as_u64),
                    &self.config,
                )))
            }
            "session/new" => Ok(AcpDispatch::Response(self.new_session(params).await?)),
            "session/list" => Ok(AcpDispatch::Response(self.list_sessions(params)?)),
            "session/load" => Ok(AcpDispatch::Response(self.load_session(params).await?)),
            "session/listProviders" => Ok(AcpDispatch::Response(self.list_providers())),
            "session/currentModel" => Ok(AcpDispatch::Response(self.current_model())),
            "session/selectModel" => Ok(AcpDispatch::Response(self.select_model(params)?)),
            "session/set_config_option" => Ok(AcpDispatch::Response(
                self.set_session_config(params).await?,
            )),
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
                Ok(AcpDispatch::Response(json!({})))
            }
            // A cancel that arrives with no prompt in flight is an idempotent
            // no-op (the in-flight case is handled by the prompt driver).
            "session/cancel" => Ok(AcpDispatch::Response(json!(null))),
            "shutdown" => Ok(AcpDispatch::Shutdown),
            _ => Err(AcpError::method_not_found(method)),
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
        let prompt = extract_prompt_text(params.get("prompt"))
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| AcpError::invalid_params("prompt must include text content"))?;
        Ok((id.to_string(), prompt))
    }
    fn shell_allowed(&self) -> bool {
        self.client_supports_terminal && self.config.allow_shell()
    }
    fn remember(&mut self, id: String, thread_id: String, cursor: u64) {
        if self.sessions.contains_key(&id) {
            return;
        }
        if self.sessions.len() >= MAX_ACP_SESSIONS
            && let Some(old) = self.insertion_order.pop_front()
        {
            self.sessions.remove(&old);
        }
        self.insertion_order.push_back(id.clone());
        self.sessions.insert(id, AcpSession { thread_id, cursor });
    }
    async fn new_session(&mut self, params: Value) -> std::result::Result<Value, AcpError> {
        let cwd = params
            .get("cwd")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| self.default_cwd.clone());
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
        let cursor = self
            .runtime
            .get_thread_detail(&thread.id)
            .await
            .map_err(|error| AcpError::internal(error.to_string()))?
            .latest_seq;
        self.remember(id.clone(), thread.id, cursor);
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
        let sessions = crate::session_manager::SessionManager::new(self.sessions_dir.clone())
            .ok()
            .and_then(|manager| manager.list_sessions().ok())
            .unwrap_or_default();
        let sessions: Vec<Value> = sessions
            .into_iter()
            .filter(|meta| {
                cwd.as_ref().is_none_or(|cwd| {
                    crate::session_manager::paths_equivalent(&meta.workspace, cwd)
                })
            })
            .map(|meta| {
                json!({
                    "sessionId": meta.id,
                    "title": meta.title,
                    "cwd": meta.workspace.to_string_lossy(),
                    "createdAt": meta.created_at.to_rfc3339(),
                    "updatedAt": meta.updated_at.to_rfc3339(),
                    "messageCount": meta.message_count,
                })
            })
            .collect();
        Ok(json!({ "sessions": sessions }))
    }

    async fn load_session(&mut self, params: Value) -> std::result::Result<Value, AcpError> {
        let requested = params
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| AcpError::invalid_params("session/load requires sessionId"))?;
        if self.sessions.contains_key(requested) {
            return self.session_configuration(requested).await;
        }
        let store = crate::session_manager::SessionManager::new(self.sessions_dir.clone())
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
                mode: Some(
                    if acp_mode(&self.config) == AppMode::Plan {
                        "plan"
                    } else {
                        saved.metadata.mode.as_deref().unwrap_or("agent")
                    }
                    .to_string(),
                ),
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
        let cursor = self
            .runtime
            .get_thread_detail(&resumed.thread_id)
            .await
            .map_err(|error| AcpError::internal(error.to_string()))?
            .latest_seq;
        self.remember(id.clone(), resumed.thread_id, cursor);
        self.session_configuration(&id).await
    }
    fn permission_value(&self) -> &'static str {
        match acp_approval_mode(&self.config) {
            ApprovalMode::Bypass => "full-access",
            ApprovalMode::Auto => "auto-review",
            ApprovalMode::Never => "never",
            ApprovalMode::Suggest => "ask",
        }
    }
    async fn session_configuration(
        &self,
        session_id: &str,
    ) -> std::result::Result<Value, AcpError> {
        use codewhale_localization::{MessageId, resolve_locale, tr};
        let settings = crate::settings::Settings::load().unwrap_or_default();
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
        let mut models = crate::provider_lake::models_for_provider(&self.config, &identity);
        if !models.contains(&thread.model) {
            models.push(thread.model.clone());
        }
        let mut modes = vec![
            json!({"id": "plan", "name": tr(locale, MessageId::AppModePlan), "description": tr(locale, MessageId::AppModePlanHint)}),
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
            modes.insert(0, json!({"id": "agent", "name": tr(locale, MessageId::AppModeAgent), "description": agent_hint}));
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
            ApprovalMode::Suggest => (
                "ask",
                MessageId::ConfigChoiceAsk,
                MessageId::PermissionsPostureAsk,
            ),
        };
        Ok(json!({
            "sessionId": session_id,
            "modes": {"currentModeId": current_mode, "availableModes": modes},
            "models": {
                "currentModelId": thread.model,
                "availableModels": models.iter().map(|model| json!({"modelId": model, "name": model})).collect::<Vec<_>>()
            },
            "configOptions": [
                {"id": "mode", "name": tr(locale, MessageId::SettingSubjectMode), "category": "mode", "type": "select", "currentValue": current_mode,
                 "options": modes.iter().map(|mode| json!({"value": mode["id"], "name": mode["name"], "description": mode["description"]})).collect::<Vec<_>>()},
                {"id": "model", "name": tr(locale, MessageId::SettingSubjectModel), "category": "model", "type": "select", "currentValue": thread.model,
                 "options": models.iter().map(|model| json!({"value": model, "name": model})).collect::<Vec<_>>()},
                // Exactly one option: the posture the server was started
                // with. Offering a looser value here would let a client relax
                // the operator's floor.
                {"id": "permission", "name": tr(locale, MessageId::SettingSubjectPermissions), "category": "_permission", "type": "select", "currentValue": posture_value,
                 "options": [{"value": posture_value, "name": tr(locale, posture_name), "description": tr(locale, posture_description)}],
                 "_meta": {"codewhale": {
                     "readOnly": true,
                     "fullAccess": posture == ApprovalMode::Bypass,
                     "enableFullAccess": ACP_FULL_ACCESS_HINT,
                 }}}
            ]
        }))
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
        let state = self.session_configuration(session_id).await?;
        let offered = state["configOptions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|option| {
                option["id"] == config_id
                    && option["options"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|choice| choice["value"] == value)
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
            "permission" => return Ok(json!({"configOptions": state["configOptions"]})),
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
        Ok(json!({"configOptions": self.session_configuration(session_id).await?["configOptions"]}))
    }

    fn list_providers(&self) -> Value {
        let mut providers = self.config.provider_identities().into_iter().filter(|identity| identity.provider != ProviderKind::Antigravity).map(|identity| {
            json!({"id": identity.key, "displayName": identity.compatibility().map(|row| row.label).unwrap_or(identity.key.as_str()), "defaultModel": crate::model_inventory::provider_default_model(&self.config, &identity)})
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
const ACP_FULL_ACCESS_HINT: &str = "Start the server with `codewhale --approval-policy full-access serve --acp`, or set approval_policy = \"full-access\" in config.toml. Full Access also turns off Codewhale's own sandbox unless sandbox_mode tightens it; Plan stays read-only.";

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
    if config.yolo.unwrap_or(false) {
        ApprovalMode::Bypass
    } else {
        config
            .approval_policy
            .as_deref()
            .and_then(ApprovalMode::from_config_value)
            .unwrap_or_default()
    }
}

/// ACP `kind` hint for a tool call, used by the client to pick an icon/label.
/// Falls back to `"other"` for tools without an obvious category.
///
/// `File` is a single canonical tool covering read/list/search/write/edit/
/// patch (#4625), so its kind depends on the `action` argument rather than
/// the tool name alone.
fn tool_call_kind(call: &PendingToolCall) -> &'static str {
    match call.name.as_str() {
        "File" => match call.input.get("action").and_then(Value::as_str) {
            Some("write" | "edit" | "patch") => "edit",
            _ => "read",
        },
        "apply_patch" => "edit",
        "Git" => "read",
        "bash" | "Bash" | "terminal/run" | "terminal/send" | "terminal/wait"
        | "terminal/cancel" | "terminal/reset" => "execute",
        _ => "other",
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

async fn write_tool_call_start<W>(
    writer: &mut W,
    session_id: &str,
    call: &PendingToolCall,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "tool_call",
                "toolCallId": call.execution_id,
                "title": tool_call_title(call),
                "kind": tool_call_kind(call),
                "status": "pending",
                "rawInput": call.input,
            }
        }
    });
    write_json_line(writer, notification).await
}

async fn write_tool_call_update<W>(
    writer: &mut W,
    session_id: &str,
    call: &PendingToolCall,
    status: &str,
    content: Option<&str>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_tool_call_update_with_blocks(writer, session_id, call, status, content, &[]).await
}

async fn write_tool_call_update_with_blocks<W>(
    writer: &mut W,
    session_id: &str,
    call: &PendingToolCall,
    status: &str,
    content: Option<&str>,
    rich_blocks: &[codewhale_tools::ToolResultContentBlock],
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut update = json!({
        "sessionUpdate": "tool_call_update",
        "toolCallId": call.execution_id,
        "status": status,
    });
    if content.is_some() || !rich_blocks.is_empty() {
        let mut blocks = Vec::with_capacity(rich_blocks.len() + usize::from(content.is_some()));
        if let Some(content) = content {
            blocks.push(json!({
                "type": "content",
                "content": { "type": "text", "text": truncate_for_acp(content) }
            }));
        }
        blocks.extend(rich_blocks.iter().map(|block| match block {
            codewhale_tools::ToolResultContentBlock::Image { mime_type, data } => json!({
                "type": "content",
                "content": { "type": "image", "data": data, "mimeType": mime_type }
            }),
        }));
        update["content"] = json!(blocks);
    }
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": update
        }
    });
    write_json_line(writer, notification).await
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

    fn method_not_found(method: &str) -> Self {
        Self {
            code: -32601,
            message: format!("method not found: {method}"),
        }
    }
}

fn initialize_result(client_protocol_version: Option<u64>, config: &Config) -> Value {
    json!({
        "protocolVersion": client_protocol_version
            .map(|version| version.min(ACP_PROTOCOL_VERSION))
            .unwrap_or(ACP_PROTOCOL_VERSION),
        "agentCapabilities": {
            "loadSession": true,
            "modelSelection": true,
            "promptCapabilities": {
                "image": false,
                "audio": false,
                "embeddedContext": true
            },
            "mcpCapabilities": {
                "http": false,
                "sse": false
            },
            // ACP `SessionCapabilities` fields are objects, never booleans:
            // `{}` means "supported", absent/null means "not supported"
            // (#5969 — a boolean here made JetBrains' strictly-typed client
            // fail the handshake and kill the agent). `session/load` support
            // is advertised by the top-level `loadSession` above; it is not a
            // field of `sessionCapabilities`. We only claim `list` because
            // `session/list` is the only one of the optional session methods
            // this server dispatches.
            "sessionCapabilities": {
                "list": {}
            }
        },
        "agentInfo": {
            "name": "codewhale",
            "title": "codewhale",
            "version": env!("CARGO_PKG_VERSION")
        },
        "authMethods": acp_auth_methods(config)
    })
}

fn acp_auth_methods(config: &Config) -> Value {
    let Some(identity) = config.active_provider_identity().ok().filter(|identity| {
        identity.provider != ProviderKind::Custom && identity.provider != ProviderKind::Antigravity
    }) else {
        return json!([]);
    };
    let provider = identity.provider.as_str();
    json!([
        {
            "id": "codewhale-terminal-auth",
            "name": "Set Codewhale API key",
            "description": format!("Run Codewhale's terminal credential setup for the {provider} provider."),
            "type": "terminal",
            "args": ["auth", "set", "--provider", provider],
            "env": {}
        }
    ])
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

async fn write_session_update<W>(writer: &mut W, session_id: &str, text: String) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {
                    "type": "text",
                    "text": text
                }
            }
        }
    });
    write_json_line(writer, notification).await
}

async fn write_jsonrpc_result<W>(writer: &mut W, id: Value, result: Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_json_line(
        writer,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result
        }),
    )
    .await
}

async fn write_jsonrpc_error<W>(
    writer: &mut W,
    id: Option<Value>,
    code: i32,
    message: impl Into<String>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_json_line(
        writer,
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message.into()
            }
        }),
    )
    .await
}

async fn write_json_line<W>(writer: &mut W, value: Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        writer.write_all(value.to_string().as_bytes()).await?;
        writer.write_all(b"\n").await?;
        writer.flush().await
    })
    .await
    .map_err(|_| anyhow!("ACP transport write did not settle within 30 seconds"))??;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JsonRpcResponseIdPolicy {
    /// JSON-RPC's normal contract: echo the request id without changing type.
    Preserve,
    /// Zed's ACP client currently decodes response ids as strings even when it
    /// sent a number. Keep this narrow compatibility mode client-identified.
    StringifyNumeric,
}

impl JsonRpcResponseIdPolicy {
    fn from_initialize_params(params: &Value) -> Self {
        let client_name = params
            .pointer("/clientInfo/name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if client_name.eq_ignore_ascii_case("zed") {
            Self::StringifyNumeric
        } else {
            Self::Preserve
        }
    }

    fn response_id(self, id: Value) -> Value {
        match (self, id) {
            (Self::StringifyNumeric, Value::Number(number)) => Value::String(number.to_string()),
            (_, id) => id,
        }
    }
}

fn is_jsonrpc_response(message: &Value) -> bool {
    message.get("id").is_some()
        && (message.get("result").is_some() || message.get("error").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_client::mock::{MockLlmClient, canned};
    use codewhale_models::{ContentBlock, Role};
    use std::time::Duration;

    pub(super) fn parse_lines(output: Vec<u8>) -> Vec<Value> {
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    pub(super) fn fixture_config() -> Config {
        let mut config = Config {
            provider: Some("acp-fixture".into()),
            providers: Some(crate::config::ProvidersConfig {
                custom: HashMap::from([(
                    "acp-fixture".into(),
                    crate::config::ProviderConfig {
                        kind: Some("openai-compatible".into()),
                        base_url: Some("http://127.0.0.1:18181/v1".into()),
                        model: Some("fixture-model".into()),
                        api_key: Some("local-test-key".into()),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }),
            snapshots: Some(crate::config::SnapshotsConfig {
                enabled: false,
                ..Default::default()
            }),
            ..Default::default()
        };
        config.set_feature("mcp", false).unwrap();
        config.set_feature("subagents", false).unwrap();
        config
    }
    pub(super) struct Rig {
        pub(super) server: AcpServer,
        mock: Arc<MockLlmClient>,
        pub(super) workspace: PathBuf,
        _home: crate::test_support::SealedHome,
        _dir: tempfile::TempDir,
    }
    impl Rig {
        pub(super) fn new(
            config: Config,
            turns: Vec<crate::llm_client::mock::CannedTurn>,
        ) -> Result<Self> {
            Self::with_base(config, turns, true)
        }
        fn with_base(
            config: Config,
            turns: Vec<crate::llm_client::mock::CannedTurn>,
            acp: bool,
        ) -> Result<Self> {
            let dir = tempfile::tempdir()?;
            let home = crate::test_support::SealedHome::at(dir.path());
            let workspace = dir.path().join("workspace");
            std::fs::create_dir(&workspace)?;
            let runtime_dir = dir.path().join("runtime");
            let manager_config = RuntimeThreadManagerConfig {
                data_dir: runtime_dir.clone(),
                task_data_dir: runtime_dir,
                sessions_dir: None,
                max_active_threads: 4,
            };
            let plugins = Arc::new(crate::plugins::PluginRegistry::empty(&workspace));
            let manager = Arc::new(if acp {
                RuntimeThreadManager::open_host(
                    config.clone(),
                    workspace.clone(),
                    manager_config,
                    plugins,
                    crate::core::engine::EngineHostProfile::Acp,
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
            let sessions = crate::session_manager::default_sessions_dir()?;
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
        async fn prompt(
            &mut self,
            session: &str,
            text: &str,
        ) -> Result<(&'static str, Vec<Value>)> {
            // Keep the transport input open: an empty reader is a real EOF/cancel.
            let (client, input) = tokio::io::duplex(4096);
            let mut reader = codewhale_app_server::BoundedLines::new(BufReader::new(input));
            let mut output = Vec::new();
            let result = tokio::time::timeout(
                Duration::from_secs(if cfg!(windows) { 120 } else { 20 }),
                self.server.drive_prompt(
                    json!({"sessionId":session,"prompt":text}),
                    &mut reader,
                    &mut output,
                ),
            )
            .await??;
            drop(client);
            Ok((result, parse_lines(output)))
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
            guest: &mut codewhale_app_server::daemon_client::OwnerClient,
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
        let (binding, generation) = codewhale_app_server::daemon_socket::owner_work(move || {
            capture.capture_control_owner()
        })
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
        let principal = codewhale_app_server::daemon_socket::owner_work(|| {
            codewhale_config::windows_identity::CurrentWindowsUser::open()?.sid_string()
        })
        .await?;
        let receipt = codewhale_protocol::RuntimeOwnerReceipt {
            version: 1,
            data_dir: binding.data_dir,
            execution_scope: binding.execution_scope,
            lease_generation: generation,
            pid: std::process::id(),
            process_start: codewhale_app_server::daemon_socket::capture_process_start(
                std::process::id(),
            )
            .await?,
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
        let (daemon, _state) = codewhale_app_server::bind_runtime_frontends(
            None,
            None,
            receipt.clone(),
            codewhale_app_server::RuntimeOwnerRouting {
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
        let shutdown = daemon.shutdown_handle();
        let owner = tokio::spawn(daemon.serve());
        let _abort = AbortOnDrop(owner.abort_handle());
        let mut guest = codewhale_app_server::daemon_client::connect_acp_if_published(
            None,
            Some(socket_path.clone()),
        )
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
        let mut shutdown_guest = codewhale_app_server::daemon_client::connect_acp_if_published(
            None,
            Some(socket_path.clone()),
        )
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
                crate::runtime_threads::ThreadListFilter::IncludeArchived,
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
                    crate::runtime_threads::StartTurnRequest {
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
                            && t.status == crate::runtime_threads::RuntimeTurnStatus::Completed
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
        let (after, generation) = codewhale_app_server::daemon_socket::owner_work(move || {
            capture.capture_control_owner()
        })
        .await?;
        assert_eq!(after.data_dir, receipt.data_dir);
        assert_eq!(
            generation, receipt.lease_generation,
            "guest EOF cannot release the host lease"
        );
        shutdown.trigger();
        tokio::time::timeout(Duration::from_secs(5), owner).await???;
        assert!(
            codewhale_app_server::daemon_client::connect_acp_if_published(None, Some(socket_path))
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
        let yolo = Config {
            yolo: Some(true),
            ..Config::default()
        };
        assert_eq!(acp_approval_mode(&yolo), ApprovalMode::Bypass);
        let policy = Config {
            approval_policy: Some("never".into()),
            ..Config::default()
        };
        assert_eq!(acp_approval_mode(&policy), ApprovalMode::Never);
        assert_eq!(acp_approval_mode(&Config::default()), ApprovalMode::Suggest);
    }

    #[test]
    fn initialize_advertises_baseline_acp_agent() {
        let result = initialize_result(Some(1), &Config::default());

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
    fn initialize_advertises_model_selection_capability() {
        let result = initialize_result(Some(1), &Config::default());

        assert_eq!(result["agentCapabilities"]["modelSelection"], true);
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

    #[tokio::test]
    async fn session_update_is_protocol_clean_single_line_json() {
        let mut out = Vec::new();

        write_session_update(&mut out, "sess_1", "hello\nworld".to_string())
            .await
            .expect("write update");

        let line = String::from_utf8(out).expect("utf8");
        assert_eq!(line.lines().count(), 1);
        let value: Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(value["method"], "session/update");
        assert_eq!(value["params"]["sessionId"], "sess_1");
        assert_eq!(value["params"]["update"]["content"]["text"], "hello\nworld");
    }

    #[tokio::test]
    async fn jsonrpc_result_preserves_numeric_ids_for_avante_acp() {
        let mut out = Vec::new();

        let params = json!({
            "protocolVersion": 1,
            "clientCapabilities": {}
        });
        let id = JsonRpcResponseIdPolicy::from_initialize_params(&params).response_id(json!(1));
        write_jsonrpc_result(&mut out, id, json!({"ok": true}))
            .await
            .expect("write result");

        let line = String::from_utf8(out).expect("utf8");
        let value: Value = serde_json::from_str(line.trim()).expect("json");
        // Numeric ID must stay numeric — avante.nvim's Lua client uses
        // strict table keys (callbacks[1] ≠ callbacks["1"]).
        assert!(
            value["id"].is_number(),
            "numeric id must stay numeric, got {:?}",
            value["id"]
        );
        assert_eq!(value["result"], json!({"ok": true}));
    }

    #[tokio::test]
    async fn jsonrpc_result_stringifies_numeric_ids_for_zed_acp() {
        let mut out = Vec::new();

        let params = json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {
                "name": "zed",
                "version": "1.2.6"
            }
        });
        let id = JsonRpcResponseIdPolicy::from_initialize_params(&params).response_id(json!(1));
        write_jsonrpc_result(&mut out, id, json!({"ok": true}))
            .await
            .expect("write result");

        let line = String::from_utf8(out).expect("utf8");
        let value: Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(value["id"], "1");
        assert_eq!(value["result"], json!({"ok": true}));
    }

    #[tokio::test]
    async fn jsonrpc_error_keeps_absent_id_null() {
        let mut out = Vec::new();

        write_jsonrpc_error(&mut out, None, -32700, "invalid json")
            .await
            .expect("write error");

        let line = String::from_utf8(out).expect("utf8");
        let value: Value = serde_json::from_str(line.trim()).expect("json");
        assert_eq!(value["id"], Value::Null);
        assert_eq!(value["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn tool_update_emits_typed_acp_image_content() {
        let mut output = Vec::new();
        let call = PendingToolCall {
            execution_id: uuid::Uuid::new_v4().to_string(),
            name: "read".to_string(),
            input: json!({"path": "shot.png"}),
        };
        write_tool_call_update_with_blocks(
            &mut output,
            "session_1",
            &call,
            "completed",
            Some("screenshot captured"),
            &[codewhale_tools::ToolResultContentBlock::Image {
                mime_type: "image/png".to_string(),
                data: "QUJD".to_string(),
            }],
        )
        .await
        .expect("ACP update");

        let lines = parse_lines(output);
        let content = lines[0]["params"]["update"]["content"]
            .as_array()
            .expect("ACP content blocks");
        assert_eq!(content[0]["content"]["type"], "text");
        assert_eq!(content[1]["content"]["type"], "image");
        assert_eq!(content[1]["content"]["mimeType"], "image/png");
        assert_eq!(content[1]["content"]["data"], "QUJD");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn new_session_returns_durable_bare_uuid_without_provider_or_history() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        let id = rig.new_session().await;
        uuid::Uuid::parse_str(&id)?;
        let store = crate::session_manager::SessionManager::new(rig.server.sessions_dir.clone())?;
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
        let listed = rig
            .server
            .list_sessions(json!({"cwd":rig.workspace}))
            .unwrap();
        assert_eq!(listed["sessions"].as_array().unwrap().len(), 2);
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
            .filter_map(|v| v.pointer("/params/update/status").and_then(Value::as_str))
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
        let saved = crate::session_manager::SessionManager::new(rig.server.sessions_dir.clone())?
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
        let saved = crate::session_manager::SessionManager::new(rig.server.sessions_dir.clone())?
            .load_session(&id)?;
        assert_eq!(saved.messages, history);
        let detail = rig
            .server
            .runtime
            .get_thread_detail(&rig.server.sessions[&id].thread_id)
            .await?;
        assert_eq!(
            detail.turns.last().unwrap().status,
            crate::runtime_threads::RuntimeTurnStatus::Failed
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
        assert_eq!(requests.len(), 2);
        assert_eq!(
            serde_json::to_vec(&requests[0].system)?,
            serde_json::to_vec(&requests[1].system)?
        );
        let prompt = crate::prompts::system_prompt_flat_text(requests[0].system.as_ref().unwrap());
        assert!(prompt.contains(crate::prompts::text::BASE_PROMPT.trim()));
        assert!(prompt.contains("initial-project-law"));
        assert!(!prompt.contains("new-self-authored-law"));
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
            .filter_map(|v| v.pointer("/params/update/status").and_then(Value::as_str))
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
        let (client, transport) = tokio::io::duplex(65536);
        let (read, mut write) = tokio::io::split(transport);
        let mut reader = codewhale_app_server::BoundedLines::new(BufReader::new(read));
        let (cr, mut cw) = tokio::io::split(client);
        let mut cr = codewhale_app_server::BoundedLines::new(BufReader::new(cr));
        let sid = id.clone();
        let target_in = target.clone();
        let client = async move {
            let mut seen = Vec::new();
            loop {
                let line = cr
                    .next_line()
                    .await?
                    .ok_or_else(|| anyhow!("permission request missing"))?;
                let v: Value = serde_json::from_str(&line)?;
                seen.push(v.clone());
                if v["method"] != "session/request_permission" {
                    continue;
                }
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
                    !seen
                        .iter()
                        .any(|v| v.pointer("/params/update/status") == Some(&json!("in_progress"))),
                    "no dispatch before the real decision"
                );
                match reply {
                    PermissionReply::AllowAfterWrong=>{
                        write_json_line(&mut cw,json!({"jsonrpc":"2.0","id":"wrong-id","result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}})).await?;
                        tokio::time::sleep(Duration::from_millis(30)).await;
                        assert!(!target_in.exists());assert_eq!(manager.get_thread_detail(&thread).await?.pending_approvals.len(),1);
                        write_json_line(&mut cw,json!({"jsonrpc":"2.0","id":v["id"],"result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}})).await?;
                    },
                    PermissionReply::ForeignCancelAndBusy=>{
                        write_json_line(&mut cw,json!({"jsonrpc":"2.0","id":41,"method":"session/cancel","params":{"sessionId":"foreign-session"}})).await?;
                        write_json_line(&mut cw,json!({"jsonrpc":"2.0","id":42,"method":"session/prompt","params":{"sessionId":sid,"prompt":"must not run"}})).await?;
                        for expected in [41,42] {
                            let response:Value=serde_json::from_str(&cr.next_line().await?.ok_or_else(||anyhow!("control response missing"))?)?;
                            assert_eq!(response["id"],expected);
                            if expected==41 {assert_eq!(response["result"],Value::Null);} else {assert_eq!(response["error"]["code"],-32603);}
                            seen.push(response);
                        }
                        assert!(!target_in.exists());
                        assert_eq!(manager.get_thread_detail(&thread).await?.pending_approvals.len(),1);
                        write_json_line(&mut cw,json!({"jsonrpc":"2.0","id":v["id"],"result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}})).await?;
                    },
                    PermissionReply::Reject=>write_json_line(&mut cw,json!({"jsonrpc":"2.0","id":v["id"],"result":{"outcome":{"outcome":"selected","optionId":"fabricated"}}})).await?,
                    PermissionReply::Eof=>cw.shutdown().await?,
                    PermissionReply::CancelLateAllow=>{
                        write_json_line(&mut cw,json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":sid}})).await?;
                        write_json_line(&mut cw,json!({"jsonrpc":"2.0","id":v["id"],"result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}})).await?;
                    },
                }
                return Ok::<_, anyhow::Error>((cr, cw, seen));
            }
        };
        let driver = rig.server.drive_prompt(
            json!({"sessionId":id,"prompt":"write under Ask"}),
            &mut reader,
            &mut write,
        );
        let (result, client) = tokio::time::timeout(Duration::from_secs(20), async {
            tokio::join!(driver, client)
        })
        .await?;
        let (mut cr, _cw, mut wire) = client?;
        write.shutdown().await?;
        while let Some(line) = cr.next_line().await? {
            wire.push(serde_json::from_str(&line)?);
        }
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
        let saved = crate::session_manager::SessionManager::new(rig.server.sessions_dir.clone())?
            .load_session(&id)?;
        assert!(saved.messages.iter().any(|m| m.role == Role::User));
        if !base_acp {
            let thread = rig.server.sessions[&id].thread_id.clone();
            let successor = rig
                .server
                .runtime
                .start_turn(
                    &thread,
                    crate::runtime_threads::StartTurnRequest {
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
    ) -> Result<crate::runtime_threads::TurnRecord> {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let detail = manager.get_thread_detail(thread).await?;
                if let Some(record) = detail.turns.iter().find(|record| {
                    record.id == turn
                        && !matches!(
                            record.status,
                            crate::runtime_threads::RuntimeTurnStatus::InProgress
                                | crate::runtime_threads::RuntimeTurnStatus::Queued
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
        let request = crate::runtime_threads::StartTurnRequest {
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
            crate::runtime_threads::RuntimeTurnStatus::Completed
        );
        assert_eq!(
            rig.mock.call_count(),
            1,
            "exact replay executes the Engine once"
        );
        rig.mock
            .push_turn(canned::simple_text_turn("ordinary initial"));
        let ordinary_request = crate::runtime_threads::StartTurnRequest {
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
            crate::runtime_threads::RuntimeTurnStatus::Completed
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
            if crate::test_support::read_shell_sentinel(&sentinel) == "rm -rf /" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
        assert_eq!(
            crate::test_support::read_shell_sentinel(&sentinel),
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
        let saved = crate::session_manager::SessionManager::new(rig.server.sessions_dir.clone())?
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
            crate::runtime_threads::RuntimeTurnStatus::Completed
        );
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn foreign_canonical_store_refuses_before_allocating_or_copying_history() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        let id = rig.new_session().await;
        let store = crate::session_manager::SessionManager::new(rig.server.sessions_dir.clone())?;
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
                crate::runtime_threads::ThreadListFilter::IncludeArchived,
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
                    crate::runtime_threads::ThreadListFilter::IncludeArchived,
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
        let image = crate::image_attach::tests::runtime_image_fixture(21);
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
        config.tui = Some(crate::config::TuiConfig {
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
            Some(crate::runtime_threads::RuntimeTurnStopReason::StepBudgetExhausted)
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
                    crate::runtime_threads::StartTurnRequest {
                        prompt: "ordinary after step stop".into(),
                        ..Default::default()
                    },
                )
                .await?;
            assert_eq!(
                wait_real_turn(&rig.server.runtime, &thread, &successor.id)
                    .await?
                    .status,
                crate::runtime_threads::RuntimeTurnStatus::Completed
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
        let input = format!(
            "{}\n{}\n{}\n{}\n",
            json!({"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":id,"prompt":[]}}),
            json!({"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"unknown","prompt":"x"}}),
            json!({"jsonrpc":"2.0","id":5,"method":"fabricated"}),
            json!({"jsonrpc":"2.0","id":6,"method":"shutdown"})
        );
        let mut reader = codewhale_app_server::BoundedLines::new(BufReader::new(input.as_bytes()));
        let mut output = Vec::new();
        rig.server.serve(&mut reader, &mut output).await?;
        let wire = parse_lines(output);
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
                    crate::runtime_threads::ThreadListFilter::IncludeArchived,
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
}

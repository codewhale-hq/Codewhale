use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use codewhale_app_server::runtime_api::{
    ExternalAgentDiscoveryRow, discover_external_agent, discover_external_agents,
};
use codewhale_engine::fleet::agents::{AGENTS, custom_agents};
use codewhale_engine::runtime_threads::manager::{
    CreateThreadExecutor, CreateThreadExecutorConfig, CreateThreadRequest,
    ExternalApprovalDecision, RuntimeThreadManager, RuntimeThreadManagerConfig, StartTurnRequest,
};
use codewhale_engine::runtime_threads::{EXECUTOR_KIND_ACP, RuntimeEventRecord};
use codewhale_engine::task_manager::TaskManagerConfig;
use codewhale_localization::MessageId;
use codewhale_runtime::config::Config;
use codewhale_runtime::runtime_policy::approval_wire;

use crate::tui::app::{App, QueuedMessage, StatusToastLevel};
use crate::tui::approval::{ApprovalRequest, ApprovalView, ReviewDecision};
use crate::tui::history::HistoryCell;
use crate::tui::views::ModalKind;

pub struct ExternalAgentSession {
    pub agent_id: String,
    pub agent_name: String,
    pub runtime: Arc<RuntimeThreadManager>,
    pub thread_id: String,
    pub active_turn_id: Option<String>,
    pub events_rx: tokio::sync::broadcast::Receiver<RuntimeEventRecord>,
    pub pending_approvals: HashSet<String>,
    pub turn_start_rx: Option<tokio::sync::oneshot::Receiver<Result<String, String>>>,
    pub interrupt_pending: bool,
}

pub fn resolve_external_agent_posture(
    permission_mode: Option<codewhale_engine::runtime_options::PermissionMode>,
    yolo_or_auto: bool,
    config: &Config,
) -> (&'static str, &'static str) {
    if yolo_or_auto {
        return ("full-access", "yolo");
    }
    match permission_mode {
        Some(codewhale_engine::runtime_options::PermissionMode::Plan) => ("ask", "plan"),
        Some(codewhale_engine::runtime_options::PermissionMode::Ask) => ("ask", "agent"),
        Some(codewhale_engine::runtime_options::PermissionMode::AcceptEdits) => {
            ("accept-edits", "agent")
        }
        Some(codewhale_engine::runtime_options::PermissionMode::AutoReview) => ("auto", "agent"),
        Some(codewhale_engine::runtime_options::PermissionMode::FullAccess) => {
            ("full-access", "yolo")
        }
        None => {
            let approval = config
                .approval_policy
                .as_deref()
                .and_then(codewhale_execpolicy::ApprovalMode::from_config_value)
                .unwrap_or(codewhale_execpolicy::ApprovalMode::Suggest);
            let mode = if approval == codewhale_execpolicy::ApprovalMode::Bypass {
                "yolo"
            } else {
                "agent"
            };
            (approval_wire(approval), mode)
        }
    }
}

pub(crate) fn start_external_agent_discovery(app: &mut App) {
    if app.external_agents_rx.is_some() {
        return;
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.external_agents_rx = Some(rx);
    tokio::spawn(async move {
        let agents = discover_external_agents().await;
        let _ = tx.send(agents);
    });
}

pub(crate) fn poll_external_agent_discovery(app: &mut App, config: &Config) -> bool {
    let Some(rx) = app.external_agents_rx.as_mut() else {
        return false;
    };
    match rx.try_recv() {
        Ok(agents) => {
            app.external_agents_rx = None;
            app.external_agents = agents;
            if app.view_stack.top_kind() == Some(ModalKind::ModelPicker)
                && let Some(mut boxed) = app.view_stack.pop()
            {
                if let Some(picker) = boxed
                    .as_any_mut()
                    .downcast_mut::<crate::tui::model_picker::ModelPickerView>()
                {
                    picker.re_resolve_from_app(app, config);
                }
                app.view_stack.push_boxed(boxed);
            }
            app.needs_redraw = true;
            true
        }
        Err(tokio::sync::oneshot::error::TryRecvError::Empty) => false,
        Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
            app.external_agents_rx = None;
            false
        }
    }
}

pub async fn create_external_agent_session(
    config: &Config,
    workspace: PathBuf,
    permission_posture: &str,
    mode: &str,
    agent_id: String,
    agent_name: Option<String>,
    plugin_registry: Arc<codewhale_plugins::PluginRegistry>,
) -> Result<ExternalAgentSession> {
    let task_config = TaskManagerConfig::from_runtime(config, workspace.clone(), None, None);
    let manager_config = RuntimeThreadManagerConfig::from_task_data_dir(task_config.data_dir);
    let runtime = Arc::new(RuntimeThreadManager::open_with_plugin_registry(
        config.clone(),
        workspace.clone(),
        manager_config,
        plugin_registry,
    )?);
    let thread = runtime
        .create_thread(CreateThreadRequest {
            workspace: Some(workspace),
            permission_posture: Some(permission_posture.to_string()),
            mode: Some(mode.to_string()),
            executor: Some(CreateThreadExecutor {
                kind: EXECUTOR_KIND_ACP.to_string(),
                agent_id: agent_id.clone(),
                config: CreateThreadExecutorConfig::default(),
            }),
            ..Default::default()
        })
        .await?;
    let events_rx = runtime.subscribe_events();
    let resolved_name = thread
        .executor
        .as_ref()
        .and_then(|record| record.agent_name.clone())
        .or(agent_name)
        .unwrap_or_else(|| agent_id.clone());
    Ok(ExternalAgentSession {
        agent_id,
        agent_name: resolved_name,
        runtime,
        thread_id: thread.id,
        active_turn_id: None,
        events_rx,
        pending_approvals: HashSet::new(),
        turn_start_rx: None,
        interrupt_pending: false,
    })
}

pub(crate) async fn switch_to_external_agent(
    app: &mut App,
    config: &Config,
    agent_id: String,
    agent_name: String,
) -> bool {
    if app.is_loading || app.dispatch_in_flight {
        let message = app.tr(MessageId::ExternalAgentSwitchBusy).into_owned();
        app.push_status_toast(message, StatusToastLevel::Warning, Some(6_000));
        return false;
    }
    if app
        .external_agent
        .as_ref()
        .is_some_and(|session| session.agent_id == agent_id)
    {
        return true;
    }
    let posture = approval_wire(app.approval_mode).to_string();
    let mode = app.mode.as_setting().to_string();
    let session = match create_external_agent_session(
        config,
        app.workspace.clone(),
        &posture,
        &mode,
        agent_id,
        Some(agent_name),
        Arc::clone(&app.plugin_registry),
    )
    .await
    {
        Ok(session) => session,
        Err(error) => {
            app.push_status_toast(error.to_string(), StatusToastLevel::Error, Some(10_000));
            return false;
        }
    };
    clear_external_agent(app);
    app.runtime_turn_status = None;
    app.runtime_turn_id = None;
    app.active_turn = None;
    app.dispatch_started_at = None;
    app.turn_started_at = None;
    app.turn_last_activity_at = None;
    app.suppress_stream_events_until_turn_complete = false;
    if !app.history.is_empty() {
        app.history.clear();
        app.streaming_message_index = None;
        app.streaming_thinking_active_entry = None;
        app.current_session_id = None;
        app.session_title = None;
        app.mark_history_updated();
    }
    let notice = app
        .tr(MessageId::ExternalAgentSessionStarted)
        .replace("{agent}", &session.agent_name);
    app.external_agent = Some(session);
    app.add_message(HistoryCell::System {
        content: notice.clone(),
    });
    app.status_message = Some(notice);
    app.needs_redraw = true;
    true
}

pub(crate) fn clear_external_agent(app: &mut App) {
    let Some(session) = app.external_agent.take() else {
        return;
    };
    let runtime = Arc::clone(&session.runtime);
    tokio::spawn(async move {
        let _ = runtime.shutdown_and_wait().await;
    });
}

pub(crate) fn dispatch_external_turn(app: &mut App, message: QueuedMessage) -> Result<()> {
    let Some(session) = app.external_agent.as_mut() else {
        bail!("no active external agent session");
    };
    if session.active_turn_id.is_some() || session.turn_start_rx.is_some() {
        bail!("an external agent turn is already running");
    }
    let prompt = message.display.clone();
    let runtime = Arc::clone(&session.runtime);
    let thread_id = session.thread_id.clone();
    let posture = approval_wire(app.approval_mode).to_string();
    let mode = app.mode.as_setting().to_string();
    let (tx, rx) = tokio::sync::oneshot::channel();
    session.turn_start_rx = Some(rx);
    session.interrupt_pending = false;

    app.add_message(HistoryCell::User {
        content: prompt.clone(),
    });
    app.is_loading = true;
    app.streaming_message_index = None;
    app.needs_redraw = true;

    tokio::spawn(async move {
        let result = runtime
            .start_turn(
                &thread_id,
                StartTurnRequest {
                    prompt,
                    permission_posture: Some(posture),
                    mode: Some(mode),
                    ..Default::default()
                },
            )
            .await
            .map(|turn| turn.id)
            .map_err(|error| error.to_string());
        let _ = tx.send(result);
    });
    Ok(())
}

pub(crate) fn interrupt_external_turn(app: &mut App) -> bool {
    let Some(session) = app.external_agent.as_mut() else {
        return false;
    };
    if let Some(turn_id) = session.active_turn_id.clone() {
        let runtime = Arc::clone(&session.runtime);
        let thread_id = session.thread_id.clone();
        tokio::spawn(async move {
            let _ = runtime.interrupt_turn(&thread_id, &turn_id).await;
        });
        app.finalize_streaming_assistant_as_interrupted();
        app.needs_redraw = true;
        return true;
    }
    if session.turn_start_rx.is_some() {
        session.interrupt_pending = true;
        app.finalize_streaming_assistant_as_interrupted();
        app.needs_redraw = true;
        return true;
    }
    false
}

pub(crate) fn handle_external_approval_decision(
    app: &mut App,
    approval_id: &str,
    decision: &ReviewDecision,
) -> Option<bool> {
    let owned = app
        .external_agent
        .as_mut()
        .is_some_and(|session| session.pending_approvals.remove(approval_id));
    if !owned {
        return None;
    }
    let Some(session) = app.external_agent.as_ref() else {
        return Some(false);
    };
    let runtime = Arc::clone(&session.runtime);
    let external_decision = match decision {
        ReviewDecision::Approved => ExternalApprovalDecision::Allow { remember: false },
        ReviewDecision::ApprovedForSession => ExternalApprovalDecision::Allow { remember: true },
        ReviewDecision::Denied | ReviewDecision::Abort => {
            ExternalApprovalDecision::Deny { remember: false }
        }
    };
    let id = approval_id.to_string();
    tokio::spawn(async move {
        let _ = runtime
            .deliver_external_approval(&id, external_decision)
            .await;
    });
    if *decision == ReviewDecision::Abort {
        interrupt_external_turn(app);
    }
    Some(true)
}

pub(crate) fn drain_external_agent_events(app: &mut App) -> bool {
    let Some(session) = app.external_agent.as_mut() else {
        return false;
    };
    let mut changed = false;

    if let Some(rx) = session.turn_start_rx.as_mut() {
        match rx.try_recv() {
            Ok(Ok(turn_id)) => {
                session.turn_start_rx = None;
                session.active_turn_id = Some(turn_id.clone());
                if std::mem::take(&mut session.interrupt_pending) {
                    let runtime = Arc::clone(&session.runtime);
                    let thread_id = session.thread_id.clone();
                    tokio::spawn(async move {
                        let _ = runtime.interrupt_turn(&thread_id, &turn_id).await;
                    });
                }
                changed = true;
            }
            Ok(Err(error)) => {
                session.turn_start_rx = None;
                session.active_turn_id = None;
                session.interrupt_pending = false;
                app.is_loading = false;
                app.add_message(HistoryCell::System {
                    content: error.clone(),
                });
                app.push_status_toast(error, StatusToastLevel::Error, Some(10_000));
                app.needs_redraw = true;
                return true;
            }
            Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {}
            Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                session.turn_start_rx = None;
                app.is_loading = false;
                changed = true;
            }
        }
    }

    let thread_id = session.thread_id.clone();
    let agent_name = session.agent_name.clone();
    let mut records = Vec::new();
    loop {
        match session.events_rx.try_recv() {
            Ok(record) => {
                if record.thread_id == thread_id {
                    records.push(record);
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(
                tokio::sync::broadcast::error::TryRecvError::Empty
                | tokio::sync::broadcast::error::TryRecvError::Closed,
            ) => break,
        }
    }

    for record in records {
        apply_external_event(app, &agent_name, record);
        changed = true;
    }

    if changed {
        app.needs_redraw = true;
    }
    changed
}

fn append_assistant_delta(app: &mut App, delta: &str) {
    if delta.is_empty() {
        return;
    }
    if let Some(index) = app.streaming_message_index
        && let Some(HistoryCell::Assistant { content, .. }) = app.history.get_mut(index)
    {
        content.push_str(delta);
        app.bump_history_cell(index);
        return;
    }
    app.add_message(HistoryCell::Assistant {
        content: delta.to_string(),
        streaming: true,
    });
    app.streaming_message_index = app.history.len().checked_sub(1);
}

fn finish_streaming_assistant(app: &mut App) {
    if let Some(index) = app.streaming_message_index.take()
        && let Some(HistoryCell::Assistant { streaming, .. }) = app.history.get_mut(index)
    {
        *streaming = false;
        app.bump_history_cell(index);
    }
}

fn apply_external_event(app: &mut App, agent_name: &str, record: RuntimeEventRecord) {
    match record.event.as_str() {
        "turn.started" => {
            if let Some(turn_id) = record.turn_id
                && let Some(session) = app.external_agent.as_mut()
            {
                session.active_turn_id = Some(turn_id);
            }
            app.is_loading = true;
        }
        "item.delta" => {
            let kind = record
                .payload
                .get("kind")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            let delta = record
                .payload
                .get("delta")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if kind == "agent_message" {
                append_assistant_delta(app, delta);
            } else if kind == "agent_reasoning" && !delta.is_empty() {
                app.status_message = Some(
                    app.tr(MessageId::ExternalAgentThinkingStatus)
                        .replace("{agent}", agent_name),
                );
            }
        }
        "item.started" => {
            if let Some(item) = record.payload.get("item") {
                let kind = item
                    .get("kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let summary = item
                    .get("summary")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if matches!(kind, "command_execution" | "file_change" | "tool_call")
                    && !summary.is_empty()
                {
                    app.status_message = Some(format!("{agent_name}: {summary}"));
                }
            }
        }
        "item.completed" => {
            if let Some(item) = record.payload.get("item") {
                let kind = item
                    .get("kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if kind == "agent_message" {
                    if app.streaming_message_index.is_some() {
                        finish_streaming_assistant(app);
                    } else {
                        let text = item
                            .get("detail")
                            .and_then(|value| value.as_str())
                            .or_else(|| item.get("summary").and_then(|value| value.as_str()))
                            .unwrap_or_default();
                        if !text.is_empty() {
                            app.add_message(HistoryCell::Assistant {
                                content: text.to_string(),
                                streaming: false,
                            });
                        }
                    }
                }
            }
        }
        "approval.required" => {
            let approval_id = record
                .payload
                .get("approval_id")
                .or_else(|| record.payload.get("id"))
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if approval_id.is_empty() {
                return;
            }
            let tool_name = record
                .payload
                .get("tool_name")
                .and_then(|value| value.as_str())
                .unwrap_or("external_tool");
            let fallback_desc = app.tr(MessageId::ExternalAgentToolCallFallback);
            let description = record
                .payload
                .get("description")
                .or_else(|| record.payload.get("summary"))
                .and_then(|value| value.as_str())
                .unwrap_or(fallback_desc.as_ref());
            let params = record
                .payload
                .get("input")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            let request = ApprovalRequest::new_with_intent(
                approval_id,
                tool_name,
                description,
                &params,
                approval_id,
                None,
                &app.workspace,
            );
            if let Some(session) = app.external_agent.as_mut() {
                session.pending_approvals.insert(approval_id.to_string());
            }
            app.view_stack
                .push(ApprovalView::new_with_default_selection(
                    request,
                    app.ui_locale,
                    Default::default(),
                ));
        }
        "turn.completed" => {
            let status = record
                .payload
                .pointer("/turn/status")
                .and_then(|value| value.as_str())
                .unwrap_or("completed");
            let had_pending_approvals = app.external_agent.as_mut().is_some_and(|session| {
                session.active_turn_id = None;
                session.interrupt_pending = false;
                let had = !session.pending_approvals.is_empty();
                session.pending_approvals.clear();
                had
            });
            if had_pending_approvals && app.view_stack.top_kind() == Some(ModalKind::Approval) {
                let _ = app.view_stack.pop();
            }
            app.is_loading = false;
            match status {
                "failed" => {
                    finish_streaming_assistant(app);
                    let fallback = app.tr(MessageId::ExternalAgentTurnFailedFallback);
                    let error = record
                        .payload
                        .pointer("/turn/error")
                        .and_then(|value| value.as_str())
                        .or_else(|| record.payload.get("error").and_then(|value| value.as_str()))
                        .unwrap_or(fallback.as_ref());
                    let detail = format!("{agent_name}: {error}");
                    app.add_message(HistoryCell::System {
                        content: detail.clone(),
                    });
                    app.push_status_toast(detail, StatusToastLevel::Error, Some(10_000));
                }
                "interrupted" => {
                    app.finalize_streaming_assistant_as_interrupted();
                    app.status_message = Some(
                        app.tr(MessageId::ExternalAgentTurnInterruptedStatus)
                            .replace("{agent}", agent_name),
                    );
                }
                _ => {
                    finish_streaming_assistant(app);
                    app.status_message = None;
                }
            }
        }
        _ => {}
    }
}

pub fn format_agent_not_runnable_error(row: &ExternalAgentDiscoveryRow) -> String {
    let mut parts = Vec::new();
    if let Some(reason) = row
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        parts.push(reason.to_string());
    } else {
        parts.push(format!("{} is not ready to run via ACP.", row.name));
    }
    if row.needs_update
        && let Some(upgrade) = row
            .upgrade
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        && !parts.iter().any(|p| p.contains(upgrade))
    {
        parts.push(format!("Update with `{upgrade}`."));
    } else if row.state == "not_installed"
        && !row.website.trim().is_empty()
        && !parts.iter().any(|p| p.contains(row.website.trim()))
    {
        parts.push(format!("See {}.", row.website.trim()));
    } else if row.state == "installed"
        && !row.needs_update
        && row.reason_code.as_deref() != Some("adapter_missing")
        && let Some(login) = row
            .login
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        && !parts.iter().any(|p| p.contains(login))
    {
        parts.push(format!("Sign in with `{login}`."));
    }
    format!(
        "External agent `{}` ({}) cannot run: {}",
        row.id,
        row.name,
        parts.join(" ")
    )
}

pub fn format_unknown_agent_error(agent_id: &str) -> String {
    let mut known: Vec<String> = AGENTS.iter().map(|agent| agent.id.to_string()).collect();
    for custom in custom_agents() {
        known.push(custom.id);
    }
    format!(
        "Unknown external agent `{agent_id}`. Recognized agent ids: {}.",
        known.join(", ")
    )
}

pub async fn validate_external_agent_for_cli(agent_id: &str) -> Result<ExternalAgentDiscoveryRow> {
    let trimmed = agent_id.trim();
    if trimmed.is_empty() {
        bail!("--agent requires a non-empty agent id");
    }
    let Some(row) = discover_external_agent(trimmed).await else {
        return Err(anyhow!(format_unknown_agent_error(trimmed)));
    };
    if !row.runnable {
        return Err(anyhow!(format_agent_not_runnable_error(&row)));
    }
    Ok(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_agent_not_runnable_error_includes_reason_and_login() {
        let row = ExternalAgentDiscoveryRow {
            id: "antigravity".to_string(),
            name: "Antigravity".to_string(),
            state: "installed".to_string(),
            version: Some("1.0.0".to_string()),
            acp: Some("unreachable".to_string()),
            website: "https://antigravity.google/docs/cli/acp/".to_string(),
            min_version: Some("1.5.0".to_string()),
            upgrade: Some("agy update".to_string()),
            login: Some("agy acp --login".to_string()),
            adapter: None,
            adapter_installed: false,
            needs_update: true,
            runnable: false,
            custom: false,
            reason_code: Some("below_min_version".to_string()),
            reason: Some(
                "Version 1.0.0 is installed; Codewhale needs 1.5.0 or newer to drive it."
                    .to_string(),
            ),
        };
        let message = format_agent_not_runnable_error(&row);
        assert!(message.contains("antigravity"));
        assert!(message.contains("Antigravity"));
        assert!(message.contains("1.5.0"));
        assert!(message.contains("agy update"));
        assert!(!message.contains("agy acp --login"));
    }

    #[test]
    fn resolve_external_agent_posture_preserves_permission_modes() {
        use codewhale_engine::runtime_options::PermissionMode;
        let config = Config::default();
        assert_eq!(
            resolve_external_agent_posture(Some(PermissionMode::Plan), false, &config),
            ("ask", "plan")
        );
        assert_eq!(
            resolve_external_agent_posture(Some(PermissionMode::AcceptEdits), false, &config),
            ("accept-edits", "agent")
        );
        assert_eq!(
            resolve_external_agent_posture(Some(PermissionMode::AutoReview), false, &config),
            ("auto", "agent")
        );
        assert_eq!(
            resolve_external_agent_posture(Some(PermissionMode::Ask), false, &config),
            ("ask", "agent")
        );
        assert_eq!(
            resolve_external_agent_posture(Some(PermissionMode::FullAccess), false, &config),
            ("full-access", "yolo")
        );
        assert_eq!(
            resolve_external_agent_posture(None, true, &config),
            ("full-access", "yolo")
        );
    }

    #[test]
    fn apply_external_event_handles_failed_and_interrupted_turn_completed_and_pops_approval() {
        let temp = tempfile::tempdir().expect("tempdir");
        let config = Config::default();
        let options = crate::tui::app::TuiOptions {
            model: "deepseek-chat".to_string(),
            workspace: temp.path().to_path_buf(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            screen_mode: crate::tui::app::ScreenMode::Fullscreen,
            use_mouse_capture: false,
            mouse_capture_preference: false,
            use_bracketed_paste: false,
            max_subagents: 1,
            skills_dir: temp.path().join("skills"),
            memory_path: temp.path().join("memory"),
            notes_path: temp.path().join("notes"),
            mcp_config_path: temp.path().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
            startup_notice: None,
        };
        let mut app = App::new(options, &config);
        app.is_loading = true;
        apply_external_event(
            &mut app,
            "OpenCode",
            RuntimeEventRecord {
                schema_version: 1,
                seq: 1,
                timestamp: chrono::Utc::now(),
                thread_id: "thr_1".to_string(),
                turn_id: Some("turn_1".to_string()),
                item_id: None,
                event: "turn.completed".to_string(),
                payload: serde_json::json!({
                    "turn": {
                        "id": "turn_1",
                        "status": "failed",
                        "error": "auth expired"
                    }
                }),
            },
        );
        assert!(!app.is_loading);
        assert!(app.history.iter().any(|cell| matches!(
            cell,
            HistoryCell::System { content } if content.contains("OpenCode: auth expired")
        )));

        app.is_loading = true;
        apply_external_event(
            &mut app,
            "OpenCode",
            RuntimeEventRecord {
                schema_version: 1,
                seq: 2,
                timestamp: chrono::Utc::now(),
                thread_id: "thr_1".to_string(),
                turn_id: Some("turn_2".to_string()),
                item_id: None,
                event: "turn.completed".to_string(),
                payload: serde_json::json!({
                    "turn": {
                        "id": "turn_2",
                        "status": "interrupted"
                    }
                }),
            },
        );
        assert!(!app.is_loading);
        assert!(
            app.status_message
                .as_deref()
                .is_some_and(|msg| msg.contains("OpenCode"))
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn liveness_watchdog_and_startup_interrupt_respect_external_agent_session() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let script_path = temp.path().join("fake-acp-2.sh");
        std::fs::write(&script_path, "#!/bin/sh\nexit 0\n").expect("write script");
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");
        let custom_json = serde_json::json!([{
            "id": "test-acp-liveness",
            "name": "Test ACP Liveness",
            "binary": script_path.display().to_string(),
            "args": []
        }])
        .to_string();
        let _env_lock = codewhale_engine::test_support::lock_test_env();
        let _runtime_dir_guard = codewhale_engine::test_support::EnvVarGuard::set(
            "CODEWHALE_RUNTIME_DIR",
            temp.path().join("runtime"),
        );
        let _env_guard = codewhale_engine::test_support::EnvVarGuard::set(
            codewhale_engine::fleet::agents::CUSTOM_AGENTS_ENV,
            custom_json,
        );
        let config = Config::default();
        let mut session = create_external_agent_session(
            &config,
            temp.path().to_path_buf(),
            "ask",
            "agent",
            "test-acp-liveness".to_string(),
            Some("Test ACP Liveness".to_string()),
            Arc::new(codewhale_plugins::PluginRegistry::default()),
        )
        .await
        .expect("ACP thread created");
        let runtime = Arc::clone(&session.runtime);
        let (_tx, rx) = tokio::sync::oneshot::channel::<Result<String, String>>();
        session.turn_start_rx = Some(rx);

        let options = crate::tui::app::TuiOptions {
            model: "deepseek-chat".to_string(),
            workspace: temp.path().to_path_buf(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            screen_mode: crate::tui::app::ScreenMode::Fullscreen,
            use_mouse_capture: false,
            mouse_capture_preference: false,
            use_bracketed_paste: false,
            max_subagents: 1,
            skills_dir: temp.path().join("skills"),
            memory_path: temp.path().join("memory"),
            notes_path: temp.path().join("notes"),
            mcp_config_path: temp.path().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
            startup_notice: None,
        };
        let mut app = App::new(options, &config);
        app.runtime_turn_status = Some("completed".to_string());
        app.is_loading = true;
        app.external_agent = Some(session);

        assert!(!crate::tui::ui::reconcile_turn_liveness(
            &mut app,
            std::time::Instant::now(),
            false,
        ));
        assert!(app.is_loading);

        assert!(interrupt_external_turn(&mut app));
        assert!(app.external_agent.as_ref().unwrap().interrupt_pending);

        let _ = runtime.shutdown_and_wait().await;
    }

    #[test]
    fn format_unknown_agent_error_lists_recognized_agents() {
        let message = format_unknown_agent_error("no-such-agent");
        assert!(message.contains("no-such-agent"));
        assert!(message.contains("antigravity"));
        assert!(message.contains("opencode"));
        assert!(message.contains("copilot"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn create_external_agent_session_wires_acp_executor_and_footer_display() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let script_path = temp.path().join("fake-acp.sh");
        std::fs::write(&script_path, "#!/bin/sh\nexit 0\n").expect("write script");
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod");

        let custom_json = serde_json::json!([{
            "id": "test-acp-agent",
            "name": "Test ACP Agent",
            "binary": script_path.display().to_string(),
            "args": []
        }])
        .to_string();
        let _env_lock = codewhale_engine::test_support::lock_test_env();
        let _runtime_dir_guard = codewhale_engine::test_support::EnvVarGuard::set(
            "CODEWHALE_RUNTIME_DIR",
            temp.path().join("runtime"),
        );
        let _env_guard = codewhale_engine::test_support::EnvVarGuard::set(
            codewhale_engine::fleet::agents::CUSTOM_AGENTS_ENV,
            custom_json,
        );

        let config = Config::default();
        let session = create_external_agent_session(
            &config,
            temp.path().to_path_buf(),
            "ask",
            "agent",
            "test-acp-agent".to_string(),
            Some("Test ACP Agent".to_string()),
            Arc::new(codewhale_plugins::PluginRegistry::default()),
        )
        .await
        .expect("ACP thread should be created");

        assert_eq!(session.agent_id, "test-acp-agent");
        assert_eq!(session.agent_name, "Test ACP Agent");

        let thread = session
            .runtime
            .get_thread(&session.thread_id)
            .await
            .expect("thread record exists");
        let executor = thread.executor.expect("thread has ACP executor");
        assert_eq!(executor.kind, EXECUTOR_KIND_ACP);
        assert_eq!(executor.agent_id, "test-acp-agent");

        let mut options = crate::tui::app::TuiOptions {
            model: "deepseek-chat".to_string(),
            workspace: temp.path().to_path_buf(),
            config_path: None,
            config_profile: None,
            allow_shell: false,
            screen_mode: crate::tui::app::ScreenMode::Fullscreen,
            use_mouse_capture: false,
            mouse_capture_preference: false,
            use_bracketed_paste: false,
            max_subagents: 1,
            skills_dir: temp.path().join("skills"),
            memory_path: temp.path().join("memory"),
            notes_path: temp.path().join("notes"),
            mcp_config_path: temp.path().join("mcp.json"),
            use_memory: false,
            start_in_agent_mode: false,
            skip_onboarding: true,
            yolo: false,
            resume_session_id: None,
            initial_input: None,
            startup_notice: None,
        };
        options.workspace = temp.path().to_path_buf();
        let mut app = App::new(options, &config);
        let runtime = Arc::clone(&session.runtime);
        app.external_agent = Some(session);

        assert_eq!(app.model_display_label(), "Test ACP Agent");
        assert_eq!(
            app.effective_route_identity_display(),
            ("ACP".to_string(), "Test ACP Agent".to_string())
        );
        assert_eq!(app.provable_reasoning_effort_label(), None);

        let _ = runtime.shutdown_and_wait().await;
    }
}

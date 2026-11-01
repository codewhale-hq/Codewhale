//! Low-level tool execution helpers for the engine turn loop.
//!
//! This module keeps the mechanics of MCP dispatch, execution locking, and
//! parallel-tool fanout out of `engine.rs`; the turn loop still owns planning,
//! approval, and how tool results are written back into session state.

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use super::*;

const TOOL_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);

fn inherited_interactive_shell_refusal(tool_name: &str, interactive: bool) -> Option<ToolError> {
    if !interactive || !matches!(tool_name, "bash" | "Bash" | "exec_shell") {
        return None;
    }
    crate::tools::shell::inherited_interactive_terminal_refusal()
        .map(|message| ToolError::execution_failed(message.to_string()))
}

#[cfg(all(any(test, feature = "test-support"), unix))]
thread_local! {
    static REPLAY_SPAN_SEQ: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// Number operation spans on this thread from 1 until the guard drops, the
/// way an otherwise idle process would (see [`OperationSpanGuard::span_id`]).
#[cfg(all(any(test, feature = "test-support"), unix))]
pub fn pin_replay_span_sequence() -> ReplaySpanSequenceGuard {
    REPLAY_SPAN_SEQ.with(|cell| cell.set(Some(1)));
    ReplaySpanSequenceGuard
}

#[cfg(all(any(test, feature = "test-support"), unix))]
pub struct ReplaySpanSequenceGuard;

#[cfg(all(any(test, feature = "test-support"), unix))]
impl Drop for ReplaySpanSequenceGuard {
    fn drop(&mut self) {
        REPLAY_SPAN_SEQ.with(|cell| cell.set(None));
    }
}

/// Pairs an observed `OperationActivityStarted` with at most one completion.
///
/// The turn loop drops an in-flight tool future when the user cancels.
/// Capacity reserved before admission keeps completion synchronous even when
/// a nested call is withdrawn while its parent continues.
pub struct OperationSpanGuard<'a> {
    start: Option<mpsc::Permit<'a, Event>>,
    span: Option<(
        mpsc::Permit<'a, Event>,
        String,
        codewhale_protocol::engine_owner::OwnerActivityKind,
        Option<String>,
    )>,
}

impl<'a> OperationSpanGuard<'a> {
    /// A process-unique span id. The model's tool-call id is not unique:
    /// gateways that elide ids fall back to `call_{block_index}`, which
    /// repeats every step, and a consumer deduplicates completed spans.
    fn span_id(call_id: &str) -> String {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        // A conformance replay numbers its own spans. The process-wide
        // counter also counts every span another test starts in a shared
        // process, so a golden recorded in isolation drifted there (#6698).
        #[cfg(all(any(test, feature = "test-support"), unix))]
        if let Some(seq) = REPLAY_SPAN_SEQ.with(|cell| {
            let seq = cell.get()?;
            cell.set(Some(seq + 1));
            Some(seq)
        }) {
            return format!("{call_id}#{seq}");
        }
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("{call_id}#{seq}")
    }

    pub async fn reserve(
        tx: &'a mpsc::Sender<Event>,
        call_id: &str,
        activity_kind: codewhale_protocol::engine_owner::OwnerActivityKind,
        action_id: Option<String>,
        cancel: Option<CancellationToken>,
    ) -> Self {
        let reserve = tx.reserve_many(2);
        let permits = match cancel.as_ref() {
            Some(cancel) => tokio::select! {
                biased;
                () = cancel.cancelled() => None,
                result = reserve => result.ok(),
            },
            None => reserve.await.ok(),
        };
        let Some(mut permits) = permits else {
            return Self {
                start: None,
                span: None,
            };
        };
        let start = permits.next().expect("reserved activity start");
        let completion = permits.next().expect("reserved activity completion");
        Self {
            start: Some(start),
            span: Some((completion, Self::span_id(call_id), activity_kind, action_id)),
        }
    }

    pub fn activate(mut self) -> Self {
        if let Some(start) = self.start.take()
            && let Some((_, span_id, activity_kind, action_id)) = &self.span
        {
            start.send(Event::OperationActivityStarted {
                span_id: span_id.clone(),
                activity_kind: *activity_kind,
                action_id: action_id.clone(),
            });
        }
        self
    }

    pub async fn start(
        tx: &'a mpsc::Sender<Event>,
        call_id: &str,
        activity_kind: codewhale_protocol::engine_owner::OwnerActivityKind,
        action_id: Option<String>,
        cancel: Option<CancellationToken>,
    ) -> Self {
        Self::reserve(tx, call_id, activity_kind, action_id, cancel)
            .await
            .activate()
    }

    pub async fn complete(
        mut self,
        outcome: codewhale_protocol::engine_owner::OwnerOperationOutcome,
    ) {
        if self.start.is_none()
            && let Some((permit, span_id, activity_kind, action_id)) = self.span.take()
        {
            permit.send(Event::OperationActivityCompleted {
                span_id,
                activity_kind,
                action_id,
                outcome,
            });
        }
    }
}

impl Drop for OperationSpanGuard<'_> {
    fn drop(&mut self) {
        if self.start.is_none()
            && let Some((permit, span_id, activity_kind, action_id)) = self.span.take()
        {
            permit.send(Event::OperationActivityCompleted {
                span_id,
                activity_kind,
                action_id,
                outcome: codewhale_protocol::engine_owner::OwnerOperationOutcome::Cancelled,
            });
        }
    }
}

/// Emits delayed, best-effort liveness pulses for one running tool.
///
/// Keep the ticker in its own task instead of embedding `tokio::time::Interval`
/// in the already-large engine turn future. Besides keeping the turn future
/// compact, this leaves pre-execution MCP discovery and approval scheduling
/// untouched. Dropping the guard cancels and aborts the ticker synchronously.
struct ToolHeartbeatGuard {
    cancel: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<()>,
}

impl ToolHeartbeatGuard {
    fn start(tx_event: mpsc::Sender<Event>, interval: Duration) -> Self {
        let cancel = tokio_util::sync::CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Tokio intervals tick immediately once. Consume that tick so fast
            // tools do not produce a pulse and the first heartbeat is delayed.
            ticker.tick().await;

            loop {
                tokio::select! {
                    biased;

                    () = task_cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        match tx_event.try_send(Event::ToolCallHeartbeat) {
                            Ok(()) | Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => break,
                        }
                    }
                }
            }
        });
        Self { cancel, task }
    }
}

impl Drop for ToolHeartbeatGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

/// RAII guard that pauses the TUI's terminal-state ownership for the duration
/// of an interactive tool, then restores it on drop.
///
/// Background: interactive tools (anything that needs the raw TTY — external
/// editor, `exec_shell` with stdin, etc.) need the TUI to leave alt-screen,
/// disable raw mode, and release mouse capture so the child sees a normal
/// terminal. The TUI listens for `Event::PauseEvents` / `Event::ResumeEvents`
/// and runs `pause_terminal` / `resume_terminal` in response.
///
/// Earlier code sent `PauseEvents` before tool execution and `ResumeEvents`
/// after. That worked on the happy path, but if the tool's future was dropped
/// — Ctrl+C cancellation, sub-agent abort, parent task cancelled while the
/// tool was awaiting — the second `await` never reached and `ResumeEvents`
/// was never sent. It also let interactive children start before the UI had
/// actually left alt-screen/raw mode. Both failures strand the TUI in a
/// regular shell scrollback: the parent shell scrollbar takes over, mouse
/// wheel scrolls the host terminal instead of the transcript, and the TUI
/// renders at the bottom of cooked-mode output.
///
/// Reserve the matching resume before pausing. `Drop` consumes that permit
/// synchronously, including on cancellation and a full channel. No detached
/// sender can outlive the tool or compete with the turn's terminal event.
pub(super) struct InteractiveTerminalGuard {
    resume: Option<(mpsc::Sender<Event>, mpsc::OwnedPermit<Event>)>,
}

impl InteractiveTerminalGuard {
    /// Send `PauseEvents` and arm the guard. If `interactive` is false the
    /// guard is a no-op — `Drop` will skip the resume.
    pub(super) async fn engage(
        tx: mpsc::Sender<Event>,
        interactive: bool,
        cancel: Option<CancellationToken>,
    ) -> Result<Self, ToolError> {
        if !interactive {
            return Ok(Self { resume: None });
        }
        let resume = super::streaming::reserve_event_capacity(
            &tx,
            cancel.as_ref(),
            super::streaming::EventReservationPolicy::Strict,
        )
        .await
        .map_err(terminal_handoff_send_error)?;
        let ack = Arc::new(tokio::sync::Notify::new());
        let pause = super::streaming::reserve_event_capacity(
            &tx,
            cancel.as_ref(),
            super::streaming::EventReservationPolicy::Strict,
        )
        .await
        .map_err(terminal_handoff_send_error)?;
        // No await separates the pause send and guard installation. A
        // cancelled reservation above never paused and needs no resume.
        pause.send(Event::PauseEvents {
            ack: Some(ack.clone()),
        });
        let guard = Self {
            resume: Some((tx, resume)),
        };
        let acknowledged = match cancel.as_ref() {
            Some(cancel) => tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    return Err(ToolError::cancelled("Terminal handoff cancelled; interactive tool was not launched."));
                }
                result = tokio::time::timeout(Duration::from_millis(750), ack.notified()) => result,
            },
            None => tokio::time::timeout(Duration::from_millis(750), ack.notified()).await,
        };
        if acknowledged.is_err() {
            return Err(ToolError::execution_failed(
                "Terminal handoff was not acknowledged; interactive tool was not launched.",
            ));
        }
        Ok(guard)
    }
}

fn terminal_handoff_send_error(reason: super::streaming::EventSendError) -> ToolError {
    match reason {
        super::streaming::EventSendError::Cancelled => {
            ToolError::cancelled("Terminal handoff cancelled; interactive tool was not launched.")
        }
        super::streaming::EventSendError::Closed => ToolError::execution_failed(
            "Terminal handoff channel closed; interactive tool was not launched.",
        ),
    }
}

impl Drop for InteractiveTerminalGuard {
    fn drop(&mut self) {
        if let Some((tx, resume)) = self.resume.take()
            && !tx.is_closed()
        {
            resume.send(Event::ResumeEvents);
        }
    }
}

pub fn emit_tool_audit(event: serde_json::Value) {
    let Some(path) = std::env::var_os("CODEWHALE_TOOL_AUDIT_LOG")
        .or_else(|| std::env::var_os("DEEPSEEK_TOOL_AUDIT_LOG"))
    else {
        return;
    };
    emit_tool_audit_to_path(&PathBuf::from(path), event);
}

fn emit_tool_audit_to_path(path: &Path, event: serde_json::Value) {
    let line = match serde_json::to_string(&event) {
        Ok(line) => line,
        Err(e) => {
            tracing::error!("Failed to serialize tool audit event: {e}");
            return;
        }
    };
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::error!(
            "Failed to create audit log directory {}: {e}",
            parent.display()
        );
        return;
    }
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(mut file) => {
            if let Err(e) = writeln!(file, "{line}") {
                tracing::error!("Failed to write to audit log {}: {e}", path.display());
            }
        }
        Err(e) => {
            tracing::error!("Failed to open audit log {}: {e}", path.display());
        }
    }
}

impl Engine {
    pub async fn execute_mcp_tool_with_pool(
        pool: Arc<AsyncMutex<McpPool>>,
        tx_event: &mpsc::Sender<Event>,
        name: &str,
        input: serde_json::Value,
        disallowed_tools: &[String],
        decision: Option<&super::HumanDecision>,
    ) -> Result<RichToolResult, ToolError> {
        McpPool::authorize_call(disallowed_tools, name, &input)
            .map_err(|error| ToolError::not_available(error.to_string()))?;
        // A synthetic `mcp_<server>_authenticate` call runs the shared OAuth
        // login flow with the pool lock released during the browser wait, so
        // parallel MCP tools and the `/mcp` manager keep working while the
        // user signs in. On success it changes the callable MCP surface (the
        // server's real tools replace the synthetic one); flag that so the
        // turn loop merges the refreshed catalog before the next model
        // request instead of leaving the model with names it cannot legally
        // call yet.
        let auth_target = pool.lock().await.authenticate_tool_target(name);
        if let Some(server) = auth_target {
            let mut notice_task = None;
            let mut result = crate::mcp::authenticate_tool_via_pool(&pool, &server, |url| {
                // The model cannot relay the URL until the call returns, and
                // the call returns only after the sign-in completes — so the
                // user must see it now. This status is the only copy of the
                // URL: `try_send` drops it on a full channel, so the send
                // waits for room on its own task instead of failing the
                // login.
                let server = server.clone();
                let url = url.to_string();
                let tx = tx_event.clone();
                notice_task = Some(super::turn_heartbeat::AbortOnDrop(tokio::spawn(async move {
                    if let Ok(permit) = super::streaming::reserve_event_capacity(
                        &tx,
                        None,
                        super::streaming::EventReservationPolicy::Receipt,
                    )
                    .await
                    {
                        permit.send(Event::status(format!(
                            "◆ auth required: sign in to MCP server '{server}' in your browser — {url}"
                        )));
                    }
                })));
            })
            .await
            .map_err(|e| ToolError::execution_failed(format!("MCP tool failed: {e}")))?;
            if let Some(task) = notice_task.as_mut() {
                let _ = (&mut task.0).await;
            }
            McpPool::filter_authenticate_result(&mut result, disallowed_tools);
            let mut rich = crate::tools::registry::mcp_result_to_bounded_rich_tool_result(result);
            if rich.result.success {
                rich.result.metadata = Some(serde_json::json!({ "mcp_catalog_changed": true }));
            }
            return Ok(rich);
        }
        let needs_auth_generation_before = pool.lock().await.needs_auth_generation();
        let result = pool
            .lock()
            .await
            .call_tool_with_disallowed(name, input, disallowed_tools, decision)
            .await;
        match result {
            Ok(result) => {
                Ok(crate::tools::registry::mcp_result_to_bounded_rich_tool_result(result))
            }
            Err(error) => {
                // A credential the server stopped accepting mid-session
                // flips it into the typed needs-auth state and drops its
                // connection — the callable surface changed. For THAT
                // transition only, return the failure as a result (not an
                // Err) carrying `mcp_catalog_changed`, so the turn loop
                // replaces the pool's catalog slice: dead tools leave, the
                // synthetic login tool arrives, and the error's own hint
                // stays model-readable. Every other failure keeps the Err
                // contract.
                let auth_surface_changed = {
                    let pool = pool.lock().await;
                    pool.needs_auth_generation() != needs_auth_generation_before
                };
                if !auth_surface_changed {
                    return Err(ToolError::execution_failed(format!(
                        "MCP tool failed: {error}"
                    )));
                }
                let tool_result =
                    crate::tools::spec::ToolResult::error(format!("MCP tool failed: {error}"))
                        .with_metadata(serde_json::json!({ "mcp_catalog_changed": true }));
                Ok(RichToolResult::plain(tool_result))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_tool_with_lock(
        lock: Arc<RwLock<()>>,
        supports_parallel: bool,
        interactive: bool,
        tx_event: mpsc::Sender<Event>,
        cancel_token: Option<CancellationToken>,
        tool_name: String,
        activity_call_id: Option<String>,
        tool_input: serde_json::Value,
        workspace: PathBuf,
        registry: Option<&crate::tools::ToolRegistry>,
        mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
        context_override: Option<crate::tools::ToolContext>,
    ) -> Result<RichToolResult, ToolError> {
        let child = context_override
            .as_ref()
            .or_else(|| registry.map(|registry| registry.context()))
            .and_then(|context| context.child_host.clone())
            .filter(|_| tool_name != EXECUTE_TOOLS_TOOL_NAME);
        let execution = Self::execute_tool_admitted(
            lock,
            supports_parallel,
            interactive,
            tx_event,
            cancel_token,
            tool_name,
            activity_call_id,
            tool_input,
            workspace,
            registry,
            mcp_pool,
            context_override,
        );
        match child {
            Some(child) => child.run_tool_bounded(execution).await,
            None => execution.await,
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn execute_tool_admitted(
        lock: Arc<RwLock<()>>,
        supports_parallel: bool,
        interactive: bool,
        tx_event: mpsc::Sender<Event>,
        cancel_token: Option<CancellationToken>,
        tool_name: String,
        activity_call_id: Option<String>,
        tool_input: serde_json::Value,
        workspace: PathBuf,
        registry: Option<&crate::tools::ToolRegistry>,
        mcp_pool: Option<Arc<AsyncMutex<McpPool>>>,
        context_override: Option<crate::tools::ToolContext>,
    ) -> Result<RichToolResult, ToolError> {
        if cancel_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ToolError::permission_denied(
                "Turn stopped by user. Tool call blocked.",
            ));
        }
        // Unix inherited-terminal shell calls are impossible without a full
        // POSIX job-control lease. Refuse them before the terminal guard so a
        // known-invalid call cannot flash host scrollback or drain input.
        if let Some(error) = inherited_interactive_shell_refusal(&tool_name, interactive) {
            return Err(error);
        }
        // This guard starts before lock acquisition, so contention as well as
        // registry/MCP/interpreter execution remains visibly live.
        let _heartbeat = ToolHeartbeatGuard::start(tx_event.clone(), TOOL_HEARTBEAT_INTERVAL);
        let started_at = std::time::Instant::now();
        let dispatch = if McpPool::is_mcp_tool(&tool_name) {
            "mcp"
        } else if matches!(
            tool_name.as_str(),
            CODE_EXECUTION_TOOL_NAME | JS_EXECUTION_TOOL_NAME | EXECUTE_TOOLS_TOOL_NAME
        ) {
            "interpreter"
        } else if registry.is_some() {
            "registry"
        } else {
            "missing"
        };
        let input_bytes = serde_json::to_string(&tool_input)
            .map(|s| s.len())
            .unwrap_or(0);
        tracing::debug!(
            target: "engine.tool_execution",
            tool = %tool_name,
            dispatch,
            interactive,
            supports_parallel,
            input_bytes,
            "tool.exec.start",
        );

        let acquire = async {
            if supports_parallel {
                ToolExecGuard::Read(lock.read().await)
            } else {
                ToolExecGuard::Write(lock.write().await)
            }
        };
        let _guard = match cancel_token.as_ref() {
            Some(cancel) => tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(ToolError::cancelled("Tool lock wait cancelled.")),
                guard = acquire => guard,
            },
            None => acquire.await,
        };

        // RAII pause/resume: ensures `Event::ResumeEvents` always fires on
        // drop, even if the tool future is cancelled mid-await. See
        // `InteractiveTerminalGuard` doc-comment for the regression this
        // closes (parent terminal scrollback hijacking the TUI after a
        // cancelled interactive tool).
        let _terminal =
            InteractiveTerminalGuard::engage(tx_event.clone(), interactive, cancel_token.clone())
                .await?;

        if cancel_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ToolError::permission_denied(
                "Turn stopped by user. Tool call blocked.",
            ));
        }

        if let Some(context) = context_override
            .as_ref()
            .or_else(|| registry.map(|registry| registry.context()))
        {
            crate::tools::execution_envelope::enforce_tool_denial(
                context,
                &tool_name,
                &tool_input,
            )?;
        }

        let tool_authority = context_override
            .as_ref()
            .and_then(|context| context.tool_authority.as_ref())
            .or_else(|| registry.and_then(|registry| registry.context().tool_authority.as_ref()));
        if let Some(authority) = tool_authority {
            if McpPool::is_mcp_tool(&tool_name)
                && !super::dispatch::mcp_tool_is_read_only(&tool_name)
            {
                return Err(ToolError::permission_denied(format!(
                    "worker '{}' cannot run mutating MCP tool {tool_name}: it has no authorized file target",
                    authority.owner
                )));
            }
            if matches!(
                tool_name.as_str(),
                CODE_EXECUTION_TOOL_NAME | JS_EXECUTION_TOOL_NAME | EXECUTE_TOOLS_TOOL_NAME
            ) {
                return Err(ToolError::permission_denied(format!(
                    "worker '{}' cannot run {tool_name}: arbitrary code execution is outside its machine-readable authority envelope",
                    authority.owner
                )));
            }
        }

        // Typed owner activity: classified only after every gate above has
        // passed, from the same authority that dispatches the call (the MCP
        // pool's resolved server map, the interpreter, or the registry plus
        // the canonical action alias). Arguments never leave here.
        let activity_kind = match activity_call_id.as_ref() {
            None => None,
            Some(_) if McpPool::is_mcp_tool(&tool_name) => match mcp_pool.as_ref() {
                Some(pool) => pool
                    .lock()
                    .await
                    .resolved_tool_servers()
                    .get(&tool_name)
                    .map(|server| crate::tools::activity::mcp_activity_kind(server)),
                None => None,
            },
            Some(_)
                if matches!(
                    tool_name.as_str(),
                    CODE_EXECUTION_TOOL_NAME | JS_EXECUTION_TOOL_NAME
                ) =>
            {
                Some(codewhale_protocol::engine_owner::OwnerActivityKind::Executing)
            }
            Some(_) => registry
                .filter(|registry| registry.get(&tool_name).is_some())
                .and_then(|_| {
                    crate::tools::activity::registry_activity_kind(&tool_name, &tool_input)
                }),
        };
        if cancel_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ToolError::cancelled("Tool activity admission cancelled."));
        }

        let child_mcp_call = if McpPool::is_mcp_tool(&tool_name) {
            let context = context_override
                .as_ref()
                .or_else(|| registry.map(|registry| registry.context()));
            if context.is_some_and(|context| context.child_host.is_some()) {
                let registry = registry.ok_or_else(|| {
                    ToolError::permission_denied(
                        "child MCP call has no canonical registered capability",
                    )
                })?;
                registry
                    .admit_child_call(
                        &tool_name,
                        &tool_input,
                        context.expect("captured child context"),
                    )
                    .await?
            } else {
                None
            }
        } else {
            None
        };
        let context = context_override
            .as_ref()
            .or_else(|| registry.map(|registry| registry.context()));
        let execution_receipt = if context
            .is_some_and(|context| context.acp_host.is_some() || context.child_host.is_some())
            && activity_call_id.is_some()
        {
            Some(
                super::streaming::reserve_event_capacity(
                    &tx_event,
                    cancel_token.as_ref(),
                    super::streaming::EventReservationPolicy::Receipt,
                )
                .await
                .map_err(|_| ToolError::cancelled("ACP dispatch observation unavailable"))?,
            )
        } else {
            None
        };
        let operation_span = match (activity_call_id.as_deref(), activity_kind) {
            (Some(call_id), Some(kind)) => Some(
                OperationSpanGuard::reserve(
                    &tx_event,
                    call_id,
                    kind,
                    Some(crate::tools::activity::action_id(&tool_name, &tool_input)),
                    cancel_token.clone(),
                )
                .await,
            ),
            _ => None,
        };
        let on_admitted = || {
            if cancel_token
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return Err(ToolError::cancelled("Tool activity admission cancelled."));
            }
            if let Some(context) = context
                && (context.acp_host.is_some() || context.child_host.is_some())
            {
                if context.acp_host.is_some() || tool_name != EXECUTE_TOOLS_TOOL_NAME {
                    let spec = registry
                        .and_then(|registry| registry.get(&tool_name))
                        .ok_or_else(|| {
                            ToolError::not_available("ACP call has no admitted registered tool")
                        })?;
                    crate::tools::registry::enforce_tool_authority(
                        &tool_name,
                        &tool_input,
                        spec.as_ref(),
                        context,
                    )?;
                }
                crate::extension_host::validate_caller_plugins(context.plugin_registry.as_deref())
                    .map_err(ToolError::not_available)?;
            }
            if let Some(permit) = execution_receipt {
                permit.send(Event::ToolExecutionStarted {
                    id: activity_call_id
                        .as_ref()
                        .expect("reserved execution identity")
                        .clone(),
                });
            }
            Ok(operation_span.map(OperationSpanGuard::activate))
        };
        let registry_dispatch = !McpPool::is_mcp_tool(&tool_name)
            && !matches!(
                tool_name.as_str(),
                CODE_EXECUTION_TOOL_NAME | JS_EXECUTION_TOOL_NAME | EXECUTE_TOOLS_TOOL_NAME
            );
        let outcome = if registry_dispatch {
            match registry {
                Some(registry) => {
                    registry
                        .execute_rich_full_with_context(
                            &tool_name,
                            tool_input.clone(),
                            context_override.as_ref(),
                            on_admitted,
                        )
                        .await
                }
                None => Err(ToolError::not_available(format!(
                    "tool '{tool_name}' is not registered"
                ))),
            }
        } else {
            let operation_span = on_admitted()?;
            let outcome: Result<RichToolResult, ToolError> = if McpPool::is_mcp_tool(&tool_name) {
                if let Some(pool) = mcp_pool {
                    let disallowed_tools = context_override
                        .as_ref()
                        .or_else(|| registry.map(|registry| registry.context()))
                        .map(|context| context.disallowed_tools.as_slice())
                        .unwrap_or_default();
                    // Only a per-call override can carry a person's decision; the
                    // registry's shared context never does.
                    let decision = context_override
                        .as_ref()
                        .and_then(|context| context.human_decision.as_ref());
                    Engine::execute_mcp_tool_with_pool(
                        pool,
                        &tx_event,
                        &tool_name,
                        tool_input,
                        disallowed_tools,
                        decision,
                    )
                    .await
                } else {
                    Err(ToolError::not_available(format!(
                        "tool '{tool_name}' is not registered"
                    )))
                }
            } else if matches!(
                tool_name.as_str(),
                CODE_EXECUTION_TOOL_NAME | JS_EXECUTION_TOOL_NAME
            ) {
                if let Some(context) = context_override
                    .as_ref()
                    .or_else(|| registry.map(|registry| registry.context()))
                {
                    let result = if tool_name == CODE_EXECUTION_TOOL_NAME {
                        execute_code_execution_tool(&tool_input, &workspace, context).await
                    } else {
                        execute_js_execution_tool(&tool_input, &workspace, context).await
                    };
                    result.map(RichToolResult::plain)
                } else {
                    Err(ToolError::not_available(
                        "local code execution requires an effective tool context",
                    ))
                }
            } else if tool_name == EXECUTE_TOOLS_TOOL_NAME {
                if let Some(registry) = registry {
                    let context = context_override
                        .as_ref()
                        .cloned()
                        .unwrap_or_else(|| registry.context().clone());
                    crate::tools::codemode::execute_tools_tool(&tool_input, registry, &context)
                        .await
                        .map(RichToolResult::plain)
                } else {
                    Err(ToolError::not_available(format!(
                        "tool '{tool_name}' is not registered"
                    )))
                }
            } else {
                Err(ToolError::not_available(format!(
                    "tool '{tool_name}' is not registered"
                )))
            };
            if let Some(operation_span) = operation_span {
                operation_span
                    .complete(crate::tools::activity::operation_outcome(
                        &outcome,
                        cancel_token
                            .as_ref()
                            .is_some_and(CancellationToken::is_cancelled),
                    ))
                    .await;
            }
            outcome
        };

        if outcome.as_ref().is_ok_and(|result| result.result.success)
            && let Some((authority, writes)) = child_mcp_call
        {
            authority.record_settled_writes(writes).await;
        }
        let duration_ms = started_at.elapsed().as_millis() as u64;
        // The surface-agnostic choke point for every tool call, so this one
        // bump covers exec and the CLI as well as the TUI. `memory_search` is
        // counted here for the same reason — one site, not one per tool.
        let telemetry = codewhale_telemetry::session_counters();
        telemetry.bump(codewhale_telemetry::Counter::ToolCalls);
        telemetry.record_tool_call(
            &tool_name,
            dispatch == "mcp",
            !outcome.as_ref().is_ok_and(|result| result.result.success),
        );
        if tool_name == "memory_search" {
            telemetry.bump(codewhale_telemetry::Counter::MemorySearch);
        }
        match &outcome {
            Ok(result) => {
                tracing::debug!(
                    target: "engine.tool_execution",
                    tool = %tool_name,
                    dispatch,
                    duration_ms,
                    success = result.result.success,
                    output_bytes = result.result.content.len(),
                    "tool.exec.end",
                );
            }
            Err(err) => {
                let kind = match err {
                    ToolError::InvalidInput { .. } => "invalid_input",
                    ToolError::MissingField { .. } => "missing_field",
                    ToolError::PathEscape { .. } => "path_escape",
                    ToolError::ExecutionFailed { .. } => "execution_failed",
                    ToolError::Timeout { .. } => "timeout",
                    ToolError::Cancelled { .. } => "cancelled",
                    ToolError::NotAvailable { .. } => "not_available",
                    ToolError::PermissionDenied { .. } => "permission_denied",
                };
                // The discriminant and nothing else. `ToolError::PathEscape`'s
                // `Display` *is* an absolute path, and several sibling
                // variants render a literal source fragment the model emitted.
                match err {
                    ToolError::PermissionDenied { .. } => {
                        telemetry.bump_error(codewhale_telemetry::ErrorCounter::ToolDeniedByPolicy)
                    }
                    ToolError::Timeout { .. } => {
                        telemetry.bump_error(codewhale_telemetry::ErrorCounter::ToolTimeout);
                    }
                    _ => {}
                }
                tracing::warn!(
                    target: "engine.tool_execution",
                    tool = %tool_name,
                    dispatch,
                    duration_ms,
                    error_kind = kind,
                    error = %err,
                    "tool.exec.end",
                );
            }
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    const TEST_HEARTBEAT_INTERVAL: Duration = Duration::from_millis(10);

    #[tokio::test]
    async fn tool_heartbeat_emits_for_slow_tool() {
        let (tx, mut rx) = mpsc::channel(4);
        let guard = ToolHeartbeatGuard::start(tx, TEST_HEARTBEAT_INTERVAL);

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("heartbeat before slow tool completes")
            .expect("event channel stays open");

        assert!(matches!(event, Event::ToolCallHeartbeat));
        drop(guard);
    }

    #[tokio::test]
    async fn event_capacity_cancelled_activity_preserves_exact_outcomes_and_releases_full_queue() {
        use codewhale_protocol::engine_owner::{OwnerActivityKind, OwnerOperationOutcome};
        for outcome in [
            OwnerOperationOutcome::Succeeded,
            OwnerOperationOutcome::Failed,
            OwnerOperationOutcome::Denied,
            OwnerOperationOutcome::Cancelled,
        ] {
            let (tx, mut rx) = mpsc::channel(2);
            let cancel = CancellationToken::new();
            let span = OperationSpanGuard::start(
                &tx,
                "completed-call",
                OwnerActivityKind::Tool,
                Some("tool".into()),
                Some(cancel.clone()),
            )
            .await;
            cancel.cancel();
            tokio::time::timeout(Duration::from_millis(100), span.complete(outcome))
                .await
                .expect("completed activity preserves available capacity after cancellation");
            let Event::OperationActivityStarted {
                span_id,
                activity_kind,
                action_id,
            } = rx.try_recv().unwrap()
            else {
                panic!("activity start");
            };
            let Event::OperationActivityCompleted {
                span_id: completed_span,
                activity_kind: completed_kind,
                outcome: completed_outcome,
                action_id: completed_action,
            } = rx
                .try_recv()
                .expect("cancellation must not discard the completion")
            else {
                panic!("activity completion");
            };
            assert_eq!(completed_span, span_id);
            assert_eq!(completed_kind, activity_kind);
            assert_eq!(completed_action, action_id);
            assert_eq!(
                completed_outcome, outcome,
                "retain the observed outcome exactly"
            );
            assert!(rx.try_recv().is_err(), "exactly one completion");
        }

        let (tx, mut rx) = mpsc::channel(512);
        let mut spans = Vec::new();
        for i in 0..256 {
            spans.push(
                OperationSpanGuard::start(
                    &tx,
                    &format!("full-call-{i}"),
                    OwnerActivityKind::Tool,
                    Some("tool".into()),
                    None,
                )
                .await,
            );
        }
        assert_eq!(tx.capacity(), 0);
        drop(spans);
        let mut starts = 0;
        let mut cancelled = 0;
        while let Ok(event) = rx.try_recv() {
            match event {
                Event::OperationActivityStarted { .. } => starts += 1,
                Event::OperationActivityCompleted {
                    outcome: OwnerOperationOutcome::Cancelled,
                    ..
                } => cancelled += 1,
                other => panic!("unexpected activity receipt: {other:?}"),
            }
        }
        assert_eq!((starts, cancelled), (256, 256));
        assert_eq!(tx.capacity(), 512);
        let (tx, mut rx) = mpsc::channel(1);
        let span =
            OperationSpanGuard::start(&tx, "too-small", OwnerActivityKind::Tool, None, None).await;
        drop(span);
        assert!(rx.try_recv().is_err(), "never publish an unpaired start");
    }

    #[tokio::test]
    async fn tool_heartbeat_is_delayed_for_fast_tool() {
        let (tx, mut rx) = mpsc::channel(4);

        let guard = ToolHeartbeatGuard::start(tx, TEST_HEARTBEAT_INTERVAL);
        drop(guard);
        tokio::time::sleep(TEST_HEARTBEAT_INTERVAL * 2).await;

        assert!(rx.try_recv().is_err(), "fast tool emitted a heartbeat");
    }

    #[tokio::test]
    async fn tool_heartbeat_stops_after_tool_completes() {
        let (tx, mut rx) = mpsc::channel(8);
        let guard = ToolHeartbeatGuard::start(tx, TEST_HEARTBEAT_INTERVAL);

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("heartbeat before slow tool completes")
            .expect("event channel stays open");
        assert!(matches!(event, Event::ToolCallHeartbeat));

        drop(guard);
        tokio::task::yield_now().await;
        while rx.try_recv().is_ok() {}
        tokio::time::sleep(TEST_HEARTBEAT_INTERVAL * 2).await;
        assert!(
            rx.try_recv().is_err(),
            "heartbeat continued after tool completion"
        );
    }

    #[tokio::test]
    async fn full_event_channel_never_blocks_tool_heartbeat() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(Event::status("filler")).expect("fill channel");

        let result = tokio::time::timeout(Duration::from_secs(1), async {
            let guard = ToolHeartbeatGuard::start(tx, TEST_HEARTBEAT_INTERVAL);
            tokio::time::sleep(TEST_HEARTBEAT_INTERVAL * 3).await;
            drop(guard);
            "done"
        })
        .await
        .expect("full event channel must not block tool completion");

        assert_eq!(result, "done");
        assert!(matches!(rx.recv().await, Some(Event::Status { .. })));
        assert!(rx.try_recv().is_err(), "heartbeat displaced queued event");
    }

    #[tokio::test]
    async fn terminal_guard_queues_resume_when_event_channel_is_full() {
        let (tx, mut rx) = mpsc::channel(2);
        let resume = tx.clone().reserve_owned().await.expect("resume capacity");
        tx.try_send(Event::status("filler")).expect("fill channel");

        drop(InteractiveTerminalGuard {
            resume: Some((tx, resume)),
        });

        assert!(matches!(rx.recv().await, Some(Event::Status { .. })));
        let resumed = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("queued resume event")
            .expect("event channel still open");
        assert!(matches!(resumed, Event::ResumeEvents));
        assert!(rx.try_recv().is_err(), "restoration is exactly once");
    }

    #[tokio::test]
    async fn terminal_guard_waits_for_pause_ack_before_returning() {
        let (tx, mut rx) = mpsc::channel(4);
        let task = tokio::spawn(InteractiveTerminalGuard::engage(tx, true, None));

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("pause event")
            .expect("event channel still open");
        let ack = match event {
            Event::PauseEvents { ack: Some(ack) } => ack,
            other => panic!("expected PauseEvents with ack, got {other:?}"),
        };

        tokio::task::yield_now().await;
        assert!(!task.is_finished(), "guard returned before pause ack");

        ack.notify_one();
        let guard = tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("guard returned after ack")
            .expect("guard task joined")
            .expect("terminal handoff acknowledged");

        drop(guard);
        let resumed = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("resume event")
            .expect("event channel still open");
        assert!(matches!(resumed, Event::ResumeEvents));
    }

    #[tokio::test]
    async fn terminal_guard_refuses_child_and_queues_resume_when_pause_is_not_acknowledged() {
        let (tx, mut rx) = mpsc::channel(4);
        let task = tokio::spawn(InteractiveTerminalGuard::engage(tx, true, None));

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("pause event")
            .expect("event channel still open");
        let _unacknowledged_pause = match event {
            Event::PauseEvents { ack: Some(ack) } => ack,
            other => panic!("expected PauseEvents with ack, got {other:?}"),
        };

        let handoff = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("guard refused child after pause timeout")
            .expect("guard task joined");
        let err = match handoff {
            Ok(_) => panic!("unacknowledged terminal handoff must fail closed"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("was not acknowledged"),
            "unexpected handoff error: {err}"
        );

        let resumed = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("queued resume event")
            .expect("event channel still open");
        assert!(matches!(resumed, Event::ResumeEvents));
    }

    #[tokio::test]
    async fn terminal_guard_cancellation_during_pause_ack_still_queues_resume() {
        let (tx, mut rx) = mpsc::channel(4);
        let task = tokio::spawn(InteractiveTerminalGuard::engage(tx, true, None));

        let event = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("pause event")
            .expect("event channel still open");
        let _unacknowledged_pause = match event {
            Event::PauseEvents { ack: Some(ack) } => ack,
            other => panic!("expected PauseEvents with ack, got {other:?}"),
        };

        task.abort();
        let cancelled = match task.await {
            Ok(_) => panic!("engage future should be cancelled"),
            Err(cancelled) => cancelled,
        };
        assert!(cancelled.is_cancelled());

        let resumed = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("queued resume event")
            .expect("event channel still open");
        assert!(matches!(resumed, Event::ResumeEvents));
    }

    #[tokio::test]
    async fn terminal_guard_cancelled_capacity_reservations_never_pause_or_restore() {
        for capacity in [1, 2] {
            let (tx, mut rx) = mpsc::channel(capacity);
            tx.try_send(Event::status("occupied")).unwrap();
            let cancel = CancellationToken::new();
            let mut engage = Box::pin(InteractiveTerminalGuard::engage(
                tx.clone(),
                true,
                Some(cancel.clone()),
            ));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), &mut engage)
                    .await
                    .is_err()
            );
            // Capacity 1 stalls the resume reservation; capacity 2 stalls
            // the pause reservation while the first permit is held.
            cancel.cancel();
            assert!(
                tokio::time::timeout(Duration::from_secs(1), &mut engage)
                    .await
                    .unwrap()
                    .is_err()
            );
            drop(engage);
            assert!(matches!(rx.try_recv(), Ok(Event::Status { .. })));
            assert!(
                rx.try_recv().is_err(),
                "a pause that never entered needs no resume"
            );
            assert_eq!(
                tx.capacity(),
                capacity,
                "cancelled waits release every permit"
            );
        }
    }

    #[tokio::test]
    async fn terminal_guard_cancelled_ack_restores_once_without_detached_sender() {
        let (tx, mut rx) = mpsc::channel(3);
        let cancel = CancellationToken::new();
        let task = tokio::spawn(InteractiveTerminalGuard::engage(
            tx.clone(),
            true,
            Some(cancel.clone()),
        ));
        assert!(matches!(rx.recv().await, Some(Event::PauseEvents { .. })));
        tx.try_send(Event::status("fill after pause")).unwrap();
        cancel.cancel();
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert!(matches!(rx.try_recv(), Ok(Event::Status { .. })));
        assert!(matches!(rx.try_recv(), Ok(Event::ResumeEvents)));
        assert!(rx.try_recv().is_err());
        assert_eq!(tx.capacity(), 3);
    }

    #[tokio::test]
    async fn terminal_guard_closed_receiver_refuses_or_releases_held_restoration() {
        let (tx, rx) = mpsc::channel(3);
        drop(rx);
        assert!(
            InteractiveTerminalGuard::engage(tx, true, None)
                .await
                .is_err()
        );

        let (tx, mut rx) = mpsc::channel(3);
        let task = tokio::spawn(InteractiveTerminalGuard::engage(tx.clone(), true, None));
        let Some(Event::PauseEvents { ack: Some(ack) }) = rx.recv().await else {
            panic!("pause with acknowledgement");
        };
        ack.notify_one();
        let guard = task.await.unwrap().unwrap();
        drop(rx);
        drop(guard);
        assert!(tx.is_closed());
        assert_eq!(
            tx.capacity(),
            3,
            "receiver closure cannot strand restoration capacity"
        );
    }

    #[cfg(unix)]
    #[test]
    fn inherited_interactive_shell_is_refused_before_terminal_handoff() {
        for tool_name in ["bash", "Bash", "exec_shell"] {
            let err = inherited_interactive_shell_refusal(tool_name, true)
                .expect("Unix inherited-interactive shell must fail preflight");
            assert!(
                err.to_string().contains("foreground TTY ownership"),
                "{tool_name}: {err}"
            );
        }
        assert!(inherited_interactive_shell_refusal("Bash", false).is_none());
        assert!(
            inherited_interactive_shell_refusal(REQUEST_USER_INPUT_NAME, true).is_none(),
            "user-input modal keeps its own terminal handoff"
        );
    }

    #[test]
    fn emit_tool_audit_to_path_writes_jsonl_lines() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("audit.log");
        let marker = path.display().to_string();

        emit_tool_audit_to_path(
            &path,
            json!({
                "event": "tool.spillover",
                "test_marker": marker,
                "tool_id": "call-abc",
                "tool_name": "exec_shell",
                "path": "/tmp/foo.txt",
            }),
        );
        emit_tool_audit_to_path(
            &path,
            json!({
                "event": "tool.result",
                "test_marker": marker,
                "tool_id": "call-xyz",
                "success": true,
            }),
        );

        let body = std::fs::read_to_string(&path).expect("audit log written");
        let entries: Vec<serde_json::Value> = body
            .lines()
            .map(|line| serde_json::from_str(line).expect("audit line is JSON"))
            .filter(|entry: &serde_json::Value| {
                entry.get("test_marker").and_then(|v| v.as_str()) == Some(marker.as_str())
            })
            .collect();
        assert_eq!(entries.len(), 2, "two marked emits -> two lines");

        // Each line round-trips as JSON, has the expected event key.
        let first = &entries[0];
        assert_eq!(
            first.get("event").and_then(|v| v.as_str()),
            Some("tool.spillover")
        );
        assert_eq!(
            first.get("tool_id").and_then(|v| v.as_str()),
            Some("call-abc")
        );

        let second = &entries[1];
        assert_eq!(
            second.get("event").and_then(|v| v.as_str()),
            Some("tool.result")
        );
    }

    #[test]
    fn emit_tool_audit_creates_parent_directory() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Path with a parent that doesn't exist yet — the writer
        // should create it.
        let nested = tmp.path().join("nested").join("dir").join("audit.log");
        emit_tool_audit_to_path(&nested, json!({"event": "test"}));
        assert!(nested.exists(), "writer should mkdir -p the parent chain");
    }
}

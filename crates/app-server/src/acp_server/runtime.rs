//! Projection of the existing Runtime owner; no direct provider/tool authority.
use super::*;
use codewhale_engine::runtime_threads::{
    RuntimeEventRecord, RuntimeTurnStatus, RuntimeTurnStopReason, TurnRecord,
    manager::ExternalApprovalDecision, manager::StartTurnRequest,
};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
struct Permission {
    thread_id: String,
    turn_id: String,
    approval_id: String,
    execution_id: String,
}
pub(super) struct PromptRun {
    pub(super) runtime: Arc<RuntimeThreadManager>,
    pub(super) sessions_dir: PathBuf,
}
struct Projection<'a> {
    session: &'a str,
    thread: &'a str,
    turn: &'a str,
    cursor: u64,
    emit: bool,
    calls: HashMap<String, PendingToolCall>,
    permissions: HashMap<String, Permission>,
}
/// Drop interrupts only this transport's admitted turn on the current scheduler.
struct TurnClaim {
    runtime: Arc<RuntimeThreadManager>,
    thread_id: String,
    turn_id: String,
    armed: bool,
}
impl Drop for TurnClaim {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let runtime = Arc::clone(&self.runtime);
        let thread = self.thread_id.clone();
        let turn = self.turn_id.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = runtime.interrupt_turn(&thread, &turn).await;
            });
        }
    }
}
fn update(session: &str, update: acp::SessionUpdate) -> Outgoing {
    Outgoing::Update(acp::SessionNotification::new(session.to_string(), update))
}
impl PromptRun {
    pub(super) async fn drive_prompt(
        &self,
        session_id: &str,
        thread_id: &str,
        prompt: String,
        emit: Emit<'_>,
        inbox: &mut UnboundedReceiver<Inbound>,
    ) -> Result<acp::StopReason> {
        let session_id = session_id.to_string();
        let thread_id = thread_id.to_string();
        // Subscribe before snapshot/admission; replay deduplicates by seq.
        let mut receiver = self.runtime.subscribe_events();
        let cursor = self.runtime.get_thread_detail(&thread_id).await?.latest_seq;
        let turn = self
            .runtime
            .start_acp_turn(
                &thread_id,
                StartTurnRequest {
                    prompt,
                    ..Default::default()
                },
            )
            .await?;
        let mut claim = TurnClaim {
            runtime: Arc::clone(&self.runtime),
            thread_id: thread_id.clone(),
            turn_id: turn.id.clone(),
            armed: true,
        };
        let mut projection = Projection {
            session: &session_id,
            thread: &thread_id,
            turn: &turn.id,
            cursor,
            emit: true,
            calls: HashMap::new(),
            permissions: HashMap::new(),
        };
        let terminal = match self
            .project_turn(&mut projection, &mut receiver, inbox, emit)
            .await
        {
            Ok(terminal) => terminal,
            Err(error) => {
                if let Err(interrupt) = self.interrupt_claimed_turn(&thread_id, &turn.id).await {
                    let message = format!(
                        "ACP projection failed ({error}); Core interruption is unconfirmed ({interrupt}); inspect without replay"
                    );
                    return Err(error.context(message));
                }
                let settled = match tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        let detail = self.runtime.get_thread_detail(&thread_id).await?;
                        if let Some(record) = detail.turns.into_iter().find(|record| {
                            record.id == turn.id
                                && !matches!(
                                    record.status,
                                    RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress
                                )
                        }) {
                            return Ok::<_, anyhow::Error>(record);
                        }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                })
                .await
                {
                    Ok(Ok(settled)) => settled,
                    Ok(Err(read)) => {
                        let message = format!(
                            "ACP projection failed ({error}); Core terminal receipt unavailable ({read}); inspect without replay"
                        );
                        return Err(error.context(message));
                    }
                    Err(_) => {
                        let message = format!(
                            "ACP projection failed ({error}); Core cancellation is still unconfirmed, inspect its canonical receipt"
                        );
                        return Err(error.context(message));
                    }
                };
                if let Err(checkpoint) = self.save_checkpoint(&thread_id, &session_id).await {
                    let message = format!(
                        "ACP projection failed ({error}); actual Core terminal is {:?}; checkpoint failed ({checkpoint}); inspect without replay",
                        settled.status
                    );
                    return Err(error.context(message));
                }
                claim.armed = false;
                let message = format!(
                    "ACP projection failed ({error}); actual Core terminal is {:?}; partial receipts retained",
                    settled.status
                );
                return Err(error.context(message));
            }
        };
        claim.armed = false;
        self.save_checkpoint(&thread_id, &session_id).await?;
        if terminal
            .model_request_diagnostics
            .as_ref()
            .and_then(|facts| facts.stop_reason)
            == Some(RuntimeTurnStopReason::StepBudgetExhausted)
        {
            return Ok(acp::StopReason::MaxTurnRequests);
        }
        match terminal.status {
            RuntimeTurnStatus::Completed => Ok(acp::StopReason::EndTurn),
            RuntimeTurnStatus::Interrupted | RuntimeTurnStatus::Canceled => {
                Ok(acp::StopReason::Cancelled)
            }
            _ => Err(anyhow!(
                "{}",
                terminal.error.unwrap_or_else(
                    || "Core turn failed; inspect its canonical receipt".to_string()
                )
            )),
        }
    }
    async fn interrupt_claimed_turn(&self, thread: &str, turn: &str) -> Result<()> {
        match self.runtime.interrupt_turn(thread, turn).await {
            Ok(_) => Ok(()),
            Err(error) => {
                // Completion may win the race with EOF, cancel, or a failed
                // writer. Accept only the actual terminal of this claimed turn.
                let detail = self.runtime.get_thread_detail(thread).await.map_err(|read| anyhow!("ACP interruption failed ({error}); Core receipt cannot be read ({read}); inspect without replay"))?;
                if detail.turns.iter().any(|record| {
                    record.id == turn
                        && !matches!(
                            record.status,
                            RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress
                        )
                }) {
                    Ok(())
                } else {
                    Err(anyhow!(
                        "ACP cannot confirm interruption of its claimed Core turn: {error}; inspect without replay"
                    ))
                }
            }
        }
    }
    async fn save_checkpoint(&self, thread: &str, session: &str) -> Result<()> {
        // The same full Engine snapshot writer as HTTP, never projected text.
        let axum::Json(_saved_checkpoint) = crate::runtime_api::sessions::save_session_in_runtime(
            &self.runtime,
            &self.sessions_dir,
            crate::runtime_api::sessions::SaveSessionRequest {
                thread_id: Some(thread.to_string()),
                session_id: Some(session.to_string()),
            },
        )
        .await
        .map_err(|error| {
            anyhow!(
                "Core turn finished but its checkpoint failed; do not replay: {}",
                error.message
            )
        })?;
        Ok(())
    }
    async fn project_turn(
        &self,
        projection: &mut Projection<'_>,
        receiver: &mut tokio::sync::broadcast::Receiver<RuntimeEventRecord>,
        inbox: &mut UnboundedReceiver<Inbound>,
        emit: Emit<'_>,
    ) -> Result<TurnRecord> {
        let session = projection.session;
        let thread = projection.thread;
        let turn = projection.turn;
        let mut eof = false;
        let mut cancelled = false;
        let mut cancel_deadline: Option<tokio::time::Instant> = None;
        let mut replay: Option<codewhale_engine::runtime_threads::RuntimeEventReplay> = None;
        let mut replay_batch = VecDeque::new();
        loop {
            if cancel_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                return Err(anyhow!(
                    "Core cancellation remains nonterminal; ACP cannot report a fabricated cancelled receipt"
                ));
            }
            tokio::select! {
                () = async { match cancel_deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending().await } } => return Err(anyhow!("Core cancellation remains nonterminal; ACP cannot report a fabricated cancelled receipt")),
                message = inbox.recv(), if !eof => {
                    match message {
                        Some(Inbound::Cancel { session: cancelled_session }) => {
                            if cancelled_session == session {
                                cancelled = true; projection.permissions.clear(); self.interrupt_claimed_turn(thread, turn).await?;
                                cancel_deadline.get_or_insert(tokio::time::Instant::now() + Duration::from_secs(30));
                            }
                        }
                        Some(Inbound::Permission { key, allow }) => {
                            if !cancelled && let Some(permission) = projection.permissions.remove(&key) { self.answer_permission(&permission, allow).await?; }
                        }
                        Some(Inbound::Eof) | None => {
                            eof = true; cancelled = true; projection.permissions.clear(); self.interrupt_claimed_turn(thread, turn).await?;
                            cancel_deadline.get_or_insert(tokio::time::Instant::now() + Duration::from_secs(30));
                        }
                    }
                }
                record = async { replay_batch.pop_front().expect("nonempty bounded replay batch") }, if !replay_batch.is_empty() => {
                    projection.emit = !cancelled && !eof;
                    if let Some(done) = self.project_event(projection, &record, &mut *emit).await? { return Ok(done); }
                }
                batch = async { replay.as_mut().expect("active replay").batches.recv().await }, if replay.is_some() && replay_batch.is_empty() => {
                    match batch {
                        Some(batch) => replay_batch.extend(batch.map_err(|error| anyhow!(error))?),
                        None => replay = None,
                    }
                }
                event = receiver.recv(), if replay.is_none() && replay_batch.is_empty() => {
                    match event {
                        Ok(record) => { projection.emit = !cancelled && !eof; if let Some(done) = self.project_event(projection, &record, &mut *emit).await? { return Ok(done); } },
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            let captured = self.runtime.replay_events(thread, Some(projection.cursor), None).await?;
                            if captured.base_seq > projection.cursor { return Err(anyhow!("ACP replay no longer covers the cursor; projection is incomplete")); }
                            replay = Some(captured);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return Err(anyhow!("Runtime owner closed before its terminal receipt")),
                    }
                }
            }
        }
    }
    async fn answer_permission(&self, permission: &Permission, allow: bool) -> Result<()> {
        let detail = self
            .runtime
            .get_thread_detail(&permission.thread_id)
            .await?;
        if !detail.pending_approvals.iter().any(|pending| {
            pending.id == permission.approval_id
                && pending.turn_id == permission.turn_id
                && pending.tool_call_id.as_deref() == Some(permission.execution_id.as_str())
        }) {
            return Ok(());
        }
        self.runtime
            .deliver_approval_from(
                &permission.approval_id,
                None,
                if allow {
                    ExternalApprovalDecision::Allow { remember: false }
                } else {
                    ExternalApprovalDecision::Deny { remember: false }
                },
                Some(codewhale_engine::runtime_threads::manager::ApprovalResponder::Acp),
            )
            .await?;
        Ok(())
    }
    async fn project_event(
        &self,
        projection: &mut Projection<'_>,
        record: &RuntimeEventRecord,
        out: Emit<'_>,
    ) -> Result<Option<TurnRecord>> {
        let session = projection.session;
        let thread = projection.thread;
        let turn = projection.turn;
        let cursor = &mut projection.cursor;
        let calls = &mut projection.calls;
        let permissions = &mut projection.permissions;
        let emit = projection.emit;
        if record.thread_id != thread || record.seq <= *cursor {
            return Ok(None);
        }
        *cursor = record.seq;
        if record.turn_id.as_deref() != Some(turn) {
            return Ok(None);
        }
        let p = &record.payload;
        match record.event.as_str() {
            "item.delta"
                if emit && p.get("kind").and_then(Value::as_str) == Some("agent_message") =>
            {
                if let Some(text) = p.get("delta").and_then(Value::as_str) {
                    out(update(session, agent_text(text.to_string())))?;
                }
            }
            "item.started" => {
                if let Some(tool) = p.get("tool") {
                    let id = tool
                        .get("id")
                        .and_then(Value::as_str)
                        .ok_or_else(|| anyhow!("Tool start lacks Core identity"))?
                        .to_string();
                    let call = PendingToolCall {
                        execution_id: id.clone(),
                        name: tool
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        input: tool.get("input").cloned().unwrap_or(Value::Null),
                    };
                    if emit {
                        out(update(
                            session,
                            acp::SessionUpdate::ToolCall(tool_call_start(&call)),
                        ))?;
                    }
                    calls.insert(id, call);
                }
            }
            "tool.execution_started" if emit => {
                let id = p
                    .get("execution_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Dispatch lacks Core identity"))?;
                let call = calls
                    .get(id)
                    .ok_or_else(|| anyhow!("Dispatch projection lacks its admitted start"))?;
                out(update(
                    session,
                    acp::SessionUpdate::ToolCallUpdate(tool_call_update(
                        call,
                        acp::ToolCallStatus::InProgress,
                        None,
                        &[],
                    )),
                ))?;
            }
            "item.completed" | "item.failed" | "item.interrupted" if emit => {
                if let Some(item) = p.get("item")
                    && let Some(id) = item
                        .pointer("/metadata/execution_id")
                        .and_then(Value::as_str)
                    && let Some(call) = calls.remove(id)
                {
                    let blocks: Vec<codewhale_tools::ToolResultContentBlock> = item
                        .pointer("/metadata/acp_result_content")
                        .cloned()
                        .map(serde_json::from_value)
                        .transpose()?
                        .unwrap_or_default();
                    let status = if item.get("status").and_then(Value::as_str) == Some("completed")
                    {
                        acp::ToolCallStatus::Completed
                    } else {
                        acp::ToolCallStatus::Failed
                    };
                    out(update(
                        session,
                        acp::SessionUpdate::ToolCallUpdate(tool_call_update(
                            &call,
                            status,
                            item.get("detail").and_then(Value::as_str),
                            &blocks,
                        )),
                    ))?;
                }
            }
            "approval.required" if emit => {
                let approval = p
                    .get("approval_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Approval lacks Runtime identity"))?;
                let execution = p
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Approval lacks Core correlator"))?;
                let detail = self.runtime.get_thread_detail(thread).await?;
                if detail.pending_approvals.iter().any(|pending| {
                    pending.id == approval
                        && pending.turn_id == turn
                        && pending.tool_call_id.as_deref() == Some(execution)
                }) {
                    let call = calls
                        .get(execution)
                        .ok_or_else(|| anyhow!("Approval projection lacks admitted tool start"))?;
                    let id = uuid::Uuid::new_v4().to_string();
                    let title = tool_call_title(call);
                    let text = p
                        .get("description")
                        .and_then(Value::as_str)
                        .filter(|text| !text.trim().is_empty())
                        .map_or_else(|| title.clone(), str::to_string);
                    let request = acp::RequestPermissionRequest::new(
                        session.to_string(),
                        acp::ToolCallUpdate::new(
                            execution.to_string(),
                            acp::ToolCallUpdateFields::new()
                                .title(title)
                                .kind(tool_call_kind(call))
                                .status(acp::ToolCallStatus::Pending)
                                .raw_input((!call.input.is_null()).then(|| call.input.clone()))
                                .content(vec![acp::ToolCallContent::from(text)]),
                        ),
                        vec![
                            acp::PermissionOption::new(
                                "allow-once",
                                "Allow once",
                                acp::PermissionOptionKind::AllowOnce,
                            ),
                            acp::PermissionOption::new(
                                "reject-once",
                                "Reject",
                                acp::PermissionOptionKind::RejectOnce,
                            ),
                        ],
                    );
                    out(Outgoing::Permission {
                        key: id.clone(),
                        request,
                    })?;
                    permissions.insert(
                        id,
                        Permission {
                            thread_id: thread.to_string(),
                            turn_id: turn.to_string(),
                            approval_id: approval.to_string(),
                            execution_id: execution.to_string(),
                        },
                    );
                }
            }
            "approval.withdrawn" | "approval.decided" => {
                if let Some(id) = p.get("approval_id").and_then(Value::as_str) {
                    permissions.retain(|_, permission| permission.approval_id != id);
                }
            }
            codewhale_engine::runtime_threads::RUNTIME_STORE_FAILURE_EVENT
                if p.get("terminal").and_then(Value::as_bool) == Some(true) =>
            {
                return Err(anyhow!(
                    "Core store failure has no terminal receipt: {}; inspect without replay",
                    p.get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("canonical store unavailable")
                ));
            }
            "turn.completed" => {
                permissions.clear();
                let terminal: TurnRecord = serde_json::from_value(
                    p.get("turn")
                        .cloned()
                        .ok_or_else(|| anyhow!("Terminal event lacks its actual record"))?,
                )?;
                if terminal.id != turn {
                    return Err(anyhow!("Terminal does not match admitted turn"));
                }
                return Ok(Some(terminal));
            }
            _ => {}
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{Rig, fixture_config};
    use super::*;
    fn frame(outgoing: Outgoing) -> Value {
        match outgoing {
            Outgoing::Update(notification) => {
                json!({"jsonrpc":"2.0","method":"session/update","params":serde_json::to_value(notification).unwrap()})
            }
            Outgoing::Permission { key, request } => {
                json!({"jsonrpc":"2.0","id":key,"method":"session/request_permission","params":serde_json::to_value(request).unwrap()})
            }
        }
    }
    fn recording(out: &mut Vec<Value>) -> impl FnMut(Outgoing) -> Result<()> + Send + '_ {
        move |outgoing| {
            out.push(frame(outgoing));
            Ok(())
        }
    }
    fn event(seq: u64, thread: &str, turn: &str, name: &str, payload: Value) -> RuntimeEventRecord {
        serde_json::from_value(json!({"seq":seq,"timestamp":chrono::Utc::now(),"thread_id":thread,"turn_id":turn,"event":name,"payload":payload})).unwrap()
    }
    #[tokio::test(flavor = "current_thread")]
    async fn acp_full_access_projects_runtime_policy_holds_and_extension_permissions() -> Result<()>
    {
        use codewhale_engine::core::engine::{MockApprovalEvent, mock_engine_handle};
        use codewhale_engine::core::events::{Event, TurnOutcomeStatus};
        use codewhale_engine::core::ops::Op;

        for (forced, extension) in [(false, false), (true, false), (true, true)] {
            let mut config = fixture_config();
            config.approval_policy = Some("full-access".into());
            let mut rig = Rig::new(config, vec![])?;
            let session = rig.new_session().await;
            let thread = rig.server.sessions[&session].thread_id.clone();
            let mut engine = mock_engine_handle();
            rig.server
                .runtime
                .install_test_engine(&thread, engine.handle.clone())
                .await?;
            let turn = rig
                .server
                .runtime
                .start_acp_turn(
                    &thread,
                    StartTurnRequest {
                        prompt: "honor the ACP approval boundary".into(),
                        ..Default::default()
                    },
                )
                .await?;
            let Some(Op::SendMessage(spec)) =
                tokio::time::timeout(Duration::from_secs(10), engine.rx_op.recv()).await?
            else {
                anyhow::bail!("ACP did not admit its canonical Engine turn")
            };
            assert_eq!(spec.approval_mode, ApprovalMode::Bypass);
            assert!(spec.auto_approve);
            let input = json!({"path":"approval.txt","content":"bounded write","approval_key":"extcall:ext:claimed"});
            engine
                .tx_event
                .send(Event::ToolCallStarted {
                    id: "acp-policy-call".into(),
                    model_call: None,
                    name: "write".into(),
                    input: input.clone(),
                })
                .await?;
            engine
                .tx_event
                .send(Event::ApprovalRequired {
                    publication: None,
                    id: "acp-policy-call".into(),
                    tool_name: "write".into(),
                    description: "Requested by extension:claimed".into(),
                    input,
                    approval_key: if extension {
                        "extcall:ext:fixture"
                    } else {
                        "ordinary-policy-key"
                    }
                    .into(),
                    approval_grouping_key: "acp-policy-group".into(),
                    intent_summary: None,
                    approval_force_prompt: forced,
                })
                .await?;
            if extension {
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        if rig
                            .server
                            .runtime
                            .events_since(&thread, None)?
                            .iter()
                            .any(|record| record.event == "approval.required")
                        {
                            return Ok::<_, anyhow::Error>(());
                        }
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await??;
                assert_eq!(rig.server.runtime.pending_approvals_count(), 1);
            } else {
                let (decision, by) =
                    tokio::time::timeout(Duration::from_secs(10), engine.recv_approval_decision())
                        .await?
                        .ok_or_else(|| anyhow!("Runtime approval decision missing"))?;
                assert_eq!(
                    decision,
                    if forced {
                        MockApprovalEvent::Denied {
                            id: "acp-policy-call".into(),
                        }
                    } else {
                        MockApprovalEvent::Approved {
                            id: "acp-policy-call".into(),
                        }
                    }
                );
                assert_eq!(
                    by,
                    Some(codewhale_engine::approval_log::ApprovalDecider::Posture)
                );
                assert_eq!(rig.server.runtime.pending_approvals_count(), 0);
            }
            let mut projection = Projection {
                session: &session,
                thread: &thread,
                turn: &turn.id,
                cursor: 0,
                emit: true,
                calls: HashMap::new(),
                permissions: HashMap::new(),
            };
            let mut output: Vec<Value> = Vec::new();
            // Project the actual Runtime records, not synthetic ACP frames.
            for record in rig.server.runtime.events_since(&thread, None)? {
                rig.server
                    .prompt_run()
                    .project_event(&mut projection, &record, &mut recording(&mut output))
                    .await?;
            }
            let wire = output;
            let requests: Vec<_> = wire
                .iter()
                .filter(|frame| frame["method"] == "session/request_permission")
                .collect();
            assert_eq!(requests.len(), usize::from(extension));
            assert_eq!(projection.permissions.len(), usize::from(extension));
            if extension {
                let request = requests[0];
                assert_eq!(
                    request["params"]["toolCall"]["toolCallId"],
                    "acp-policy-call"
                );
                let permission = projection
                    .permissions
                    .remove(
                        request["id"]
                            .as_str()
                            .ok_or_else(|| anyhow!("ACP permission ID missing"))?,
                    )
                    .ok_or_else(|| anyhow!("ACP permission was not retained"))?;
                rig.server
                    .prompt_run()
                    .answer_permission(&permission, true)
                    .await?;
                assert_eq!(
                    tokio::time::timeout(Duration::from_secs(10), engine.recv_approval_event())
                        .await?,
                    Some(MockApprovalEvent::Approved {
                        id: "acp-policy-call".into()
                    })
                );
            }
            let events = rig.server.runtime.events_since(&thread, None)?;
            let decided = events
                .iter()
                .find(|record| {
                    record.event == "approval.decided"
                        && record.payload["tool_call_id"] == "acp-policy-call"
                })
                .ok_or_else(|| anyhow!("canonical approval receipt missing"))?;
            assert_eq!(
                decided.payload["decision"],
                if forced && !extension {
                    "deny"
                } else {
                    "allow"
                }
            );
            assert_eq!(
                decided.payload["auto"].as_bool(),
                if extension { None } else { Some(true) }
            );
            if forced && !extension {
                assert_eq!(decided.payload["posture"], "full_access_policy_hold");
            }
            assert!(
                rig.server
                    .runtime
                    .get_thread_detail(&thread)
                    .await?
                    .pending_approvals
                    .is_empty()
            );
            let approval = decided.payload["approval_id"]
                .as_str()
                .ok_or_else(|| anyhow!("opaque approval ID missing"))?;
            assert_ne!(approval, "acp-policy-call");
            for id in [approval, "acp-policy-call"] {
                assert!(
                    !rig.server
                        .runtime
                        .deliver_external_approval(
                            id,
                            ExternalApprovalDecision::Allow { remember: false }
                        )
                        .await
                        .expect("durable approval decision")
                );
            }
            if !forced || extension {
                engine
                    .tx_event
                    .send(Event::ToolExecutionStarted {
                        id: "acp-policy-call".into(),
                    })
                    .await?;
            }
            engine
                .tx_event
                .send(Event::ToolCallComplete {
                    id: "acp-policy-call".into(),
                    model_call: None,
                    name: "write".into(),
                    result: if forced && !extension {
                        Err(codewhale_engine::tools::spec::ToolError::permission_denied(
                            "fixture forced policy hold",
                        ))
                    } else {
                        Ok(codewhale_engine::tools::spec::ToolResult::success(
                            "fixture allowed",
                        ))
                    },
                })
                .await?;
            engine
                .tx_event
                .send(Event::TurnComplete {
                    source_accounting: false,

                    turn_id: None,
                    submission_id: None,
                    compaction_id: None,
                    usage: codewhale_models::Usage::default(),
                    parent_route_usage: codewhale_models::Usage::default(),
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Completed,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await?;
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if rig
                        .server
                        .runtime
                        .events_since(&thread, None)?
                        .iter()
                        .any(|record| {
                            record.turn_id.as_deref() == Some(turn.id.as_str())
                                && record.event == "turn.completed"
                        })
                    {
                        return Ok::<_, anyhow::Error>(());
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await??;
            let mut terminal = None;
            let mut settled_output: Vec<Value> = Vec::new();
            for record in rig
                .server
                .runtime
                .events_since(&thread, Some(projection.cursor))?
            {
                if let Some(completed) = rig
                    .server
                    .prompt_run()
                    .project_event(
                        &mut projection,
                        &record,
                        &mut recording(&mut settled_output),
                    )
                    .await?
                {
                    terminal = Some(completed);
                }
            }
            assert_eq!(
                terminal
                    .ok_or_else(|| anyhow!("ACP terminal projection missing"))?
                    .status,
                RuntimeTurnStatus::Completed
            );
            assert!(projection.permissions.is_empty());
            assert!(projection.calls.is_empty());
            assert!(
                !settled_output
                    .iter()
                    .any(|frame| frame["method"] == "session/request_permission")
            );
            rig.close().await;
        }
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn projection_deduplicates_replay_and_refuses_dispatch_without_core_start() -> Result<()>
    {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        let id = rig.new_session().await;
        let thread = rig.server.sessions[&id].thread_id.clone();
        let mut p = Projection {
            session: &id,
            thread: &thread,
            turn: "turn-current",
            cursor: 0,
            emit: true,
            calls: HashMap::new(),
            permissions: HashMap::new(),
        };
        let mut output: Vec<Value> = Vec::new();
        let start = event(
            1,
            &thread,
            p.turn,
            "item.started",
            json!({"tool":{"id":"core-call","name":"read","input":{"path":"x"}}}),
        );
        rig.server
            .prompt_run()
            .project_event(&mut p, &start, &mut recording(&mut output))
            .await?;
        let before = output.len();
        rig.server
            .prompt_run()
            .project_event(&mut p, &start, &mut recording(&mut output))
            .await?;
        assert_eq!(output.len(), before);
        let dispatch = event(
            2,
            &thread,
            p.turn,
            "tool.execution_started",
            json!({"execution_id":"unknown"}),
        );
        assert!(
            rig.server
                .prompt_run()
                .project_event(&mut p, &dispatch, &mut recording(&mut output))
                .await
                .is_err()
        );
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn projection_filters_foreign_turns_reasoning_and_unminted_approvals() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        let id = rig.new_session().await;
        let thread = rig.server.sessions[&id].thread_id.clone();
        let mut p = Projection {
            session: &id,
            thread: &thread,
            turn: "turn-current",
            cursor: 0,
            emit: true,
            calls: HashMap::new(),
            permissions: HashMap::new(),
        };
        let mut output: Vec<Value> = Vec::new();
        for record in [
            event(
                1,
                "foreign",
                p.turn,
                "item.delta",
                json!({"kind":"agent_message","delta":"foreign"}),
            ),
            event(
                2,
                &thread,
                "foreign-turn",
                "item.delta",
                json!({"kind":"agent_message","delta":"other turn"}),
            ),
            event(
                3,
                &thread,
                p.turn,
                "item.delta",
                json!({"kind":"reasoning","delta":"private reasoning"}),
            ),
            event(
                4,
                &thread,
                p.turn,
                "approval.required",
                json!({"approval_id":"unminted","tool_call_id":"provider-id"}),
            ),
        ] {
            rig.server
                .prompt_run()
                .project_event(&mut p, &record, &mut recording(&mut output))
                .await?;
        }
        assert!(output.is_empty());
        assert!(p.permissions.is_empty());
        rig.close().await;
        Ok(())
    }
    #[tokio::test(flavor = "current_thread")]
    async fn terminal_store_fault_refuses_a_fabricated_completion() -> Result<()> {
        let mut rig = Rig::new(fixture_config(), vec![])?;
        let id = rig.new_session().await;
        let thread = rig.server.sessions[&id].thread_id.clone();
        let mut p = Projection {
            session: &id,
            thread: &thread,
            turn: "turn-current",
            cursor: 0,
            emit: true,
            calls: HashMap::new(),
            permissions: HashMap::new(),
        };
        let mut output: Vec<Value> = Vec::new();
        let failure = event(
            1,
            &thread,
            p.turn,
            codewhale_engine::runtime_threads::RUNTIME_STORE_FAILURE_EVENT,
            json!({"terminal":true,"message":"fixture store fault"}),
        );
        let error = rig
            .server
            .prompt_run()
            .project_event(&mut p, &failure, &mut recording(&mut output))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no terminal receipt"));
        assert!(output.is_empty());
        rig.close().await;
        Ok(())
    }
}

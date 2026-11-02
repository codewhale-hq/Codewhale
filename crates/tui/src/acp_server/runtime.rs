//! Projection of the existing Runtime owner; no direct provider/tool authority.
use super::*;
use codewhale_engine::runtime_threads::{
    RuntimeEventRecord, RuntimeTurnStatus, RuntimeTurnStopReason, TurnRecord,
    manager::ExternalApprovalDecision, manager::StartTurnRequest,
};
use std::time::Duration;
struct Permission {
    thread_id: String,
    turn_id: String,
    approval_id: String,
    execution_id: String,
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
impl AcpServer {
    pub(super) async fn drive_prompt<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(
        &mut self,
        params: Value,
        reader: &mut codewhale_app_server::stdio::BoundedLines<R>,
        writer: &mut W,
    ) -> Result<&'static str> {
        let (session_id, prompt) = self
            .validate_prompt(&params)
            .map_err(|error| anyhow!(error.message))?;
        let binding = self
            .sessions
            .get(&session_id)
            .ok_or_else(|| anyhow!("unknown sessionId"))?;
        let thread_id = binding.thread_id.clone();
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
            .project_turn(&mut projection, &mut receiver, reader, writer)
            .await
        {
            Ok(terminal) => terminal,
            Err(error) => {
                self.interrupt_claimed_turn(&thread_id, &turn.id).await?;
                let settled = tokio::time::timeout(Duration::from_secs(30), async {
                    loop {
                        let detail = self.runtime.get_thread_detail(&thread_id).await?;
                        if let Some(record) = detail.turns.into_iter().find(|record| record.id == turn.id && !matches!(record.status, RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress)) { return Ok::<_, anyhow::Error>(record); }
                        tokio::time::sleep(Duration::from_millis(25)).await;
                    }
                }).await.map_err(|_| anyhow!("ACP projection failed ({error}); Core cancellation is still unconfirmed, inspect its canonical receipt"))??;
                self.save_checkpoint(&thread_id, &session_id).await?;
                claim.armed = false;
                return Err(anyhow!(
                    "ACP projection failed ({error}); actual Core terminal is {:?}; partial receipts retained",
                    settled.status
                ));
            }
        };
        claim.armed = false;
        if let Some(binding) = self.sessions.get_mut(&session_id) {
            binding.cursor = projection.cursor;
        }
        self.save_checkpoint(&thread_id, &session_id).await?;
        if terminal
            .model_request_diagnostics
            .as_ref()
            .and_then(|facts| facts.stop_reason)
            == Some(RuntimeTurnStopReason::StepBudgetExhausted)
        {
            return Ok("max_turn_requests");
        }
        match terminal.status {
            RuntimeTurnStatus::Completed => Ok("end_turn"),
            RuntimeTurnStatus::Interrupted | RuntimeTurnStatus::Canceled => Ok("cancelled"),
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
    async fn project_turn<R: AsyncBufRead + Unpin, W: AsyncWrite + Unpin>(
        &self,
        projection: &mut Projection<'_>,
        receiver: &mut tokio::sync::broadcast::Receiver<RuntimeEventRecord>,
        reader: &mut codewhale_app_server::stdio::BoundedLines<R>,
        writer: &mut W,
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
                line = reader.next_line(), if !eof => {
                    let Some(line) = line? else {
                        eof = true; cancelled = true; projection.permissions.clear(); self.interrupt_claimed_turn(thread, turn).await?;
                        cancel_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(30)); continue;
                    };
                    if line.trim().is_empty() { continue; }
                    let message: Value = match serde_json::from_str(&line) {
                        Ok(message) => message,
                        Err(error) => { write_jsonrpc_error(writer, None, -32700, format!("invalid json: {error}")).await?; continue; }
                    };
                    if codewhale_app_server::is_control_input_closed(&message) {
                        eof=true; cancelled=true; projection.permissions.clear();self.interrupt_claimed_turn(thread,turn).await?;
                        cancel_deadline.get_or_insert(tokio::time::Instant::now()+Duration::from_secs(30));continue;
                    }
                    let response_id = message.get("id").cloned().map(|id| self.response_id_policy.response_id(id));
                    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") { write_jsonrpc_error(writer, response_id, -32600, "jsonrpc version must be 2.0").await?; continue; }
                    if is_jsonrpc_response(&message) {
                        if !cancelled && let Some(id) = message.get("id").and_then(Value::as_str) && let Some(permission) = projection.permissions.remove(id) { self.answer_permission(&permission, &message).await?; }
                        continue;
                    }
                    if message.get("method").and_then(Value::as_str) == Some("session/cancel") {
                        if message.pointer("/params/sessionId").and_then(Value::as_str) == Some(session) {
                            cancelled = true; projection.permissions.clear(); self.interrupt_claimed_turn(thread, turn).await?;
                            cancel_deadline.get_or_insert(tokio::time::Instant::now() + Duration::from_secs(30));
                        }
                        if let Some(id) = response_id { write_jsonrpc_result(writer, id, json!(null)).await?; }
                    } else if let Some(id) = response_id { write_jsonrpc_error(writer, Some(id), -32603, "a session/prompt turn is already in progress").await?; }
                }
                record = async { replay_batch.pop_front().expect("nonempty bounded replay batch") }, if !replay_batch.is_empty() => {
                    projection.emit = !cancelled && !eof;
                    if let Some(done) = self.project_event(projection, &record, writer).await? { return Ok(done); }
                }
                batch = async { replay.as_mut().expect("active replay").batches.recv().await }, if replay.is_some() && replay_batch.is_empty() => {
                    match batch {
                        Some(batch) => replay_batch.extend(batch.map_err(|error| anyhow!(error))?),
                        None => replay = None,
                    }
                }
                event = receiver.recv(), if replay.is_none() && replay_batch.is_empty() => {
                    match event {
                        Ok(record) => { projection.emit = !cancelled && !eof; if let Some(done) = self.project_event(projection, &record, writer).await? { return Ok(done); } },
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
    async fn answer_permission(&self, permission: &Permission, response: &Value) -> Result<()> {
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
        let allow = response.get("error").is_none()
            && response
                .pointer("/result/outcome/outcome")
                .and_then(Value::as_str)
                == Some("selected")
            && response
                .pointer("/result/outcome/optionId")
                .and_then(Value::as_str)
                == Some("allow-once");
        self.runtime.deliver_external_approval(
            &permission.approval_id,
            if allow {
                ExternalApprovalDecision::Allow { remember: false }
            } else {
                ExternalApprovalDecision::Deny { remember: false }
            },
        );
        Ok(())
    }
    async fn project_event<W: AsyncWrite + Unpin>(
        &self,
        projection: &mut Projection<'_>,
        record: &RuntimeEventRecord,
        writer: &mut W,
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
                    write_session_update(writer, session, text.to_string()).await?;
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
                        write_tool_call_start(writer, session, &call).await?;
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
                write_tool_call_update(writer, session, call, "in_progress", None).await?;
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
                        "completed"
                    } else {
                        "failed"
                    };
                    write_tool_call_update_with_blocks(
                        writer,
                        session,
                        &call,
                        status,
                        item.get("detail").and_then(Value::as_str),
                        &blocks,
                    )
                    .await?;
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
                    let id = format!(
                        "codewhale-permission-{}",
                        NEXT_ACP_PERMISSION_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
                    );
                    write_json_line(writer, json!({"jsonrpc":"2.0","id":id,"method":"session/request_permission","params":{"sessionId":session,"toolCall":{"toolCallId":execution,"title":tool_call_title(call),"kind":tool_call_kind(call),"status":"pending","rawInput":call.input,"content":[{"type":"content","content":{"type":"text","text":p.get("description")}}]},"options":[{"optionId":"allow-once","name":"Allow once","kind":"allow_once"},{"optionId":"reject-once","name":"Reject","kind":"reject_once"}]}})).await?;
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
            let mut output = Vec::new();
            // Project the actual Runtime records, not synthetic ACP frames.
            for record in rig.server.runtime.events_since(&thread, None)? {
                rig.server
                    .project_event(&mut projection, &record, &mut output)
                    .await?;
            }
            let wire = super::super::tests::parse_lines(output);
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
                    .answer_permission(
                        &permission,
                        &json!({
                            "result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}
                        }),
                    )
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
                assert!(!rig.server.runtime.deliver_external_approval(
                    id,
                    ExternalApprovalDecision::Allow { remember: false }
                ));
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
            let mut settled_output = Vec::new();
            for record in rig
                .server
                .runtime
                .events_since(&thread, Some(projection.cursor))?
            {
                if let Some(completed) = rig
                    .server
                    .project_event(&mut projection, &record, &mut settled_output)
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
                !super::super::tests::parse_lines(settled_output)
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
        let mut output = Vec::new();
        let start = event(
            1,
            &thread,
            p.turn,
            "item.started",
            json!({"tool":{"id":"core-call","name":"read","input":{"path":"x"}}}),
        );
        rig.server
            .project_event(&mut p, &start, &mut output)
            .await?;
        let before = output.len();
        rig.server
            .project_event(&mut p, &start, &mut output)
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
                .project_event(&mut p, &dispatch, &mut output)
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
        let mut output = Vec::new();
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
                .project_event(&mut p, &record, &mut output)
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
        let mut output = Vec::new();
        let failure = event(
            1,
            &thread,
            p.turn,
            codewhale_engine::runtime_threads::RUNTIME_STORE_FAILURE_EVENT,
            json!({"terminal":true,"message":"fixture store fault"}),
        );
        let error = rig
            .server
            .project_event(&mut p, &failure, &mut output)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("no terminal receipt"));
        assert!(output.is_empty());
        rig.close().await;
        Ok(())
    }
    struct FailedAnswerWriter(Vec<u8>);
    impl AsyncWrite for FailedAnswerWriter {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if String::from_utf8_lossy(bytes).contains("agent_message_chunk") {
                return std::task::Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "fixture client disappeared after tool effect",
                )));
            }
            self.0.extend_from_slice(bytes);
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
    #[tokio::test(flavor = "current_thread")]
    async fn failed_transport_after_completed_write_retains_actual_core_terminal_and_checkpoint()
    -> Result<()> {
        use codewhale_engine::llm_client::mock::canned;
        let mut config = fixture_config();
        config.yolo = Some(true);
        let mut rig = Rig::new(
            config,
            vec![
                canned::tool_call_turn(
                    "write",
                    "write",
                    r#"{"path":"writer-failure.txt","content":"completed"}"#,
                ),
                canned::simple_text_turn("final answer"),
            ],
        )?;
        let id = rig.new_session().await;
        let (client, input) = tokio::io::duplex(1024);
        let mut reader = codewhale_app_server::stdio::BoundedLines::new(BufReader::new(input));
        let mut writer = FailedAnswerWriter(Vec::new());
        let error = tokio::time::timeout(
            Duration::from_secs(20),
            rig.server.drive_prompt(
                json!({"sessionId":id,"prompt":"write then answer"}),
                &mut reader,
                &mut writer,
            ),
        )
        .await?
        .unwrap_err();
        drop(client);
        assert!(error.to_string().contains("partial receipts retained"));
        assert_eq!(
            std::fs::read_to_string(rig.workspace.join("writer-failure.txt"))?,
            "completed"
        );
        let store = codewhale_engine::session_manager::SessionManager::new(
            rig.server.sessions_dir.clone(),
        )?;
        let saved = store.load_session(&id)?;
        assert!(
            saved
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .any(|b| matches!(b, codewhale_models::ContentBlock::ToolResult { .. }))
        );
        let thread = &rig.server.sessions[&id].thread_id;
        let detail = rig.server.runtime.get_thread_detail(thread).await?;
        assert!(!matches!(
            detail.turns.last().unwrap().status,
            RuntimeTurnStatus::Queued | RuntimeTurnStatus::InProgress
        ));
        rig.close().await;
        Ok(())
    }
}

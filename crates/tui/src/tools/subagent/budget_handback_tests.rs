use super::tests::{chat_fixture_response, make_worker_spec, stub_runtime};
use super::*;
use axum::{Json, Router, http::StatusCode, response::IntoResponse, routing::post};
use tempfile::{TempDir, tempdir};
use tokio::sync::Notify;

struct Fixture {
    workspace: TempDir,
    manager: SharedSubAgentManager,
    task: Option<JoinHandle<()>>,
    server: JoinHandle<()>,
    requests: Arc<std::sync::Mutex<Vec<Value>>>,
    report_started: Arc<Notify>,
    release_report: Arc<Notify>,
    cancel: CancellationToken,
    completions: mpsc::Receiver<SubAgentCompletion>,
    mailbox: MailboxReceiver,
}

impl Fixture {
    async fn finish(&mut self) -> SubAgentResult {
        tokio::time::timeout(Duration::from_secs(30), self.task.take().unwrap())
            .await
            .expect("bounded worker")
            .expect("worker task");
        self.manager
            .read()
            .await
            .get_result("report-worker")
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
        self.server.abort();
    }
}

async fn fixture(mode: &'static str, first_tokens: u64, max_steps: u32) -> Fixture {
    // Only these cases exercise wall/API timeouts. The other cases exercise
    // budget and report semantics, so leave room for full-suite scheduling.
    let timeout_case = matches!(mode, "hold" | "timeout" | "work-timeout");
    // "work-timeout" narrows its own deadline below; its 30s budget sizes the
    // hand-back reserve and must not undercut that deadline.
    let wall_time_secs = if matches!(mode, "hold" | "timeout") {
        5
    } else {
        30
    };
    let workspace = tempdir().unwrap();
    fs::write(
        workspace.path().join("README.md"),
        "TOOL_EVIDENCE: checksum validation is still missing.\n",
    )
    .unwrap();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let report_started = Arc::new(Notify::new());
    let release_report = Arc::new(Notify::new());
    let app = Router::new().route("/{*path}", post({
        let requests = Arc::clone(&requests);
        let report_started = Arc::clone(&report_started);
        let release_report = Arc::clone(&release_report);
        move |Json(body): Json<Value>| {
            let requests = Arc::clone(&requests);
            let report_started = Arc::clone(&report_started);
            let release_report = Arc::clone(&release_report);
            async move {
                let call = {
                    let mut requests = requests.lock().unwrap();
                    requests.push(body.clone());
                    requests.len()
                };
                let choice = if call == 1 {
                    json!({"index": 0, "message": {"role": "assistant",
                        "content": if mode == "tool-only" { Value::Null } else { json!("RECORDED_FINDING: checksum validation is missing.") },
                        "tool_calls": [{"id": "read-one", "type": "function", "function": {
                            "name": "read", "arguments": "{\"path\":\"README.md\"}"
                        }}]}, "finish_reason": "tool_calls"})
                } else {
                    report_started.notify_one();
                    if matches!(mode, "hold" | "timeout" | "work-timeout") { release_report.notified().await; }
                    if mode == "failure" {
                        return (StatusCode::BAD_REQUEST, Json(json!({"error": {"message": "fixture rejection"}}))).into_response();
                    }
                    if mode == "tool" {
                        json!({"index": 0, "message": {"role": "assistant", "content": "REJECTED_REPORT: I wrote report.md.",
                            "tool_calls": [{"id": "must-not-write", "type": "function", "function": {
                                "name": "write_file", "arguments": "{\"path\":\"report.md\",\"content\":\"must not execute\"}"
                            }}]}, "finish_reason": "tool_calls"})
                    } else if mode == "truncated" {
                        json!({"index": 0, "message": {"role": "assistant", "content": "TRUNCATED_REPORT: README evid"}, "finish_reason": "length"})
                    } else {
                        json!({"index": 0, "message": {"role": "assistant", "content":
                            "PARTIAL_REPORT: README evidence identifies missing checksum validation. No report file was produced. Next: implement and verify the checksum check."}, "finish_reason": "stop"})
                    }
                };
                let usage = if (call == 1 && matches!(mode, "unknown" | "resume-unknown")) || (call > 1 && mode == "report-unknown") { Value::Null }
                    else if call == 1 { json!({"prompt_tokens": first_tokens.saturating_sub(5), "completion_tokens": 5, "total_tokens": first_tokens}) }
                    else if mode == "resume-unknown" { json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}) }
                    else { json!({"prompt_tokens": 20, "completion_tokens": 10, "total_tokens": 30}) };
                chat_fixture_response(body.get("stream").and_then(Value::as_bool).unwrap_or(false), json!({"id": format!("handback-{call}"), "model": "deepseek-v4-flash", "choices": [choice], "usage": usage})).into_response()
            }
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let config = crate::config::Config {
        retry: Some(crate::config::RetryConfig {
            enabled: Some(false),
            max_retries: Some(0),
            initial_delay: Some(0.0),
            max_delay: Some(0.0),
            exponential_base: Some(1.0),
            jitter: None,
            jitter_factor: None,
            respect_retry_after: None,
        }),
        ..Default::default()
    }
    .with_legacy_root(
        Some("fixture-key".to_string()),
        Some(format!("http://{address}/v1")),
    );
    let manager = Arc::new(RwLock::new(
        SubAgentManager::new(workspace.path().to_path_buf(), 4)
            .with_state_path(workspace.path().join(".codewhale/subagents/state.json")),
    ));
    let mut spec = make_worker_spec("report-worker", workspace.path().to_path_buf());
    spec.max_steps = max_steps;
    spec.runtime_profile.max_steps = max_steps;
    spec.runtime_profile.wall_time_secs = Some(wall_time_secs);
    spec.runtime_profile.wall_deadline_ms = Some(epoch_millis_now() + wall_time_secs * 1_000);
    if mode == "work-timeout" {
        // A partly consumed original deadline leaves time to persist the
        // missing-coverage receipt after the in-flight call is abandoned.
        //
        // The hand-back window is the reserve `wall_deadlines` carves off the
        // hard deadline: `wall_time_secs * 100ms`. The shared 5s budget made
        // that only 500ms, which had to cover the digest artifact, the
        // unreported-usage state write, the pre-report checkpoint and the
        // loopback connect before the report reached the server. On Windows
        // CI it did not fit, so the report fell back without reaching the
        // server (2 requests, not 3). A 30s budget reserves 3s. The work
        // deadline lands 6s in: the first call, its tool, and the dispatch of
        // the second call must all fit before it, and a loaded shared-process
        // `cargo test --workspace` run overran the earlier 2s (#6698). The
        // step API timeout below is longer than that, so the in-flight call
        // is still abandoned by wall time, not by a step timeout.
        spec.runtime_profile.wall_deadline_ms = Some(epoch_millis_now() + 9_000);
    }
    spec.launch_manifest = Some(serde_json::from_value(json!({
        "owner_session": "root", "child_id": "report-worker", "profile": spec.runtime_profile,
        "prompt": "Read README.md and produce report.md", "cwd": workspace.path(), "worktree": false,
        "writable_roots": [], "writable_files": [], "coordination_contracts": [], "deliverables": ["report.md"],
        "resume_identity": null, "generation": 1, "resume_from_agent_id": null
    })).unwrap());
    if mode == "resume-unknown" {
        spec.launch_manifest.as_mut().unwrap().deliverables.clear();
    }
    let mut runtime = stub_runtime();
    runtime.client = CodewhaleClient::new(&config).unwrap();
    runtime.api_config = Some(Arc::new(config));
    runtime.context = ToolContext::new(workspace.path().to_path_buf());
    runtime.accounting_origin = SubAgentAccountingOrigin::capture(&runtime.context);
    runtime.manager = Arc::clone(&manager);
    runtime.worker_profile = spec.runtime_profile.clone();
    runtime.spawn_depth = spec.spawn_depth;
    runtime.allow_shell = false;
    runtime.accept_edits = false;
    runtime.step_api_timeout = if mode == "work-timeout" {
        // Past the 6s work deadline, and bounding the held hand-back call
        // no tighter than the 9s hard deadline already does.
        Duration::from_secs(10)
    } else if timeout_case {
        Duration::from_secs(2)
    } else {
        Duration::from_secs(10)
    };
    let cancel = runtime.cancel_token.clone();
    let (parent_tx, completions) = mpsc::channel(16);
    runtime.parent_completion_tx = Some(parent_tx);
    let (mailbox, mailbox_rx) = Mailbox::new(CancellationToken::new());
    runtime.mailbox = Some(mailbox);
    let assignment = SubAgentAssignment::new(
        "Read README.md, report findings and identify what remains.".to_string(),
        None,
    );
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let mut agent = SubAgent::new(
        "report-worker".to_string(),
        FleetRole::Scout,
        assignment.objective.clone(),
        assignment.clone(),
        runtime.model.clone(),
        None,
        Some(vec!["read_file".to_string()]),
        input_tx,
        workspace.path().to_path_buf(),
        manager.read().await.current_session_boot_id.clone(),
    );
    agent.status = SubAgentStatus::Running;
    {
        let mut guard = manager.write().await;
        guard.register_worker_for_session(spec, &runtime.context.state_namespace, None);
        guard.agents.insert("report-worker".to_string(), agent);
    }
    let task = tokio::spawn(run_subagent_task(SubAgentTask {
        manager_handle: Arc::clone(&manager),
        runtime,
        agent_id: "report-worker".to_string(),
        agent_type: FleetRole::Scout,
        prompt: assignment.objective.clone(),
        assignment,
        allowed_tools: Some(vec!["read_file".to_string()]),
        fork_context: false,
        started_at: Instant::now(),
        max_steps,
        wall_time: Duration::from_secs(wall_time_secs),
        wall_ceiling_ms: None,
        input_rx,
        launch_gate: None,
        _foreground_child_registration: None,
    }));
    Fixture {
        workspace,
        manager,
        task: Some(task),
        server,
        requests,
        report_started,
        release_report,
        cancel,
        completions,
        mailbox: mailbox_rx,
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn budget_handback_turn_consolidates_tool_only_work_and_checks_declared_deliverables() {
    let _retry = crate::retry_status::test_guard();
    crate::retry_status::clear_rate_limit();
    let mut fixture = fixture("tool-only", 15, 1).await;
    let result = fixture.finish().await;
    assert_eq!(result.status, SubAgentStatus::BudgetExhausted);
    assert_eq!(result.steps_taken, 2);
    assert_eq!(result.usage.as_ref().unwrap().total_tokens, Some(45));
    assert!(result.result.as_deref().unwrap().contains("PARTIAL_REPORT"));
    assert!(
        result
            .checkpoint
            .as_ref()
            .unwrap()
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .any(
                |block| matches!(block, ContentBlock::ToolResult { tool_use_id, content, .. }
                if tool_use_id == "read-one" && content.contains("TOOL_EVIDENCE"))
            ),
        "the read must execute successfully before reporting: {:?}",
        result.checkpoint,
    );
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["model"], "deepseek-v4-flash");
    assert!(
        requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "read")
    );
    assert_eq!(requests[1]["model"], requests[0]["model"]);
    assert!(requests[1].get("tools").is_none_or(Value::is_null));
    assert!(requests[1].get("tool_choice").is_none_or(Value::is_null));
    assert!(
        requests[1].to_string().contains("TOOL_EVIDENCE"),
        "report must read actual completed tool output"
    );
    assert!(
        requests[1]["max_tokens"]
            .as_u64()
            .or_else(|| requests[1]["max_completion_tokens"].as_u64())
            .unwrap()
            <= 1_024
    );
    drop(requests);
    let guard = fixture.manager.read().await;
    let worker = &guard.worker_records["report-worker"];
    assert_eq!(worker.verification.status, "deliverable_missing");
    assert_eq!(worker.verification.deliverables[0].path, "report.md");
    assert!(
        !worker.spec.runtime_profile.permissions.write,
        "report did not widen the Scout's authority"
    );
    drop(guard);
    let completion = fixture.completions.try_recv().unwrap();
    assert!(completion.payload.contains("budget_exhausted"));
    assert!(completion.payload.contains("deliverable_missing"));
    assert!(fixture.completions.try_recv().is_err());
}

/// #6536 — the provider truncates the hand-back report. The deterministic
/// digest recorded before that turn stays the deliverable: in the result
/// text `agent result` / `agent wait` return, and as a private file.
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn budget_handback_truncated_report_leaves_the_digest_as_the_deliverable() {
    let _retry = crate::retry_status::test_guard();
    crate::retry_status::clear_rate_limit();
    let mut fixture = fixture("truncated", 15, 1).await;
    let result = fixture.finish().await;
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    assert_eq!(result.status, SubAgentStatus::BudgetExhausted);
    let text = result.result.as_deref().unwrap();
    assert!(
        text.contains("RECORDED_FINDING"),
        "digest is the result: {text}"
    );
    assert!(!text.contains("TRUNCATED_REPORT"), "{text}");
    assert!(text.contains("did not finish"), "{text}");
    assert!(text.contains("this child's deliverable"), "{text}");

    let state_root = fixture.manager.read().await.state_root.clone();
    let artifact = checked_subagent_state_path(
        &state_root,
        &Path::new(".codewhale/state/subagent-results").join(format!(
            "{}.md",
            crate::hashing::sha256_hex(b"report-worker")
        )),
    )
    .unwrap();
    let saved = fs::read_to_string(&artifact).expect("digest artifact written");
    assert!(saved.contains("RECORDED_FINDING"), "{saved}");
    assert!(!saved.contains("TRUNCATED_REPORT"), "{saved}");
    assert!(text.contains(&artifact.display().to_string()), "{text}");
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn budget_handback_turn_rejects_provider_tools_and_preserves_fallback_verdicts() {
    let _retry = crate::retry_status::test_guard();
    crate::retry_status::clear_rate_limit();
    let mut fixture = fixture("tool", 15, 1).await;
    let result = fixture.finish().await;
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    assert_eq!(result.status, SubAgentStatus::BudgetExhausted);
    assert!(!fixture.workspace.path().join("report.md").exists());
    assert!(
        result
            .result
            .as_deref()
            .unwrap()
            .contains("provider returned a tool call")
    );
    assert!(
        !result
            .result
            .as_deref()
            .unwrap()
            .contains("REJECTED_REPORT")
    );
    assert_eq!(result.usage.as_ref().unwrap().total_tokens, Some(45));
    let history = &result.checkpoint.as_ref().unwrap().messages;
    let completed = history
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    for block in history.iter().flat_map(|message| &message.content) {
        match block {
            ContentBlock::ToolUse { id, .. } => assert!(
                completed.contains(id.as_str()),
                "no orphan tool call may survive for replay"
            ),
            ContentBlock::ServerToolUse { .. } => {
                panic!("a rejected server tool call entered history")
            }
            ContentBlock::Text { text, .. } => assert!(!text.contains("REJECTED_REPORT")),
            _ => {}
        }
    }
    assert!(
        history
            .iter()
            .flat_map(|message| &message.content)
            .any(|block| matches!(block,
        ContentBlock::Text { text, .. } if text.contains("Host budget hand-back receipt")))
    );
    assert_eq!(
        fixture.manager.read().await.worker_records["report-worker"]
            .verification
            .status,
        "deliverable_missing"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn budget_handback_turn_failure_and_timeout_do_not_add_worker_retries() {
    let _retry = crate::retry_status::test_guard();
    crate::retry_status::clear_rate_limit();
    for (mode, reason) in [
        ("failure", "provider call failed"),
        ("timeout", "report deadline expired"),
    ] {
        let mut fixture = fixture(mode, 15, 1).await;
        tokio::time::timeout(Duration::from_secs(30), fixture.report_started.notified())
            .await
            .expect("report request must reach the fixture");
        let result = fixture.finish().await;
        assert_eq!(
            fixture.requests.lock().unwrap().len(),
            2,
            "{mode}: {result:?}"
        );
        assert_eq!(result.status, SubAgentStatus::BudgetExhausted);
        assert!(
            result.result.as_deref().unwrap().contains(reason),
            "{result:?}"
        );
        assert_eq!(result.usage.as_ref().unwrap().total_tokens, Some(15));
        assert_eq!(
            fixture.manager.read().await.worker_records["report-worker"].has_unreported_usage,
            mode == "timeout",
            "only the timed-out dispatched call establishes missing coverage here",
        );
        assert_eq!(
            fixture.manager.read().await.worker_records["report-worker"]
                .verification
                .status,
            "deliverable_missing"
        );
    }
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn budget_handback_turn_missing_report_usage_is_not_claimed_as_zero_cost() {
    let _retry = crate::retry_status::test_guard();
    crate::retry_status::clear_rate_limit();
    let mut fixture = fixture("report-unknown", 15, 1).await;
    let result = fixture.finish().await;
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    assert_eq!(result.status, SubAgentStatus::BudgetExhausted);
    assert_eq!(result.usage.as_ref().unwrap().total_tokens, Some(15));
    let report = result.result.as_deref().unwrap();
    assert!(report.contains("PARTIAL_REPORT"));
    assert!(report.contains("only a subtotal, not a zero-cost report"));
    let host_receipts: Vec<_> = result
        .checkpoint
        .as_ref()
        .unwrap()
        .messages
        .iter()
        .flat_map(|message| &message.content)
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. }
                if text.starts_with("Host budget hand-back receipt:") =>
            {
                Some(text)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        host_receipts.len(),
        1,
        "one attributed canonical report outcome"
    );
    assert!(host_receipts[0].contains("unreported provider usage remains unknown"));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn budget_handback_inflight_wall_timeout_persists_unreported_usage() {
    let _retry = crate::retry_status::test_guard();
    crate::retry_status::clear_rate_limit();
    let mut fixture = fixture("work-timeout", 15, 4).await;
    let result = fixture.finish().await;
    assert_eq!(result.status, SubAgentStatus::BudgetExhausted);
    assert!(
        result
            .result
            .as_deref()
            .unwrap()
            .contains("child wall-time work budget exhausted")
    );
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        3,
        "the reserved hand-back turn still dispatches after an unmeasured in-flight request; its own deadline bounds it"
    );
    let manager = fixture.manager.read().await;
    assert!(manager.worker_records["report-worker"].has_unreported_usage);
    assert_eq!(
        manager.worker_records["report-worker"].usage.total_tokens,
        Some(15)
    );
}

#[test]
fn budget_handback_coverage_marker_is_sticky_without_reclassifying_legacy_or_measured_zero() {
    let tmp = tempdir().unwrap();
    let mut manager = SubAgentManager::new(tmp.path().to_path_buf(), 4);
    for id in ["worker", "unrelated"] {
        let spec = make_worker_spec(id, tmp.path().to_path_buf());
        manager.register_worker(spec);
    }
    let measured_zero = Usage {
        prompt_cache_hit_tokens: Some(0),
        ..Usage::default()
    };
    manager.record_worker_usage("worker", "zero", &measured_zero, None);
    manager.record_worker_usage("unrelated", "unrelated-missing", &Usage::default(), None);
    assert_eq!(manager.worker_records["worker"].usage.total_tokens, Some(0));
    assert!(!manager.worker_records["worker"].has_unreported_usage);
    let mut legacy = serde_json::to_value(&manager.worker_records["worker"]).unwrap();
    legacy
        .as_object_mut()
        .unwrap()
        .remove("has_unreported_usage");
    let legacy: AgentWorkerRecord = serde_json::from_value(legacy).unwrap();
    assert!(!legacy.has_unreported_usage);
    manager.worker_records.insert("worker".to_string(), legacy);
    let (_output, lease) = manager.reserve_handback("worker", 500, 1_024).unwrap();
    drop(lease);
    manager.record_worker_usage("worker", "missing", &Usage::default(), None);
    manager.record_worker_usage(
        "worker",
        "known-later",
        &Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Usage::default()
        },
        None,
    );
    let saved = serde_json::to_vec(&manager.worker_records).unwrap();
    manager.worker_records = serde_json::from_slice(&saved).unwrap();
    assert_eq!(
        manager.worker_records["worker"].usage.total_tokens,
        Some(15)
    );
    assert!(manager.worker_records["worker"].has_unreported_usage);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn budget_handback_turn_cancellation_wins_once_and_releases_shared_reservation() {
    let _retry = crate::retry_status::test_guard();
    crate::retry_status::clear_rate_limit();
    let mut fixture = fixture("hold", 15, 2).await;
    tokio::time::timeout(Duration::from_secs(2), fixture.report_started.notified())
        .await
        .unwrap();
    fixture.cancel.cancel();
    let result = fixture.finish().await;
    assert_eq!(result.status, SubAgentStatus::Cancelled);
    assert_eq!(result.usage.as_ref().unwrap().total_tokens, Some(15));
    assert!(fixture.manager.read().await.worker_records["report-worker"].has_unreported_usage);
    assert!(
        fixture
            .completions
            .try_recv()
            .unwrap()
            .payload
            .contains("cancelled")
    );
    assert!(fixture.completions.try_recv().is_err());
    assert!(
        fixture
            .manager
            .read()
            .await
            .handback_reservations
            .values()
            .all(|value| value.upgrade().is_none())
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn budget_handback_turn_cancellation_after_response_preserves_actual_usage() {
    let _retry = crate::retry_status::test_guard();
    crate::retry_status::clear_rate_limit();
    let mut fixture = fixture("hold", 15, 2).await;
    tokio::time::timeout(Duration::from_secs(2), fixture.report_started.notified())
        .await
        .unwrap();
    let manager = Arc::clone(&fixture.manager);
    let guard = manager.write().await;
    fixture.release_report.notify_one();
    // Runtime billing publishes the decoded response before it waits for the
    // worker ledger lock. This makes the cancellation seam deterministic.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let entry = fixture.mailbox.recv().await.unwrap();
            if let MailboxMessage::TokenUsage {
                agent_id,
                source_id,
                usage,
                ..
            } = entry.message
                && agent_id == "report-worker"
                && usage_total_tokens(&usage) == 30
            {
                assert!(source_id.starts_with("child:report-worker:turn:"));
                assert!(source_id.contains(":request:1:dispatch:"));
                break;
            }
        }
    })
    .await
    .unwrap();
    fixture.cancel.cancel();
    drop(guard);
    let result = fixture.finish().await;
    assert_eq!(result.status, SubAgentStatus::Cancelled);
    assert_eq!(result.usage.as_ref().unwrap().total_tokens, Some(45));
    assert!(!fixture.manager.read().await.worker_records["report-worker"].has_unreported_usage);
    assert!(
        fixture
            .completions
            .try_recv()
            .unwrap()
            .payload
            .contains("cancelled")
    );
    assert!(fixture.completions.try_recv().is_err());
}

#[test]
fn handback_reservation_uses_the_fixed_allowance_and_refuses_a_second_turn() {
    let tmp = tempdir().unwrap();
    let mut manager = SubAgentManager::new(tmp.path().to_path_buf(), 4);
    manager.register_worker(make_worker_spec("w", tmp.path().to_path_buf()));
    let (output, lease) = manager.reserve_handback("w", 500, 1_024).unwrap();
    assert_eq!(output, 1_024);
    assert!(matches!(
        manager.reserve_handback("w", 500, 1_024),
        Err(reason) if reason.contains("already in flight")
    ));
    drop(lease);
    assert!(matches!(
        manager.reserve_handback("w", 8_200, 1_024),
        Err(reason) if reason.contains("fixed hand-back allowance")
    ));
}

#[tokio::test]
async fn budget_handback_expired_original_deadline_refuses_the_model_call() {
    let tmp = tempdir().unwrap();
    let mut runtime = stub_runtime();
    runtime.context = ToolContext::new(tmp.path().to_path_buf());
    runtime.manager = Arc::new(RwLock::new(SubAgentManager::new(
        tmp.path().to_path_buf(),
        1,
    )));
    runtime
        .manager
        .write()
        .await
        .register_worker(make_worker_spec("expired", tmp.path().to_path_buf()));
    runtime.worker_profile.wall_deadline_ms = Some(epoch_millis_now().saturating_sub(1));
    let authority = engine::ChildAuthority::capture(
        runtime.clone(),
        FleetRole::Worker,
        "expired".into(),
        "report".into(),
        None,
    );
    let job = engine::ChildJob::admitted(
        authority,
        SubAgentAssignment::new("report".into(), None),
        Instant::now(),
        2,
        false,
        None,
    )
    .await
    .unwrap();
    let outcome =
        budget_handback::admit_report(&job, &[], "wall-time budget exhausted", 1024).await;
    assert!(matches!(outcome, Err(ref why) if why.contains("deadline has expired")));
    assert_eq!(job.steps(), 0, "no model turn was admitted");
    assert!(!runtime.manager.read().await.worker_records["expired"].has_unreported_usage);
    assert!(
        runtime
            .manager
            .read()
            .await
            .handback_reservations
            .is_empty()
    );
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Addendum F4 (fleet-5): cancelling keeps the work. A Stop on a
/// write-scoped child appends the same preservation receipt a budget death
/// gets, exactly once, and leaves a read-only child's result alone.
#[tokio::test]
async fn cancel_appends_work_preservation_note_once() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Budget test"]);
    git(root, &["config", "user.email", "budget@example.invalid"]);
    fs::write(root.join("src.rs"), "baseline\n").unwrap();
    git(root, &["add", "--", "src.rs"]);
    git(root, &["commit", "--quiet", "-m", "baseline"]);

    let manager = Arc::new(RwLock::new(SubAgentManager::new(root.to_path_buf(), 2)));
    for (agent_id, write) in [("cancel-writer", true), ("cancel-scout", false)] {
        let mut spec = make_worker_spec(agent_id, root.to_path_buf());
        spec.runtime_profile.permissions.write = write;
        let mut guard = manager.write().await;
        guard.register_worker(spec);
        let (input_tx, _input_rx) = mpsc::unbounded_channel();
        let mut agent = SubAgent::new(
            agent_id.to_string(),
            FleetRole::Worker,
            "work that gets stopped".to_string(),
            SubAgentAssignment {
                native_preset: None,
                objective: "edit".to_string(),
                role: Some("worker".to_string()),
            },
            "deepseek-v4-flash".to_string(),
            None,
            None,
            input_tx,
            root.to_path_buf(),
            guard.current_session_boot_id.clone(),
        );
        agent.task_handle = Some(tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }));
        guard.agents.insert(agent_id.to_string(), agent);
    }
    fs::create_dir_all(root.join("scratch")).unwrap();
    fs::write(root.join("scratch/half-done.rs"), "wip\n").unwrap();

    let stopped = manager.write().await.cancel_agent("cancel-writer").unwrap();
    let preserved = preserve_cancelled_work(&manager, stopped).await;
    let text = preserved.result.as_deref().unwrap_or_default();
    assert!(text.starts_with(CANCELLED_BY_PARENT_RESULT), "{text}");
    assert!(text.contains("scratch/half-done.rs"), "{text}");
    let stored = manager.read().await.get_result("cancel-writer").unwrap();
    assert_eq!(stored.result, preserved.result, "the receipt is persisted");

    // A repeated Stop does not stack a second receipt.
    let again = manager.write().await.cancel_agent("cancel-writer").unwrap();
    let again = preserve_cancelled_work(&manager, again).await;
    assert_eq!(again.result, preserved.result);

    // A read-only child has no baseline: its result stays the plain Stop.
    let scout = manager.write().await.cancel_agent("cancel-scout").unwrap();
    let scout = preserve_cancelled_work(&manager, scout).await;
    assert_eq!(scout.result.as_deref(), Some(CANCELLED_BY_PARENT_RESULT));
}

/// F4: a Stop cascades to descendants, and each write-scoped descendant
/// stopped with the parent gets its own receipt; a read-only one does not.
#[tokio::test]
async fn cancel_receipts_each_writing_descendant_stopped_with_its_parent() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Budget test"]);
    git(root, &["config", "user.email", "budget@example.invalid"]);
    fs::write(root.join("src.rs"), "baseline\n").unwrap();
    git(root, &["add", "--", "src.rs"]);
    git(root, &["commit", "--quiet", "-m", "baseline"]);

    let manager = Arc::new(RwLock::new(SubAgentManager::new(root.to_path_buf(), 4)));
    for (agent_id, write, parent) in [
        ("tree-parent", true, None),
        ("tree-writer", true, Some("tree-parent")),
        ("tree-scout", false, Some("tree-parent")),
        ("tree-stranger", true, None),
    ] {
        let mut spec = make_worker_spec(agent_id, root.to_path_buf());
        spec.runtime_profile.permissions.write = write;
        spec.parent_run_id = parent.map(str::to_string);
        let mut guard = manager.write().await;
        guard.register_worker(spec);
        let (input_tx, _input_rx) = mpsc::unbounded_channel();
        let mut agent = SubAgent::new(
            agent_id.to_string(),
            FleetRole::Worker,
            "work that gets stopped".to_string(),
            SubAgentAssignment {
                native_preset: None,
                objective: "edit".to_string(),
                role: Some("worker".to_string()),
            },
            "deepseek-v4-flash".to_string(),
            None,
            None,
            input_tx,
            root.to_path_buf(),
            guard.current_session_boot_id.clone(),
        );
        agent.task_handle = Some(tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
        }));
        guard.agents.insert(agent_id.to_string(), agent);
    }
    fs::create_dir_all(root.join("scratch")).unwrap();
    fs::write(root.join("scratch/half-done.rs"), "wip\n").unwrap();

    // The cascade order of `cancel_agent_for_session`: descendants, then the
    // target. The unrelated writer is stopped too but is not a descendant.
    let parent = {
        let mut guard = manager.write().await;
        for id in ["tree-writer", "tree-scout", "tree-stranger"] {
            guard.cancel_agent(id).unwrap();
        }
        guard.cancel_agent("tree-parent").unwrap()
    };
    let parent = preserve_cancelled_work(&manager, parent).await;
    assert!(
        parent
            .result
            .as_deref()
            .is_some_and(|text| text.contains("scratch/half-done.rs")),
        "{:?}",
        parent.result
    );

    let guard = manager.read().await;
    let writer = guard.get_result("tree-writer").unwrap();
    let writer_text = writer.result.as_deref().unwrap_or_default();
    assert!(
        writer_text.starts_with(CANCELLED_BY_PARENT_RESULT),
        "{writer_text}"
    );
    assert!(
        writer_text.contains("scratch/half-done.rs"),
        "{writer_text}"
    );
    let scout = guard.get_result("tree-scout").unwrap();
    assert_eq!(scout.result.as_deref(), Some(CANCELLED_BY_PARENT_RESULT));
    let stranger = guard.get_result("tree-stranger").unwrap();
    assert_eq!(stranger.result.as_deref(), Some(CANCELLED_BY_PARENT_RESULT));
}

/// #5529: a budget death must name the work the worker left on disk. The
/// spawn-time delivery baseline is what makes the inventory attributable to
/// this worker rather than the parent's own dirty files.
#[tokio::test]
async fn run_death_preservation_note_names_surviving_workspace_changes() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Budget test"]);
    git(root, &["config", "user.email", "budget@example.invalid"]);
    fs::write(root.join("src.rs"), "baseline\n").unwrap();
    git(root, &["add", "--", "src.rs"]);
    git(root, &["commit", "--quiet", "-m", "baseline"]);

    let manager = Arc::new(RwLock::new(SubAgentManager::new(root.to_path_buf(), 2)));
    let mut spec = make_worker_spec("preserve-worker", root.to_path_buf());
    spec.runtime_profile.permissions.write = true;
    manager.write().await.register_worker(spec);

    // The worker's unfinished work lands after the baseline was captured.
    fs::create_dir_all(root.join("scratch")).unwrap();
    fs::write(root.join("scratch/leftover.rs"), "wip\n").unwrap();

    let mut runtime = stub_runtime();
    runtime.manager = Arc::clone(&manager);

    let note =
        budget_work_preservation_note(&runtime.manager, "preserve-worker", "wall_time_budget")
            .await
            .expect("write-scoped worker has a baseline");
    assert!(
        note.contains("scratch/leftover.rs"),
        "note should name the surviving path: {note}"
    );
    assert!(note.contains(&root.display().to_string()), "{note}");

    // A read-only worker captured no baseline — there is no file work to
    // inventory and the note stays absent rather than lying.
    let mut scout_spec = make_worker_spec("scout-worker", root.to_path_buf());
    scout_spec.runtime_profile.permissions.write = false;
    manager.write().await.register_worker(scout_spec);
    assert!(
        budget_work_preservation_note(&runtime.manager, "scout-worker", "wall_time_budget")
            .await
            .is_none()
    );

    // A write-scoped worker that changed nothing still gets an explicit
    // "no changes" receipt instead of silence.
    let mut clean_spec = make_worker_spec("clean-worker", root.to_path_buf());
    clean_spec.runtime_profile.permissions.write = true;
    clean_spec.workspace = root.to_path_buf();
    let clean_root = tempdir().unwrap();
    let clean_path = clean_root.path();
    git(clean_path, &["init", "--quiet"]);
    git(clean_path, &["config", "user.name", "Budget test"]);
    git(
        clean_path,
        &["config", "user.email", "budget@example.invalid"],
    );
    fs::write(clean_path.join("src.rs"), "baseline\n").unwrap();
    git(clean_path, &["add", "--", "src.rs"]);
    git(clean_path, &["commit", "--quiet", "-m", "baseline"]);
    clean_spec.workspace = clean_path.to_path_buf();
    manager.write().await.register_worker(clean_spec);
    let note = budget_work_preservation_note(&runtime.manager, "clean-worker", "wall_time_budget")
        .await
        .expect("baseline exists");
    assert!(note.contains("No workspace changes"), "{note}");
}

fn git_out(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// #6194 item 4 / #5529: on an isolated worktree a budget death commits the
/// worker's uncommitted changes as labeled salvage instead of leaving them
/// for manual recovery.
#[tokio::test]
async fn budget_death_checkpoint_commits_uncommitted_work_on_isolated_worktree() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Budget test"]);
    git(root, &["config", "user.email", "budget@example.invalid"]);
    fs::write(root.join("src.rs"), "baseline\n").unwrap();
    git(root, &["add", "--", "src.rs"]);
    git(root, &["commit", "--quiet", "-m", "baseline"]);

    let manager = Arc::new(RwLock::new(SubAgentManager::new(root.to_path_buf(), 2)));
    let mut spec = make_worker_spec("checkpoint-worker", root.to_path_buf());
    spec.runtime_profile.permissions.write = true;
    spec.launch_manifest = Some(ChildLaunchManifest {
        owner_session: "root".to_string(),
        child_id: "checkpoint-worker".to_string(),
        profile: spec.runtime_profile.clone(),
        prompt: spec.objective.clone(),
        cwd: Some(root.display().to_string()),
        worktree: true,
        writable_roots: vec![root.display().to_string()],
        writable_files: Vec::new(),
        coordination_contracts: Vec::new(),
        expected_artifact: None,
        deliverables: Vec::new(),
        resume_identity: None,
        generation: 1,
        resume_from_agent_id: None,
    });
    manager.write().await.register_worker(spec);
    fs::write(root.join("src.rs"), "baseline\nuncommitted fix\n").unwrap();
    fs::write(root.join("new.rs"), "wip\n").unwrap();

    let mut runtime = stub_runtime();
    runtime.manager = Arc::clone(&manager);
    let note =
        budget_work_preservation_note(&runtime.manager, "checkpoint-worker", "wall_time_budget")
            .await
            .expect("note");
    assert!(
        note.contains("checkpointed in commit"),
        "note should name the salvage commit: {note}"
    );
    let subject = git_out(root, &["log", "--format=%s", "-1"]);
    assert!(
        subject.starts_with("checkpoint: checkpoint-worker (wall_time_budget)"),
        "marker message names the worker and cause: {subject}"
    );
    assert!(
        git_out(root, &["status", "--porcelain=v1", "--"]).is_empty(),
        "checkpoint leaves a clean tree"
    );
}

/// A shared checkout may hold the parent's or a sibling's dirty files, so no
/// auto-commit happens there — the note keeps the manual-salvage wording.
#[tokio::test]
async fn budget_death_checkpoint_skips_shared_checkout() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Budget test"]);
    git(root, &["config", "user.email", "budget@example.invalid"]);
    fs::write(root.join("src.rs"), "baseline\n").unwrap();
    git(root, &["add", "--", "src.rs"]);
    git(root, &["commit", "--quiet", "-m", "baseline"]);

    let manager = Arc::new(RwLock::new(SubAgentManager::new(root.to_path_buf(), 2)));
    let mut spec = make_worker_spec("shared-worker", root.to_path_buf());
    spec.runtime_profile.permissions.write = true;
    manager.write().await.register_worker(spec);
    fs::write(root.join("src.rs"), "baseline\nuncommitted fix\n").unwrap();

    let mut runtime = stub_runtime();
    runtime.manager = Arc::clone(&manager);
    let note = budget_work_preservation_note(&runtime.manager, "shared-worker", "wall_time_budget")
        .await
        .expect("note");
    assert!(!note.contains("checkpointed in commit"), "{note}");
    assert!(note.contains("survive on disk"), "{note}");
    assert!(
        !git_out(root, &["status", "--porcelain=v1", "--"]).is_empty(),
        "shared checkout stays dirty"
    );
}

/// When the worker committed everything itself before death, the note says so
/// instead of claiming a checkpoint or manual salvage.
#[tokio::test]
async fn budget_death_checkpoint_reports_worker_committed_tree() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    git(root, &["init", "--quiet"]);
    git(root, &["config", "user.name", "Budget test"]);
    git(root, &["config", "user.email", "budget@example.invalid"]);
    fs::write(root.join("src.rs"), "baseline\n").unwrap();
    git(root, &["add", "--", "src.rs"]);
    git(root, &["commit", "--quiet", "-m", "baseline"]);

    let manager = Arc::new(RwLock::new(SubAgentManager::new(root.to_path_buf(), 2)));
    let mut spec = make_worker_spec("tidy-worker", root.to_path_buf());
    spec.runtime_profile.permissions.write = true;
    spec.launch_manifest = Some(ChildLaunchManifest {
        owner_session: "root".to_string(),
        child_id: "tidy-worker".to_string(),
        profile: spec.runtime_profile.clone(),
        prompt: spec.objective.clone(),
        cwd: Some(root.display().to_string()),
        worktree: true,
        writable_roots: vec![root.display().to_string()],
        writable_files: Vec::new(),
        coordination_contracts: Vec::new(),
        expected_artifact: None,
        deliverables: Vec::new(),
        resume_identity: None,
        generation: 1,
        resume_from_agent_id: None,
    });
    manager.write().await.register_worker(spec);
    fs::write(root.join("src.rs"), "baseline\nworker fix\n").unwrap();
    git(root, &["add", "--", "src.rs"]);
    git(root, &["commit", "--quiet", "-m", "worker fix"]);

    let mut runtime = stub_runtime();
    runtime.manager = Arc::clone(&manager);
    let note = budget_work_preservation_note(&runtime.manager, "tidy-worker", "wall_time_budget")
        .await
        .expect("note");
    assert!(note.contains("committed before death"), "{note}");
}

fn assistant_message(content: Vec<ContentBlock>) -> Message {
    Message {
        role: Role::Assistant,
        content,
    }
}

fn tool_use(name: &str, input: Value) -> ContentBlock {
    ContentBlock::ToolUse {
        execution_id: None,
        id: format!("call_{name}"),
        name: name.to_string(),
        input,
        caller: None,
        thought_signature: None,
    }
}

#[test]
fn fallback_partial_text_prefers_last_assistant_text() {
    let messages = vec![
        assistant_message(vec![ContentBlock::Text {
            text: "first".to_string(),
            cache_control: None,
        }]),
        assistant_message(vec![
            tool_use("Read", json!({"path": "src/main.rs"})),
            ContentBlock::Text {
                text: "second".to_string(),
                cache_control: None,
            },
        ]),
    ];
    assert_eq!(budget_handback::fallback_partial_text(&messages), "second");
}

#[test]
fn fallback_partial_text_digests_thinking_and_tool_calls_without_text() {
    let messages = vec![
        assistant_message(vec![ContentBlock::Thinking {
            thinking: "checking whether the ring slot write precedes the read".to_string(),
            signature: None,
            state: None,
            redacted_data: None,
        }]),
        assistant_message(vec![tool_use("Read", json!({"path": "ring.rs"}))]),
        assistant_message(vec![tool_use(
            "Grep",
            json!({"pattern": "slot", "path": "ring.rs"}),
        )]),
    ];
    let digest = budget_handback::fallback_partial_text(&messages);
    assert!(digest.contains("Tool calls (newest first)"), "{digest}");
    assert!(digest.contains("- Grep ring.rs"), "{digest}");
    assert!(digest.contains("- Read ring.rs"), "{digest}");
    assert!(
        digest.find("- Grep").unwrap() < digest.find("- Read").unwrap(),
        "{digest}"
    );
    assert!(digest.contains("unverified"), "{digest}");
    assert!(
        digest.contains("ring slot write precedes the read"),
        "{digest}"
    );
}

#[test]
fn fallback_partial_text_caps_tool_entries_and_reports_overflow() {
    let messages: Vec<Message> = (0..14)
        .map(|i| {
            assistant_message(vec![tool_use(
                "Read",
                json!({"path": format!("file_{i}.rs")}),
            )])
        })
        .collect();
    let digest = budget_handback::fallback_partial_text(&messages);
    assert!(digest.contains("...and 2 more"), "{digest}");
    assert!(!digest.contains("file_0.rs"), "{digest}");
    assert!(digest.contains("file_13.rs"), "{digest}");
}

#[test]
fn fallback_partial_text_is_silent_only_when_nothing_was_recorded() {
    assert!(budget_handback::fallback_partial_text(&[]).contains("No assistant text was recorded"));
    let user_only = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: "do the thing".to_string(),
            cache_control: None,
        }],
    }];
    assert!(
        budget_handback::fallback_partial_text(&user_only)
            .contains("No assistant text was recorded")
    );
}

#[test]
fn budget_repair_only_rewrites_the_newly_synthesized_final_execution() {
    for (first, last) in [(Some("first"), Some("last")), (None, None)] {
        let mut messages: Vec<Message> = serde_json::from_value(json!([
            {"role":"assistant","content":[{"type":"tool_use","id":"reused","execution_id":first,"name":"read","input":{}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"reused","execution_id":first,"content":"earlier completed output"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"reused","execution_id":last,"name":"read","input":{}}]}
        ])).unwrap();
        let earlier = messages[..2].to_vec();
        budget_handback::repair_stopped_tool_calls(&mut messages, "fixture deadline");
        assert_eq!(&messages[..2], earlier.as_slice());
        assert!(
            matches!(&messages[3].content[0], ContentBlock::ToolResult { execution_id, content, .. }
            if execution_id.as_deref() == last && content.contains("budget_exhausted"))
        );
    }
}

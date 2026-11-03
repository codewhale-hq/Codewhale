use super::*;
use crate::llm_client::mock::{MockLlmClient, canned};
use crate::tools::spec::{ToolCapability, ToolContext, ToolSpec};

#[cfg(unix)]
#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn engine_cancel_stops_started_foreground_descendants_and_preserves_background() {
    let _env = lock_test_env();
    let workspace = tempdir().expect("workspace");
    let pid_file = workspace.path().join("descendant.pid");
    let command = format!(
        "printf 'foreground-started\\n'; CODEWHALE_SHELL_DESCENDANT_HELPER=1 \
         CODEWHALE_SHELL_DESCENDANT_PID_FILE={} {} --exact \
         tools::shell::tests::shell_descendant_helper_process --nocapture",
        shell_words::quote(&pid_file.display().to_string()),
        shell_words::quote(&std::env::current_exe().unwrap().display().to_string()),
    );
    let arguments = json!({"command": command, "timeout": 3600}).to_string();
    let mock = Arc::new(MockLlmClient::new(vec![
        tool_batch_turn(&[
            ("call-foreground", "bash", &arguments),
            (
                "call-skipped",
                "bash",
                r#"{"command":"touch must-not-start"}"#,
            ),
        ]),
        canned::simple_text_turn("Next user turn completed."),
    ]));
    let config = Config::default();
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        mock.clone(),
    );
    let shell_manager = engine.shell_manager.clone();
    let session_id = engine.session.id.clone();
    let background_id = shell_manager
        .lock()
        .unwrap()
        .execute_with_options_env_for_session(
            "sleep 30",
            None,
            30_000,
            true,
            None,
            false,
            None,
            HashMap::new(),
            &session_id,
        )
        .expect("start intentionally backgrounded control")
        .task_id
        .unwrap();
    let task = tokio::spawn(engine.run());
    let mut op = external_user_message_op("Run the foreground fixture.", AppMode::Agent, &config);
    if let Op::SendMessage(TurnSpec {
        trust_mode,
        auto_approve,
        approval_mode,
        ..
    }) = &mut op
    {
        *trust_mode = true;
        *auto_approve = true;
        *approval_mode = ApprovalMode::Bypass;
    }
    handle.send(op).await.expect("dispatch real shell tool");
    let descendant: libc::pid_t = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(raw) = fs::read_to_string(&pid_file)
                && let Ok(pid) = raw.trim().parse()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the actual descendant must start before cancellation");
    handle.cancel();

    let mut receipts = Vec::new();
    let mut foreground_execution_id = None;
    let mut skipped = false;
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut events = handle.rx_event.write().await;
        loop {
            match events.recv().await.expect("engine remains alive") {
                Event::ToolCallComplete {
                    id,
                    result,
                    model_call: Some(model_call),
                    ..
                } if model_call.provider_id == "call-foreground" => {
                    foreground_execution_id = Some(id);
                    receipts.push(result.expect("model-visible cancellation receipt"));
                }
                Event::ToolCallComplete {
                    model_call: Some(model_call),
                    result,
                    ..
                } if model_call.provider_id == "call-skipped" => {
                    let result = result.unwrap();
                    assert_eq!(result.metadata.unwrap()["executed"], false);
                    assert!(result.content.contains("before this tool ran"));
                    skipped = true;
                }
                Event::TurnComplete { status, error, .. } => {
                    assert_eq!(status, TurnOutcomeStatus::Interrupted, "{error:?}");
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .expect("the cancelled turn must settle");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            // This is the PID written by the isolated helper, not an ambient process.
            if unsafe { libc::kill(descendant, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("foreground descendant must be gone before recovery");
    assert_eq!(receipts.len(), 1);
    assert!(
        skipped,
        "the unstarted call must have its own truthful receipt"
    );
    assert!(!workspace.path().join("must-not-start").exists());
    assert!(!receipts[0].success);
    assert!(receipts[0].content.contains("after shell work started"));
    assert!(receipts[0].content.contains("Killed"));
    assert!(!receipts[0].content.contains("before this tool ran"));
    assert_eq!(receipts[0].metadata.as_ref().unwrap()["executed"], true);
    {
        let mut manager = shell_manager.lock().unwrap();
        let jobs = manager.list_jobs_for_session(&session_id);
        let foreground = jobs
            .iter()
            .find(|job| job.origin_tool_call_id.as_deref() == foreground_execution_id.as_deref())
            .expect("the exact foreground owner remains inspectable");
        assert_eq!(foreground.status, crate::tools::shell::ShellStatus::Killed);
        assert!(foreground.stdout_tail.contains("foreground-started"));
        assert_eq!(
            manager.inspect_job(&background_id).unwrap().snapshot.status,
            crate::tools::shell::ShellStatus::Running
        );
        assert!(
            !manager.has_finished_unreported_jobs_for_session(&session_id),
            "cancelled foreground work must not wake an unsolicited model turn"
        );
    }
    assert_eq!(mock.call_count(), 1);
    let snapshot = tokio::time::timeout(Duration::from_secs(10), handle.get_session_snapshot())
        .await
        .expect("cancelled session must remain inspectable")
        .unwrap();
    let manager =
        crate::session_manager::SessionManager::new(workspace.path().join("checkpoints")).unwrap();
    let saved = crate::session_manager::create_saved_session_with_id_and_mode(
        session_id.clone(),
        &snapshot.messages,
        "mock-model",
        workspace.path(),
        0,
        None,
        Some("agent"),
    );
    manager.save_checkpoint(&saved).unwrap();
    let restored = manager
        .load_session_checkpoint(&session_id)
        .unwrap()
        .unwrap();
    assert!(restored.messages.iter().flat_map(|message| &message.content).any(|block| matches!(
        block, ContentBlock::ToolResult { tool_use_id, content, is_error: Some(true), .. }
            if tool_use_id == "call-foreground" && content.contains("after shell work started")
    )));

    handle
        .send(external_user_message_op(
            "Continue after cancellation.",
            AppMode::Agent,
            &config,
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), handle.get_session_snapshot())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        mock.call_count(),
        2,
        "only the next explicit user turn may resume"
    );
    assert_eq!(
        guardian_tool_results(&mock.captured_requests()[1], "call-foreground")[0].1,
        Some(true)
    );
    shell_manager.lock().unwrap().kill(&background_id).unwrap();
    handle.send(Op::Shutdown).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("engine must stop after shutdown")
        .unwrap();
}

struct ReturnedResultTool;

#[async_trait::async_trait]
impl ToolSpec for ReturnedResultTool {
    fn name(&self) -> &str {
        "returned_result"
    }
    fn description(&self) -> &str {
        "Return the selected success or failure fixture."
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"fail":{"type":"boolean"}},"required":["fail"]})
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::ReadOnly]
    }
    async fn execute(&self, input: Value, _context: &ToolContext) -> Result<ToolResult, ToolError> {
        Ok(if input["fail"] == true {
            ToolResult::error("fixture failure")
        } else {
            ToolResult::success("fixture success")
        })
    }
}

#[tokio::test]
async fn returned_tool_failure_reaches_next_model_request_as_error() {
    let workspace = tempdir().unwrap();
    let mock = Arc::new(MockLlmClient::new(vec![
        tool_batch_turn(&[
            ("call-failure", "returned_result", r#"{"fail":true}"#),
            ("call-success", "returned_result", r#"{"fail":false}"#),
        ]),
        canned::simple_text_turn("Both tool results received."),
    ]));
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        mock.clone(),
    );
    let mut registry = crate::tools::ToolRegistry::new(ToolContext::new(workspace.path()));
    registry.register(Arc::new(ReturnedResultTool));
    let tools = Some(registry.to_api_tools_with_cache(true));
    let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
    let mut turn = TurnContext::new(4);
    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        guardian_tool_results(&requests[1], "call-failure"),
        vec![("fixture failure", Some(true))]
    );
    assert_eq!(
        guardian_tool_results(&requests[1], "call-success"),
        vec![("fixture success", None)]
    );
    let mut events = handle.rx_event.write().await;
    let results = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| match event {
            Event::ToolCallComplete {
                model_call: Some(model_call),
                result,
                ..
            } => Some((model_call.provider_id, result)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        results.iter().any(|(id, result)| id == "call-failure"
            && result.as_ref().is_ok_and(|output| !output.success)),
        "this must cover Ok(ToolResult::error), not Err(ToolError)"
    );
}

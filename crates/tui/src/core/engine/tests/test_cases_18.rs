/// A thinking-only mid-stream drop must recover without ever claiming a
/// visible partial reply was preserved, and must leave exactly one
/// authoritative assistant answer in the persisted conversation.
#[tokio::test]
async fn interactive_thinking_only_drop_preserves_nothing_and_never_claims_it_did() {
    let model = std::sync::Arc::new(ThinkingOnlyDropModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        failures: 1,
    });
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = Config::default();
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        subagents_enabled: false,
        terminal_chrome_enabled: true,
        ..EngineConfig::default()
    };
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    let run_task = tokio::spawn(engine.run());

    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "solve the task".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&config, crate::config::DEFAULT_TEXT_MODEL),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::ExternalUser,
            submission_id: None,
        }))
        .await
        .expect("send thinking-only drop turn");

    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("thinking-only drop event timeout")
        .expect("thinking-only drop event");
        let terminal = matches!(event, Event::TurnComplete { .. });
        events.push(event);
        if terminal {
            break;
        }
    }
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");

    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the thinking-only drop must be re-issued exactly once"
    );
    let status = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, .. } => Some(*status),
            _ => None,
        })
        .expect("terminal TurnComplete");
    assert_eq!(status, TurnOutcomeStatus::Completed);

    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Status { message } if message.starts_with("Retry attempt: stream-resume 1/")
        )),
        "the first retry must be visible without claiming hidden reasoning was preserved"
    );

    // The persisted conversation keeps the operator's turn and exactly one
    // authoritative assistant answer — no synthetic `[runtime]` user message,
    // no duplicated answer, no orphaned thinking-only assistant cell.
    let transcript = events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::SessionUpdated { messages, .. } => Some(messages.clone()),
            _ => None,
        })
        .expect("final SessionUpdated");
    assert_eq!(
        transcript
            .iter()
            .filter(|message| message.role == "user")
            .count(),
        1,
        "the operator's own turn must be the only user message: {transcript:?}"
    );
    assert_eq!(
        transcript
            .iter()
            .filter(|message| message.role == "assistant")
            .count(),
        1,
        "exactly one authoritative assistant answer after recovery: {transcript:?}"
    );
    let transcript_text = transcript
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !transcript_text.contains("[runtime]"),
        "a retried turn must not insert a synthetic user message: {transcript_text}"
    );
    assert!(
        !transcript_text.contains("hidden reasoning that no operator ever saw"),
        "an invisible thinking-only fragment must not be persisted as reply text: {transcript_text}"
    );
    assert_eq!(
        transcript_text
            .matches("the one authoritative answer")
            .count(),
        1,
        "the recovered answer must be persisted exactly once: {transcript_text}"
    );
}

/// A model client that answers with ONLY hidden reasoning and a clean stop for
/// its first `reasoning_only` calls, then a real text answer. No transport
/// error: the stream completes normally but carries no sendable content — the
/// reasoning-model failure mode #5546-adjacent that used to dead-end the turn.
struct ReasoningOnlyCleanFinishModelClient {
    calls: std::sync::atomic::AtomicUsize,
    reasoning_only: usize,
    stop_reason: &'static str,
    /// Every outbound request's messages, in order, so a test can tell a
    /// request-scoped nudge from one written into the session.
    requests: std::sync::Mutex<Vec<Vec<codewhale_models::Message>>>,
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for ReasoningOnlyCleanFinishModelClient {
    fn provider_name(&self) -> &str {
        "reasoning-only"
    }

    fn model(&self) -> &str {
        "local-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("reasoning-only recovery uses the streaming model boundary")
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        use crate::llm_client::mock::canned;
        if let Ok(mut requests) = self.requests.lock() {
            requests.push(_request.messages.clone());
        }
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        if call <= self.reasoning_only {
            // A protocol-complete response that opened and closed only a
            // thinking block: no text, no tool call, and a clean stop reason.
            let events: Vec<anyhow::Result<codewhale_models::StreamEvent>> = vec![
                Ok(canned::message_start("reasoning_only_msg")),
                Ok(StreamEvent::ContentBlockStart {
                    index: 0,
                    content_block: codewhale_models::ContentBlockStart::Thinking {
                        thinking: String::new(),
                    },
                }),
                Ok(canned::thinking_delta(0, "reasoning with no final channel")),
                Ok(canned::block_stop(0)),
                Ok(canned::message_delta(self.stop_reason, None)),
                Ok(canned::message_stop()),
            ];
            return Ok(Box::pin(futures_util::stream::iter(events)));
        }
        let events = canned::simple_text_turn("the recovered answer")
            .into_iter()
            .map(Ok);
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

async fn run_reasoning_only_turn(
    reasoning_only: usize,
    stop_reason: &'static str,
) -> (
    std::sync::Arc<ReasoningOnlyCleanFinishModelClient>,
    Vec<Event>,
) {
    run_reasoning_only_turn_with_reprompts(
        reasoning_only,
        stop_reason,
        crate::config::DEFAULT_REASONING_ONLY_REPROMPTS,
    )
    .await
}

async fn run_reasoning_only_turn_with_reprompts(
    reasoning_only: usize,
    stop_reason: &'static str,
    max_reprompts: u32,
) -> (
    std::sync::Arc<ReasoningOnlyCleanFinishModelClient>,
    Vec<Event>,
) {
    let model = std::sync::Arc::new(ReasoningOnlyCleanFinishModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        reasoning_only,
        stop_reason,
        requests: std::sync::Mutex::new(Vec::new()),
    });
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = Config::default();
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        subagents_enabled: false,
        terminal_chrome_enabled: true,
        reasoning_only_max_reprompts: max_reprompts,
        ..EngineConfig::default()
    };
    let (engine, handle) = Engine::new_with_model_client(engine_config, &config, client);
    let run_task = tokio::spawn(engine.run());
    handle
        .send(Op::SendMessage(TurnSpec {
            profile_constitution: None,
            max_output_tokens: None,
            content: "solve the task".to_string(),
            images: Vec::new(),
            mode: AppMode::Agent,
            route: resolved_route_for_test(&config, crate::config::DEFAULT_TEXT_MODEL),
            compaction: Box::new(CompactionConfig::default()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: crate::tools::goal::GoalStatus::Active,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            translation_enabled: false,
            allowed_tools: None,
            dynamic_tools: Vec::new(),
            hook_executor: None,
            verbosity: None,
            provenance: UserInputProvenance::ExternalUser,
            submission_id: None,
        }))
        .await
        .expect("send reasoning-only turn");
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("reasoning-only event timeout")
        .expect("reasoning-only event");
        let terminal = matches!(event, Event::TurnComplete { .. });
        events.push(event);
        if terminal {
            break;
        }
    }
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    (model, events)
}

/// A reasoning-only clean-stop response is re-requested and the turn recovers
/// with the real answer. Local fixtures make no cache-hit or billing claim.
#[tokio::test]
async fn reasoning_only_clean_stop_is_retried_and_recovers() {
    let (model, events) = run_reasoning_only_turn(1, "stop").await;

    let terminal = events
        .iter()
        .find_map(|event| match event {
            Event::ToolRequestSnapshot { snapshot } => snapshot.terminal.as_ref(),
            _ => None,
        })
        .expect("terminal diagnostics through existing event authority");
    assert_eq!(terminal.model_requests_started, 2);
    assert_eq!(terminal.reasoning_only_reprompts, 1);
    assert_eq!(terminal.transparent_stream_retries, 0);
    assert_eq!(terminal.status, Some(TurnOutcomeStatus::Completed));

    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the reasoning-only response must be re-requested exactly once"
    );
    let status = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, .. } => Some(*status),
            _ => None,
        })
        .expect("terminal TurnComplete");
    assert_eq!(status, TurnOutcomeStatus::Completed);

    let recovery = events
        .iter()
        .filter_map(|event| match event {
            Event::Status { message } if message.contains("re-requesting the answer") => {
                Some(message.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(recovery.len(), 1, "exactly one recovery notice: {events:?}");
    assert!(
        recovery[0].starts_with("Retry attempt: reasoning-only 1/"),
        "attempt announced: {recovery:?}"
    );

    // No hard failure surfaced.
    assert!(
        !events.iter().any(|event| matches!(
            event,
            Event::Error { envelope, .. } if envelope.message.contains("no answer or tool call")
        )),
        "a recovered turn must not surface the incomplete-response error: {events:?}"
    );
    assert_eq!(events.iter().filter(|event| matches!(event, Event::Status { message } if message.starts_with("Retry recovery: reasoning-only used 1/") && message.ends_with("turn completed"))).count(), 1);
}

/// An output-length stop is a real budget hit, not a transient — it must NOT
/// be retried and must fail honestly.
#[tokio::test]
async fn reasoning_only_length_stop_fails_without_retry() {
    let (model, events) = run_reasoning_only_turn(1, "length").await;

    let diagnostic = events
        .iter()
        .find_map(|event| match event {
            Event::ToolRequestSnapshot { snapshot } => snapshot.terminal.as_ref(),
            _ => None,
        })
        .expect("terminal request diagnostics");
    assert!(
        diagnostic
            .last_prepared_output_limit_tokens
            .is_some_and(|tokens| tokens > 0)
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Error { envelope, .. }
                if envelope.message.contains("response output limit")
                    && envelope.message.contains("including reasoning")
        )),
        "a length stop must explain the actual output constraint"
    );

    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a length stop must not be retried"
    );
    assert!(
        !events.iter().any(|event| matches!(
            event,
            Event::Status { message } if message.contains("re-requesting the answer")
        )),
        "a length stop must not announce a retry: {events:?}"
    );
    let status = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, .. } => Some(*status),
            _ => None,
        })
        .expect("terminal TurnComplete");
    assert_eq!(status, TurnOutcomeStatus::Failed);
}

/// The reasoning-only nudge rides one request and is never written to the
/// session.
///
/// This is the distinction that matters: a nudge added with
/// `add_session_message` would persist into the transcript, the exports, and
/// every later turn's context — a message the user never sent. A
/// request-scoped nudge appears in exactly one outbound request and leaves the
/// conversation as it found it.
///
/// The two are told apart by message counts across successive requests. With
/// a ceiling of 3 the model is asked four times. Persisted, the counts would
/// grow cumulatively (n, n, n+1, n+2); request-scoped, the nudged requests
/// each carry exactly one extra message over the same baseline.
#[tokio::test]
async fn the_reasoning_only_nudge_rides_one_request_and_never_joins_the_session() {
    let (model, events) = run_reasoning_only_turn_with_reprompts(usize::MAX, "stop", 3).await;

    let requests = model.requests.lock().expect("captured requests").clone();
    assert_eq!(requests.len(), 4, "one initial request plus three retries");

    let baseline = requests[0].len();
    assert_eq!(
        requests[1].len(),
        baseline,
        "the first retry is a bare cached-prefix re-request, with no nudge"
    );
    assert_eq!(
        requests[2].len(),
        baseline + 1,
        "the second retry carries the nudge"
    );
    assert_eq!(
        requests[3].len(),
        baseline + 1,
        "the nudge did not accumulate: it was spent on the previous request, \
         not added to the session"
    );

    let nudge = crate::config::DEFAULT_REASONING_ONLY_REPROMPT_MESSAGE;
    let carries_nudge = |messages: &Vec<codewhale_models::Message>| {
        serde_json::to_string(messages)
            .expect("messages serialize")
            .contains(nudge)
    };
    assert!(!carries_nudge(&requests[0]), "no nudge before any failure");
    assert!(!carries_nudge(&requests[1]), "no nudge on the first retry");
    assert!(
        carries_nudge(&requests[2]),
        "nudge present once retrying again"
    );

    // C02-04: model-visible means logged. Every request the nudge rides
    // leaves a durable (internal) receipt carrying its exact text.
    let receipts: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            Event::Status { message }
                if message.starts_with(super::turn_loop::REQUEST_NUDGE_RECEIPT_PREFIX) =>
            {
                Some(message.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        receipts.len(),
        requests
            .iter()
            .filter(|request| carries_nudge(request))
            .count(),
        "one receipt per nudged request"
    );
    assert!(receipts.iter().all(|receipt| receipt.ends_with(nudge)));
    assert_eq!(
        crate::core::events::status_visibility(receipts[0]),
        crate::core::events::StatusVisibility::Internal,
        "durable clients keep the receipt, collapsed"
    );
}

/// A model that only ever returns reasoning is bounded: it retries up to the
/// ceiling and then fails honestly rather than looping forever.
#[tokio::test]
async fn reasoning_only_forever_is_bounded_then_fails() {
    let (model, events) = run_reasoning_only_turn(usize::MAX, "stop").await;

    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1 + crate::config::DEFAULT_REASONING_ONLY_REPROMPTS as usize,
        "reasoning-only retries are bounded by [reasoning_only] max_reprompts"
    );
    let status = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, .. } => Some(*status),
            _ => None,
        })
        .expect("terminal TurnComplete");
    assert_eq!(status, TurnOutcomeStatus::Failed);
    let attempts = events
        .iter()
        .filter_map(|event| match event {
            Event::Status { message } if message.starts_with("Retry attempt: reasoning-only ") => {
                Some(message)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    let max = crate::config::DEFAULT_REASONING_ONLY_REPROMPTS;
    assert_eq!(attempts.len(), max as usize);
    for (index, message) in attempts.iter().enumerate() {
        assert!(message.starts_with(&format!(
            "Retry attempt: reasoning-only {}/{};",
            index + 1,
            max
        )));
    }
    assert_eq!(events.iter().filter(|event| matches!(event, Event::Status { message } if message == &format!("Retry stopped: reasoning-only used {max}/{max} retries; turn failed"))).count(), 1);
}

#[tokio::test]
async fn headless_turn_fails_with_real_error_after_network_drop_budget_exhausted() {
    let (model, events) =
        run_headless_turn_with_flaky_network(1 + super::MAX_STREAM_RETRIES as usize).await;

    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1 + super::MAX_STREAM_RETRIES as usize,
        "initial attempt plus the bounded resume budget, then the turn fails"
    );
    let (status, error) = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, error, .. } => Some((status, error)),
            _ => None,
        })
        .expect("terminal TurnComplete");
    assert_eq!(*status, TurnOutcomeStatus::Failed);
    let error = error
        .as_deref()
        .expect("exhausted network-drop retries must report the real error");
    assert!(
        error.contains("Provider stream connection dropped"),
        "the surfaced error must name the network drop: {error}"
    );
    assert!(
        error.contains("error decoding response body"),
        "the underlying provider error must stay attached: {error}"
    );
    assert_eq!(
        crate::error_taxonomy::classify_error_message(error),
        crate::error_taxonomy::ErrorCategory::Network,
        "the terminal failure must classify as retryable infra (network)"
    );
    let error_events = events
        .iter()
        .filter(|event| matches!(event, Event::Error { .. }))
        .count();
    assert_eq!(
        error_events, 1,
        "only the final, budget-exhausted attempt may emit an error event: {events:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event,
                Event::Status { message } if message.starts_with("Retry attempt: stream-resume ")
            ))
            .count(),
        super::MAX_STREAM_RETRIES as usize,
        "every admitted resume must have its own numbered progress receipt"
    );
    for attempt in 1..=super::MAX_STREAM_RETRIES {
        assert!(events.iter().any(|event| matches!(event,
            Event::Status { message } if message.starts_with(&format!("Retry attempt: stream-resume {attempt}/"))
        )));
    }
}

// === Issue #66: error taxonomy wired through engine + audit + capacity ===

/// A failed-tool audit entry must carry the typed `category` and `severity`
/// fields derived from the underlying `ToolError`. This is what makes
/// downstream tooling able to bucket failures without scraping the message
/// string.
#[test]
fn tool_failure_audit_payload_carries_category_and_severity() {
    use crate::error_taxonomy::ErrorEnvelope;
    use crate::tools::spec::ToolError;

    let error = ToolError::Timeout { seconds: 30 };
    let envelope: ErrorEnvelope = error.clone().into();
    let payload = json!({
        "event": "tool.result",
        "tool_id": "tool-1",
        "tool_name": "exec_shell",
        "status": ToolExecutionOutcome::from_legacy(Err(error.clone())).status.as_str(),
        "success": false,
        "error": error.to_string(),
        "category": envelope.category.to_string(),
        "severity": envelope.severity.to_string(),
    });

    assert_eq!(payload["category"], "timeout");
    assert_eq!(payload["severity"], "warning");
    assert_eq!(payload["status"], "timed_out");
    assert_eq!(payload["success"], false);
}

// ── #136: post-edit LSP diagnostics hook ─────────────────────────────────

#[test]
fn edited_paths_scenario() {
    // Scenario consolidation of: edited_paths_for_edit_file_returns_path, edited_paths_for_write_file_returns_path, edited_paths_for_apply_patch_with_replace_returns_each_path, edited_paths_for_apply_patch_with_legacy_changes_returns_each_path, edited_paths_for_apply_patch_with_diff_text_extracts_paths, edited_paths_for_apply_patch_with_invalid_diff_returns_empty, edited_paths_for_unknown_tool_returns_empty
    // from edited_paths_for_edit_file_returns_path
    {
        let input = json!({ "path": "src/foo.rs", "search": "x", "replace": "y" });
        let paths = edited_paths_for_tool("edit_file", &input);
        assert_eq!(paths, vec![PathBuf::from("src/foo.rs")]);
    }
    // from edited_paths_for_write_file_returns_path
    {
        let input = json!({ "path": "src/bar.rs", "content": "fn main() {}" });
        let paths = edited_paths_for_tool("write_file", &input);
        assert_eq!(paths, vec![PathBuf::from("src/bar.rs")]);
    }
    // from edited_paths_for_apply_patch_with_replace_returns_each_path
    {
        let input = json!({
            "replace": [
                { "path": "a.rs", "content": "" },
                { "path": "b.rs", "content": "" }
            ]
        });
        let paths = edited_paths_for_tool("apply_patch", &input);
        assert_eq!(paths, vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
    }
    // from edited_paths_for_apply_patch_with_legacy_changes_returns_each_path
    {
        let input = json!({
            "changes": [
                { "path": "a.rs", "content": "" },
                { "path": "b.rs", "content": "" }
            ]
        });
        let paths = edited_paths_for_tool("apply_patch", &input);
        assert_eq!(paths, vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
    }
    // from edited_paths_for_apply_patch_with_diff_text_extracts_paths
    {
        let input = json!({
            "patch": "--- a/foo.rs\n+++ b/foo.rs\n@@ -1 +1 @@\n-let x: i32 = 0;\n+let x: i32 = \"oops\";\n"
        });
        let paths = edited_paths_for_tool("apply_patch", &input);
        assert_eq!(paths, vec![PathBuf::from("foo.rs")]);
    }
    // from edited_paths_for_apply_patch_with_invalid_diff_returns_empty
    {
        let input = json!({
            "patch": "@@ -1 +1 @@\n-old\n+new\n"
        });
        let paths = edited_paths_for_tool("apply_patch", &input);
        assert!(paths.is_empty());
    }
    // from edited_paths_for_unknown_tool_returns_empty
    {
        let input = json!({ "path": "irrelevant.rs" });
        let paths = edited_paths_for_tool("read_file", &input);
        assert!(paths.is_empty());
        let paths = edited_paths_for_tool("grep_files", &input);
        assert!(paths.is_empty());
    }
}

#[test]
fn parse_patch_paths_skips_dev_null() {
    let patch = "--- a/keep.rs\n+++ b/keep.rs\n@@ -1 +1 @@\n-old\n+new\n--- a/deleted.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-delete me\n";
    let paths = edited_paths_for_tool("apply_patch", &json!({ "patch": patch }));
    assert_eq!(paths, vec![PathBuf::from("keep.rs")]);
}

#[tokio::test]
async fn post_edit_hook_injects_diagnostics_message_before_next_request() {
    use crate::lsp::{Diagnostic, Language, Severity};
    use std::sync::Arc;

    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().to_path_buf();
    let target = workspace.join("src").join("main.rs");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(&target, "let x: i32 = \"not a number\";").unwrap();

    let lsp_config = crate::lsp::LspConfig::default();
    let engine_config = EngineConfig {
        workspace: workspace.clone(),
        lsp_config: Some(lsp_config),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(engine_config, &Config::default());

    // Install a fake transport that always reports a type error.
    let fake = Arc::new(crate::lsp::tests::FakeTransport::new(vec![Diagnostic {
        line: 1,
        column: 14,
        severity: Severity::Error,
        message: "expected i32, found &str".to_string(),
    }]));
    engine
        .lsp_manager
        .install_test_transport(Language::Rust, fake)
        .await;

    // Simulate the success path of an edit_file tool call.
    let input = json!({ "path": "src/main.rs", "search": "0", "replace": "\"not a number\"" });
    engine.run_post_edit_lsp_hook("edit_file", &input).await;
    assert_eq!(engine.pending_lsp_blocks.len(), 1);

    // Flush prepares the synthetic message.
    let messages_before = engine.session.messages.len();
    engine.flush_pending_lsp_diagnostics().await;
    assert_eq!(engine.session.messages.len(), messages_before + 1);

    let last = engine.session.messages.last().expect("message appended");
    assert_eq!(last.role, "user");
    // turn_meta is now at the tail of the content array (PR #2517).
    let meta = match last.content.last() {
        Some(codewhale_models::ContentBlock::Text { text, .. }) => text.clone(),
        other => panic!("expected text block at tail, got {other:?}"),
    };
    assert!(meta.starts_with("<turn_meta>\n"));
    let diagnostic_text = last
        .content
        .iter()
        .find_map(|block| match block {
            codewhale_models::ContentBlock::Text { text, .. }
                if text.contains("<diagnostics file=\"") =>
            {
                Some(text)
            }
            _ => None,
        })
        .expect("diagnostics text block");
    assert!(diagnostic_text.contains("ERROR [1:14] expected i32, found &str"));
}

#[tokio::test]
async fn post_edit_hook_is_silent_when_lsp_disabled() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().to_path_buf();
    let target = workspace.join("src").join("main.rs");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(&target, "fn main() {}").unwrap();

    let lsp_config = crate::lsp::LspConfig {
        enabled: false,
        ..Default::default()
    };
    let engine_config = EngineConfig {
        workspace: workspace.clone(),
        lsp_config: Some(lsp_config),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(engine_config, &Config::default());

    let input = json!({ "path": "src/main.rs", "search": "x", "replace": "y" });
    engine.run_post_edit_lsp_hook("edit_file", &input).await;
    assert!(engine.pending_lsp_blocks.is_empty());

    let messages_before = engine.session.messages.len();
    engine.flush_pending_lsp_diagnostics().await;
    assert_eq!(engine.session.messages.len(), messages_before);
}

#[tokio::test]
async fn post_edit_hook_skips_unknown_tool_names() {
    use crate::lsp::{Diagnostic, Language, Severity};
    use std::sync::Arc;

    let tmp = tempdir().expect("tempdir");
    let engine_config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        lsp_config: Some(crate::lsp::LspConfig::default()),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(engine_config, &Config::default());
    let fake = Arc::new(crate::lsp::tests::FakeTransport::new(vec![Diagnostic {
        line: 1,
        column: 1,
        severity: Severity::Error,
        message: "should not be reported".to_string(),
    }]));
    engine
        .lsp_manager
        .install_test_transport(Language::Rust, fake.clone())
        .await;

    let input = json!({ "path": "src/main.rs" });
    engine.run_post_edit_lsp_hook("read_file", &input).await;
    assert!(engine.pending_lsp_blocks.is_empty());
    assert_eq!(fake.call_count(), 0);
}

// ── #3802: non-blocking send for ListSubAgents refresh events ─────────────

#[test]
fn agent_list_event_carries_the_typed_coordination_projection() {
    use crate::tools::subagent::coord::{DecisionRecord, DecisionStatus};

    let mut manager = SubAgentManager::new(PathBuf::from("."), 1);
    let recorded = manager
        .record_coordination_decision(DecisionRecord {
            decision_id: "decision-event".to_string(),
            subject: "typed event".to_string(),
            status: DecisionStatus::Accepted,
            owner: "root".to_string(),
            scope: Vec::new(),
            constraints: Vec::new(),
            evidence_handles: Vec::new(),
            version: 1,
            sequence: 0,
        })
        .expect("record decision");
    manager
        .stamp_coordination_sequence_for_session(recorded.sequence, "session-a")
        .expect("stamp decision owner");

    let Event::AgentList {
        owner_session_id,
        agents,
        coordination,
        ..
    } = agent_list_event(&manager, "session-a")
    else {
        panic!("expected AgentList event");
    };
    assert!(agents.is_empty());
    assert_eq!(owner_session_id, "session-a");
    assert_eq!(coordination.decisions.len(), 1);
    assert_eq!(coordination.decisions[0].decision_id, "decision-event");
    assert_eq!(coordination.decisions[0].status, DecisionStatus::Accepted);
    assert!(coordination.bounded);
    assert_eq!(coordination.limit, 24);
}

#[test]
fn engine_handle_try_send_does_not_block_when_op_channel_is_full() {
    use tokio::sync::mpsc;

    // Create a channel with the smallest possible capacity.
    let (tx_op, rx_op) = mpsc::channel::<Op>(1);

    // Construct a minimal EngineHandle with the tiny channel.
    let cancel_token = CancellationToken::new();
    let handle = EngineHandle {
        goal_state: new_shared_goal_state(),
        tx_op,
        rx_event: Arc::new(RwLock::new(mpsc::channel::<Event>(1).1)),
        cancel_token: Arc::new(StdMutex::new(cancel_token)),
        cancel_reason: Arc::new(StdMutex::new(None)),
        tx_approval: mpsc::channel(1).0,
        tx_user_input: mpsc::channel(1).0,
        tx_steer: mpsc::channel(1).0,
        turn_controls: Arc::new(StdMutex::new(handle::TurnControls::default())),
        shared_paused: Arc::new(StdMutex::new(false)),
        client_preflight_required: true,
        live_runtime_authority: Arc::new(StdMutex::new(LiveRuntimeAuthorityState::new(
            LiveRuntimeAuthority::from_fields(
                AppMode::Agent,
                false,
                false,
                false,
                ApprovalMode::Suggest,
                None,
            ),
        ))),
        compaction_cancellation: Arc::new(StdMutex::new(CompactionCancellationState::default())),
        turn_heartbeat: turn_heartbeat::TurnHeartbeat::new(),
        subagent_manager: crate::tools::subagent::new_shared_subagent_manager(
            std::env::temp_dir(),
            1,
        ),
    };

    // Fill the op channel with one message (capacity = 1).
    handle
        .tx_op
        .try_send(Op::ListSubAgents)
        .expect("first send should succeed");

    // A live posture update must publish immediately even though its wake-up
    // cannot fit. The already-queued operation will wake the engine, which
    // applies this pending authority before handling it.
    let result = handle.try_send(Op::ChangeMode {
        mode: AppMode::Operate,
        allow_shell: true,
        trust_mode: false,
        auto_approve: false,
        approval_mode: ApprovalMode::Auto,
        configured_sandbox_mode: None,
    });
    let error = result.expect_err("try_send should fail when channel is full");
    assert!(matches!(
        error.downcast_ref::<mpsc::error::TrySendError<Op>>(),
        Some(mpsc::error::TrySendError::Full(Op::ChangeMode { .. }))
    ));
    let authority = handle.runtime_permission_authority();
    assert_eq!(authority.approval_mode, ApprovalMode::Auto);
    assert!(!authority.auto_approve);

    handle
        .cancel_compaction("compact-full-mailbox")
        .expect("full mailbox must not block compaction cancellation");
    assert!(
        handle
            .compaction_cancellation
            .lock()
            .expect("cancellation state")
            .claim("compact-full-mailbox")
            .is_none(),
        "cancellation authority remains visible even when its wake-up op cannot fit"
    );
    drop(rx_op);
    let error = handle.try_send(Op::ListSubAgents).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<mpsc::error::TrySendError<Op>>(),
        Some(mpsc::error::TrySendError::Closed(Op::ListSubAgents))
    ));
}

#[tokio::test]
async fn full_mailbox_posture_update_supersedes_queued_change_mode() {
    use ApprovalMode;

    let tmp = tempdir().expect("tempdir");
    let config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (engine, handle) = Engine::new(config, &Config::default());

    handle
        .try_send(Op::ChangeMode {
            mode: AppMode::Plan,
            allow_shell: false,
            trust_mode: false,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            configured_sandbox_mode: None,
        })
        .expect("queue older posture");
    for _ in 1..ENGINE_OP_CHANNEL_CAPACITY {
        handle
            .try_send(Op::ListSubAgents)
            .expect("fill operation mailbox");
    }

    let result = handle.try_send(Op::ChangeMode {
        mode: AppMode::Operate,
        allow_shell: true,
        trust_mode: false,
        auto_approve: false,
        approval_mode: ApprovalMode::Auto,
        configured_sandbox_mode: Some("read-only".to_string()),
    });
    assert!(
        result.is_err(),
        "latest posture wake-up must see a full mailbox"
    );

    let run = tokio::spawn(engine.run());
    let snapshot = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        handle.get_session_snapshot(),
    )
    .await
    .expect("snapshot after mailbox drain")
    .expect("session snapshot");

    assert_eq!(snapshot.mode, "operate");
    let authority = handle.runtime_permission_authority();
    assert_eq!(authority.approval_mode, ApprovalMode::Auto);
    assert!(!authority.auto_approve);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run.await.expect("engine task");
}

#[tokio::test]
async fn reload_mcp_op_recovers_from_invalid_initial_config_in_process() {
    let tmp = tempdir().expect("tempdir");
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let config_path = tmp.path().join("mcp.json");
    let secret = "mcp-op-secret-must-not-escape";
    std::fs::write(
        &config_path,
        format!(r#"{{"servers":{{"bad":{{"token":"{secret}"}} trailing}}}}"#),
    )
    .expect("invalid config");
    let engine_config = EngineConfig {
        workspace,
        mcp_config_path: config_path.clone(),
        ..Default::default()
    };
    let (engine, handle) = Engine::new(engine_config, &Config::default());
    let task = tokio::spawn(async move { engine.run().await });

    let error = handle
        .reload_mcp(config_path.clone())
        .await
        .expect_err("invalid config must fail closed");
    assert!(!error.to_string().contains(secret));
    std::fs::write(
        &config_path,
        r#"{"servers":{"ready":{"command":"node","disabled":true}}}"#,
    )
    .expect("fixed config");

    let snapshot = handle
        .reload_mcp(config_path.clone())
        .await
        .expect("fixed config reloads without restarting the engine")
        .snapshot;
    assert!(!snapshot.reload_required);
    assert_eq!(snapshot.servers.len(), 1);
    assert_eq!(snapshot.servers[0].name, "ready");
    assert!(!snapshot.servers[0].enabled);

    let alternate_path = tmp.path().join("alternate-mcp.json");
    std::fs::write(
        &alternate_path,
        r#"{"servers":{"alternate":{"command":"node","disabled":true}}}"#,
    )
    .expect("alternate config");
    let alternate = handle
        .reload_mcp(alternate_path.clone())
        .await
        .expect("a changed config path replaces the engine pool in process")
        .snapshot;
    assert_eq!(alternate.config_path, alternate_path);
    assert_eq!(alternate.servers.len(), 1);
    assert_eq!(alternate.servers[0].name, "alternate");

    handle.send(Op::Shutdown).await.expect("shutdown");
    task.await.expect("engine task");
}

#[tokio::test]
async fn mcp_boot_reports_ready_server_before_stalled_server_finishes() {
    assert_incremental_mcp_boot(false).await;
}

#[tokio::test]
async fn first_turn_waits_for_explicit_mcp_schema_without_waiting_for_unrelated_server() {
    let Some(node) = crate::dependencies::resolve_node() else {
        return;
    };
    let tmp = tempdir().expect("tempdir");
    let server = tmp.path().join("server.mjs");
    let release = tmp.path().join("release-slow");
    let release_fast = tmp.path().join("release-fast");
    fs::write(&server, r#"import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
readline.createInterface({ input: process.stdin }).on('line', async line => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (request.method === 'initialize') {
    fs.writeFileSync(path.join(process.argv[3], 'started-' + process.argv[2]), 'ready');
    while (!fs.existsSync(path.join(process.argv[3], 'release-' + process.argv[2]))) await new Promise(r => setTimeout(r, 10));
  }
  const result = request.method === 'initialize'
    ? { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: process.argv[2], version: '1' } }
    : { tools: ['ready', 'denied', 'hidden'].map(name => ({ name, inputSchema: { type: 'object' } })) };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});"#).expect("fixture");
    let config_path = tmp.path().join("mcp.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&json!({
            // `slow` must still be connecting when the fast build completes,
            // and `fast` must not be declared dead while a cold Windows runner
            // spawns Node. Ordering here is proven by the release files below,
            // never by a timeout, so this bound only has to outlast the test.
            "timeouts": { "connect_timeout": 120 },
            "servers": {
                "fast": { "command": node, "args": [server, "fast", tmp.path()] },
                // `slow` is deliberately unselected: `required` keeps it in
                // the eager boot set under lazy boot (#6033) so it can stand
                // in for "an unrelated server still connecting".
                "slow": { "command": node, "args": [server, "slow", tmp.path()], "required": true },
                "failed": { "command": "codewhale-missing-mcp-fixture-38911" }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let api_config = Config::default();
    let (mut engine, _handle) = Engine::new(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            mcp_config_path: config_path,
            tools_always_load: HashSet::from(["mcp_fast_ready".to_string()]),
            ..Default::default()
        },
        &api_config,
    );
    engine
        .start_mcp_session_boot(McpConnectRefresh::IfChanged)
        .await
        .expect("session boot starts");
    assert!(
        engine.mcp_tools().await.is_empty(),
        "ordinary startup remains nonblocking"
    );
    // Separate Windows/CI process startup from the schema-wait assertion.
    // Both children have received initialize, but neither can answer until
    // this test releases its own gate. No fixed delay stands in for readiness.
    // The budget is generous because it covers two cold Node spawns on a
    // windows-latest runner that has just finished a ~15 min compile; a tight
    // bound here fails the setup, not the behavior under test.
    tokio::time::timeout(Duration::from_secs(60), async {
        while !tmp.path().join("started-fast").exists() || !tmp.path().join("started-slow").exists()
        {
            engine.drain_mcp_boot_updates().await;
            for name in ["fast", "slow"] {
                assert!(
                    !engine.mcp_connection_errors.contains_key(name),
                    "{name} fixture failed before initialize: {:?}",
                    engine.mcp_connection_errors.get(name)
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both MCP fixtures must reach initialize before checking first-turn ordering");
    let route = TurnRouteContext {
        provider: ProviderKind::Deepseek,
        model: DEFAULT_TEXT_MODEL.to_string(),
        capabilities: codewhale_config::route::RouteCapabilities::default(),
        limits: None,
        client: engine.codewhale_client.clone(),
        api_config: Box::new(api_config),
        locale_tag: engine.config.locale_tag.clone(),
        role_models: engine.subagent_role_models(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
    };
    let policy = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        false,
        false,
        false,
        ApprovalMode::Suggest,
    );
    let build = {
        let build = engine.build_turn_tool_registry_and_catalog(
            &policy,
            &[],
            Some(vec![
                "mcp_fast_ready".to_string(),
                "mcp_failed_ready".to_string(),
            ]),
            SubAgentWiring::Inert,
            McpAccess::Connect,
            route,
            "",
        );
        tokio::pin!(build);
        std::future::poll_fn(|cx| {
            assert!(
                std::future::Future::poll(build.as_mut(), cx).is_pending(),
                "the first turn must wait for the explicitly selected fast schema"
            );
            std::task::Poll::Ready(())
        })
        .await;
        fs::write(&release_fast, "release").unwrap();
        tokio::time::timeout(Duration::from_secs(5), build).await
    };
    let unrelated_pending = !release.exists()
        && engine.mcp_boot_in_flight
        && !engine.mcp_connection_errors.contains_key("slow");
    let connected = engine
        .mcp_pool
        .as_ref()
        .unwrap()
        .lock()
        .await
        .connected_servers()
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    engine.cancel_token.cancel();
    tokio::time::timeout(
        Duration::from_millis(100),
        engine.wait_for_explicit_mcp_boot(Some(&["mcp_slow_ready".to_string()])),
    )
    .await
    .expect("stop interrupts explicit schema wait");
    let _turn_control = engine.begin_turn_control();
    fs::write(&release, "release").unwrap();
    let build = build.unwrap_or_else(|error| {
        panic!(
            "explicit fast/failed selections must not wait for slow: {error:?}; connected={connected:?}; errors={:?}",
            engine.mcp_connection_errors
        )
    });
    assert!(
        unrelated_pending,
        "success must precede the unrelated server's release, completion, or timeout"
    );
    let active = build.surface.active.unwrap_or_default();
    assert_eq!(
        active
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["mcp_fast_ready"]
    );
    assert!(engine.mcp_connection_errors.contains_key("failed"));

    // The unrelated connection finishes during the same turn. Refresh into a
    // narrowed policy, then execute the actual tool-search activation path.
    let policy = ToolSurfacePolicy::new(
        ToolRegistryBuilder::new().build(ToolContext::for_empty_registry()),
        Some(vec![api_tool("read")]),
        AppMode::Agent,
        &HashSet::new(),
        &[],
        false,
        Some(vec![
            "tool_search".into(),
            "mcp_slow_ready".into(),
            "mcp_slow_denied".into(),
        ]),
        Some(vec!["mcp_slow_denied".into()]),
        None,
        crate::core::engine::tool_catalog::ToolMode::Direct,
    );
    let mut catalog = policy.catalog.clone();
    let mut active = policy.active_names.clone();
    catalog.push(api_tool("mcp_removed_ready"));
    active.insert("mcp_removed_ready".to_string());
    tokio::time::timeout(Duration::from_secs(5), async {
        while !catalog.iter().any(|tool| tool.name == "mcp_slow_ready") {
            engine
                .refresh_boot_mcp_catalog(&policy, &mut catalog, &mut active)
                .await;
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed tools join this turn");
    assert!(
        !active.contains("mcp_slow_ready"),
        "fresh MCP tools stay deferred"
    );
    assert!(
        !active.contains("mcp_removed_ready"),
        "removed authority leaves active tools"
    );
    assert!(
        catalog
            .iter()
            .all(|tool| !tool.name.starts_with("mcp_") || tool.name == "mcp_slow_ready")
    );
    let result = tool_catalog::execute_tool_search_with_cache(
        "tool_search",
        &json!({"query":"mcp_slow_ready", "match":"regex"}),
        &catalog,
        &mut active,
        &mut engine.session.tool_activation_cache,
    )
    .expect("real search");
    assert!(result.success);
    assert!(active.contains("mcp_slow_ready"));
    engine.wait_for_mcp_boot().await;
}

#[tokio::test]
async fn mcp_boot_does_not_restore_servers_removed_during_handshake() {
    assert_incremental_mcp_boot(true).await;
}

async fn assert_incremental_mcp_boot(invalidate_config: bool) {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        tracing::warn!("skipping MCP stdio fixture because node is unavailable");
        return;
    }
    let tmp = tempdir().expect("tempdir");
    let server = tmp.path().join("server.mjs");
    let release = tmp.path().join("release-slow");
    std::fs::write(
        &server,
        r#"import fs from 'node:fs';
import readline from 'node:readline';
const lines = readline.createInterface({ input: process.stdin });
lines.on('line', async (line) => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (process.argv[2] === 'slow' && request.method === 'initialize') {
    while (!fs.existsSync(process.argv[3])) {
      await new Promise(resolve => setTimeout(resolve, 10));
    }
  }
  const result = request.method === 'initialize'
    ? { protocolVersion: '2024-11-05', capabilities: { tools: {} },
        serverInfo: { name: process.argv[2], version: '1' } }
    : { tools: [{ name: 'ready', inputSchema: { type: 'object' } }] };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});
"#,
    )
    .expect("server fixture");
    let config_path = tmp.path().join("mcp.json");
    std::fs::write(
        &config_path,
        serde_json::to_vec(&serde_json::json!({
            "timeouts": { "connect_timeout": 30 },
            "servers": {
                // Both marked `required` so lazy boot (#6033) still starts
                // them eagerly — this test proves progress ordering, not the
                // lazy/eligible split.
                "fast": { "command": "node", "args": [server, "fast", release], "required": true },
                "slow": { "command": "node", "args": [server, "slow", release], "required": true }
            }
        }))
        .expect("config JSON"),
    )
    .expect("MCP config");
    let (mut engine, handle) = Engine::new(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            mcp_config_path: config_path.clone(),
            ..Default::default()
        },
        &Config::default(),
    );
    let pool = engine.ensure_mcp_pool().await.expect("engine pool");
    let task = tokio::spawn(async move { engine.run().await });
    let mut events = handle.rx_event.write().await;
    // This proves ordering, not Node cold-start speed on a loaded runner.
    let progress = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = events.recv().await {
            if let Event::McpSessionBoot {
                snapshot,
                connecting,
                finished: false,
                ..
            } = event
                && connecting == ["slow"]
            {
                return snapshot;
            }
        }
        panic!("engine event channel closed");
    })
    .await;
    let ready_tools = pool.lock().await.to_api_tools();
    if invalidate_config {
        std::fs::write(
            &config_path,
            r#"{"servers":{"slow":{"command":"node","disabled":true}}}"#,
        )
        .expect("remove servers");
        pool.lock()
            .await
            .reload_if_config_changed()
            .await
            .expect("reload config");
    }
    // Release and shut down even when testing the old batch-buffered behavior.
    std::fs::write(&release, "continue").expect("release stalled fixture");
    let finished = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = events.recv().await {
            if let Event::McpSessionBoot {
                snapshot,
                finished: true,
                ..
            } = event
            {
                return snapshot;
            }
        }
        panic!("engine event channel closed");
    })
    .await;
    drop(events);
    handle.send(Op::Shutdown).await.expect("shutdown");
    task.await.expect("engine task");
    let progress = progress.expect("fast server must be visible before slow server is released");
    assert!(
        progress
            .servers
            .iter()
            .any(|row| row.name == "fast" && row.connected)
    );
    assert!(
        progress
            .servers
            .iter()
            .any(|row| row.name == "slow" && !row.connected)
    );
    assert!(ready_tools.iter().any(|tool| tool.name == "mcp_fast_ready"));
    assert!(!ready_tools.iter().any(|tool| tool.name == "mcp_slow_ready"));
    let finished = finished.expect("finished boot");
    if invalidate_config {
        assert_eq!(finished.servers.len(), 1);
        assert!(!finished.servers[0].enabled);
        assert!(!finished.servers[0].connected);
        assert!(pool.lock().await.to_api_tools().is_empty());
    } else {
        assert!(finished.servers.iter().all(|row| row.connected));
    }
}

/// Lazy boot (#6033): a configured server nobody selected and nobody marked
/// `required` must not be spawned at session start. The fixture writes a
/// `started-<name>` marker when it receives `initialize`, so the lazy
/// server's absence is proven by the file that never appears — not by a
/// timeout on "it would have started by now".
#[tokio::test]
async fn lazy_boot_leaves_unselected_servers_unspawned() {
    let Some(node) = crate::dependencies::resolve_node() else {
        return;
    };
    let tmp = tempdir().expect("tempdir");
    let server = tmp.path().join("server.mjs");
    fs::write(
        &server,
        r#"import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
readline.createInterface({ input: process.stdin }).on('line', line => {
  const request = JSON.parse(line);
  if (request.id === undefined) return;
  if (request.method === 'initialize') {
    fs.writeFileSync(path.join(process.argv[3], 'started-' + process.argv[2]), 'ready');
  }
  const result = request.method === 'initialize'
    ? { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: process.argv[2], version: '1' } }
    : { tools: [{ name: 'ready', inputSchema: { type: 'object' } }] };
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id: request.id, result }) + '\n');
});"#,
    )
    .expect("fixture");
    let config_path = tmp.path().join("mcp.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&serde_json::json!({
            "servers": {
                "eager": { "command": node, "args": [server, "eager", tmp.path()], "required": true },
                "lazy": { "command": node, "args": [server, "lazy", tmp.path()], "lazy": true }
            }
        }))
        .unwrap(),
    )
    .expect("MCP config");
    let (engine, handle) = Engine::new(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            mcp_config_path: config_path,
            ..Default::default()
        },
        &Config::default(),
    );
    let task = tokio::spawn(async move { engine.run().await });
    let mut events = handle.rx_event.write().await;
    let finished = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = events.recv().await {
            if let Event::McpSessionBoot {
                snapshot,
                connecting,
                finished: true,
                ..
            } = event
            {
                return (snapshot, connecting);
            }
        }
        panic!("engine event channel closed");
    })
    .await
    .expect("boot must finish");
    drop(events);
    handle.send(Op::Shutdown).await.expect("shutdown");
    task.await.expect("engine task");

    let (snapshot, connecting) = finished;
    assert!(
        tmp.path().join("started-eager").exists(),
        "a required server is still eager"
    );
    assert!(
        !tmp.path().join("started-lazy").exists(),
        "an unselected, unrequired server must not be spawned at boot"
    );
    assert!(connecting.is_empty());
    let lazy = snapshot
        .servers
        .iter()
        .find(|server| server.name == "lazy")
        .expect("configured lazy server still appears in the snapshot");
    assert!(!lazy.connected);
    assert!(lazy.error.is_none(), "lazy is a state, not a failure");
    let eager = snapshot
        .servers
        .iter()
        .find(|server| server.name == "eager")
        .expect("required server in snapshot");
    assert!(eager.connected);
}

#[tokio::test]
async fn mcp_boot_updates_preserve_authority_errors_and_replace_ordinary_errors() {
    let tmp = tempdir().expect("tempdir");
    let engine_config = EngineConfig {
        workspace: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let (mut engine, _handle) = Engine::new(engine_config, &Config::default());
    engine.mcp_event_generation = 3;
    engine.mcp_boot_generation = Some(3);
    engine.mcp_boot_in_flight = true;
    engine.mcp_connection_errors = HashMap::from([(
        "stale-transport".to_string(),
        "obsolete connection failure".to_string(),
    )]);
    let authority_errors = Arc::new(HashMap::from([(
        "revoked-plugin".to_string(),
        "plugin authority revoked or changed".to_string(),
    )]));

    engine
        .apply_mcp_boot_update(McpBootUpdate::Progress {
            generation: 3,
            authority_errors: Arc::clone(&authority_errors),
            connection_errors: HashMap::from([(
                "current-transport".to_string(),
                "current connection failure".to_string(),
            )]),
            connecting: Vec::new(),
        })
        .await;
    assert_eq!(
        engine.mcp_connection_errors,
        HashMap::from([
            (
                "revoked-plugin".to_string(),
                "plugin authority revoked or changed".to_string(),
            ),
            (
                "current-transport".to_string(),
                "current connection failure".to_string(),
            ),
        ])
    );
    assert!(!engine.mcp_connection_errors.contains_key("stale-transport"));
    assert_eq!(
        engine.session.pending_prefix_change_reason.as_deref(),
        Some("mcp-session-boot")
    );

    engine.mcp_connection_errors.insert(
        "stale-between-updates".to_string(),
        "must not survive finished".to_string(),
    );
    engine
        .apply_mcp_boot_update(McpBootUpdate::Finished {
            generation: 3,
            authority_errors,
            connection_errors: HashMap::from([(
                "final-transport".to_string(),
                "final connection failure".to_string(),
            )]),
        })
        .await;
    assert_eq!(
        engine.mcp_connection_errors,
        HashMap::from([
            (
                "revoked-plugin".to_string(),
                "plugin authority revoked or changed".to_string(),
            ),
            (
                "final-transport".to_string(),
                "final connection failure".to_string(),
            ),
        ])
    );
}
// Actual optional stdio servers; no provider request or invented tool catalogue.
async fn mcp_search_fixture(
    allowed: Option<Vec<String>>,
) -> (
    Engine,
    tool_catalog::ToolSurfacePolicy,
    tempfile::TempDir,
    Arc<crate::llm_client::mock::MockLlmClient>,
) {
    let node =
        crate::dependencies::resolve_node().expect("MCP discovery qualification requires Node");
    let tmp = tempdir().expect("tempdir");
    let server = tmp.path().join("discovery.mjs");
    fs::write(&server, r#"import fs from 'node:fs';
import path from 'node:path';
import readline from 'node:readline';
readline.createInterface({input:process.stdin}).on('line', line => {
  const r = JSON.parse(line); if (r.id === undefined) return;
  const name = process.argv[2], root = process.argv[3];
  if (r.method === 'initialize') {
    fs.writeFileSync(path.join(root, 'started-' + name), 'started');
    if (name === 'stalled') return;
  }
  if (r.method === 'tools/list') fs.writeFileSync(path.join(root, 'listed-' + name), 'listed');
  const result = r.method === 'initialize'
    ? {protocolVersion:'2024-11-05',capabilities:{tools:{}},serverInfo:{name,version:'1'}}
    : {tools:[{name:'actual',description:'Actual MCP fixture',inputSchema:{type:'object',properties:{verified:{type:'boolean'}},required:['verified']}}]};
  process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result}) + '\n');
});"#).unwrap();
    let config_path = tmp.path().join("mcp.json");
    fs::write(
        &config_path,
        serde_json::to_vec(&json!({"servers": {
            "engram": {"command":node,"args":[server,"engram",tmp.path()],"required":false,"lazy":true},
            "unrelated": {"command":node,"args":[server,"unrelated",tmp.path()],"required":false,"lazy":true},
            "disabled": {"command":node,"args":[server,"disabled",tmp.path()],"enabled":false},
            "stalled": {"command":node,"args":[server,"stalled",tmp.path()],"required":false,"lazy":true},
            "failed": {"command":"codewhale-missing-mcp-discovery-6828","lazy":true}
        }}))
        .unwrap(),
    )
    .unwrap();
    let api = Config::default();
    let model = Arc::new(crate::llm_client::mock::MockLlmClient::new(Vec::new()));
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, _) = Engine::new_with_model_client(
        EngineConfig {
            workspace: tmp.path().to_path_buf(),
            mcp_config_path: config_path,
            ..Default::default()
        },
        &api,
        client,
    );
    engine
        .start_mcp_session_boot(McpConnectRefresh::IfChanged)
        .await
        .unwrap();
    assert!(
        !tmp.path().join("started-engram").exists(),
        "optional boot must remain lazy"
    );
    let authority = crate::core::authority::TurnAuthority::from_effective_fields(
        AppMode::Agent,
        true,
        false,
        false,
        ApprovalMode::Suggest,
    );
    let route = TurnRouteContext {
        provider: ProviderKind::Deepseek,
        model: DEFAULT_TEXT_MODEL.to_string(),
        capabilities: codewhale_config::route::RouteCapabilities::default(),
        limits: None,
        client: engine.codewhale_client.clone(),
        api_config: Box::new(api),
        locale_tag: engine.config.locale_tag.clone(),
        role_models: engine.subagent_role_models(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
    };
    let build = engine
        .build_turn_tool_registry_and_catalog(
            &authority,
            &[],
            allowed,
            SubAgentWiring::Inert,
            McpAccess::Connect,
            route,
            "discovery",
        )
        .await;
    (engine, build.surface, tmp, model)
}

#[tokio::test]
async fn mcp_tool_search_discovers_optional_server_real_schema_without_eager_siblings() {
    let (mut engine, policy, tmp, model) = mcp_search_fixture(None).await;
    let mut surface = crate::core::engine::ChildSurfaceProbe {
        policy,
        cache: crate::core::session::ToolActivationCache::default(),
    };
    let result = engine
        .probe_child_tool_batch(
            &mut surface,
            crate::core::engine::ChildProbeCall {
                id: "mcp-discovery".into(),
                execution_id: "mcp-discovery".into(),
                name: "tool_search".into(),
                input: json!({"query":"engram","match":"bm25"}),
            },
        )
        .await
        .expect("actual Core planner and executor search");
    assert!(result.result.success);
    let catalog = &surface.policy.catalog;
    assert!(
        tmp.path().join("listed-engram").exists(),
        "schema must come from tools/list"
    );
    assert!(!tmp.path().join("started-unrelated").exists());
    let tool = catalog
        .iter()
        .find(|tool| tool.name == "mcp_engram_actual")
        .expect("real advertised tool");
    assert_eq!(
        tool.input_schema["properties"]["verified"]["type"],
        "boolean"
    );
    assert_eq!(tool.input_schema["required"], json!(["verified"]));
    assert!(surface.policy.active_names.contains("mcp_engram_actual"));
    assert!(
        result.result.metadata.as_ref().unwrap()["tool_references"]
            .as_array()
            .unwrap()
            .iter()
            .any(|name| name == "mcp_engram_actual")
    );
    assert!(!catalog.iter().any(|tool| tool.name == "mcp_engram_guessed"));
    assert_eq!(
        model.call_count(),
        0,
        "MCP discovery must not call a provider"
    );
}

#[tokio::test]
async fn mcp_tool_search_respects_captured_turn_ceiling_and_disabled_servers() {
    let (mut engine, policy, tmp, model) =
        mcp_search_fixture(Some(vec!["tool_search".into()])).await;
    let mut catalog = policy.catalog.clone();
    let mut active = policy.active_names.clone();
    engine
        .discover_mcp_for_tool_search(
            ("tool_search", &json!({"query":"mcp_.*","match":"regex"})),
            &policy,
            &mut catalog,
            &mut active,
            None,
        )
        .await
        .unwrap();
    for name in ["engram", "unrelated", "disabled", "stalled"] {
        assert!(!tmp.path().join(format!("started-{name}")).exists());
    }
    assert!(!catalog.iter().any(|tool| tool.name.starts_with("mcp_")));
    assert_eq!(
        model.call_count(),
        0,
        "MCP discovery must not call a provider"
    );
}

#[tokio::test]
async fn mcp_tool_search_failed_connection_is_bounded_without_guessed_tools() {
    let (mut engine, policy, _tmp, model) = mcp_search_fixture(None).await;
    let mut catalog = policy.catalog.clone();
    let mut active = policy.active_names.clone();
    tokio::time::timeout(
        Duration::from_secs(6),
        engine.discover_mcp_for_tool_search(
            ("tool_search", &json!({"query":"failed"})),
            &policy,
            &mut catalog,
            &mut active,
            None,
        ),
    )
    .await
    .expect("existing five-second bound")
    .unwrap();
    assert!(engine.mcp_connection_errors.contains_key("failed"));
    assert!(
        !catalog
            .iter()
            .any(|tool| tool.name.starts_with("mcp_failed_"))
    );
    assert_eq!(
        model.call_count(),
        0,
        "MCP discovery must not call a provider"
    );
}

#[tokio::test]
async fn mcp_tool_search_cancel_aborts_handshake_and_clears_connecting() {
    let (mut engine, policy, tmp, model) = mcp_search_fixture(None).await;
    let pool = engine.mcp_pool.clone().unwrap();
    let cancel = engine.cancel_token.clone();
    let mut catalog = policy.catalog.clone();
    let mut active = policy.active_names.clone();
    let input = json!({"query":"stalled"});
    let discovery = engine.discover_mcp_for_tool_search(
        ("tool_search", &input),
        &policy,
        &mut catalog,
        &mut active,
        None,
    );
    tokio::pin!(discovery);
    let result = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            tokio::select! {
                result = &mut discovery => break result,
                () = tokio::time::sleep(Duration::from_millis(10)) => {
                    if tmp.path().join("started-stalled").exists() { cancel.cancel(); }
                }
            }
        }
    })
    .await
    .expect("cancel remains bounded");
    assert!(matches!(result, Err(ToolError::PermissionDenied { .. })));
    assert!(pool.lock().await.connecting_servers().is_empty());
    assert!(pool.lock().await.to_api_tools().is_empty());
    assert_eq!(
        model.call_count(),
        0,
        "MCP discovery must not call a provider"
    );
}

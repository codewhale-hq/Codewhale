//! Admission gates between a provider response and tool execution: a failed
//! stream (C02-05), the step-budget final report (C02-10), the stream content
//! cap over tool arguments (C02-13), and bounded tool errors (C02-12). Every
//! fixture counts handler entries; none runs a shell or reaches a provider.

use super::*;
use crate::llm_client::mock::{MockLlmClient, canned};
use crate::tools::spec::{ApprovalRequirement, ToolCapability, ToolSpec};
use std::sync::atomic::{AtomicUsize, Ordering};

const WRITE_TOOL: &str = "fixture_write";

/// A mutating tool that needs no approval, so a call that is admitted runs.
struct CountingWriteTool(Arc<AtomicUsize>);

#[async_trait::async_trait]
impl ToolSpec for CountingWriteTool {
    fn name(&self) -> &str {
        WRITE_TOOL
    }
    fn description(&self) -> &str {
        "Count every execution of a mutating fixture."
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::WritesFiles]
    }
    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }
    async fn execute(&self, _: Value, _: &ToolContext) -> Result<ToolResult, ToolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ToolResult::success("wrote"))
    }
}

fn write_surface(
    engine: &Engine,
    workspace: &Path,
    executions: Arc<AtomicUsize>,
) -> ToolSurfacePolicy {
    let mut registry = crate::tools::ToolRegistry::new(ToolContext::new(workspace));
    registry.register(Arc::new(CountingWriteTool(executions)));
    let tools = Some(registry.to_api_tools_with_cache(true));
    test_tool_surface(engine, registry, tools, AppMode::Agent)
}

/// Streams a scripted response per call, `Err` items included, which the
/// queue-driven mock cannot express.
struct ScriptedStreamClient {
    calls: AtomicUsize,
    script: fn(usize) -> Vec<anyhow::Result<StreamEvent>>,
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for ScriptedStreamClient {
    fn provider_name(&self) -> &str {
        "scripted"
    }
    fn model(&self) -> &str {
        "scripted-model"
    }
    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("scripted fixture only streams")
    }
    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::pin(futures_util::stream::iter((self.script)(call))))
    }
    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

async fn drain_events(handle: &EngineHandle) -> Vec<Event> {
    let mut rx = handle.rx_event.write().await;
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn not_started(result: &Result<ToolResult, ToolError>) -> bool {
    result.as_ref().is_ok_and(|output| {
        !output.success
            && output
                .metadata
                .as_ref()
                .is_some_and(|metadata| metadata["side_effect_status"] == "not_started")
    })
}

#[tokio::test]
async fn terminal_stream_error_after_a_complete_tool_call_never_reaches_the_handler() {
    // C02-05: a complete mutating call followed by a provider error frame.
    let workspace = tempdir().unwrap();
    let mock = Arc::new(MockLlmClient::new(vec![
        vec![
            canned::message_start("tool-then-error"),
            canned::tool_use_block_start(0, "call-write", WRITE_TOOL),
            canned::tool_input_delta(0, r#"{"path":"target.txt"}"#),
            canned::block_stop(0),
            StreamEvent::Error {
                error: json!({ "message": "Model not exist." }),
            },
        ],
        canned::simple_text_turn("this second request must never be issued"),
    ]));
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        mock.clone(),
    );
    engine.session.auto_approve = true;
    let executions = Arc::new(AtomicUsize::new(0));
    let surface = write_surface(&engine, workspace.path(), executions.clone());
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;

    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "the handler must never run"
    );
    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("Model not exist.")),
        "{error:?}"
    );
    assert_eq!(
        mock.call_count(),
        1,
        "a failed response authorizes no further request"
    );
    assert_eq!(
        turn.stop_diagnostics.last_response_tool_calls_suppressed,
        Some(1)
    );
    let completions: Vec<_> = drain_events(&handle)
        .await
        .into_iter()
        .filter_map(|event| match event {
            Event::ToolCallComplete { result, .. } => Some(result),
            _ => None,
        })
        .collect();
    assert_eq!(completions.len(), 1, "the call is settled exactly once");
    assert!(not_started(&completions[0]), "{:?}", completions[0]);
    assert!(
        !engine
            .session
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .any(|block| matches!(
                block,
                ContentBlock::ToolUse { .. } | ContentBlock::ToolResult { .. }
            )),
        "no unpaired or fabricated tool history"
    );
}

#[tokio::test]
async fn exhausted_stream_resume_never_executes_the_failed_batch() {
    // C02-05, retry path: every attempt streams a complete call and then
    // drops. Each resume discards its batch; the last, unretried attempt is
    // refused at admission instead of executing.
    fn tool_then_drop(_call: usize) -> Vec<anyhow::Result<StreamEvent>> {
        vec![
            Ok(canned::message_start("tool-then-drop")),
            Ok(canned::tool_use_block_start(0, "call-write", WRITE_TOOL)),
            Ok(canned::tool_input_delta(0, "{}")),
            Ok(canned::block_stop(0)),
            Err(anyhow::anyhow!(
                "Stream read error: error decoding response body"
            )),
        ]
    }
    let workspace = tempdir().unwrap();
    let client = Arc::new(ScriptedStreamClient {
        calls: AtomicUsize::new(0),
        script: tool_then_drop,
    });
    let (mut engine, _handle) = Engine::new_with_model_client(
        EngineConfig {
            terminal_chrome_enabled: false,
            ..deterministic_engine_config(workspace.path())
        },
        &Config::default(),
        client.clone(),
    );
    engine.session.auto_approve = true;
    let resumes = engine.config.stream_retry_limits.max_resumes as usize;
    let executions = Arc::new(AtomicUsize::new(0));
    let surface = write_surface(&engine, workspace.path(), executions.clone());
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;

    assert_eq!(
        executions.load(Ordering::SeqCst),
        0,
        "no attempt's batch may run"
    );
    assert_eq!(client.calls.load(Ordering::SeqCst), 1 + resumes);
    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("error decoding response body")),
        "{error:?}"
    );
}

#[tokio::test]
async fn tool_argument_bytes_count_toward_the_stream_content_cap() {
    // C02-13: a response made only of tool-argument JSON must still hit the
    // per-step content cap, and the capped call must not run.
    let workspace = tempdir().unwrap();
    let huge_args = format!(r#"{{"blob":"{}"}}"#, "x".repeat(4096));
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-big", WRITE_TOOL, &huge_args),
        canned::simple_text_turn("this second request must never be issued"),
    ]));
    let (mut engine, _handle) = Engine::new_with_model_client(
        EngineConfig {
            stream_max_content_bytes: 64,
            ..deterministic_engine_config(workspace.path())
        },
        &Config::default(),
        mock.clone(),
    );
    engine.session.auto_approve = true;
    let executions = Arc::new(AtomicUsize::new(0));
    let surface = write_surface(&engine, workspace.path(), executions.clone());
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, _error) = engine.run_turn(&mut turn, surface, None, None).await;

    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert_eq!(mock.call_count(), 1);
}

#[tokio::test]
async fn text_fallback_tool_calls_share_the_response_tool_limit() {
    // C02-14: calls parsed from text markers obey the same per-response
    // ceiling as native tool starts. An over-limit batch fails the turn
    // before any call is announced, planned or run.
    let workspace = tempdir().unwrap();
    let over_limit = super::super::streaming::MAX_TOOL_CALLS_PER_RESPONSE + 1;
    let text: String = (0..over_limit)
        .map(|_| format!(r#"[TOOL_CALL]{{"tool": "{WRITE_TOOL}", "args": {{}}}}[/TOOL_CALL]"#))
        .collect();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn(&text),
        canned::simple_text_turn("this second request must never be issued"),
    ]));
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        mock.clone(),
    );
    engine.session.auto_approve = true;
    let executions = Arc::new(AtomicUsize::new(0));
    let surface = write_surface(&engine, workspace.path(), executions.clone());
    let mut turn = crate::core::turn::TurnContext::new(4);

    // Undrained, an admitted over-limit batch cannot even be announced: its
    // 257 starts overflow the event queue. Bound the wait so that is a
    // failure, not a hang.
    let (status, _error) = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        engine.run_turn(&mut turn, surface, None, None),
    )
    .await
    .expect("an over-limit text batch must be refused, not admitted");

    assert_eq!(executions.load(Ordering::SeqCst), 0, "no parsed call runs");
    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert_eq!(mock.call_count(), 1);
    assert_eq!(
        turn.stop_diagnostics.last_response_tool_calls_suppressed,
        Some(over_limit)
    );
    assert!(
        !drain_events(&handle)
            .await
            .iter()
            .any(|event| matches!(event, Event::ToolCallStarted { .. })),
        "no over-limit call is announced"
    );
}

#[tokio::test]
async fn step_budget_final_report_is_report_only() {
    // C02-10: the one response granted after the step budget is spent asks
    // for no tools, and a call it returns anyway is refused, not executed.
    let workspace = tempdir().unwrap();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-step-1", WRITE_TOOL, "{}"),
        canned::tool_call_turn("call-final-report", WRITE_TOOL, "{}"),
    ]));
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        mock.clone(),
    );
    engine.session.auto_approve = true;
    let executions = Arc::new(AtomicUsize::new(0));
    let surface = write_surface(&engine, workspace.path(), executions.clone());
    let mut turn = crate::core::turn::TurnContext::new(1);

    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;

    assert_eq!(
        executions.load(Ordering::SeqCst),
        1,
        "only the in-budget step runs"
    );
    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("Maximum model steps")),
        "{error:?}"
    );
    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 2);
    assert_ne!(requests[0].tool_choice, Some(json!("none")));
    assert_eq!(requests[1].tool_choice, Some(json!("none")));
    let refused = drain_events(&handle).await.into_iter().any(|event| {
        matches!(
            event,
            Event::ToolCallComplete {
                model_call: Some(ref call),
                result: Err(ToolError::PermissionDenied { ref message }),
                ..
            } if call.provider_id == "call-final-report" && message.contains("final report")
        )
    });
    assert!(refused, "the final-report call is refused with the reason");
}

#[test]
fn oversized_tool_errors_are_bounded_before_fanout() {
    // C02-12: an error is published to the event stream and the session like
    // a result, so an oversized one gets the same bounded projection.
    struct HugeErrorTool {
        as_result: bool,
    }
    #[async_trait::async_trait]
    impl ToolSpec for HugeErrorTool {
        fn name(&self) -> &str {
            "fixture_huge_error"
        }
        fn description(&self) -> &str {
            "Fail with an oversized message."
        }
        fn input_schema(&self) -> Value {
            json!({"type": "object"})
        }
        fn capabilities(&self) -> Vec<ToolCapability> {
            vec![ToolCapability::ReadOnly]
        }
        async fn execute(&self, _: Value, _: &ToolContext) -> Result<ToolResult, ToolError> {
            let message = format!("FIRST{}LAST", "e".repeat(300_000));
            if self.as_result {
                Ok(ToolResult::error(message))
            } else {
                Err(ToolError::execution_failed(message))
            }
        }
    }

    with_artifact_home(|home| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                for as_result in [false, true] {
                    let mock = Arc::new(MockLlmClient::new(vec![
                        canned::tool_call_turn("call-huge", "fixture_huge_error", "{}"),
                        canned::simple_text_turn("done"),
                    ]));
                    let (mut engine, handle) = Engine::new_with_model_client(
                        deterministic_engine_config(home),
                        &Config::default(),
                        mock.clone(),
                    );
                    let mut registry = crate::tools::ToolRegistry::new(ToolContext::new(home));
                    registry.register(Arc::new(HugeErrorTool { as_result }));
                    let tools = Some(registry.to_api_tools_with_cache(true));
                    let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
                    let mut turn = crate::core::turn::TurnContext::new(4);
                    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
                    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                    let published = drain_events(&handle)
                        .await
                        .into_iter()
                        .find_map(|event| match event {
                            Event::ToolCallComplete { result, .. } => Some(match result {
                                Ok(output) => output.content,
                                Err(error) => error.to_string(),
                            }),
                            _ => None,
                        })
                        .expect("the call completes");
                    assert!(
                        published.len() < crate::tools::truncate::SPILLOVER_THRESHOLD_BYTES,
                        "as_result={as_result}: {} bytes published",
                        published.len()
                    );
                    assert!(published.contains("FIRST"), "the head survives");
                    assert!(published.contains("LAST"), "the tail survives");
                    assert!(
                        published.contains(codewhale_tools::names::SPILLOVER_RECOVERY_HINT),
                        "the omitted range stays recoverable"
                    );
                }
            });
    });
}

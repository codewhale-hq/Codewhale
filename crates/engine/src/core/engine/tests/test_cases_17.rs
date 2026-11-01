#[test]
fn stream_read_scenario() {
    // Scenario consolidation of: stream_read_error_message_explains_retry_before_output, stream_read_error_message_explains_no_replay_after_output
    // from stream_read_error_message_explains_retry_before_output
    {
        let message = super::stream_read_error_user_message(
            "Stream read error: error decoding response body",
            false,
        );

        assert!(message.contains("Provider stream connection dropped"));
        assert!(message.contains("No output had streamed yet"));
        assert!(message.contains("retry automatically"));
        assert!(message.contains("Stream read error: error decoding response body"));
    }
    // from stream_read_error_message_explains_no_replay_after_output
    {
        let message = super::stream_read_error_user_message(
            "Stream read error: error decoding response body",
            true,
        );

        assert!(message.contains("Provider stream connection dropped"));
        assert!(message.contains("Some output had already streamed"));
        assert!(message.contains("risking duplicated output"));
        assert!(message.contains("Stream read error: error decoding response body"));
        assert_eq!(
            codewhale_error_taxonomy::classify_error_message(&message),
            codewhale_error_taxonomy::ErrorCategory::Network
        );
    }
}

#[test]
fn stream_retry_budget_caps_transparent_retries_at_two() {
    // Case 4 from issue #103: after DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES attempts
    // we stop trying transparently and let the outer error path surface.
    // (The outer per-turn `stream_retry_attempts` retry is a separate layer
    // and is still in effect at the whole-turn level.)
    assert!(
        super::should_transparently_retry_stream(
            false,
            super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES - 1,
            super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
            false,
        ),
        "one short of the cap should still retry"
    );
    assert!(
        !super::should_transparently_retry_stream(
            false,
            super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
            super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
            false,
        ),
        "at the cap, no further transparent retries"
    );
    assert!(
        !super::should_transparently_retry_stream(
            false,
            super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES + 5,
            super::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES,
            false,
        ),
        "well past the cap, definitely no transparent retries"
    );
}

// === #2990 sleep-resume policy ================================================

#[test]
fn sleep_gap_requires_wallclock_to_outrun_monotonic_clock() {
    use std::time::Duration;
    // No divergence: ordinary network failure, clocks agree.
    assert!(
        !super::sleep_gap_detected(Duration::from_secs(30), Duration::from_secs(30)),
        "equal elapsed times must not register as a sleep gap"
    );
    // Divergence below the threshold: NTP slew / scheduling jitter.
    assert!(
        !super::sleep_gap_detected(Duration::from_secs(5), Duration::from_secs(14)),
        "9s of divergence is below the 10s threshold"
    );
    // Divergence above the threshold: the host was suspended.
    assert!(
        super::sleep_gap_detected(Duration::from_secs(5), Duration::from_secs(16)),
        "11s of divergence must register as a sleep gap"
    );
    // Wall clock went backwards (NTP step): saturating_sub → zero gap.
    assert!(
        !super::sleep_gap_detected(Duration::from_secs(60), Duration::from_secs(5)),
        "wall clock behind monotonic must never register as a sleep gap"
    );
}

#[test]
fn sleep_resume_scenario() {
    // Scenario consolidation of: sleep_resume_retries_even_after_content_streamed, sleep_resume_requires_a_detected_gap, sleep_resume_respects_budget_and_cancellation
    // from sleep_resume_retries_even_after_content_streamed
    {
        // The whole point of #2990: unlike the #103 transparent retry, a
        // detected sleep gap retries regardless of streamed content — the
        // partial output predates the sleep and the user was not watching.
        assert!(
            super::should_resume_after_sleep(true, 0, super::DEFAULT_STREAM_MAX_RESUMES, false),
            "detected sleep with full budget must resume"
        );
        assert!(
            super::should_resume_after_sleep(
                true,
                super::DEFAULT_STREAM_MAX_RESUMES - 1,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "detected sleep one short of the budget must still resume"
        );
    }
    // from sleep_resume_requires_a_detected_gap
    {
        // Without a sleep gap this layer stays out of the way entirely, so the
        // deliberate no-retry-after-content policy for ordinary flakes (#103)
        // is preserved.
        assert!(
            !super::should_resume_after_sleep(false, 0, super::DEFAULT_STREAM_MAX_RESUMES, false),
            "no sleep gap → never resume via this layer"
        );
    }
    // from sleep_resume_respects_budget_and_cancellation
    {
        assert!(
            !super::should_resume_after_sleep(
                true,
                super::DEFAULT_STREAM_MAX_RESUMES,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "budget exhausted → surface the failure instead of looping"
        );
        assert!(
            !super::should_resume_after_sleep(true, 0, super::DEFAULT_STREAM_MAX_RESUMES, true),
            "cancelled turn must not be resumed behind the user's back"
        );
    }
}

// === headless mid-stream network-drop resume (v0.9.4 Terminal-Bench P0) ======
//
// Terminal-Bench 2.1 on the 0.9.4 bundle forfeited tasks when the DeepSeek
// stream dropped mid-response ("error decoding response body" after partial
// content): the #103 policy surfaced the warning and failed the turn, and
// `codewhale exec` exited 1. In a headless host no operator watches the
// partial deltas and the fragment is never committed, so the turn loop now
// re-issues the request instead (bounded by DEFAULT_STREAM_MAX_RESUMES), exactly
// like the #2990 sleep-resume.

#[test]
fn network_drop_scenario() {
    // Scenario consolidation of: network_drop_resume_only_fires_for_headless_hosts, network_drop_resume_requires_network_class_error, network_drop_resume_respects_budget_and_cancellation
    // from network_drop_resume_only_fires_for_headless_hosts
    {
        assert!(
            super::should_resume_after_network_drop(
                true,
                true,
                0,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "headless host + network-class drop with budget must resume"
        );
        assert!(
            !super::should_resume_after_network_drop(
                false,
                true,
                0,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "interactive sessions keep the #103 surface-the-warning policy: \
             the user saw the partial deltas and replay would render them twice"
        );
    }
    // from network_drop_resume_requires_network_class_error
    {
        assert!(
            !super::should_resume_after_network_drop(
                true,
                false,
                0,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "non-network failures (model/parse/auth) must never be replayed"
        );
    }
    // from network_drop_resume_respects_budget_and_cancellation
    {
        assert!(
            super::should_resume_after_network_drop(
                true,
                true,
                super::DEFAULT_STREAM_MAX_RESUMES - 1,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false,
            ),
            "one short of the budget should still resume"
        );
        assert!(
            !super::should_resume_after_network_drop(
                true,
                true,
                super::DEFAULT_STREAM_MAX_RESUMES,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "budget exhausted → surface the failure instead of looping"
        );
        assert!(
            !super::should_resume_after_network_drop(
                true,
                true,
                0,
                super::DEFAULT_STREAM_MAX_RESUMES,
                true
            ),
            "cancelled turn must not be resumed behind the operator's back"
        );
    }
}

// === interactive mid-stream network-drop resume (0.9.4; reworked 0.9.10) =========
//
// The interactive TUI used to fail the turn when a provider stream dropped
// after partial output because the #103 policy treated any post-content error
// as terminal. The model now preserves a visible partial reply as a committed
// assistant message and re-issues the request. Since 0.9.10 the recovery is
// typed engine-internal state (`StreamResume`): no synthetic `[runtime]` user
// continuation message is appended, and a thinking-only drop preserves
// nothing and never claims it did.

#[test]
fn interactive_network_scenario() {
    // Scenario consolidation of: interactive_network_drop_resume_only_fires_for_interactive_hosts, interactive_network_drop_resume_requires_partial_content_and_no_tools, interactive_network_drop_resume_requires_network_class_error
    // from interactive_network_drop_resume_only_fires_for_interactive_hosts
    {
        assert!(
            super::should_resume_interactive_after_network_drop(
                true,
                true,
                true,
                true,
                0,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "interactive TUI + partial text + no tools + budget must resume"
        );
        assert!(
            !super::should_resume_interactive_after_network_drop(
                false,
                true,
                true,
                true,
                0,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "headless hosts must use the headless resume path, not this one"
        );
    }
    // from interactive_network_drop_resume_requires_partial_content_and_no_tools
    {
        assert!(
            !super::should_resume_interactive_after_network_drop(
                true,
                true,
                false,
                true,
                0,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "no streamed content → transparent retry or nothing-streamed path"
        );
        assert!(
            !super::should_resume_interactive_after_network_drop(
                true,
                true,
                true,
                false,
                0,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "in-flight tool calls must never be resumed (side-effect duplication)"
        );
    }
    // from interactive_network_drop_resume_requires_network_class_error
    {
        assert!(
            !super::should_resume_interactive_after_network_drop(
                true,
                false,
                true,
                true,
                0,
                super::DEFAULT_STREAM_MAX_RESUMES,
                false
            ),
            "non-network failures must surface normally"
        );
    }
}

#[test]
fn interactive_network_drop_resume_respects_budget_and_cancellation() {
    assert!(
        super::should_resume_interactive_after_network_drop(
            true,
            true,
            true,
            true,
            super::DEFAULT_STREAM_MAX_RESUMES - 1,
            super::DEFAULT_STREAM_MAX_RESUMES,
            false,
        ),
        "one short of the budget should still resume"
    );
    assert!(
        !super::should_resume_interactive_after_network_drop(
            true,
            true,
            true,
            true,
            super::DEFAULT_STREAM_MAX_RESUMES,
            super::DEFAULT_STREAM_MAX_RESUMES,
            false,
        ),
        "budget exhausted → surface the failure"
    );
    assert!(
        !super::should_resume_interactive_after_network_drop(
            true,
            true,
            true,
            true,
            0,
            super::DEFAULT_STREAM_MAX_RESUMES,
            true
        ),
        "cancelled turn must not resume"
    );
}

/// Model client whose first `failures` streams emit partial content and then
/// die with the network-class read error reqwest reports for a dropped
/// chunked-transfer body; later streams complete a normal text turn.
/// Exercises the headless mid-stream network-drop resume through the real
/// turn loop.
struct FlakyNetworkDropModelClient {
    calls: std::sync::atomic::AtomicUsize,
    failures: usize,
    terminal_before_drop: bool,
    content_before_drop: bool,
    /// #6699: fail the request itself, before any stream exists, in the
    /// given shape.
    open_failure: Option<StreamOpenFailure>,
}

/// #6699/#6711: how a request fails before any stream exists.
#[derive(Clone, Copy, Debug)]
enum StreamOpenFailure {
    /// The typed transport error `open_sse_response` returns on a header
    /// stall.
    HeaderStall,
    /// A reqwest connect error under the Anthropic adapter's outer context,
    /// as an HTTP/1.1-pinned open returns it: the outer message names no
    /// transport cause.
    WrappedConnectError,
    /// The Responses adapter shape: the typed retry-layer network error
    /// under the adapter's outer context.
    WrappedTypedNetworkError,
    /// A provider answered with an HTTP rejection whose body mentions a
    /// connection reset, as the Anthropic adapter reports it (untyped).
    HttpRejectionMentioningConnection,
}

impl StreamOpenFailure {
    async fn error(self) -> anyhow::Error {
        use anyhow::Context as _;
        match self {
            Self::HeaderStall => anyhow::Error::new(crate::llm_client::LlmError::NetworkError(
                "SSE stream request did not receive response headers after 45s \
                 (HTTP/2 and HTTP/1.1)."
                    .to_string(),
            )),
            Self::WrappedConnectError => {
                // A port that was just released refuses the connection.
                let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
                let addr = listener.local_addr().expect("local addr");
                drop(listener);
                let client = codewhale_release::tls::reqwest_client_builder()
                    .no_proxy()
                    .build()
                    .expect("client");
                let send = client
                    .post(format!("http://{addr}/v1/messages"))
                    .send()
                    .await;
                let err = send
                    .context("Anthropic Messages API request failed")
                    .expect_err("closed port must refuse the connection");
                assert_eq!(err.to_string(), "Anthropic Messages API request failed");
                err
            }
            Self::WrappedTypedNetworkError => Err::<(), _>(anyhow::Error::new(
                crate::llm_client::LlmError::NetworkError(
                    "Connection failed: error sending request".to_string(),
                ),
            ))
            .context("Responses API request failed")
            .expect_err("wrapped network error"),
            Self::HttpRejectionMentioningConnection => anyhow::anyhow!(
                "Anthropic API error (HTTP 500 Internal Server Error api_error): \
                 upstream connection reset"
            ),
        }
    }
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for FlakyNetworkDropModelClient {
    fn provider_name(&self) -> &str {
        "flaky-network"
    }

    fn model(&self) -> &str {
        "local-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("flaky-network regression uses the streaming model boundary")
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        use crate::llm_client::mock::canned;
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        if call <= self.failures {
            if let Some(open_failure) = self.open_failure {
                return Err(open_failure.error().await);
            }
            if self.terminal_before_drop {
                let start_usage = Usage {
                    input_tokens: 31,
                    ..Default::default()
                };
                let delta_usage = Usage {
                    output_tokens: 8,
                    ..Default::default()
                };
                let mut message_start = canned::message_start("terminal_then_drop");
                if let StreamEvent::MessageStart { message } = &mut message_start {
                    message.usage = start_usage;
                }
                let events: Vec<anyhow::Result<codewhale_models::StreamEvent>> = vec![
                    Ok(message_start),
                    Ok(canned::text_block_start(0)),
                    Ok(canned::text_delta(0, "billed truncated fragment")),
                    Ok(canned::block_stop(0)),
                    Ok(canned::message_delta("max_tokens", Some(delta_usage))),
                    Err(anyhow::anyhow!(
                        "Stream read error: error decoding response body"
                    )),
                ];
                return Ok(Box::pin(futures_util::stream::iter(events)));
            }
            if !self.content_before_drop {
                return Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(canned::message_start("empty_then_drop")),
                    Err(anyhow::anyhow!(
                        "Stream read error: error decoding response body"
                    )),
                ])));
            }
            // Partial content first — this flips `any_content_received` so
            // the #103 transparent retry cannot fire — then the transport
            // dies the way the 0.9.4 Terminal-Bench crashes did.
            let events: Vec<anyhow::Result<codewhale_models::StreamEvent>> = vec![
                Ok(canned::message_start("flaky_msg")),
                Ok(canned::text_block_start(0)),
                Ok(canned::text_delta(
                    0,
                    "partial answer that must be discarded",
                )),
                Err(anyhow::anyhow!(
                    "Stream read error: error decoding response body"
                )),
            ];
            return Ok(Box::pin(futures_util::stream::iter(events)));
        }
        let events = canned::simple_text_turn("recovered after retry")
            .into_iter()
            .map(Ok);
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

/// Drive one headless (`terminal_chrome_enabled = false`, the exec /
/// stream-json posture) turn against the flaky client and collect every
/// event through the terminal TurnComplete.
async fn run_headless_turn_with_flaky_network(
    failures: usize,
) -> (std::sync::Arc<FlakyNetworkDropModelClient>, Vec<Event>) {
    let model = std::sync::Arc::new(FlakyNetworkDropModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        failures,
        terminal_before_drop: false,
        content_before_drop: true,
        open_failure: None,
    });
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = Config::default();
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        subagents_enabled: false,
        terminal_chrome_enabled: false,
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
            route: resolved_route_for_test(
                &config,
                codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL,
            ),
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
        .expect("send flaky-network turn");

    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("flaky-network event timeout")
        .expect("flaky-network event");
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

#[tokio::test]
async fn headless_turn_retries_mid_stream_network_drop_and_recovers() {
    let (model, events) = run_headless_turn_with_flaky_network(1).await;

    let terminal = events
        .iter()
        .find_map(|event| match event {
            Event::ToolRequestSnapshot { snapshot } => snapshot.terminal.as_ref(),
            _ => None,
        })
        .expect("terminal diagnostics through existing event authority");
    assert_eq!(terminal.model_requests_started, 2);
    assert_eq!(terminal.stream_resumes, 1);
    assert_eq!(terminal.transparent_stream_retries, 0);

    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the dropped stream must be re-issued exactly once"
    );
    let (status, error) = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, error, .. } => Some((status, error)),
            _ => None,
        })
        .expect("terminal TurnComplete");
    assert_eq!(
        *status,
        TurnOutcomeStatus::Completed,
        "a recovered retry must complete the turn: {error:?}"
    );
    assert!(error.is_none(), "recovered turn must not report an error");
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Status { message } if message.starts_with("Retry attempt: stream-resume 1/")
        )),
        "the first actual resume must remain visible: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Error { .. })),
        "a transient drop that the retry recovers must not surface an error event: {events:?}"
    );
    // The discarded fragment from the dropped attempt must never reach the
    // transcript — only the retried turn's content is committed.
    let transcript_text = events
        .iter()
        .filter_map(|event| match event {
            Event::SessionUpdated { messages, .. } => Some(messages),
            _ => None,
        })
        .flat_map(|messages| messages.iter())
        .flat_map(|message| message.content.iter())
        .filter_map(|block| match block {
            ContentBlock::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        transcript_text.contains("recovered after retry"),
        "retried content must be committed: {transcript_text}"
    );
    assert!(
        !transcript_text.contains("partial answer that must be discarded"),
        "the dropped attempt's fragment must be discarded, not committed: {transcript_text}"
    );
}

#[tokio::test]
async fn terminal_diagnostics_count_transparent_stream_requests_without_extra_snapshots() {
    let workspace = tempdir().expect("tempdir");
    let model = std::sync::Arc::new(FlakyNetworkDropModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        failures: 1,
        terminal_before_drop: false,
        content_before_drop: false,
        open_failure: None,
    });
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let (mut engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        client,
    );
    let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
        workspace.path().to_path_buf(),
    ));
    let surface = test_tool_surface(&engine, registry, None, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    let snapshot = turn
        .terminal_request_snapshot(status)
        .expect("terminal snapshot");
    let terminal = snapshot.terminal.expect("terminal facts");
    assert_eq!(terminal.model_requests_started, 2);
    assert_eq!(terminal.transparent_stream_retries, 1);
    assert_eq!(terminal.stream_resumes, 0);
    assert_eq!(terminal.model_step_index, 0);
    let mut receiver = handle.rx_event.write().await;
    let events = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    assert!(events.iter().any(|event| matches!(event,
        Event::Status { message } if message.starts_with("Retry attempt: transparent-stream 1/")
    )));
    assert!(events.iter().any(|event| matches!(event,
        Event::Status { message } if message == "Retry recovery: transparent stream recovered after 1 retries"
    )));
    let snapshots = events
        .iter()
        .filter(|event| matches!(event, Event::ToolRequestSnapshot { .. }))
        .count();
    assert_eq!(
        snapshots, 1,
        "request construction is distinct from stream retries"
    );
}

/// #6699: drive one interactive turn whose first `failures` requests fail
/// before a stream opens, with `max_resumes` as the configured budget.
async fn run_turn_with_stream_open_failures(
    failures: usize,
    max_resumes: Option<u32>,
    open_failure: StreamOpenFailure,
) -> (
    std::sync::Arc<FlakyNetworkDropModelClient>,
    TurnOutcomeStatus,
    Option<String>,
    crate::tool_inspection::TurnStopDiagnostics,
    Vec<Event>,
) {
    let workspace = tempdir().expect("tempdir");
    let model = std::sync::Arc::new(FlakyNetworkDropModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        failures,
        terminal_before_drop: false,
        content_before_drop: false,
        open_failure: Some(open_failure),
    });
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let engine_config = EngineConfig {
        terminal_chrome_enabled: true,
        stream_retry_limits: turn_budget::resolve_stream_retry_limits(max_resumes, None, None),
        ..deterministic_engine_config(workspace.path())
    };
    let (mut engine, handle) =
        Engine::new_with_model_client(engine_config, &Config::default(), client);
    let registry = crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
        workspace.path().to_path_buf(),
    ));
    let surface = test_tool_surface(&engine, registry, None, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(4);
    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
    let terminal = turn
        .terminal_request_snapshot(status)
        .expect("terminal snapshot")
        .terminal
        .expect("terminal facts");
    let mut rx = handle.rx_event.write().await;
    let events = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    (model, status, error, terminal, events)
}

#[tokio::test]
async fn stream_open_failure_is_retried_through_the_resume_budget() {
    let (model, status, error, terminal, events) =
        run_turn_with_stream_open_failures(1, None, StreamOpenFailure::HeaderStall).await;
    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "a request that never opened must be re-issued once"
    );
    assert_eq!(terminal.model_requests_started, 2);
    assert_eq!(terminal.stream_resumes, 1);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Error { .. })),
        "a recovered open failure must not surface an error event: {events:?}"
    );
}

#[tokio::test]
async fn stream_open_failure_fails_the_turn_once_the_budget_is_spent() {
    let (model, status, error, terminal, events) =
        run_turn_with_stream_open_failures(usize::MAX, None, StreamOpenFailure::HeaderStall).await;
    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1 + super::DEFAULT_STREAM_MAX_RESUMES as usize,
        "initial attempt plus the default resume budget, then the turn fails"
    );
    assert_eq!(terminal.stream_resumes, super::DEFAULT_STREAM_MAX_RESUMES);
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("did not receive response headers")),
        "the real transport error must surface: {error:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Error { .. }))
            .count(),
        1,
        "only the exhausted attempt emits an error event: {events:?}"
    );
}

#[tokio::test]
async fn stream_open_failure_honors_a_configured_resume_budget() {
    let (model, status, _error, terminal, _events) =
        run_turn_with_stream_open_failures(usize::MAX, Some(0), StreamOpenFailure::HeaderStall)
            .await;
    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "`stream_max_resumes = 0` disables the turn-level retry"
    );
    assert_eq!(terminal.stream_resumes, 0);
}

/// #6711: an HTTP/1.1-pinned Anthropic open returns the reqwest connect
/// error under an outer context that names no transport cause. The retry gate
/// must read the error chain, not the outer message.
#[tokio::test]
async fn stream_open_failure_behind_adapter_context_is_retried() {
    for open_failure in [
        StreamOpenFailure::WrappedConnectError,
        StreamOpenFailure::WrappedTypedNetworkError,
    ] {
        let (model, status, error, terminal, _events) =
            run_turn_with_stream_open_failures(1, None, open_failure).await;
        assert_eq!(
            status,
            TurnOutcomeStatus::Completed,
            "{open_failure:?}: {error:?}"
        );
        assert_eq!(
            model.calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "{open_failure:?}: a wrapped transport failure must be re-issued"
        );
        assert_eq!(terminal.stream_resumes, 1, "{open_failure:?}");
    }
}

/// #6711: a provider that answered with an HTTP rejection is not an open
/// failure, even when its body mentions a connection or a timeout. The request
/// must not be re-issued through the resume budget.
#[tokio::test]
async fn stream_open_http_rejection_mentioning_connection_is_not_retried() {
    let (model, status, _error, terminal, events) = run_turn_with_stream_open_failures(
        usize::MAX,
        None,
        StreamOpenFailure::HttpRejectionMentioningConnection,
    )
    .await;
    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a provider rejection must fail the turn on the first answer"
    );
    assert_eq!(terminal.stream_resumes, 0);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Error { .. }))
            .count(),
        1,
        "{events:?}"
    );
}

#[tokio::test]
async fn terminal_output_limit_followed_by_stream_error_is_charged_and_not_retried() {
    let model = std::sync::Arc::new(FlakyNetworkDropModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        failures: 1,
        terminal_before_drop: true,
        content_before_drop: true,
        open_failure: None,
    });
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = Config::default();
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        subagents_enabled: false,
        terminal_chrome_enabled: false,
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
            route: resolved_route_for_test(
                &config,
                codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL,
            ),
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
        .expect("send terminal-then-drop turn");

    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("terminal-then-drop event timeout")
        .expect("terminal-then-drop event");
        let terminal = matches!(event, Event::TurnComplete { .. });
        events.push(event);
        if terminal {
            break;
        }
    }

    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a provider-declared terminal response must never be re-issued"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::TurnUsage { usage, .. }
            if usage.input_tokens == 31 && usage.output_tokens == 8
    )));
    let (status, error) = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, error, .. } => Some((status, error)),
            _ => None,
        })
        .expect("terminal TurnComplete");
    assert_eq!(*status, TurnOutcomeStatus::Failed);
    assert!(
        error
            .as_deref()
            .is_some_and(|error| error.contains("max_tokens")),
        "{error:?}"
    );
    assert!(!events.iter().any(|event| matches!(
        event,
        Event::Status { message } if crate::core::events::is_retry_status_receipt(message)
    )));

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

/// Run one user turn against scripted streams; returns its events and how
/// many model requests were made.
async fn error_frame_turn_events(turns: Vec<Vec<StreamEvent>>) -> (Vec<Event>, usize) {
    let model = std::sync::Arc::new(crate::llm_client::mock::MockLlmClient::new(turns));
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = Config::default();
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        subagents_enabled: false,
        terminal_chrome_enabled: false,
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
            route: resolved_route_for_test(
                &config,
                codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL,
            ),
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
        .expect("send turn");
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("turn event timeout")
        .expect("turn event");
        let terminal = matches!(event, Event::TurnComplete { .. });
        events.push(event);
        if terminal {
            break;
        }
    }
    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
    (events, model.call_count())
}

/// #6795: a transient upstream failure delivered as an error frame inside a
/// successful response, before anything streamed, is retried like every other
/// no-content stream death. A terminal-class frame still fails on the first
/// request.
#[tokio::test]
async fn transient_error_frame_with_no_content_is_retried_and_terminal_frame_is_not() {
    use crate::llm_client::mock::canned;
    let frame = |message: &str| {
        vec![StreamEvent::Error {
            error: serde_json::json!({ "message": message }),
        }]
    };

    let (events, requests) = error_frame_turn_events(vec![
        frame("Provider returned an empty response"),
        canned::simple_text_turn("recovered answer"),
    ])
    .await;
    assert_eq!(requests, 2, "the empty-upstream frame is re-issued once");
    assert!(events.iter().any(|event| matches!(event,
        Event::Status { message } if message.starts_with("Retry attempt: stream-resume 1/")
    )));
    assert!(events.iter().any(|event| matches!(event,
        Event::Status { message } if message == "Retry recovery: stream recovered after 1 retries"
    )));
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::TurnComplete {
                status: TurnOutcomeStatus::Completed,
                error: None,
                ..
            }
        )),
        "a successful retry completes the turn"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Error { .. })),
        "a retried frame leaves no error card behind"
    );

    let (events, requests) = error_frame_turn_events(vec![
        frame("Model not exist."),
        canned::simple_text_turn("must never be requested"),
    ])
    .await;
    assert_eq!(requests, 1, "a terminal-class frame is never retried");
    assert!(events.iter().any(|event| matches!(
        event,
        Event::TurnComplete {
            status: TurnOutcomeStatus::Failed,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Error { envelope, .. }
            if envelope.message.contains("Model not exist.") && !envelope.recoverable
    )));
}

/// #6795: when the retry budget is spent on a transient error frame, the turn
/// fails once with the provider's reason, and the one card it posts is an
/// error that does not promise a retry.
#[tokio::test]
async fn exhausted_transient_error_frame_posts_one_error_envelope() {
    let frame = || {
        vec![StreamEvent::Error {
            error: serde_json::json!({ "message": "Provider returned an empty response" }),
        }]
    };
    let (events, requests) = error_frame_turn_events((0..8).map(|_| frame()).collect()).await;
    assert!(
        (2..8).contains(&requests),
        "the frame is retried within the budget, then given up on: {requests}"
    );
    let cards: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Event::Error { envelope, .. } => Some(envelope),
            _ => None,
        })
        .collect();
    assert_eq!(cards.len(), 1, "one card, after the budget: {cards:?}");
    assert_eq!(
        cards[0].severity,
        codewhale_error_taxonomy::ErrorSeverity::Error
    );
    assert!(!cards[0].recoverable);
    assert!(
        cards[0]
            .message
            .contains("Provider returned an empty response")
    );
    assert!(events.iter().any(|event| matches!(event,
        Event::TurnComplete { status: TurnOutcomeStatus::Failed, error: Some(error), .. }
            if error.contains("Provider returned an empty response")
    )));
}

/// #6843: a frame whose whole message is a placeholder is not retried and the
/// transcript says the upstream error was unreadable.
#[tokio::test]
async fn placeholder_error_frame_is_reported_as_unreadable_and_not_retried() {
    use crate::llm_client::mock::canned;
    let (events, requests) = error_frame_turn_events(vec![
        vec![StreamEvent::Error {
            error: serde_json::json!({ "message": "ERROR" }),
        }],
        canned::simple_text_turn("must never be requested"),
    ])
    .await;
    assert_eq!(requests, 1, "an unreadable error is not retryable");
    assert!(events.iter().any(|event| matches!(event,
        Event::Error { envelope, .. }
            if envelope.category == codewhale_error_taxonomy::ErrorCategory::Parse
                && envelope.severity == codewhale_error_taxonomy::ErrorSeverity::Error
                && envelope.message.contains("unreadable error")
    )));
    assert!(events.iter().any(|event| matches!(event,
        Event::TurnComplete { status: TurnOutcomeStatus::Failed, error: Some(error), .. }
            if error.contains("unreadable error")
    )));
}

#[tokio::test]
async fn midstream_error_frame_stops_the_stream_and_drops_trailing_deltas() {
    // The reported incident: a provider delivered a chunk-level error
    // object ("Model not exist.") mid-stream, and the stream kept parsing
    // later frames as deltas — so reasoning/text rendered *after* the
    // failure. The `StreamEvent::Error` arm must surface a terminal error
    // event, stop consuming the stream, and never forward deltas that
    // arrive after the failure frame.
    let model = std::sync::Arc::new(crate::llm_client::mock::MockLlmClient::new(vec![vec![
        crate::llm_client::mock::canned::message_start("midstream-error"),
        crate::llm_client::mock::canned::text_block_start(0),
        StreamEvent::Error {
            error: serde_json::json!({ "message": "Model not exist." }),
        },
        // Everything after the error frame must never reach the UI.
        crate::llm_client::mock::canned::text_delta(0, "TRAILING-DELTA-AFTER-FAILURE"),
        crate::llm_client::mock::canned::block_stop(0),
    ]]));
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = Config::default();
    let engine_config = EngineConfig {
        max_steps: 1,
        snapshots_enabled: false,
        subagents_enabled: false,
        terminal_chrome_enabled: false,
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
            route: resolved_route_for_test(
                &config,
                codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL,
            ),
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
        .expect("send midstream-error turn");

    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("midstream-error event timeout")
        .expect("midstream-error event");
        let terminal = matches!(event, Event::TurnComplete { .. });
        events.push(event);
        if terminal {
            break;
        }
    }

    // The failure is surfaced as a typed error event at Error severity.
    assert!(events.iter().any(|event| matches!(
        event,
        Event::Error { envelope, .. }
            if envelope.message.contains("Model not exist.")
                && envelope.category == codewhale_error_taxonomy::ErrorCategory::InvalidInput
                && envelope.severity == codewhale_error_taxonomy::ErrorSeverity::Error
    )));
    // Deltas after the failure frame never reach the UI.
    assert!(
        !events.iter().any(|event| matches!(
            event,
            Event::MessageDelta { content, .. } if content.contains("TRAILING-DELTA")
        )),
        "deltas after a mid-stream failure must be dropped: {events:?}"
    );
    // The turn fails with the provider's message, not a generic truncation.
    let (status, error) = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, error, .. } => Some((status, error)),
            _ => None,
        })
        .expect("midstream-error TurnComplete");
    assert_eq!(*status, TurnOutcomeStatus::Failed);
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("Model not exist.")),
        "{error:?}"
    );
    // The stream is consumed exactly once — no re-issue of a terminal
    // rejection.
    assert_eq!(model.call_count(), 1);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

/// Emits a full billed response, then cancels the engine's own token while
/// yielding the final stream event — modeling Esc arriving right after the
/// provider finished charging for the response.
struct CancelAfterTerminalUsageModelClient {
    calls: std::sync::atomic::AtomicUsize,
    // The engine mints a fresh token per turn; read the live one through the
    // engine's shared cell at stream time.
    token: std::sync::Mutex<Option<Arc<StdMutex<tokio_util::sync::CancellationToken>>>>,
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for CancelAfterTerminalUsageModelClient {
    fn provider_name(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        "mock-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("unused")
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        use crate::llm_client::mock::canned;

        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let shared = self
            .token
            .lock()
            .expect("token cell")
            .clone()
            .expect("token installed before turn");
        let token = shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut message_start = canned::message_start("cancel_after_usage");
        if let StreamEvent::MessageStart { message } = &mut message_start {
            message.usage = Usage {
                input_tokens: 47,
                ..Default::default()
            };
        }
        let events = vec![
            message_start,
            canned::text_block_start(0),
            canned::text_delta(0, "answer the user was billed for"),
            canned::block_stop(0),
            canned::message_delta(
                "end_turn",
                Some(Usage {
                    output_tokens: 9,
                    ..Default::default()
                }),
            ),
            canned::message_stop(),
        ];
        let last = events.len() - 1;
        let stream = futures_util::stream::iter(events.into_iter().enumerate().map(
            move |(index, event)| {
                if index == last {
                    token.cancel();
                }
                Ok(event)
            },
        ));
        Ok(Box::pin(stream))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[tokio::test]
async fn cancellation_after_terminal_usage_still_charges_the_turn() {
    let model = std::sync::Arc::new(CancelAfterTerminalUsageModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        token: std::sync::Mutex::new(None),
    });
    let client: crate::core::model_client::SharedModelClient = model.clone();
    let config = Config::default();
    let workspace = tempdir().expect("tempdir");
    let (engine, handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &config,
        client,
    );
    *model.token.lock().expect("token cell") = Some(engine.shared_cancel_token.clone());
    let run_task = tokio::spawn(engine.run());

    handle
        .send(external_user_message_op(
            "solve the task",
            AppMode::Agent,
            &config,
        ))
        .await
        .expect("send cancel-after-usage turn");

    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("cancel-after-usage event timeout")
        .expect("cancel-after-usage event");
        let terminal = matches!(event, Event::TurnComplete { .. });
        events.push(event);
        if terminal {
            break;
        }
    }

    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::TurnUsage { usage, .. }
                if usage.input_tokens == 47 && usage.output_tokens == 9
        )),
        "billed usage must be accounted even though the turn was cancelled"
    );
    let status = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, .. } => Some(*status),
            _ => None,
        })
        .expect("terminal TurnComplete");
    assert_eq!(status, TurnOutcomeStatus::Interrupted);

    handle.send(Op::Shutdown).await.expect("shutdown engine");
    run_task.await.expect("engine task");
}

/// Drive one interactive (`terminal_chrome_enabled = true`) turn against the
/// flaky client and collect every event through the terminal TurnComplete.
async fn run_interactive_turn_with_flaky_network(
    failures: usize,
) -> (std::sync::Arc<FlakyNetworkDropModelClient>, Vec<Event>) {
    let model = std::sync::Arc::new(FlakyNetworkDropModelClient {
        calls: std::sync::atomic::AtomicUsize::new(0),
        failures,
        terminal_before_drop: false,
        content_before_drop: true,
        open_failure: None,
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
            route: resolved_route_for_test(
                &config,
                codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL,
            ),
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
        .expect("send interactive flaky-network turn");

    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(model_turn_event_timeout(), async {
            handle.rx_event.write().await.recv().await
        })
        .await
        .expect("interactive flaky-network event timeout")
        .expect("interactive flaky-network event");
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

#[tokio::test]
async fn interactive_turn_preserves_partial_reply_and_recovers_after_network_drop() {
    let (model, events) = run_interactive_turn_with_flaky_network(1).await;

    assert_eq!(
        model.calls.load(std::sync::atomic::Ordering::SeqCst),
        2,
        "the dropped stream must be re-issued exactly once"
    );
    let (status, error) = events
        .iter()
        .find_map(|event| match event {
            Event::TurnComplete { status, error, .. } => Some((status, error)),
            _ => None,
        })
        .expect("terminal TurnComplete");
    assert_eq!(
        *status,
        TurnOutcomeStatus::Completed,
        "a recovered retry must complete the turn: {error:?}"
    );
    assert!(error.is_none(), "recovered turn must not report an error");
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::Status { message } if message.starts_with("Retry attempt: stream-resume 1/")
        )),
        "the first actual resume must remain visible: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Error { .. })),
        "a transient drop that the retry recovers must not surface an error event: {events:?}"
    );

    // The visible fragment must survive as an assistant message, followed by
    // the retried assistant content — and nothing else. The recovery is
    // typed internal state, so no synthetic `[runtime]` user turn may appear.
    let transcript = events
        .iter()
        .rev()
        .find_map(|event| match event {
            Event::SessionUpdated { messages, .. } => Some(messages.clone()),
            _ => None,
        })
        .expect("final SessionUpdated");
    assert!(
        transcript
            .iter()
            .flat_map(|message| message.content.iter())
            .all(|block| match block {
                ContentBlock::Text { text, .. } => !text.contains("[runtime]"),
                _ => true,
            }),
        "a retried turn must not insert a synthetic [runtime] user message: {transcript:?}"
    );
    assert_eq!(
        transcript
            .iter()
            .filter(|message| message.role == "user")
            .count(),
        1,
        "the operator's own turn must be the only user message: {transcript:?}"
    );
    let assistant_cells = transcript
        .iter()
        .filter(|message| message.role == "assistant")
        .count();
    assert_eq!(
        assistant_cells, 2,
        "preserved fragment + one authoritative continuation: {transcript:?}"
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
        transcript_text.contains("partial answer that must be discarded"),
        "the visible partial reply must be preserved in the session: {transcript_text}"
    );
    assert_eq!(
        transcript_text.matches("recovered after retry").count(),
        1,
        "exactly one authoritative final answer, not a duplicate: {transcript_text}"
    );
}

/// Streams only hidden reasoning and then dies with the network-class read
/// error; later streams complete a normal text turn. This is the shape of the
/// 0.9.10 regression: a thinking-only drop used to persist a synthetic
/// `[runtime]` user message claiming a partial answer had been preserved.
struct ThinkingOnlyDropModelClient {
    calls: std::sync::atomic::AtomicUsize,
    failures: usize,
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for ThinkingOnlyDropModelClient {
    fn provider_name(&self) -> &str {
        "flaky-network"
    }

    fn model(&self) -> &str {
        "local-model"
    }

    async fn create_message(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("thinking-only drop regression uses the streaming model boundary")
    }

    async fn create_message_stream(
        &self,
        _request: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        use crate::llm_client::mock::canned;
        let call = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .saturating_add(1);
        if call <= self.failures {
            // Hidden reasoning only — no text block is ever opened, so nothing
            // visible streams before the transport dies. This still flips
            // `any_content_received`, which is what routes the drop to the
            // interactive resume path instead of the transparent retry.
            let events: Vec<anyhow::Result<codewhale_models::StreamEvent>> = vec![
                Ok(canned::message_start("thinking_only_msg")),
                Ok(StreamEvent::ContentBlockStart {
                    index: 0,
                    content_block: codewhale_models::ContentBlockStart::Thinking {
                        thinking: String::new(),
                    },
                }),
                Ok(canned::thinking_delta(
                    0,
                    "hidden reasoning that no operator ever saw",
                )),
                Err(anyhow::anyhow!(
                    "Stream read error: error decoding response body"
                )),
            ];
            return Ok(Box::pin(futures_util::stream::iter(events)));
        }
        let events = canned::simple_text_turn("the one authoritative answer")
            .into_iter()
            .map(Ok);
        Ok(Box::pin(futures_util::stream::iter(events)))
    }

    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

struct TransportRetryModelClient {
    attempts: std::sync::atomic::AtomicUsize,
    failures: usize,
}

#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for TransportRetryModelClient {
    fn provider_name(&self) -> &str {
        "custom"
    }
    fn model(&self) -> &str {
        codewhale_config::descriptors::defaults::DEFAULT_TEXT_MODEL
    }
    async fn create_message(
        &self,
        _: codewhale_models::MessageRequest,
    ) -> anyhow::Result<codewhale_models::MessageResponse> {
        anyhow::bail!("streaming fixture only")
    }
    async fn create_message_stream(
        &self,
        _: codewhale_models::MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        crate::llm_client::with_retry(
            &crate::llm_client::RetryConfig {
                max_retries: 2,
                initial_delay: 0.0,
                jitter: false,
                ..Default::default()
            },
            || async {
                let attempt = self
                    .attempts
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if attempt < self.failures {
                    Err(crate::llm_client::LlmError::ServerError {
                        status: 503,
                        message: "RAW-ENGINE-RETRY-FAILURE".into(),
                    })
                } else {
                    Ok(())
                }
            },
            None,
        )
        .await
        .map_err(|error| anyhow::Error::new(error.last_error))?;
        Ok(Box::pin(futures_util::stream::iter(
            crate::llm_client::mock::canned::simple_text_turn("transport recovered answer")
                .into_iter()
                .map(Ok),
        )))
    }
    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

#[tokio::test]
async fn engine_transport_retry_receipts_and_counts_match_actual_scripted_attempts() {
    for (failures, quiet) in [(1, false), (3, false), (1, true)] {
        let workspace = tempdir().unwrap();
        let model = std::sync::Arc::new(TransportRetryModelClient {
            attempts: std::sync::atomic::AtomicUsize::new(0),
            failures,
        });
        let config = {
            let __cw_config_init_0_0 = Some(codewhale_config::notifications::NotificationsConfig {
                quiet,
                ..Default::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.notifications = __cw_config_init_0_0;
            __cw_config_init_0_value
        };
        let (mut engine, handle) = Engine::new_with_model_client(
            deterministic_engine_config(workspace.path()),
            &config,
            model.clone(),
        );
        let surface = test_tool_surface(
            &engine,
            crate::tools::ToolRegistry::new(crate::tools::ToolContext::new(
                workspace.path().to_path_buf(),
            )),
            None,
            AppMode::Agent,
        );
        let mut turn = crate::core::turn::TurnContext::new(1);
        let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;
        let expected_retries = failures.min(2) as u32;
        assert_eq!(
            model.attempts.load(std::sync::atomic::Ordering::SeqCst),
            (expected_retries + 1) as usize
        );
        assert_eq!(turn.stop_diagnostics.model_requests_started, 1);
        assert_eq!(turn.stop_diagnostics.transport_retries, expected_retries);
        assert_eq!(turn.stop_diagnostics.stream_resumes, 0);
        assert_eq!(turn.stop_diagnostics.transparent_stream_retries, 0);
        let mut receiver = handle.rx_event.write().await;
        let events = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
        if quiet {
            assert!(
                !events.iter().any(|event| matches!(event,
                    Event::Status { message } if message.starts_with("Retry attempt:")
                )),
                "quiet suppresses attempt lines, not actual counts or completed summaries"
            );
        } else {
            for attempt in 1..=expected_retries {
                assert!(events.iter().any(|event| matches!(event,
                    Event::Status { message } if message.starts_with(&format!("Retry attempt: transport {attempt}/2;"))
                )));
            }
        }
        assert!(
            !events.iter().any(|event| matches!(event,
                Event::Status { message } if message.contains("RAW-ENGINE-RETRY-FAILURE")
            )),
            "remote payload belongs to the original error, never a retry receipt"
        );
        if failures == 1 {
            assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
            assert!(events.iter().any(|event| matches!(event,
                Event::Status { message } if message == "Retry recovery: transport request recovered after 1 retries"
            )));
        } else {
            assert_eq!(status, TurnOutcomeStatus::Failed);
            assert!(error.unwrap().contains("RAW-ENGINE-RETRY-FAILURE"));
            assert!(events.iter().any(|event| matches!(event,
                Event::Status { message } if message.starts_with("Retry exhaustion: transport request stopped after 2 retries;")
            )));
        }
        assert!(engine.session.messages.iter().all(|message| message.content.iter().all(|block|
            !matches!(block, ContentBlock::Text { text, .. } if crate::core::events::is_retry_status_receipt(text))
        )), "receipts must never join the provider prompt or session message graph");
    }
}

#[tokio::test]
async fn full_retry_receipt_queue_cancellation_does_not_dispatch_an_extra_request() {
    let workspace = tempdir().unwrap();
    let (engine, _handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        std::sync::Arc::new(TransportRetryModelClient {
            attempts: std::sync::atomic::AtomicUsize::new(0),
            failures: 0,
        }),
    );
    while engine.tx_event.try_send(Event::status("occupied")).is_ok() {}
    let observation = engine.request_retry_observation();
    let count = observation.retries.clone();
    let mut calls = 0;
    let policy = crate::llm_client::RetryConfig {
        max_retries: 2,
        initial_delay: 0.0,
        jitter: false,
        ..Default::default()
    };
    let outcome = {
        let request = crate::llm_client::observe_request_retries(
            Some(observation),
            crate::llm_client::with_retry(
                &policy,
                || {
                    calls += 1;
                    async {
                        Err::<(), _>(crate::llm_client::LlmError::ServerError {
                            status: 503,
                            message: "private".into(),
                        })
                    }
                },
                None,
            ),
        );
        tokio::pin!(request);
        assert!(
            futures_util::poll!(request.as_mut()).is_pending(),
            "the receipt waits on the one full queue"
        );
        engine.cancel_token.cancel();
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                biased;
                () = engine.cancel_token.cancelled() => None,
                result = request.as_mut() => Some(result),
            }
        })
        .await
        .unwrap()
    };
    assert!(outcome.is_none());
    assert_eq!(calls, 1);
    assert_eq!(
        count.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a scheduled retry cancelled before dispatch is not spent"
    );
}

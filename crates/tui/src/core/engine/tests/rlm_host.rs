//! Actual Core fixtures for the migrated RLM contract. Replies are adapted to
//! the existing stream decoder; there is no fixture model/code loop.
use super::*;
use crate::core::engine::rlm_host::{CapturedRlmCaller, RlmInvocation, RlmMode};
use crate::llm_client::LlmClient;
use crate::rlm::bridge::{RlmBridge, RlmUsageAccumulator};
use codewhale_models::{MessageRequest, MessageResponse, StreamEvent};
use std::future::Future;
use std::pin::Pin;

pub(crate) trait Replies: Send + Sync {
    fn effective_route_envelope(
        &self,
        model: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> crate::cost_status::EffectiveRouteEnvelope;
    fn effective_max_output_tokens(&self, model: &str) -> u32;
    fn create_message_boxed(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<MessageResponse>> + Send + '_>>;
}
impl<T: LlmClient + Send + Sync> Replies for T {
    fn effective_route_envelope(
        &self,
        model: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> crate::cost_status::EffectiveRouteEnvelope {
        LlmClient::effective_route_envelope(self, model, at)
    }
    fn effective_max_output_tokens(&self, model: &str) -> u32 {
        LlmClient::effective_max_output_tokens(self, model)
    }
    fn create_message_boxed(
        &self,
        request: MessageRequest,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<MessageResponse>> + Send + '_>> {
        Box::pin(LlmClient::create_message(self, request))
    }
}
struct ReplyStream {
    replies: Arc<dyn Replies>,
    model: String,
}
#[async_trait::async_trait]
impl crate::core::model_client::ModelClient for ReplyStream {
    fn provider_name(&self) -> &str {
        "custom"
    }
    fn model(&self) -> &str {
        &self.model
    }
    fn effective_route_envelope(
        &self,
        model: &str,
        at: chrono::DateTime<chrono::Utc>,
    ) -> crate::cost_status::EffectiveRouteEnvelope {
        self.replies.effective_route_envelope(model, at)
    }
    fn effective_max_output_tokens(&self, model: &str) -> u32 {
        self.replies.effective_max_output_tokens(model)
    }
    async fn create_message(&self, request: MessageRequest) -> anyhow::Result<MessageResponse> {
        self.replies.create_message_boxed(request).await
    }
    async fn create_message_stream(
        &self,
        request: MessageRequest,
    ) -> anyhow::Result<crate::llm_client::StreamEventBox> {
        let response = self.replies.create_message_boxed(request).await?;
        let mut start = response.clone();
        start.content.clear();
        let mut events = vec![StreamEvent::MessageStart { message: start }];
        for (index, block) in response.content.iter().enumerate() {
            let index = u32::try_from(index).expect("fixture block index");
            match block {
                ContentBlock::Text { text, .. } => {
                    events.push(StreamEvent::ContentBlockStart {
                        index,
                        content_block: codewhale_models::ContentBlockStart::Text {
                            text: String::new(),
                        },
                    });
                    events.push(StreamEvent::ContentBlockDelta {
                        index,
                        delta: codewhale_models::Delta::TextDelta { text: text.clone() },
                    });
                }
                _ => {
                    events.push(StreamEvent::ContentBlockStart {
                        index,
                        content_block: serde_json::from_value(serde_json::to_value(block)?)?,
                    });
                }
            }
            events.push(StreamEvent::ContentBlockStop { index });
        }
        events.push(StreamEvent::MessageDelta {
            delta: codewhale_models::MessageDelta {
                stop_reason: response.stop_reason,
                stop_sequence: response.stop_sequence,
            },
            usage: Some(response.usage),
        });
        events.push(StreamEvent::MessageStop);
        Ok(Box::pin(futures_util::stream::iter(
            events.into_iter().map(Ok),
        )))
    }
    async fn health_check(&self) -> anyhow::Result<bool> {
        Ok(true)
    }
}

pub(crate) fn fixture_config(model: &str) -> Config {
    let mut config: Config = toml::from_str(&format!("provider = 'rlm-fixture'\n[providers.rlm-fixture]\nkind = 'openai-compatible'\napi_key = 'owned-fixture-key'\nbase_url = 'http://127.0.0.1:9/v1'\nmodel = {}\ncontext_window = 1000000\n", serde_json::to_string(model).unwrap())).expect("fixture Config");
    config.default_text_model = Some(model.to_string());
    config
}

pub(crate) fn install_fixture_route(engine: &mut Engine) {
    let identity = engine
        .api_config
        .active_provider_identity()
        .expect("fixture admitted exact id");
    let resolved = resolve_runtime_route_for_identity(
        &engine.api_config,
        &identity,
        Some(&engine.session.model),
    )
    .expect("fixture route");
    let client = CodewhaleClient::new(&resolved.config)
        .expect("fixture captured client, never called unless loopback transport was supplied");
    engine.install_validated_runtime_route(ValidatedRuntimeRoute {
        identity: resolved.identity,
        candidate: resolved.candidate,
        config: resolved.config,
        model: resolved.model,
        context_window: resolved.context_window,
        client,
    });
}

/// Use the actual configured Engine's posture, route, registry and services.
/// No client-only or ambient default configuration grants fixture authority.
pub(crate) fn admitted_context(engine: &Engine, turn_id: &str) -> ToolContext {
    let posture = engine.applied_runtime_authority();
    let authority = TurnAuthority::from_effective_fields(
        posture.mode,
        posture.allow_shell,
        posture.trust_mode,
        posture.auto_approve,
        posture.approval_mode,
    );
    let route = TurnRouteContext {
        provider: engine.api_provider,
        model: engine.session.model.clone(),
        capabilities: engine.active_route_capabilities,
        limits: engine.active_route_limits,
        client: engine.codewhale_client.clone(),
        api_config: Box::new(engine.api_config.clone()),
        locale_tag: "en".into(),
        role_models: engine.subagent_role_models(),
        auto_model: false,
        reasoning_effort: None,
        reasoning_effort_auto: false,
    };
    let mut context = engine.build_tool_context_for_turn(&authority, &route);
    let caller = CapturedRlmCaller::capture(engine, &authority, &route, &context, turn_id)
        .expect("actual Core admission");
    context.rlm_caller = Some(Arc::new(caller));
    context
}

pub(crate) fn caller_for(replies: Arc<dyn Replies>, model: &str) -> CapturedRlmCaller {
    let workspace = Arc::new(tempfile::tempdir().expect("RLM fixture workspace"));
    let config = fixture_config(model);
    let (mut engine, _handle) = Engine::new_with_model_client(
        EngineConfig {
            workspace: workspace.path().to_path_buf(),
            model: model.to_string(),
            snapshots_enabled: false,
            memory_enabled: false,
            subagents_enabled: false,
            terminal_chrome_enabled: false,
            turn_wall_clock: Duration::from_secs(60),
            ..EngineConfig::default()
        },
        &config,
        Arc::new(ReplyStream {
            replies,
            model: model.to_string(),
        }),
    );
    install_fixture_route(&mut engine);
    engine.session.system_prompt = Some(SystemPrompt::Text(
        "Captured operator Core policy. Task guidance is additive.".into(),
    ));
    let context = admitted_context(&engine, "fixture-origin-turn");
    let mut caller = context
        .rlm_caller
        .as_ref()
        .expect("captured receipt")
        .as_ref()
        .clone();
    caller.hold_fixture_workspace(workspace);
    caller
}

/// Admit a loopback LlmClient through a real Engine with the supplied Config
/// and tool context services, rather than granting authority to its URL alone.
pub(crate) fn context_for_replies(
    config: &Config,
    model: &str,
    context: &ToolContext,
    replies: Arc<dyn Replies>,
) -> ToolContext {
    let (mut engine, _handle) = Engine::new_with_model_client(
        EngineConfig {
            workspace: context.workspace.clone(),
            model: model.to_string(),
            session_id: Some(context.state_namespace.clone()),
            runtime_services: context.runtime.clone(),
            plugin_registry: context.plugin_registry.clone(),
            snapshots_enabled: false,
            memory_enabled: false,
            subagents_enabled: false,
            terminal_chrome_enabled: false,
            turn_wall_clock: Duration::from_secs(60),
            ..EngineConfig::default()
        },
        config,
        Arc::new(ReplyStream {
            replies,
            model: model.to_string(),
        }),
    );
    install_fixture_route(&mut engine);
    let mut admitted = admitted_context(&engine, "fixture-tool-origin-turn");
    admitted.nested_call_gate = context.nested_call_gate.clone();
    admitted
}

pub(crate) struct BridgeFixture {
    caller: CapturedRlmCaller,
    depth: u32,
    pub(crate) usage: RlmUsageAccumulator,
    deadline: Option<tokio::time::Instant>,
    gate: Option<crate::tools::codemode::NestedCallGate>,
    events: Option<mpsc::Sender<Event>>,
}
impl BridgeFixture {
    pub(crate) fn new(replies: Arc<dyn Replies>, model: String, depth: u32) -> Self {
        Self::with_usage_accumulator(replies, model, depth, RlmUsageAccumulator::new())
    }
    pub(crate) fn with_usage_accumulator(
        replies: Arc<dyn Replies>,
        model: String,
        depth: u32,
        usage: RlmUsageAccumulator,
    ) -> Self {
        Self {
            caller: caller_for(replies, &model),
            depth,
            usage,
            deadline: None,
            gate: None,
            events: None,
        }
    }
    fn bridge(&self) -> RlmBridge<'_> {
        let mut bridge =
            RlmBridge::with_usage_accumulator(&self.caller, self.depth, self.usage.clone())
                .with_deadline(self.deadline)
                .with_gate(self.gate.clone());
        if let Some(events) = &self.events {
            bridge = bridge.with_events(events.clone());
        }
        bridge
    }
    pub(crate) fn with_deadline(mut self, deadline: Option<tokio::time::Instant>) -> Self {
        self.deadline = deadline;
        self
    }
    pub(crate) fn with_gate(
        mut self,
        gate: Option<crate::tools::codemode::NestedCallGate>,
    ) -> Self {
        self.gate = gate;
        self
    }
    pub(crate) fn with_events(mut self, events: mpsc::Sender<Event>) -> Self {
        self.events = Some(events);
        self
    }
    pub(crate) async fn usage_snapshot(&self) -> crate::rlm::bridge::RlmUsageSnapshot {
        self.usage.snapshot().await
    }
    pub(crate) async fn dispatch_rlm(
        &self,
        prompt: String,
        model: Option<String>,
    ) -> codewhale_repl::runtime::SingleResp {
        self.bridge().dispatch_rlm(prompt, model).await
    }
}
impl codewhale_repl::runtime::RpcDispatcher for BridgeFixture {
    fn dispatch<'a>(
        &'a self,
        request: codewhale_repl::runtime::RpcRequest,
    ) -> Pin<Box<dyn Future<Output = codewhale_repl::runtime::RpcResponse> + Send + 'a>> {
        Box::pin(async move {
            codewhale_repl::runtime::RpcDispatcher::dispatch(&self.bridge(), request).await
        })
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_admitted_fixture(
    replies: Arc<dyn Replies>,
    model: String,
    prompt: String,
    task: Option<String>,
    _old_child_model: String,
    events: mpsc::Sender<Event>,
    depth: u32,
    usage: RlmUsageAccumulator,
    deadline: tokio::time::Instant,
    gate: Option<crate::tools::codemode::NestedCallGate>,
) -> crate::rlm::turn::RlmTurnResult {
    let caller = caller_for(replies, &model);
    caller
        .dispatch(RlmInvocation {
            prompt,
            mode: RlmMode::Recursive {
                depth_remaining: depth,
            },
            max_tokens: None,
            task_instructions: task,
            deadline: Some(deadline),
            gate,
            events: Some(events),
            usage,
        })
        .await
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_fixture(
    replies: Arc<dyn Replies>,
    model: String,
    prompt: String,
    task: Option<String>,
    old_child_model: String,
    events: mpsc::Sender<Event>,
    depth: u32,
) -> crate::rlm::turn::RlmTurnResult {
    run_admitted_fixture(
        replies,
        model,
        prompt,
        task,
        old_child_model,
        events,
        depth,
        RlmUsageAccumulator::new(),
        tokio::time::Instant::now() + Duration::from_secs(60),
        Some(crate::tools::codemode::NestedCallGate::admitting_for_test()),
    )
    .await
}

#[tokio::test]
async fn captured_identity_cancel_and_task_bounds_refuse_before_any_provider() {
    let workspace = tempfile::tempdir().unwrap();
    let config = fixture_config("captured-model");
    let mock = Arc::new(crate::llm_client::mock::MockLlmClient::new(Vec::new()));
    let context = context_for_replies(
        &config,
        "captured-model",
        &ToolContext::new(workspace.path()),
        mock.clone(),
    );
    let caller = context.rlm_caller.as_ref().unwrap();
    for altered in [
        {
            let mut c = context.clone();
            c.workspace = workspace.path().join("other");
            c
        },
        {
            let mut c = context.clone();
            c.state_namespace.push_str("-other");
            c
        },
        {
            let mut c = context.clone();
            c.owner_agent_id = Some("foreign-child".into());
            c
        },
    ] {
        assert!(
            caller.validate_context(&altered).is_err(),
            "caller mutation cannot grant a nested route"
        );
    }
    let zero_tokens = caller
        .dispatch(RlmInvocation {
            prompt: "no effect".into(),
            mode: RlmMode::Completion,
            max_tokens: Some(0),
            task_instructions: None,
            deadline: caller.deadline(),
            gate: None,
            events: None,
            usage: RlmUsageAccumulator::new(),
        })
        .await;
    assert!(
        zero_tokens
            .error
            .unwrap()
            .contains("max_tokens must be greater than zero")
    );
    assert_eq!(mock.call_count(), 0);
    let rejected = caller
        .dispatch(RlmInvocation {
            prompt: "no effect".into(),
            mode: RlmMode::Completion,
            max_tokens: None,
            task_instructions: Some("x".repeat(4097)),
            deadline: caller.deadline(),
            gate: None,
            events: None,
            usage: RlmUsageAccumulator::new(),
        })
        .await;
    assert!(rejected.error.unwrap().contains("4096 bytes"));
    context.cancel_token.as_ref().unwrap().cancel();
    let cancelled = caller
        .dispatch(RlmInvocation {
            prompt: "no effect".into(),
            mode: RlmMode::Completion,
            max_tokens: None,
            task_instructions: None,
            deadline: caller.deadline(),
            gate: None,
            events: None,
            usage: RlmUsageAccumulator::new(),
        })
        .await;
    assert!(
        cancelled
            .error
            .unwrap()
            .contains("originating turn is cancelled")
    );
    assert_eq!(mock.call_count(), 0);
}

#[tokio::test]
async fn dropping_an_actual_dispatched_core_call_keeps_unknown_usage_without_retaining_authority() {
    struct Pending;
    impl Replies for Pending {
        fn effective_route_envelope(
            &self,
            model: &str,
            at: chrono::DateTime<chrono::Utc>,
        ) -> crate::cost_status::EffectiveRouteEnvelope {
            crate::cost_status::EffectiveRouteEnvelope::capture_observed(
                codewhale_config::ProviderKind::Custom,
                "rlm-fixture",
                model,
                Some("http://127.0.0.1:9/v1"),
                at,
            )
        }
        fn effective_max_output_tokens(&self, _: &str) -> u32 {
            8192
        }
        fn create_message_boxed(
            &self,
            _: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<MessageResponse>> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }
    let caller = Arc::new(caller_for(Arc::new(Pending), "captured-model"));
    let weak_caller = Arc::downgrade(&caller);
    let usage = RlmUsageAccumulator::new();
    let result = caller
        .dispatch(RlmInvocation {
            prompt: "pending".into(),
            mode: RlmMode::Completion,
            max_tokens: None,
            task_instructions: None,
            deadline: Some(tokio::time::Instant::now() + Duration::from_millis(100)),
            gate: None,
            events: None,
            usage: usage.clone(),
        })
        .await;
    assert!(
        result
            .error
            .as_deref()
            .unwrap()
            .contains("wall-clock deadline")
    );
    let snapshot = usage.snapshot().await;
    assert_eq!(snapshot.records.len(), 0);
    assert_eq!(snapshot.drop_records.len(), 1);
    assert_eq!(
        snapshot.drop_records[0].reason,
        crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown
    );
    assert_eq!(snapshot.drop_records[0].route.model, "captured-model");
    assert_eq!(snapshot.dropped_records, 1);
    assert_eq!(
        usage.snapshot().await.drop_records,
        snapshot.drop_records,
        "inspection never republishes or loses the Drop receipt"
    );
    drop(caller);
    assert!(
        weak_caller.upgrade().is_none(),
        "dropped nested work retains no caller authority"
    );
}

#[tokio::test]
async fn full_caller_channel_never_delays_original_cancel_or_loses_usage_and_kernel_cleanup() {
    struct FirstThenPending {
        mock: crate::llm_client::mock::MockLlmClient,
        entered: tokio::sync::Notify,
    }
    impl Replies for FirstThenPending {
        fn effective_route_envelope(
            &self,
            model: &str,
            at: chrono::DateTime<chrono::Utc>,
        ) -> crate::cost_status::EffectiveRouteEnvelope {
            Replies::effective_route_envelope(&self.mock, model, at)
        }
        fn effective_max_output_tokens(&self, model: &str) -> u32 {
            Replies::effective_max_output_tokens(&self.mock, model)
        }
        fn create_message_boxed(
            &self,
            request: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = anyhow::Result<MessageResponse>> + Send + '_>> {
            Box::pin(async move {
                if self.mock.call_count() > 0 {
                    self.entered.notify_one();
                    std::future::pending().await
                } else {
                    Replies::create_message_boxed(&self.mock, request).await
                }
            })
        }
    }
    let replies = Arc::new(FirstThenPending {
        mock: crate::llm_client::mock::MockLlmClient::new(Vec::new()),
        entered: tokio::sync::Notify::new(),
    });
    replies.mock.push_message_response(MessageResponse {
        id: "completed-python-round".into(),
        r#type: "message".into(),
        role: "assistant".into(),
        content: vec![ContentBlock::Text {
            text: "```repl\nprint(_os.environ['RLM_CONTEXT_FILE'])\n```".into(),
            cache_control: None,
        }],
        model: "captured-model".into(),
        stop_reason: Some("end_turn".into()),
        stop_sequence: None,
        container: None,
        usage: Usage {
            input_tokens: 7,
            output_tokens: 11,
            ..Usage::default()
        },
    });
    let caller = caller_for(replies.clone(), "captured-model");
    let cancel = caller.fixture_origin_cancel();
    let usage = RlmUsageAccumulator::new();
    let (tx, mut rx) = mpsc::channel(1);
    let call = caller.dispatch(RlmInvocation {
        prompt: "context held only by Python".into(),
        mode: RlmMode::Recursive { depth_remaining: 0 },
        max_tokens: None,
        task_instructions: None,
        deadline: Some(tokio::time::Instant::now() + Duration::from_secs(60)),
        gate: Some(crate::tools::codemode::NestedCallGate::admitting_for_test()),
        events: Some(tx.clone()),
        usage: usage.clone(),
    });
    tokio::pin!(call);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                biased;
                _ = replies.entered.notified() => break,
                result = &mut call => panic!("second provider request was never reached: {:?}", result.error),
                event = rx.recv() => assert!(event.is_some()),
            }
        }
    }).await.expect("completed Python round must reach the second actual Core request");
    // Preserve the receiver but stop observing: it can no longer release a
    // blocked forwarding send. Cancellation, not its 60-second deadline,
    // must now retire the owned provider and Python work.
    let _ = tx.try_send(Event::status("occupied caller channel"));
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(1), &mut call)
        .await
        .expect("original cancellation must interrupt a full observational channel");
    assert_eq!(result.iterations, 2);
    assert_eq!(result.usage.input_tokens, 7);
    assert_eq!(result.usage.output_tokens, 11);
    assert_eq!(result.routed_usage.len(), 1);
    assert_eq!(result.routed_usage_drop_records.len(), 1);
    assert_eq!(
        result.routed_usage_drop_records[0].reason,
        crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown
    );
    let context_path = PathBuf::from(
        result
            .trace
            .first()
            .expect("completed round trace")
            .stdout_preview
            .trim(),
    );
    assert!(context_path.is_absolute());
    assert!(
        !context_path.exists(),
        "cancelled nested Engine releases its owned kernel/context file"
    );
    assert_eq!(usage.snapshot().await.dropped_records, 1);
}

#[tokio::test]
async fn nested_host_never_claims_or_overwrites_parent_posture_revision() {
    let workspace = tempfile::tempdir().unwrap();
    let config = fixture_config("captured-model");
    let mock = Arc::new(crate::llm_client::mock::MockLlmClient::new(Vec::new()));
    let (mut parent, _handle) = Engine::new_with_model_client(
        EngineConfig {
            workspace: workspace.path().to_path_buf(),
            model: "captured-model".into(),
            memory_enabled: false,
            snapshots_enabled: false,
            subagents_enabled: false,
            ..EngineConfig::default()
        },
        &config,
        mock,
    );
    install_fixture_route(&mut parent);
    {
        let mut state = parent.live_runtime_authority.lock().unwrap();
        state.revision = 17;
        state.applied_revision = 8;
    }
    let context = admitted_context(&parent, "origin-posture-turn");
    let caller = context.rlm_caller.as_ref().unwrap().clone();
    let (nested, _handle) = Engine::new_rlm_admitted(
        caller,
        RlmInvocation {
            prompt: "no provider invocation".into(),
            mode: RlmMode::Completion,
            max_tokens: None,
            task_instructions: None,
            deadline: Some(tokio::time::Instant::now() + Duration::from_secs(60)),
            gate: None,
            events: None,
            usage: RlmUsageAccumulator::new(),
        },
    )
    .await
    .unwrap();
    assert!(Arc::ptr_eq(
        &nested.subagent_manager,
        &parent.subagent_manager
    ));
    assert!(!Arc::ptr_eq(
        &nested.live_runtime_authority,
        &parent.live_runtime_authority
    ));
    nested.record_applied_runtime_authority(&TurnAuthority::from_effective_fields(
        AppMode::Plan,
        false,
        false,
        false,
        ApprovalMode::Never,
    ));
    let state = parent.live_runtime_authority.lock().unwrap();
    assert_eq!(state.revision, 17);
    assert_eq!(state.applied_revision, 8);
    assert_eq!(parent.current_mode, AppMode::Agent);
}

#[tokio::test]
async fn repeated_real_core_preparation_failures_have_bounded_receipts_without_provider_work() {
    let workspace = tempfile::tempdir().unwrap();
    let mut config = fixture_config("captured-model");
    let identity = config.active_provider_identity().unwrap();
    config
        .provider_config_for_mut(&identity)
        .unwrap()
        .context_window = Some(1);
    let mock = Arc::new(crate::llm_client::mock::MockLlmClient::new(Vec::new()));
    let (mut engine, _handle) = Engine::new_with_model_client(
        EngineConfig {
            workspace: workspace.path().to_path_buf(),
            model: "captured-model".into(),
            memory_enabled: false,
            snapshots_enabled: false,
            subagents_enabled: false,
            terminal_chrome_enabled: false,
            ..EngineConfig::default()
        },
        &config,
        mock.clone(),
    );
    install_fixture_route(&mut engine);
    assert_eq!(
        engine
            .active_route_limits
            .and_then(|limits| limits.context_tokens),
        Some(1),
        "the installed actual Core route carries the fixture's tiny context budget",
    );
    let context = admitted_context(&engine, "bounded-failed-turn");
    let caller = context.rlm_caller.as_ref().unwrap();
    let usage = RlmUsageAccumulator::new();
    let invocation = || RlmInvocation {
        prompt: "real request preparation must retain this whole input".into(),
        mode: RlmMode::Completion,
        max_tokens: None,
        task_instructions: None,
        deadline: Some(tokio::time::Instant::now() + Duration::from_secs(60)),
        gate: None,
        events: None,
        usage: usage.clone(),
    };
    for _ in 0..crate::cost_status::MAX_CHILD_USAGE_RECORDS {
        let result = caller.dispatch(invocation()).await;
        assert!(
            result.error.as_deref().unwrap().contains("history exceeds"),
            "{result:?}"
        );
        assert_eq!(result.iterations, 0);
    }
    let before = usage.snapshot().await;
    assert_eq!(
        before.nested_events.len(),
        crate::cost_status::MAX_CHILD_USAGE_RECORDS
    );
    assert!(before.records.is_empty());
    assert!(before.drop_records.is_empty());
    assert_eq!(before.dropped_records, 0);
    assert_eq!(mock.call_count(), 0);
    let refused = caller.dispatch(invocation()).await;
    assert!(
        refused
            .error
            .as_deref()
            .unwrap()
            .contains("nested-turn receipt limit")
    );
    let after = usage.snapshot().await;
    assert_eq!(after.nested_events, before.nested_events);
    assert_eq!(mock.call_count(), 0);
}

#[tokio::test]
async fn actual_expired_python_startup_releases_the_staged_context() {
    let mock = Arc::new(crate::llm_client::mock::MockLlmClient::new(Vec::new()));
    let caller = Arc::new(caller_for(mock.clone(), "captured-model"));
    let prompt = format!("owned startup cleanup fixture {}", uuid::Uuid::new_v4());
    let result = Engine::new_rlm_admitted(
        caller,
        RlmInvocation {
            prompt: prompt.clone(),
            mode: RlmMode::Recursive { depth_remaining: 0 },
            max_tokens: None,
            task_instructions: None,
            deadline: Some(tokio::time::Instant::now()),
            gate: Some(crate::tools::codemode::NestedCallGate::admitting_for_test()),
            events: None,
            usage: RlmUsageAccumulator::new(),
        },
    )
    .await;
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("Python startup failed")
    );
    let prefix = format!("session_{}_", std::process::id());
    for entry in std::fs::read_dir(std::env::temp_dir().join("deepseek_rlm_ctx")).unwrap() {
        let entry = entry.unwrap();
        // Only inspect this test process's similarly sized fixture files;
        // parallel RLM tests keep their own unrelated kernels untouched.
        if !entry.file_name().to_string_lossy().starts_with(&prefix) {
            continue;
        }
        if let Ok(metadata) = entry.metadata()
            && metadata.len() == u64::try_from(prompt.len()).unwrap()
            && let Ok(content) = std::fs::read_to_string(entry.path())
        {
            assert_ne!(
                content, prompt,
                "failed startup must remove its staged file"
            );
        }
    }
    assert_eq!(mock.call_count(), 0);
}

//! Captured, call-local RLM projection onto the canonical Engine.
//!
//! A Python kernel never owns this receipt or a bridge. The only model/code
//! driver is Engine::run_turn; the RPC adapter owns its bounded invocation.
use super::turn_loop::usage_has_reported_data;
use super::*;
use crate::core::authority::auto_review::AutoReviewPolicy;
use crate::rlm::bridge::{RlmUsageAccumulator, RlmUsageReservation, wait_for_deadline};
use crate::rlm::turn::{RlmRoundTrace, RlmTermination, RlmTurnResult};
use anyhow::anyhow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RlmMode {
    Completion,
    Recursive { depth_remaining: u32 },
}

pub(crate) struct RlmInvocation {
    pub prompt: String,
    pub mode: RlmMode,
    pub max_tokens: Option<u32>,
    pub task_instructions: Option<String>,
    pub deadline: Option<tokio::time::Instant>,
    pub gate: Option<crate::tools::codemode::NestedCallGate>,
    pub events: Option<mpsc::Sender<Event>>,
    pub usage: RlmUsageAccumulator,
}

/// Frozen from the actual serving Core turn, never from a client/string alone.
/// `context` predates attaching this receipt, so it cannot contain itself.
#[derive(Clone)]
pub(crate) struct CapturedRlmCaller {
    #[cfg(test)]
    _fixture_workspace: Option<Arc<tempfile::TempDir>>,
    context: ToolContext,
    authority: TurnAuthority,
    route: ValidatedRuntimeRoute,
    client: SharedModelClient,
    subagent_manager: SharedSubAgentManager,
    injected: bool,
    config: EngineConfig,
    system: SystemPrompt,
    extension_prompt_block: Option<String>,
    constitution_block: Option<String>,
    approval_store: Result<ApprovalReceiptStore, String>,
    review_policy: Arc<AutoReviewPolicy>,
    deadline: Option<tokio::time::Instant>,
    origin_scope: crate::cost_status::CostScopeToken,
    origin_session: String,
    origin_turn: String,
    child_accounting: Option<crate::tools::subagent::engine::ChildAccountingProjection>,
    scheduler: tokio::runtime::Handle,
    runtime_owner: Option<String>,
    _runtime_lease: Option<crate::cost_status::RuntimeUsageLease>,
}
impl std::fmt::Debug for CapturedRlmCaller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CapturedRlmCaller")
            .field("workspace", &self.context.workspace)
            .field("session", &self.context.state_namespace)
            .finish_non_exhaustive()
    }
}
impl CapturedRlmCaller {
    pub(super) fn capture(
        engine: &Engine,
        authority: &TurnAuthority,
        route: &TurnRouteContext,
        context: &ToolContext,
        origin_turn: &str,
    ) -> Result<Self, ToolError> {
        if context.acp_host.is_some() || origin_turn.trim().is_empty() {
            return Err(ToolError::not_available(
                "this host has no RLM model-call authority",
            ));
        }
        let identity = engine
            .api_provider_identity
            .as_ref()
            .ok_or_else(|| ToolError::not_available("the Core route has no admitted identity"))?;
        route
            .api_config
            .verify_provider_identity(identity)
            .map_err(ToolError::permission_denied)?;
        let resolved =
            resolve_runtime_route_for_identity(&route.api_config, identity, Some(&route.model))
                .map_err(ToolError::permission_denied)?;
        let client = engine
            .codewhale_client
            .as_ref()
            .ok_or_else(|| ToolError::not_available("the Core route has no captured client"))?;
        if client.admitted_provider_identity() != identity
            || engine
                .active_route_endpoint
                .as_ref()
                .map(Engine::endpoint_identity)
                != Some(Engine::endpoint_identity(resolved.candidate.endpoint()))
        {
            return Err(ToolError::permission_denied(
                "the RLM route differs from the installed Core client",
            ));
        }
        let mut captured_context = context.clone();
        captured_context.rlm_caller = None;
        let child_accounting = engine
            .child_host
            .as_ref()
            .map(|child| child.authority.accounting_projection());
        let scheduler = tokio::runtime::Handle::try_current()
            .map_err(|_| ToolError::not_available("RLM caller has no held Engine scheduler"))?;
        let runtime_owner = engine.config.compaction.runtime_cost_owner.clone();
        let runtime_lease = runtime_owner
            .as_deref()
            .and_then(crate::cost_status::acquire_runtime_usage_lease);
        let deadline = context
            .turn_deadline
            .into_iter()
            .chain(engine.nested_work_deadline())
            .min();
        let result =
            Self {
                #[cfg(test)]
                _fixture_workspace: None,
                context: captured_context,
                authority: authority.clone(),
                route: ValidatedRuntimeRoute {
                    identity: resolved.identity,
                    candidate: resolved.candidate,
                    config: resolved.config,
                    model: resolved.model,
                    context_window: resolved.context_window,
                    client: client.clone(),
                },
                client: engine.model_client.clone().ok_or_else(|| {
                    ToolError::not_available("the Core model client is unavailable")
                })?,
                subagent_manager: Arc::clone(&engine.subagent_manager),
                injected: engine.model_client_injected,
                config: engine.config.clone(),
                system: engine.session.system_prompt.clone().ok_or_else(|| {
                    ToolError::not_available("the Core policy prompt is unavailable")
                })?,
                extension_prompt_block: engine.extension_prompt_block.clone(),
                constitution_block: engine.constitution_block.clone(),
                approval_store: engine.approval_receipt_store.clone(),
                review_policy: Arc::clone(&engine.shared_auto_review_policy),
                deadline,
                origin_scope: crate::cost_status::scope_token(),
                origin_session: context.state_namespace.clone(),
                origin_turn: origin_turn.to_string(),
                child_accounting,
                scheduler,
                runtime_owner,
                _runtime_lease: runtime_lease,
            };
        result.validate_context(context)?;
        Ok(result)
    }

    pub(crate) fn validate_context(&self, context: &ToolContext) -> Result<(), ToolError> {
        if context.workspace != self.context.workspace
            || context.state_namespace != self.context.state_namespace
            || context.owner_agent_id != self.context.owner_agent_id
            || context
                .plugin_registry
                .as_ref()
                .map(|p| p.caller_selection())
                != self
                    .context
                    .plugin_registry
                    .as_ref()
                    .map(|p| p.caller_selection())
        {
            return Err(ToolError::permission_denied(
                "RLM caller identity, workspace or composition changed",
            ));
        }
        self.validate_live()
    }

    pub(super) fn validate_live(&self) -> Result<(), ToolError> {
        if self
            .context
            .cancel_token
            .as_ref()
            .is_none_or(CancellationToken::is_cancelled)
        {
            return Err(ToolError::cancelled(
                "RLM originating turn is cancelled or unavailable",
            ));
        }
        if self
            .deadline
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            return Err(ToolError::execution_failed(
                "RLM original parent deadline exhausted",
            ));
        }
        crate::extension_host::validate_caller_plugins(self.context.plugin_registry.as_deref())
            .map_err(ToolError::permission_denied)?;
        self.route
            .config
            .verify_provider_identity(&self.route.identity)
            .map_err(ToolError::permission_denied)
    }

    pub(crate) fn deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline
    }

    /// Boxing breaks recursive future types; the future still borrows this
    /// caller and never enters the persistent session map or PythonRuntime.
    pub(crate) fn dispatch<'call>(
        &'call self,
        invocation: RlmInvocation,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RlmTurnResult> + Send + 'call>> {
        Box::pin(self.dispatch_admitted(invocation))
    }

    async fn dispatch_admitted(&self, mut invocation: RlmInvocation) -> RlmTurnResult {
        let started = Instant::now();
        invocation.deadline = invocation.deadline.into_iter().chain(self.deadline).min();
        if let Err(error) = self.validate_live() {
            return RlmTurnResult::failed(error.to_string(), started.elapsed());
        }
        if invocation
            .deadline
            .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
        {
            return RlmTurnResult::failed(
                "RLM original parent deadline exhausted".into(),
                started.elapsed(),
            );
        }
        if invocation.mode != RlmMode::Completion && invocation.gate.is_none() {
            return RlmTurnResult::failed(
                "no permission gate is serving this RLM turn".into(),
                started.elapsed(),
            );
        }
        // Every admitted completion/recursive host owns bounded receipt
        // capacity, including setup/preparation failures with no provider call.
        if let Err(error) = invocation.usage.reserve_nested_turn().await {
            return RlmTurnResult::failed(error, started.elapsed());
        }
        let usage = invocation.usage.clone();
        let events = invocation.events.clone();
        let depth = match invocation.mode {
            RlmMode::Completion => 0,
            RlmMode::Recursive { depth_remaining } => depth_remaining,
        };
        let run_id = uuid::Uuid::new_v4().simple().to_string();
        let (mut engine, handle) =
            match Engine::new_rlm_admitted(Arc::new(self.clone()), invocation).await {
                Ok(host) => host,
                Err(error) => return RlmTurnResult::failed(error.to_string(), started.elapsed()),
            };
        let spec = match engine.rlm_turn_spec() {
            Ok(spec) => spec,
            Err(error) => return RlmTurnResult::failed(error.to_string(), started.elapsed()),
        };
        let cancel = self
            .context
            .cancel_token
            .as_ref()
            .expect("validated caller cancel")
            .clone();
        let deadline = engine.rlm_host.as_ref().expect("RLM host").deadline;
        engine.rlm_host.as_mut().expect("RLM host").run_id = run_id;
        let mut receiver = handle.rx_event.write().await;
        let local_cancel = engine.cancel_token.clone();
        // Drive the existing admission/turn method while draining its existing
        // bounded event channel. No authority-bearing task is detached.
        let outcome = {
            let mut call = Box::pin(engine.handle_send_message(spec));
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => { local_cancel.cancel(); break None; }
                    () = wait_for_deadline(deadline) => { local_cancel.cancel(); break None; }
                    result = &mut call => break Some(result),
                    event = receiver.recv() => {
                        let Some(event) = event else { local_cancel.cancel(); break None; };
                        forward_rlm_event(events.as_ref(), event, depth, &cancel, deadline).await;
                    }
                }
            }
        };
        while let Ok(event) = receiver.try_recv() {
            forward_rlm_event(events.as_ref(), event, depth, &cancel, deadline).await;
        }
        let mut result = engine.rlm_result(outcome, started.elapsed());
        let terminal = format!(
            "RLM finished: {:?} after {} iteration(s), {} sub-LLM call(s), answer {} chars{}",
            result.termination,
            result.iterations,
            result.total_rpcs,
            result.answer.chars().count(),
            result
                .error
                .as_ref()
                .map_or(String::new(), |error| format!(" — {error}"))
        );
        usage.record_nested_event(serde_json::json!({"run_id": engine.rlm_host.as_ref().expect("RLM host").run_id, "depth_remaining": depth, "kind":"status", "content":terminal})).await;
        // Settlement is already in the canonical nested-event ledger. The
        // terminal observation must not wait past cancellation/deadline, and
        // an available channel can still observe it after the deadline.
        if let Some(events) = events.as_ref()
            && let Some(message) =
                crate::rlm::bridge::nested_rlm_status_line(Event::status(terminal), depth)
        {
            let _ = events.try_send(Event::status(message));
        }
        let snapshot = usage.snapshot().await;
        result.usage = snapshot.usage;
        result.routed_usage = snapshot.records;
        result.routed_usage_drop_records = snapshot.drop_records;
        result.routed_usage_dropped_records = snapshot.dropped_records;
        result
    }
}

pub(super) struct RlmHostSetup {
    pub caller: Arc<CapturedRlmCaller>,
    pub invocation: RlmInvocation,
    pub system_prompt: SystemPrompt,
}

pub(super) struct RlmHostState {
    pub caller: Arc<CapturedRlmCaller>,
    pub run_id: String,
    pub mode: RlmMode,
    pub system_prompt: SystemPrompt,
    pub prompt: String,
    pub deadline: Option<tokio::time::Instant>,
    pub gate: Option<crate::tools::codemode::NestedCallGate>,
    pub usage: RlmUsageAccumulator,
    pub max_tokens: Option<u32>,
    pub trace: Vec<RlmRoundTrace>,
    pub last_response: String,
    pub final_answer: Option<String>,
    pub total_rpcs: u32,
    pub consecutive_no_code: u32,
    pub termination: Option<RlmTermination>,
    pub error: Option<String>,
    pub model_rounds: u32,
}
impl From<RlmHostSetup> for RlmHostState {
    fn from(setup: RlmHostSetup) -> Self {
        Self {
            caller: setup.caller,
            run_id: String::new(),
            mode: setup.invocation.mode,
            system_prompt: setup.system_prompt,
            prompt: setup.invocation.prompt,
            deadline: setup.invocation.deadline,
            gate: setup.invocation.gate,
            usage: setup.invocation.usage,
            max_tokens: setup.invocation.max_tokens,
            trace: Vec::new(),
            last_response: String::new(),
            final_answer: None,
            total_rpcs: 0,
            consecutive_no_code: 0,
            termination: None,
            error: None,
            model_rounds: 0,
        }
    }
}

impl Engine {
    pub(super) async fn new_rlm_admitted(
        caller: Arc<CapturedRlmCaller>,
        invocation: RlmInvocation,
    ) -> Result<(Self, EngineHandle)> {
        caller.validate_live().map_err(anyhow::Error::new)?;
        let mut config = caller.config.clone();
        config.model = caller.route.model.clone();
        config.workspace = caller.context.workspace.clone();
        config.session_id = Some(caller.context.state_namespace.clone());
        config.plugin_registry = caller.context.plugin_registry.clone();
        config.runtime_services = caller.context.runtime.clone();
        config.allow_shell = caller.authority.allow_shell;
        config.trust_mode = caller.authority.trust_mode;
        config.network_policy = caller.context.network_policy.clone();
        config.max_steps = if invocation.mode == RlmMode::Completion {
            1
        } else {
            caller.config.max_steps
        };
        config.snapshots_enabled = false;
        config.memory_enabled = false;
        config.terminal_chrome_enabled = false;
        config.subagents_enabled = false;
        config.advisor_config = crate::tools::subagent::AdvisorConfig::disabled();
        config.goal_objective = None;
        config.goal_status = GoalStatus::Paused;
        config.goal_state = new_shared_goal_state();
        config.turn_wall_clock = invocation.deadline.map_or(Duration::MAX, |deadline| {
            deadline.saturating_duration_since(tokio::time::Instant::now())
        });
        config.compaction.runtime_cost_owner = caller.runtime_owner.clone();
        let system_prompt = crate::rlm::prompt::captured_rlm_prompt(
            &caller.system,
            invocation.mode,
            invocation.task_instructions.as_deref(),
        )?;
        let mut kernel = None;
        if invocation.mode != RlmMode::Completion {
            let path = tempfile::TempPath::try_from_path(codewhale_repl::write_context_file(
                &invocation.prompt,
            )?)?;
            let cancel = caller
                .context
                .cancel_token
                .as_ref()
                .expect("validated originating cancellation");
            let startup = tokio::select! {
                biased;
                () = cancel.cancelled() => Err("RLM Python startup cancelled by its originating turn".to_string()),
                () = wait_for_deadline(invocation.deadline) => Err("RLM Python startup exhausted its original wall-clock deadline".to_string()),
                result = codewhale_repl::PythonRuntime::spawn_with_context(&path) => result,
            };
            match startup {
                Ok(runtime) => {
                    // The successfully bootstrapped runtime owns this same
                    // path. Before that handoff, TempPath cleans every Drop.
                    let _context_path = path.keep()?;
                    kernel = Some(runtime);
                }
                Err(error) => return Err(anyhow!("RLM Python startup failed: {error}")),
            }
        }
        let api = (*caller.route.config).clone();
        let setup = RlmHostSetup {
            caller: Arc::clone(&caller),
            invocation,
            system_prompt,
        };
        let (mut engine, mut handle) =
            Self::new_admitted(config, &api, Some(EngineHostSetup::Rlm(setup)));
        engine.install_validated_runtime_route(caller.route.clone());
        engine.model_client = Some(Arc::clone(&caller.client));
        engine.model_client_injected = caller.injected;
        handle.client_preflight_required = false;
        engine.repl_kernel = kernel;
        engine.extension_prompt_block = caller.extension_prompt_block.clone();
        engine.constitution_block = caller.constitution_block.clone();
        Ok((engine, handle))
    }

    fn rlm_turn_spec(&self) -> Result<TurnSpec> {
        let state = self
            .rlm_host
            .as_ref()
            .ok_or_else(|| anyhow!("not an admitted RLM host"))?;
        state.caller.validate_live().map_err(anyhow::Error::new)?;
        let max_output_tokens = state
            .max_tokens
            .map(|limit| {
                std::num::NonZeroU32::new(limit)
                    .ok_or_else(|| anyhow!("RLM max_tokens must be greater than zero"))
            })
            .transpose()?;
        let content = if state.mode == RlmMode::Completion {
            state.prompt.clone()
        } else {
            crate::rlm::turn::metadata_text(&state.prompt, 0, None, None)
        };
        Ok(TurnSpec {
            profile_constitution: None,
            content,
            images: Vec::new(),
            mode: state.caller.authority.mode,
            route: Box::new(state.caller.route.clone().into_resolved()),
            compaction: Box::new(self.config.compaction.clone()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: GoalStatus::Paused,
            reasoning_effort: self.session.reasoning_effort.clone(),
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: state.caller.authority.allow_shell,
            trust_mode: state.caller.authority.trust_mode,
            auto_approve: state.caller.authority.auto_approve,
            approval_mode: state.caller.authority.approval_mode,
            translation_enabled: false,
            allowed_tools: Some(if state.mode == RlmMode::Completion {
                Vec::new()
            } else {
                vec![tool_catalog::CODE_EXECUTION_TOOL_NAME.to_string()]
            }),
            dynamic_tools: Vec::new(),
            hook_executor: self.config.hook_executor.clone(),
            verbosity: None,
            provenance: UserInputProvenance::Runtime,
            submission_id: None,
            max_output_tokens,
        })
    }

    fn rlm_result(&self, outcome: Option<SendMessageOutcome>, duration: Duration) -> RlmTurnResult {
        let state = self.rlm_host.as_ref().expect("admitted RLM state");
        let error = state.error.clone().or_else(|| match outcome {
            Some(SendMessageOutcome::Finished {
                status: TurnOutcomeStatus::Completed,
                error,
            }) => error,
            Some(SendMessageOutcome::Finished { error, .. })
            | Some(SendMessageOutcome::NotStarted { error }) => {
                error.or_else(|| Some("RLM Core turn did not complete".into()))
            }
            None => Some(
                "RLM evaluation cancelled or exhausted its original wall-clock deadline".into(),
            ),
        });
        RlmTurnResult {
            answer: state
                .final_answer
                .clone()
                .unwrap_or_else(|| state.last_response.clone()),
            iterations: state.model_rounds,
            duration,
            error,
            usage: Usage::default(),
            routed_usage: Vec::new(),
            routed_usage_drop_records: Vec::new(),
            routed_usage_dropped_records: 0,
            termination: state.termination.unwrap_or(RlmTermination::Error),
            trace: state.trace.clone(),
            total_rpcs: state.total_rpcs,
        }
        .require_answer_or_error()
    }
}

impl EngineHostSetup {
    pub(super) fn initial_authority(&self, api: &Config) -> LiveRuntimeAuthority {
        self.context()
            .live_posture
            .as_ref()
            .map(LivePosture::read)
            .unwrap_or_else(|| match self {
                Self::Child(setup) => {
                    let runtime = &setup.authority.runtime;
                    let context = &runtime.context;
                    LiveRuntimeAuthority::from_fields(
                        runtime.parent_mode,
                        runtime.allow_shell,
                        context.trust_mode,
                        context.auto_approve,
                        context.approval_mode,
                        api.sandbox_mode.clone(),
                    )
                }
                Self::Rlm(setup) => LiveRuntimeAuthority::from_fields(
                    setup.caller.authority.mode,
                    setup.caller.authority.allow_shell,
                    setup.caller.authority.trust_mode,
                    setup.caller.authority.auto_approve,
                    setup.caller.authority.approval_mode,
                    api.sandbox_mode.clone(),
                ),
            })
    }
    pub(super) fn cancel_token(&self) -> &CancellationToken {
        match self {
            Self::Child(setup) => &setup.authority.runtime.cancel_token,
            Self::Rlm(setup) => setup
                .caller
                .context
                .cancel_token
                .as_ref()
                .expect("captured RLM cancel"),
        }
    }
    pub(super) fn context(&self) -> &ToolContext {
        match self {
            Self::Child(setup) => &setup.authority.runtime.context,
            Self::Rlm(setup) => &setup.caller.context,
        }
    }
    pub(super) fn subagent_manager(&self) -> &SharedSubAgentManager {
        match self {
            Self::Child(setup) => &setup.authority.runtime.manager,
            Self::Rlm(setup) => &setup.caller.subagent_manager,
        }
    }
    pub(super) fn owner_agent_id(&self) -> Option<String> {
        match self {
            Self::Child(setup) => Some(setup.authority.owner_agent_id.clone()),
            Self::Rlm(setup) => setup.caller.context.owner_agent_id.clone(),
        }
    }
    pub(super) fn client(&self) -> &CodewhaleClient {
        match self {
            Self::Child(setup) => &setup.authority.runtime.client,
            Self::Rlm(setup) => &setup.caller.route.client,
        }
    }
    pub(super) fn system_prompt(&self) -> &SystemPrompt {
        match self {
            Self::Child(setup) => &setup.system_prompt,
            Self::Rlm(setup) => &setup.system_prompt,
        }
    }
    pub(super) fn review_policy(&self) -> Arc<AutoReviewPolicy> {
        match self {
            Self::Child(setup) => Arc::clone(&setup.authority.runtime.auto_review_policy),
            Self::Rlm(setup) => Arc::clone(&setup.caller.review_policy),
        }
    }
    pub(super) fn approval_store(&self) -> Result<ApprovalReceiptStore, String> {
        match self {
            Self::Child(setup) => setup.authority.approval_receipt_store(),
            Self::Rlm(setup) => setup.caller.approval_store.clone(),
        }
    }
}

async fn forward_rlm_event(
    events: Option<&mpsc::Sender<Event>>,
    event: Event,
    depth: u32,
    cancel: &CancellationToken,
    deadline: Option<tokio::time::Instant>,
) {
    if let Some(events) = events
        && let Some(message) = crate::rlm::bridge::nested_rlm_status_line(event, depth)
    {
        tokio::select! {
            biased;
            () = cancel.cancelled() => {},
            _ = wait_for_deadline(deadline) => {},
            _ = events.send(Event::status(message)) => {},
        }
    }
}

impl Engine {
    pub(super) fn rlm_tool_build(
        &self,
        authority: &TurnAuthority,
        _route: &TurnRouteContext,
    ) -> TurnToolBuild {
        let state = self.rlm_host.as_ref().expect("admitted RLM host");
        let mut context = state.caller.context.clone();
        context.rlm_caller = Some(Arc::clone(&state.caller));
        context.cancel_token = Some(self.cancel_token.clone());
        context.turn_deadline = state.deadline;
        let registry = ToolRegistryBuilder::new().build(context);
        let mut catalog = Vec::new();
        if state.mode != RlmMode::Completion {
            tool_catalog::ensure_advanced_tooling(
                &mut catalog,
                authority.mode,
                &HashSet::new(),
                tool_catalog::ToolMode::Direct,
            );
            catalog.retain(|tool| tool.name == tool_catalog::CODE_EXECUTION_TOOL_NAME);
        }
        let allowed = Some(catalog.iter().map(|tool| tool.name.clone()).collect());
        TurnToolBuild {
            surface: ToolSurfacePolicy::new(
                registry,
                Some(catalog),
                authority.mode,
                &HashSet::new(),
                &[],
                false,
                allowed,
                None,
                self.config.max_tool_calls,
                tool_catalog::ToolMode::Direct,
            ),
            mcp_tool_names: Vec::new(),
            mcp: McpToolState::Disabled,
            subagent_runtime_model: None,
            mailbox: None,
            plugin_tool_names: HashSet::new(),
        }
    }
}

/// One actual producer dispatch, reserved before entering the client. Its Drop
/// records unknown execution synchronously; no future is needed to retain it.
pub(super) struct RlmDispatchedRequest {
    caller: Arc<CapturedRlmCaller>,
    usage: RlmUsageAccumulator,
    reservation: RlmUsageReservation,
    source: String,
    route: crate::cost_status::EffectiveRouteEnvelope,
    settled: Option<(
        Usage,
        Option<u64>,
        crate::cost_status::RuntimeUsageMissingReason,
    )>,
    projected: bool,
    refused: bool,
}
impl RlmDispatchedRequest {
    pub(super) async fn settle_open_error(&mut self, error: &anyhow::Error) {
        if crate::tools::subagent::engine::provider_request_refusal_proven(error) {
            self.cancel_unexecuted();
        } else {
            self.settle(&Usage::default(), false).await;
        }
    }
    pub(super) fn cancel_unexecuted(&mut self) {
        self.usage.cancel_sync(
            self.reservation,
            false,
            crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown,
        );
        self.refused = true;
    }
    pub(super) async fn settle(&mut self, usage: &Usage, complete: bool) {
        if self.settled.is_some() || self.refused {
            return;
        }
        let reason = if complete {
            crate::cost_status::RuntimeUsageMissingReason::SuccessWithoutUsage
        } else {
            crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown
        };
        // Publication precedes any cancellable worker projection. Drop retains
        // this exact settlement and never synthesizes a second charge.
        let priced = self
            .caller
            .publish_response(&self.source, &self.route, usage, reason);
        self.settled = Some((usage.clone(), priced, reason));
        if usage_has_reported_data(usage) {
            self.usage.complete(self.reservation, usage).await;
        } else {
            self.usage.cancel_sync(self.reservation, true, reason);
        }
        if let Some(child) = &self.caller.child_accounting {
            child
                .project_settled(&self.source, &self.route, usage, priced, reason)
                .await;
        }
        self.projected = true;
    }
}
impl Drop for RlmDispatchedRequest {
    fn drop(&mut self) {
        if self.refused || self.projected {
            return;
        }
        let (usage, priced, reason) = self.settled.clone().unwrap_or_else(|| {
            let reason = crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown;
            let usage = Usage::default();
            let priced = self
                .caller
                .publish_response(&self.source, &self.route, &usage, reason);
            self.usage.cancel_sync(self.reservation, true, reason);
            (usage, priced, reason)
        });
        if let Some(child) = &self.caller.child_accounting {
            child.recover_settled(
                &self.caller.scheduler,
                self.source.clone(),
                self.route.clone(),
                usage,
                priced,
                reason,
            );
        }
    }
}
impl CapturedRlmCaller {
    fn publish_response(
        &self,
        source: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
        usage: &Usage,
        reason: crate::cost_status::RuntimeUsageMissingReason,
    ) -> Option<u64> {
        if let Some(child) = &self.child_accounting {
            return child.publish(source, route, usage, reason);
        }
        if usage_has_reported_data(usage) {
            if let Some(owner) = self.runtime_owner.as_deref() {
                crate::cost_status::report_effective_route_for_runtime(
                    self.origin_scope,
                    Some(owner),
                    source,
                    route,
                    usage,
                );
            } else {
                crate::cost_status::report_effective_route_for_interactive_origin(
                    self.origin_scope,
                    &self.origin_session,
                    &self.origin_turn,
                    source,
                    route,
                    usage,
                );
            }
        } else if let Some(owner) = self.runtime_owner.as_deref() {
            crate::cost_status::report_missing_runtime_usage(
                self.origin_scope,
                Some(owner),
                source,
                route,
                reason,
            );
        } else {
            crate::cost_status::report_missing_usage_for_interactive_origin(
                self.origin_scope,
                &self.origin_session,
                &self.origin_turn,
                source,
                route,
                reason,
            );
        }
        None
    }
}
impl Engine {
    pub(super) async fn rlm_provider_request(
        &mut self,
        client: &SharedModelClient,
        request: &codewhale_models::MessageRequest,
    ) -> Result<Option<RlmDispatchedRequest>, String> {
        let Some(state) = self.rlm_host.as_mut() else {
            return Ok(None);
        };
        state
            .caller
            .validate_live()
            .map_err(|error| error.to_string())?;
        let route = client.effective_route_envelope(&request.model, chrono::Utc::now());
        let reservation = state.usage.reserve(route.clone()).await?;
        let source = state
            .usage
            .source_id(reservation)
            .await
            .ok_or_else(|| "RLM dispatch reservation disappeared".to_string())?;
        state.model_rounds = state.model_rounds.saturating_add(1);
        Ok(Some(RlmDispatchedRequest {
            caller: Arc::clone(&state.caller),
            usage: state.usage.clone(),
            reservation,
            source,
            route,
            settled: None,
            projected: false,
            refused: false,
        }))
    }
}

#[cfg(test)]
impl CapturedRlmCaller {
    pub(crate) fn fixture_origin_cancel(&self) -> CancellationToken {
        self.context
            .cancel_token
            .as_ref()
            .expect("captured originating token")
            .clone()
    }
    pub(crate) fn hold_fixture_workspace(&mut self, workspace: Arc<tempfile::TempDir>) {
        self._fixture_workspace = Some(workspace);
    }
}

//! Private captured child configuration for the canonical Engine.
use super::*;
use crate::tools::subagent::engine::ChildAuthority;
use anyhow::anyhow;

pub(super) struct ChildHostSetup {
    pub authority: Arc<ChildAuthority>,
    pub system_prompt: SystemPrompt,
    pub attachment: Option<crate::extension_host::HostAttachment>,
}
pub(super) struct ChildHostState {
    pub authority: Arc<ChildAuthority>,
    pub system_prompt: SystemPrompt,
    job: Option<Arc<crate::tools::subagent::engine::ChildJob>>,
    handle: Option<EngineHandle>,
    pub(super) report: Option<crate::tools::subagent::budget_handback::ReportAdmission>,
    route_runtime: crate::tools::subagent::SubAgentRuntime,
    pub(super) fork_cache_checked: bool,
    pub(super) fork_trial_first_response_logged: bool,
    pub(super) fork_cache_inherited:
        Option<crate::prompt_zones::LiveHeaderSnapshot<crate::route_receipt::TurnRouteReceipt>>,
    pub(super) fork_cache_prefix: Vec<String>,
    replacements_tried: usize,
    pin_fallback_used: bool,
    pending_route: Option<(
        crate::tools::subagent::SubAgentRuntime,
        ValidatedRuntimeRoute,
    )>,
    pub(super) request_stop_reason: Option<&'static str>,
}
impl From<ChildHostSetup> for ChildHostState {
    fn from(setup: ChildHostSetup) -> Self {
        let route_runtime = setup.authority.runtime.clone();
        Self {
            route_runtime,
            fork_cache_checked: false,
            fork_trial_first_response_logged: false,
            fork_cache_inherited: None,
            fork_cache_prefix: Vec::new(),
            replacements_tried: 0,
            pin_fallback_used: false,
            pending_route: None,
            request_stop_reason: None,
            authority: setup.authority,
            system_prompt: setup.system_prompt,
            job: None,
            handle: None,
            report: None,
        }
    }
}
impl Engine {
    pub(super) fn new_tool_execution_id(&self) -> String {
        self.child_host.as_ref().map_or_else(
            || uuid::Uuid::new_v4().to_string(),
            |child| child.authority.new_execution_id(),
        )
    }

    pub(crate) fn new_child_admitted(
        mut config: EngineConfig,
        api_config: &Config,
        authority: Arc<ChildAuthority>,
        system_prompt: SystemPrompt,
        attachment: Option<crate::extension_host::HostAttachment>,
    ) -> Result<(Self, EngineHandle)> {
        anyhow::ensure!(
            config.workspace == authority.runtime.context.workspace,
            "child Engine workspace differs from its captured caller"
        );
        anyhow::ensure!(
            !authority.runtime.context.state_namespace.trim().is_empty(),
            "child Engine needs its captured root session namespace"
        );
        config.session_id = Some(authority.runtime.context.state_namespace.clone());
        config.plugin_registry = authority.runtime.context.plugin_registry.clone();
        config.runtime_services = authority.runtime.context.runtime.clone();
        config.todos = authority.runtime.todos.clone();
        config.max_spawn_depth = authority.runtime.max_spawn_depth;
        config.allow_shell = authority.runtime.allow_shell;
        config.trust_mode = authority.runtime.context.trust_mode;
        config.network_policy = authority.runtime.context.network_policy.clone();
        if let Some(skills_dir) = authority.runtime.context.skills_dir.as_ref() {
            config.skills_dir = skills_dir.clone();
        }
        config.skills_discovery_mode = authority.runtime.context.skills_discovery_mode;
        config.features = authority.runtime.context.features.clone();
        config.snapshots_enabled = false;
        config.memory_enabled = false;
        config.terminal_chrome_enabled = false;
        config.advisor_config = crate::tools::subagent::AdvisorConfig::disabled();
        config.goal_objective = None;
        config.goal_status = GoalStatus::Paused;
        config.exec_policy_engine = api_config.exec_policy_engine.clone();
        let (mut engine, handle) = Self::new_admitted(
            config,
            api_config,
            Some(EngineHostSetup::Child(ChildHostSetup {
                authority,
                system_prompt,
                attachment,
            })),
        );
        engine
            .child_host
            .as_mut()
            .expect("captured child setup")
            .handle = Some(handle.clone());
        // The actual caller supplied this frozen concrete client. Route
        // identity is still resolved and installed by the same Core seam.
        engine.model_client_injected = true;
        let posture = engine.runtime_authority_snapshot();
        engine.apply_runtime_mode_policy(&TurnAuthority::from_effective_fields(
            posture.mode,
            posture.allow_shell,
            posture.trust_mode,
            posture.auto_approve,
            posture.approval_mode,
        ));
        Ok((engine, handle))
    }
}

impl Engine {
    pub(crate) fn install_child_job(
        &mut self,
        job: Arc<crate::tools::subagent::engine::ChildJob>,
        seed: Vec<Message>,
    ) -> Result<()> {
        let state = self
            .child_host
            .as_mut()
            .ok_or_else(|| anyhow!("not a captured child Engine"))?;
        anyhow::ensure!(
            Arc::ptr_eq(&state.authority, &job.authority),
            "child job changed captured authority"
        );
        anyhow::ensure!(state.job.is_none(), "child assignment is already installed");
        let system = state.system_prompt.clone();
        state.job = Some(job);
        self.restore_session_history(seed, Some(system), true);
        Ok(())
    }

    pub(crate) fn child_turn_spec(
        &self,
        content: String,
        allowed_tools: Option<Vec<String>>,
    ) -> Result<TurnSpec> {
        let state = self
            .child_host
            .as_ref()
            .ok_or_else(|| anyhow!("not a captured child Engine"))?;
        state
            .authority
            .validate_context(&state.authority.context())?;
        let posture = self.runtime_authority_snapshot();
        Ok(TurnSpec {
            profile_constitution: None,
            content,
            images: Vec::new(),
            mode: posture.mode,
            route: Box::new(self.current_runtime_route().map_err(anyhow::Error::msg)?),
            compaction: Box::new(self.config.compaction.clone()),
            initial_routed_usage: Box::default(),
            goal_objective: None,
            goal_token_budget: None,
            goal_status: GoalStatus::Paused,
            reasoning_effort: state.route_runtime.reasoning_effort.clone(),
            reasoning_effort_auto: false,
            auto_model: false,
            allow_shell: state.authority.runtime.allow_shell && posture.allow_shell,
            trust_mode: state.authority.runtime.context.trust_mode,
            auto_approve: posture.auto_approve,
            approval_mode: posture.approval_mode,
            translation_enabled: false,
            allowed_tools,
            dynamic_tools: Vec::new(),
            hook_executor: self.config.hook_executor.clone(),
            verbosity: None,
            provenance: UserInputProvenance::Runtime,
            submission_id: None,
            max_output_tokens: None,
        })
    }

    pub(super) fn child_job(&self) -> Option<Arc<crate::tools::subagent::engine::ChildJob>> {
        self.child_host.as_ref().and_then(|state| state.job.clone())
    }
    pub(super) fn child_request_protocol(&self) -> Option<codewhale_config::provider::WireFormat> {
        self.child_host
            .as_ref()
            .map(|state| state.route_runtime.client.wire_format())
    }
    pub(super) fn child_report(&self) -> bool {
        self.child_host
            .as_ref()
            .is_some_and(|state| state.report.is_some())
    }

    /// Executes a queued assignment or its single bounded reporting turn.
    /// Both go through handle_send_message -> run_turn, the same Session and
    /// Core approval inbox. The next report is reserved on the existing FIFO.
    pub(super) async fn handle_child_send_message(
        &mut self,
        spec: TurnSpec,
    ) -> Result<Option<crate::tools::subagent::SubAgentResult>> {
        use crate::tools::subagent::SubAgentStatus;
        let job = self
            .child_job()
            .ok_or_else(|| anyhow!("child Engine has no admitted assignment"))?;
        self.child_host
            .as_mut()
            .expect("captured child")
            .request_stop_reason = None;
        let reporting = self.child_report();
        let replay_content = spec.content.clone();
        let replay_scope = spec.allowed_tools.clone();
        let deadline = if reporting {
            self.child_host
                .as_ref()
                .and_then(|state| state.report.as_ref())
                .map(|report| report.deadline)
        } else {
            job.work_deadline
        };
        let admitted = self
            .admitted_turn_control
            .as_ref()
            .map(|control| control.cancel.clone());
        // This is the exact queued turn token, never the parent's or a later
        // report's token. Dropping the timer aborts it after settlement.
        let timer = deadline.zip(admitted).map(|(deadline, cancel)| {
            let job = job.clone();
            turn_heartbeat::AbortOnDrop(tokio::spawn(async move {
                tokio::time::sleep_until(deadline.into()).await;
                if !reporting {
                    job.stop_for_budget("child wall-time work budget exhausted");
                }
                cancel.cancel();
            }))
        });
        let outcome = Box::pin(self.handle_send_message(spec)).await;
        // Capture expiry at the actual Core outcome. Later checkpoint/ledger
        // contention cannot relabel an already-settled provider refusal.
        let report_deadline_expired =
            reporting && deadline.is_some_and(|deadline| Instant::now() >= deadline);
        drop(timer);
        job.project(
            &self.session.messages,
            job.steps(),
            self.session.system_prompt.as_ref(),
        )
        .await?;
        if job.authority.runtime.cancel_token.is_cancelled() {
            return job
                .finish(
                    &self.session.messages,
                    job.steps(),
                    SubAgentStatus::Cancelled,
                    None,
                    None,
                )
                .await
                .map(Some);
        }
        let pending_route = self
            .child_host
            .as_mut()
            .and_then(|state| state.pending_route.take());
        if let Some((runtime, route)) = pending_route {
            // The old request failed before a response. A fresh admitted turn
            // reuses the exact original task on this same Engine/FIFO; it rebuilds
            // tools, Native prompt sections and Core dispatch receipts together.
            let client = runtime.client.clone();
            self.install_validated_runtime_route(route);
            self.model_client = Some(Arc::new(client));
            self.session.reasoning_effort = runtime.reasoning_effort.clone();
            self.session.reasoning_effort_auto = runtime.reasoning_effort_auto;
            job.installed_replacement(&runtime.model);
            self.child_host
                .as_mut()
                .expect("captured child")
                .route_runtime = runtime;
            let spec = self.child_turn_spec(replay_content, replay_scope)?;
            self.child_host
                .as_ref()
                .expect("captured child")
                .handle
                .as_ref()
                .expect("same Engine handle")
                .send(Op::SendMessage(spec))
                .await?;
            return Ok(None);
        }
        let (status, error) = match outcome {
            SendMessageOutcome::Finished { status, error } => (status, error),
            SendMessageOutcome::NotStarted { error } => (TurnOutcomeStatus::Failed, error),
        };
        let text = self
            .session
            .messages
            .iter()
            .rev()
            .find(|message| {
                message.role == codewhale_models::Role::Assistant
                    || (matches!(
                        status,
                        TurnOutcomeStatus::Interrupted | TurnOutcomeStatus::Failed
                    ) && message.role == codewhale_models::Role::InterruptedAssistant)
            })
            .map(|message| {
                message
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text { text, .. } if !text.trim().is_empty() => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.trim().is_empty());
        if let Some(cause) = job.budget_reason() {
            if !reporting {
                match crate::tools::subagent::budget_handback::admit_report(
                    &job,
                    &self.session.messages,
                    &cause,
                    self.model_client
                        .as_ref()
                        .expect("installed selected client")
                        .effective_max_output_tokens(&self.session.model),
                )
                .await
                {
                    Ok(report) => {
                        let content = report
                            .messages
                            .iter()
                            .flat_map(|message| &message.content)
                            .filter_map(|block| match block {
                                ContentBlock::Text { text, .. } => Some(text.as_str()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let mut spec = self.child_turn_spec(content, Some(Vec::new()))?;
                        spec.allow_shell = false;
                        spec.max_output_tokens = std::num::NonZeroU32::new(report.output_tokens);
                        self.config.max_steps = 1;
                        let state = self.child_host.as_mut().expect("captured child");
                        state.report = Some(report);
                        // A fresh queued scope cannot erase parent cancellation:
                        // it is rechecked by the transport and every dispatch.
                        state
                            .handle
                            .as_ref()
                            .expect("same Core handle")
                            .send(Op::SendMessage(spec))
                            .await?;
                        return Ok(None);
                    }
                    Err(reason) => {
                        let partial =
                            crate::tools::subagent::budget_handback::fallback_partial_text(
                                &self.session.messages,
                            );
                        return job
                            .finish(
                                &self.session.messages,
                                job.steps(),
                                SubAgentStatus::BudgetExhausted,
                                Some(format!("{partial}\n\n{reason}")),
                                Some(&cause),
                            )
                            .await
                            .map(Some);
                    }
                }
            }
            let report = if status == TurnOutcomeStatus::Completed && error.is_none() {
                text
            } else {
                None
            };
            let mut partial = report.unwrap_or_else(|| {
                crate::tools::subagent::budget_handback::fallback_partial_text(
                    &self.session.messages,
                )
            });
            let mut receipt = if status == TurnOutcomeStatus::Completed && error.is_none() {
                "Host budget hand-back receipt: one completed bounded report; assignment remains incomplete.".to_string()
            } else if report_deadline_expired {
                "Host budget hand-back receipt: report deadline expired; recorded work is preserved.".to_string()
            } else {
                format!(
                    "Host budget hand-back receipt: provider call failed or report did not finish: {}. Recorded work is preserved.",
                    error.as_deref().unwrap_or("Core report interrupted")
                )
            };
            if job
                .authority
                .runtime
                .manager
                .read()
                .await
                .get_worker_record(&job.authority.owner_agent_id)
                .is_some_and(|record| record.has_unreported_usage)
            {
                receipt.push_str(" Measured usage is only a subtotal, not a zero-cost report; unreported provider usage remains unknown.");
            }
            partial.push_str(&format!("\n\n{receipt}"));
            // Record the host's actual outcome alongside this same canonical
            // Session, before finish saves/projects the result checkpoint.
            self.add_session_message(
                self.runtime_text_message_with_turn_metadata(receipt, UserInputProvenance::Runtime),
            )
            .await;
            return job
                .finish(
                    &self.session.messages,
                    job.steps(),
                    SubAgentStatus::BudgetExhausted,
                    Some(partial),
                    Some(&cause),
                )
                .await
                .map(Some);
        }
        let child_status = match (status, error.as_ref(), text.as_ref()) {
            (TurnOutcomeStatus::Completed, None, Some(_)) => SubAgentStatus::Completed,
            (TurnOutcomeStatus::Interrupted, _, _) => SubAgentStatus::Interrupted(
                error
                    .clone()
                    .unwrap_or_else(|| "Core child turn interrupted".into()),
            ),
            (TurnOutcomeStatus::Failed, Some(reason), _)
                if reason == super::dispatch::FLEET_NO_PROGRESS_STOP =>
            {
                // Core's terminal no-progress report stays Failed even after
                // earlier steps; checkpoint preservation cannot erase its cause.
                SubAgentStatus::Failed(reason.clone())
            }
            (TurnOutcomeStatus::Failed, _, _) if job.steps() > 1 => {
                SubAgentStatus::Interrupted(error.clone().unwrap_or_else(|| {
                    "child stopped after prior work; checkpoint preserved".into()
                }))
            }
            _ => SubAgentStatus::Failed(
                error
                    .clone()
                    .unwrap_or_else(|| "child stopped without a final summary".into()),
            ),
        };
        job.finish(
            &self.session.messages,
            job.steps(),
            child_status,
            text,
            self.child_host
                .as_ref()
                .and_then(|state| state.request_stop_reason)
                .or(error.as_deref()),
        )
        .await
        .map(Some)
    }

    pub(super) fn clear_child_pending_route(&mut self) {
        if let Some(state) = self.child_host.as_mut() {
            state.pending_route = None;
        }
    }

    pub(super) async fn retract_child_unsent_message(
        &mut self,
        mark: crate::core::turn::UnansweredUserMessage,
    ) -> Result<bool> {
        if !self.can_retract_unanswered_user_message(mark) {
            return Ok(false);
        }
        if let Some(job) = self.child_job() {
            let old = self.session.messages.to_vec();
            // Preserve durable dispatch/history evidence before changing the
            // live projection. The one actor owns both sides of this await.
            if let Err(error) = job.before_replace(&old, &old[..mark.len - 1]).await {
                self.child_host
                    .as_mut()
                    .expect("captured child")
                    .pending_route = None;
                return Err(error);
            }
        }
        Ok(self.retract_unanswered_user_message(mark))
    }

    pub(super) async fn admit_child_first_request_replacement(
        &mut self,
        error: &anyhow::Error,
    ) -> Result<bool> {
        let Some(job) = self.child_job() else {
            return Ok(false);
        };
        if self.child_report() || !job.can_replace_first_request() {
            return Ok(false);
        }
        let state = self.child_host.as_mut().expect("captured child");
        let Some((next, source, note)) =
            crate::tools::subagent::engine::approved_first_request_replacement(
                &job,
                &state.route_runtime,
                &mut state.replacements_tried,
                &mut state.pin_fallback_used,
                error,
            )?
        else {
            return Ok(false);
        };
        let api = next
            .api_config
            .as_deref()
            .ok_or_else(|| anyhow!("approved child replacement has no captured config"))?;
        let resolved = crate::route_runtime::resolve_runtime_route_for_identity(
            api,
            next.client.admitted_provider_identity(),
            Some(&next.model),
        )
        .map_err(anyhow::Error::msg)?;
        let mut route = resolved.validate().map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            next.client.admitted_provider_identity() == &route.identity
                && route.candidate.endpoint().base_url == next.client.base_url(),
            "approved child replacement client differs from its exact route"
        );
        route.client = next.client.clone();
        job.authority.validate_context(&job.authority.context())?;
        job.record_route_replacement(&state.route_runtime, &next, source, note.clone(), error)
            .await;
        state.pending_route = Some((next, route));
        let _ = self.send_event(Event::status(note)).await;
        Ok(true)
    }

    pub(crate) async fn run_child(self) -> Result<crate::tools::subagent::SubAgentResult> {
        self.run_owned().await.ok_or_else(|| {
            anyhow!("child Core actor stopped without a terminal assignment receipt")
        })?
    }
}

/// Test observer of Core's actual surface policy and activation cache. It has
/// no executor, permission policy or request producer of its own.
#[cfg(test)]
pub(crate) struct ChildSurfaceProbe {
    pub(super) policy: ToolSurfacePolicy,
    pub(super) cache: crate::core::session::ToolActivationCache,
}
#[cfg(test)]
impl ChildSurfaceProbe {
    pub(crate) fn new(catalog: Vec<codewhale_models::Tool>, warm: &[String]) -> Self {
        let policy = ToolSurfacePolicy::new(
            crate::tools::ToolRegistry::new(ToolContext::for_empty_registry()),
            Some(catalog),
            AppMode::Agent,
            &HashSet::new(),
            &[],
            false,
            None,
            None,
            None,
            tool_catalog::ToolMode::Direct,
        );
        let mut probe = Self {
            policy,
            cache: Default::default(),
        };
        let activated = probe.cache.activate(&probe.policy.catalog, warm);
        probe.policy.active_names.extend(activated.admitted);
        probe
    }
    pub(crate) fn catalog(&self) -> &[codewhale_models::Tool] {
        &self.policy.catalog
    }
    pub(crate) fn catalog_mut(&mut self) -> &mut Vec<codewhale_models::Tool> {
        &mut self.policy.catalog
    }
    pub(crate) fn active_names(&self) -> &HashSet<String> {
        &self.policy.active_names
    }
    pub(crate) fn request_tools(
        &mut self,
        catalog: Vec<codewhale_models::Tool>,
        strict: bool,
    ) -> Vec<codewhale_models::Tool> {
        self.policy.catalog = catalog;
        self.cache.revalidate(&self.policy.catalog);
        self.policy.active_names = tool_catalog::initial_active_tools(&self.policy.catalog);
        self.policy
            .active_names
            .extend(self.cache.names().map(str::to_owned));
        tool_catalog::active_tools_for_request(
            &self.policy.catalog,
            &self.policy.active_names,
            strict,
        )
        .unwrap_or_default()
    }
    /// Pure cache admission probe; execution still runs Core's planner below.
    pub(crate) fn hydrate(
        &mut self,
        name: &str,
        input: &serde_json::Value,
    ) -> Result<Option<String>> {
        let activation = self
            .cache
            .activate(&self.policy.catalog, &[name.to_owned()]);
        tool_catalog::remove_evicted_cache_activations(
            &self.policy.catalog,
            &mut self.policy.active_names,
            activation.evicted,
        );
        self.policy
            .active_names
            .extend(activation.admitted.iter().cloned());
        anyhow::ensure!(
            activation.admitted.iter().any(|admitted| admitted == name),
            "tool was not admitted by Core's bounded activation cache"
        );
        let definition = self
            .policy
            .catalog
            .iter()
            .find(|tool| tool.name == name)
            .ok_or_else(|| anyhow!("tool left the Core catalog"))?;
        Ok(
            (!tool_catalog::deferred_first_call_matches_schema(definition, input)).then(|| {
                tool_catalog::deferred_tool_schema_hydration_result(definition, input).content
            }),
        )
    }
}
#[cfg(test)]
pub(crate) struct ChildProbeCall {
    pub(crate) id: String,
    pub(crate) execution_id: String,
    pub(crate) name: String,
    pub(crate) input: serde_json::Value,
}

#[cfg(test)]
impl Engine {
    /// Direct tests use the real admitted Core planner/executor and approval
    /// inbox. The loop below only observes/relays those Core events.
    pub(crate) async fn probe_child_call(
        authority: Arc<ChildAuthority>,
        registry: crate::tools::ToolRegistry,
        surface: &mut ChildSurfaceProbe,
        call: ChildProbeCall,
    ) -> Result<crate::tools::spec::RichToolResult> {
        let runtime = authority.runtime.clone();
        let owner = authority.owner_agent_id.clone();
        let api = runtime
            .api_config
            .as_deref()
            .ok_or_else(|| anyhow!("fixture needs its explicit captured Config"))?;
        let (mut engine, handle) = Self::new_child_admitted(
            EngineConfig {
                workspace: runtime.context.workspace.clone(),
                model: runtime.model.clone(),
                max_steps: 1,
                auto_review_policy: runtime.auto_review_policy.as_ref().clone(),
                ..Default::default()
            },
            api,
            authority,
            SystemPrompt::Text("Core child batch probe".into()),
            None,
        )?;
        surface.policy.registry = registry;
        let mut events = handle.rx_event.write().await;
        let execute = engine.probe_child_tool_batch(surface, call);
        tokio::pin!(execute);
        let mut approvals = futures_util::stream::FuturesUnordered::new();
        use futures_util::StreamExt;
        let answer = loop {
            tokio::select! {
                biased;
                result = &mut execute => break result,
                () = runtime.cancel_token.cancelled(), if !handle.is_cancelled() => handle.cancel(),
                Some((id, decision)) = approvals.next(), if !approvals.is_empty() => {
                    if runtime.cancel_token.is_cancelled() || handle.is_cancelled() {
                        handle.cancel();
                    } else {
                        match decision {
                            Ok(crate::tools::subagent::ChildApprovalOutcome::Approved { by }) => handle.approve_tool_call_by(id, by).await?,
                            Ok(crate::tools::subagent::ChildApprovalOutcome::Denied { by }) => handle.deny_tool_call_by(id, by).await?,
                            _ => handle.deny_tool_call_unavailable(id).await?,
                        }
                    }
                }
                event = events.recv() => {
                    let Some(event) = event else { break Err(anyhow!("Core probe event stream closed")) };
                    match &event {
                        Event::ApprovalRequired { id, tool_name, description, .. } => {
                            let (_, receiver) = runtime.manager.write().await.register_child_approval(&owner, id, tool_name, description)?;
                            let key = id.clone(); approvals.push(async move { (key, receiver.await) });
                            if let Some(tx) = runtime.event_tx.as_ref().filter(|_| runtime.parent_can_prompt) {
                                let sent = tokio::select! {
                                    biased;
                                    () = runtime.cancel_token.cancelled() => false,
                                    result = tx.send(event.clone()) => result.is_ok(),
                                };
                                if !sent {
                                    runtime.manager.write().await.cancel_child_approval(id);
                                    handle.deny_tool_call_unavailable(id).await?;
                                }
                            } else {
                                runtime.manager.write().await.cancel_child_approval(id);
                                handle.deny_tool_call_unavailable(id).await?;
                            }
                        }
                        Event::ApprovalWithdrawn { id } => {
                            runtime.manager.write().await.cancel_child_approval(id);
                            if let Some(tx) = &runtime.event_tx {
                                tokio::select! {
                                    biased;
                                    () = runtime.cancel_token.cancelled() => { let _ = tx.try_send(event.clone()); },
                                    _ = tx.send(event.clone()) => {},
                                }
                            }
                        }
                        Event::ToolGateDecision { .. } => {
                            crate::tools::subagent::engine::forward_child_gate_observation(
                                &runtime,
                                &owner,
                                event,
                                &handle.captured_turn_cancel(),
                                runtime.context.turn_deadline,
                            ).await;
                        }
                        _ => {}
                    }
                }
            }
        };
        // The canonical batch may settle immediately after enqueueing its
        // last receipt. Observe those exact queued decisions before handback.
        while let Ok(event) = events.try_recv() {
            if let Event::ApprovalWithdrawn { id } = &event {
                runtime.manager.write().await.cancel_child_approval(id);
                if let Some(tx) = &runtime.event_tx {
                    let _ = tx.try_send(event);
                }
            } else if matches!(event, Event::ToolGateDecision { .. }) {
                crate::tools::subagent::engine::forward_child_gate_observation(
                    &runtime,
                    &owner,
                    event,
                    &handle.captured_turn_cancel(),
                    runtime.context.turn_deadline,
                )
                .await;
            }
        }
        let pending = runtime
            .manager
            .read()
            .await
            .pending_requests_for_agent(&owner);
        for pending in pending {
            runtime
                .manager
                .write()
                .await
                .cancel_child_approval(&pending.approval_id);
            if let Some(tx) = &runtime.event_tx {
                let _ = tx.try_send(Event::ApprovalWithdrawn {
                    id: pending.approval_id,
                });
            }
        }
        answer
    }
}

#[cfg(test)]
impl Engine {
    pub(crate) fn probe_child_catalog(
        authority: &ChildAuthority,
        registry: &crate::tools::ToolRegistry,
    ) -> Vec<codewhale_models::Tool> {
        let catalog = tool_catalog::build_model_tool_catalog(
            authority.tools_for_model(registry, &authority.agent_type),
            Vec::new(),
            AppMode::Agent,
            &HashSet::new(),
        );
        let mut copied = crate::tools::ToolRegistry::new(ToolContext::for_empty_registry());
        copied.register_all(registry.all());
        let mut policy = ToolSurfacePolicy::new(
            copied,
            Some(catalog),
            AppMode::Agent,
            &HashSet::new(),
            &[],
            false,
            None,
            None,
            None,
            tool_catalog::ToolMode::Direct,
        );
        Self::narrow_child_surface(authority, &mut policy);
        policy.catalog
    }
}
impl Engine {
    pub(super) fn narrow_child_surface(
        authority: &ChildAuthority,
        surface: &mut ToolSurfacePolicy,
    ) {
        let discoverable = !authority.grant.scope.as_ref().is_some_and(Vec::is_empty);
        let code_mode = surface.registry.context().tool_authority.is_none()
            && authority
                .validate(
                    &surface.registry,
                    tool_catalog::EXECUTE_TOOLS_TOOL_NAME,
                    &serde_json::json!({}),
                )
                .is_ok();
        surface.catalog.retain(|tool| {
            surface.registry.contains(&tool.name)
                || (discoverable && tool_catalog::is_tool_search_tool(&tool.name))
                || (code_mode && tool.name == tool_catalog::EXECUTE_TOOLS_TOOL_NAME)
        });
        surface
            .active_names
            .retain(|name| surface.catalog.iter().any(|tool| &tool.name == name));
        surface.active = tool_catalog::active_tools_for_request(
            &surface.catalog,
            &surface.active_names,
            surface.strict_tool_mode,
        );
    }
}

//! One private phase of the existing Engine turn loop.

use super::*;

impl Engine {
    pub(super) async fn prepare_model_step(
        &mut self,
        turn: &mut TurnContext,
        tool_policy: &ToolSurfacePolicy,
        progress: &mut TurnLoopProgress,
        client: &SharedModelClient,
        inspection_surface: Option<&crate::tool_inspection::ToolSurfaceContext>,
    ) -> PhaseResult<PreparedModelStep> {
        if self.rlm_host.is_some() {
            return self
                .prepare_rlm_model_step(turn, client, inspection_surface)
                .await;
        }
        let strict_tool_mode = tool_policy.strict_tool_mode;

        if self.cancel_token.is_cancelled() {
            let _ = self.send_event(Event::status("Request cancelled")).await;
            return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
        }
        self.turn_heartbeat.enter(
            super::turn_heartbeat::TurnPhase::Preparing,
            None,
            Some(super::turn_heartbeat::PREPARING_PHASE_BOUND),
        );
        self.refresh_boot_mcp_catalog(
            tool_policy,
            &mut progress.tool_catalog,
            &mut progress.active_tool_names,
        )
        .await;
        self.record_mcp_server_instructions(&progress.tool_catalog)
            .await;
        self.record_mcp_configured_servers(tool_policy, &progress.tool_catalog)
            .await;
        self.record_current_constitution().await;
        self.record_current_extension_prompt_contributions().await;

        // R1: the cumulative per-turn wall-clock budget. Checked at the
        // provider-request boundary so a turn that runs out of time stops
        // before authorizing another billable request, and every tool
        // result already produced stays in the transcript. Hitting it is
        // never a clean success — the turn ends `Failed` with the limit
        // named, matching how the step ceiling below reports.
        if let Some(error) = self.turn_wall_clock_exhausted_error() {
            self.post_turn_budget_stop(&error).await;
            return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error)));
        }

        if self.apply_pending_runtime_authority().await {
            if let Some(guard) = progress.fleet_denial_guard.as_mut() {
                guard.reset();
                turn.stop_diagnostics
                    .permission_denial_rounds_without_progress = 0;
            }
            progress.mode = self.current_mode;
        }

        let mut accepted_steer = false;
        while let Some(pending) = self.next_turn_steer() {
            if pending.content.trim().is_empty() {
                // Nothing to deliver; dropping `pending` settles it.
                continue;
            }
            let steer = pending.commit().trim().to_string();
            accepted_steer = true;
            self.session
                .working_set
                .observe_user_message(&steer, &self.session.workspace);
            self.add_session_message(self.user_text_message_with_turn_metadata(steer.clone()))
                .await;
            let _ = self
                .send_event(Event::status(format!(
                    "Steer input accepted: {}",
                    summarize_text(&steer, 120)
                )))
                .await;
        }
        if accepted_steer && let Some(guard) = progress.fleet_denial_guard.as_mut() {
            guard.reset();
            turn.stop_diagnostics
                .permission_denial_rounds_without_progress = 0;
        }

        // Child agents can finish while the parent model is still taking
        // tool steps. Surface queued completions before the next provider
        // request so the parent can use them immediately instead of
        // discovering them only when it eventually emits no more tools or
        // the idle handler starts a separate follow-up turn.
        if !self.is_acp_turn() {
            self.drain_subagent_completion_events("queued").await;
        }

        // The pinned system + tools prefix is frozen for the session:
        // recomposing it here from disk on every tool step is exactly what
        // kills DeepSeek's KV prefix cache once the agent writes a file
        // (the project pack listing changes -> the system hash changes ->
        // the next same-turn request is a full miss). Header changes come
        // only from explicit ops (`/model`, mode, goal, session sync),
        // which refresh under a declared reason. Volatile facts the model
        // must see mid-turn (LSP diagnostics, steer input, subagent
        // completions) are appended to history above, never spliced into
        // the frozen prefix.
        // A zero-tool turn (plain `exec`) spends extra steps only on
        // output-limit continuations. It has no work to wrap up or report
        // on, so the agent wrap-up notices below would only bend a
        // one-shot answer; at the limit it ends honestly instead.
        let zero_tool_turn = progress.tool_catalog.is_empty();
        if let Some(job) = self.child_job() {
            if job.authority.runtime.cancel_token.is_cancelled() {
                return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
            }
            if !self.child_report() {
                if let Some(notice) = job.pacing_notice() {
                    self.add_session_message(self.user_text_message_with_turn_metadata(notice))
                        .await;
                }
                if job
                    .work_deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                {
                    job.stop_for_budget("child wall-time work budget exhausted");
                    return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
                }
                if turn.at_max_steps() {
                    job.stop_for_budget("child model-step work budget exhausted");
                    return PhaseResult::Return((
                        TurnOutcomeStatus::Failed,
                        Some("child model-step work budget exhausted".into()),
                    ));
                }
            }
        }
        // A1 soft landing: with a finite step budget, once ~80% of it is
        // spent tell the model once to stop exploring and write its final
        // report. Savings proved out by the grok-style parity work (ops
        // A1): a step-faithful harness ends mid-report far too often.
        if self.child_host.is_none()
            && !zero_tool_turn
            && !turn.stop_diagnostics.soft_landing_sent
            && let Some(step_limit) = turn.step_limit()
            && step_limit > 0
            && turn.steps_used() >= ((step_limit as f32 * 0.8).floor() as u32).max(1)
        {
            turn.stop_diagnostics.soft_landing_sent = true;
            let notice = format!(
                "Step budget soft landing: you have used about {}% of your {} step budget ({}). Stop exploring; write your final, complete report now, in final form, with evidence.",
                80,
                turn.max_steps,
                turn.budget_source.key_label(),
            );
            self.add_session_message(self.user_text_message_with_turn_metadata(notice))
                .await;
            let _ = self
                .send_event(Event::status(
                    "Soft landing: wrap up with your final report",
                ))
                .await;
        }

        if turn.at_max_steps() {
            turn.stop_diagnostics.reason = Some(TurnStopReason::StepBudgetExhausted);
            if self.child_host.is_none()
                && progress.step_budget_exhaustion_is_terminal
                && !progress.final_report_sent
                && !zero_tool_turn
            {
                // A2 report-on-exhaustion: the budget died while the model
                // still owes work. Never finish silently — grant exactly
                // one final provider turn to write a bounded report, then
                // let the natural no-tool termination close the turn.
                progress.final_report_sent = true;
                turn.budget_exhausted_final_report = true;
                let notice = format!(
                    "Your model-step budget was exhausted (limit: {}, {}). You cannot continue working. Write your final report now: what you did, what you proved or found, what remains, and exact evidence. This is your last turn.",
                    turn.max_steps,
                    turn.budget_source.key_label(),
                );
                self.add_session_message(self.user_text_message_with_turn_metadata(notice))
                    .await;
                let _ = self
                    .send_event(Event::status(
                        "Model budget exhausted — final report requested",
                    ))
                    .await;
            } else if !progress.step_budget_exhaustion_is_terminal {
                return PhaseResult::Break;
            } else {
                let error = format!(
                    "Maximum model steps reached before completion (limit: {}, {})",
                    turn.max_steps,
                    turn.budget_source.key_label(),
                );
                self.post_turn_budget_stop(&error).await;
                return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error)));
            }
        }

        // A tool-producing response can spend the remaining goal budget
        // before this loop reaches the no-tool continuation check below.
        // Stop at the provider-request boundary so tool results remain in
        // the transcript, but no additional model request is authorized.
        // GoalState remains untouched here: the outer turn bookkeeping
        // records this usage once, then the normal cross-turn reconciler
        // publishes the terminal Blocked projection.
        // Token budget is advisory (unbounded) — surface telemetry but don't break.
        // Like grokbuild/kimicode, only verifier completion/block or backstop ends the run.
        if let Some(snapshot) = self.goal_snapshot_with_current_turn_usage(&turn.usage)
            && let Some(budget) = snapshot.token_budget
            && snapshot.tokens_used >= u64::from(budget)
        {
            let _ = self.send_event(Event::status(format!(
                        "Goal over token budget ({} / {budget} tokens) — continuing (unbounded); verify or /goal clear when done.",
                        snapshot.tokens_used
                    )))
                    .await;
        }

        let mut active_tools = active_tools_for_request(
            &progress.tool_catalog,
            &progress.active_tool_names,
            strict_tool_mode,
        );
        let fork_route = client.effective_route_envelope(&self.session.model, chrono::Utc::now());
        let fork_receipt = client.cache_route_receipt();
        let fork_job = self.child_job();
        let child_reporting = self.child_report();
        let mut fork_header_changed = false;
        if let Some(child) = self.child_host.as_mut() {
            if child.fork_cache_inherited.as_ref().is_some_and(|snapshot| {
                client.wire_format()
                    != Some(codewhale_config::provider::WireFormat::ChatCompletions)
                    || client.model() != self.session.model
                    || fork_receipt.is_none()
                    || snapshot.route_receipt.as_ref() != fork_receipt.as_ref()
                    || snapshot.model != fork_route.model
                    || snapshot.provider != fork_route.provider
                    || snapshot.provider_identity != fork_route.provider_identity
                    || snapshot.endpoint_fingerprint != fork_route.endpoint_fingerprint
            }) {
                child.fork_cache_inherited = None;
                child.fork_cache_prefix.clear();
                self.session.system_prompt = Some(child.system_prompt.clone());
                fork_header_changed = true;
            }
            if !child.fork_cache_checked && turn.step == 0 && !child_reporting {
                child.fork_cache_checked = true;
                let snapshot = fork_job
                    .as_ref()
                    .filter(|job| job.fork_context)
                    .and_then(|_| child.authority.runtime.fork_context.as_ref())
                    .and_then(|context| {
                        context
                            .live_header
                            .lock()
                            .ok()
                            .and_then(|cell| cell.clone())
                    });
                if client.model() == self.session.model
                    && let Some(wire) = client.wire_format()
                {
                    let decision = crate::prompt_zones::resolve_fork_inherit(
                        crate::prompt_zones::fork_inherit_enabled(),
                        snapshot.as_ref(),
                        &fork_route.model,
                        fork_route.provider,
                        &fork_route.provider_identity,
                        wire,
                        fork_route.endpoint_fingerprint.as_deref(),
                        fork_receipt.as_ref(),
                        active_tools.as_deref().unwrap_or_default(),
                        self.session.messages.len(),
                    );
                    #[allow(clippy::print_stderr)]
                    if crate::prompt_zones::fork_trial_diag_enabled() {
                        eprintln!(
                            "{}",
                            serde_json::json!({
                                "codewhale_fork_trial": "decision",
                                "agent_id": child.authority.owner_agent_id,
                                "inherited": decision.inherit,
                                "reason": decision.reason,
                                "prefix": decision.prefix_tools.len(),
                                "parent_n": snapshot.as_ref().map_or(0, |value| value.active_names.len()),
                            })
                        );
                    }
                    if decision.inherit
                        && let Some(snapshot) = snapshot
                    {
                        self.session.system_prompt = snapshot.system.clone();
                        child.fork_cache_prefix = decision
                            .prefix_tools
                            .iter()
                            .map(|tool| tool.name.clone())
                            .collect();
                        let prefix = &child.fork_cache_prefix;
                        progress.tool_catalog.sort_by_key(|tool| {
                            prefix
                                .iter()
                                .position(|name| name == &tool.name)
                                .unwrap_or(prefix.len())
                        });
                        child.fork_cache_inherited = Some(snapshot);
                        fork_header_changed = true;
                    }
                }
            }
            if let Some(tools) = active_tools.as_mut() {
                let prefix = &child.fork_cache_prefix;
                tools.sort_by_key(|tool| {
                    prefix
                        .iter()
                        .position(|name| name == &tool.name)
                        .unwrap_or(prefix.len())
                });
            }
        }
        if fork_header_changed {
            self.session.last_system_prompt_hash =
                Some(system_prompt_hash(self.session.system_prompt.as_ref()));
            self.session.pending_prefix_change_reason = Some("fork-cache".to_string());
            self.emit_session_updated().await;
            if self.cancel_token.is_cancelled() {
                return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
            }
        }
        // The report already has a bounded text-only captured request. It
        // cannot authorize an extra paid summarizer or mutate its evidence.
        if !self.child_report() {
            match self
                .run_auto_compaction(
                    client.as_ref(),
                    active_tools.as_deref(),
                    turn,
                    &mut progress.auto_compaction_suppressed,
                )
                .await
            {
                AutoCompactionStep::Proceed => {}
                AutoCompactionStep::Restart => return PhaseResult::Retry,
                AutoCompactionStep::EndTurn(status, error) => {
                    return PhaseResult::Return((status, error));
                }
            }
        }

        // The guard measures what the compaction gate measures: the honest
        // estimate, lifted to the provider's last bill plus the growth
        // since it. The ×1.5-inflated overflow estimate, compared against
        // the honest ceiling, refused at two
        // thirds of the budget, and emergency compaction — which targets
        // the honest budget — could never satisfy it (#6374). A request
        // the estimate still undercounts is rejected by the provider and
        // takes the bounded context-length recovery below.
        let estimated_input = turn
            .live_input_tokens_for_compaction(
                &self.session.messages,
                self.session.system_prompt.as_ref(),
                self.session.latest_parent_input_tokens,
            )
            .and_then(|tokens| usize::try_from(tokens).ok())
            .unwrap_or(0);
        if let Some(budget) = route_context_budget_for_route(
            self.api_provider,
            &self.session.model,
            self.active_route_limits,
            estimated_input,
        ) {
            let input_budget = usize::try_from(budget.input_budget_ceiling).unwrap_or(usize::MAX);
            let triggered = estimated_input > input_budget;
            let output_ceiling =
                crate::route_budget::output_ceiling_source(self.api_provider, &self.session.model);
            let route_input_limit =
                crate::route_budget::route_input_limit_tokens(self.active_route_limits);
            let input_ceiling_source =
                route_input_limit.map_or("window-minus-output-headroom", |limit| {
                    if u64::from(limit) <= budget.input_budget_ceiling {
                        "route-declared-input-limit"
                    } else {
                        "window-minus-output-headroom"
                    }
                });
            tracing::debug!(
                target: "context_budget",
                provider = self.api_provider.as_str(),
                model = %self.session.model,
                resolved_route_window_tokens = budget.window_tokens,
                resolved_model_output_ceiling_tokens = ?output_ceiling.clamp_tokens(),
                resolved_model_output_ceiling_source = output_ceiling.as_str(),
                effective_request_output_cap_tokens = crate::route_budget::effective_max_output_tokens_for_turn(
                    self.api_provider,
                    &self.session.model,
                    self.active_route_limits,
                    turn.max_output_tokens,
                ),
                reserved_response_headroom_tokens = budget.output_cap_tokens,
                safety_headroom_tokens = crate::context_budget::CONTEXT_HEADROOM_TOKENS,
                resolved_route_input_limit_tokens = ?route_input_limit,
                estimated_input_tokens = estimated_input,
                input_budget_ceiling_tokens = budget.input_budget_ceiling,
                input_budget_ceiling_source = input_ceiling_source,
                remaining_input_budget_tokens = budget.available_input_tokens,
                compaction_trigger_tokens = budget.compaction_trigger_tokens,
                trigger = if triggered { "preflight-token-budget" } else { "none" },
                "resolved route context budget"
            );
            if triggered {
                if progress.context_recovery_attempts >= MAX_CONTEXT_RECOVERY_ATTEMPTS {
                    let message = context_overflow_exhausted_message(
                        self.config.terminal_chrome_enabled,
                        turn.stop_diagnostics.emergency_compaction_attempts,
                        estimated_input,
                        input_budget,
                    );
                    progress.turn_error = Some(message.clone());
                    let _ = self
                        .send_event(Event::error(ErrorEnvelope::context_overflow(message)))
                        .await;
                    return PhaseResult::Return((
                        TurnOutcomeStatus::Failed,
                        progress.turn_error.take(),
                    ));
                }

                if self
                    .recover_context_overflow(
                        client.as_ref(),
                        active_tools.as_deref(),
                        "preflight token budget",
                        turn,
                    )
                    .await
                {
                    progress.context_recovery_attempts =
                        progress.context_recovery_attempts.saturating_add(1);
                    return PhaseResult::Retry;
                }
                if self.cancel_token.is_cancelled() {
                    return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
                }
                // One failure, one true sentence (experience mark 2): a
                // provider that refused the recovery request is the
                // cause, and a history with nothing to summarize is a
                // window problem, not a failed compaction.
                if let Some(rejection) = turn.context_recovery_rejection.take() {
                    let display_message = self.decorate_auth_error_message(
                        initial_stream_error_user_message(&self.config.locale_tag, &rejection),
                    );
                    let mut envelope = crate::error_taxonomy::envelope_for_llm_error(
                        rejection,
                        display_message.clone(),
                    );
                    envelope.message = display_message.clone();
                    let _ = self.send_event(Event::error(envelope)).await;
                    return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(display_message)));
                }
                let message = if crate::compaction::has_compactable_history(&self.session.messages)
                {
                    "The request still exceeds this model's context budget and automatic recovery did not complete. The conversation is saved; retry or choose a larger context route.".to_string()
                } else {
                    let prefix_tokens = crate::compaction::estimate_input_tokens_for_pressure(
                        &[],
                        self.session.system_prompt.as_ref(),
                    );
                    super::context::context_does_not_fit_message(
                        self.config.terminal_chrome_enabled,
                        self.api_provider == crate::config::ProviderKind::Ollama,
                        &self.session.model,
                        estimated_input,
                        input_budget,
                        prefix_tokens,
                    )
                };
                let _ = self
                    .send_event(Event::error(ErrorEnvelope::context_overflow(
                        message.clone(),
                    )))
                    .await;
                return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(message)));
            }
        }

        // #136: drain any LSP diagnostics collected since the last
        // request and inject them as a synthetic user message so the
        // model sees compile errors before its next reasoning step.
        self.flush_pending_lsp_diagnostics().await;

        // Build the request. Tool selection goes through the same
        // helper that seeded this turn and that `/preview-request`
        // reports, so a deferred tool activated mid-turn is reflected
        // identically in both places.
        // Resolve `auto` reasoning_effort to a concrete tier (#663).
        let effective_reasoning_effort = resolve_auto_effort(
            self.session.reasoning_effort.as_deref(),
            self.api_provider,
            &self.api_config.active_route_base_url(),
            &self.config.model,
        );

        // Check prefix-cache stability before building the request.
        // This detects system-prompt or tool-set drift that would
        // invalidate DeepSeek's KV prefix cache for this turn.
        // Sends an event on EVERY check so the TUI can maintain
        // its own counter for the stable-checks tally.
        let declared_change = self.session.pending_prefix_change_reason.take();
        if let Some(pm) = self.session.prefix_stability.as_mut() {
            let system_text = codewhale_core::prefix_cache::system_prompt_text(
                self.session.system_prompt.as_ref(),
            );
            let tools_ref: Option<&[codewhale_models::Tool]> = active_tools.as_deref();
            let outcome = pm.check(&system_text, tools_ref, declared_change.as_deref());
            // C5: request N's prefix may only diverge from N-1 across a
            // DECLARED change. An undeclared drift means the pinned header
            // moved without stamping a context update — the failure that
            // silently kills the provider cache while stability claims
            // still read well. The first check initializes the pin, so it
            // is exempt.
            #[cfg(debug_assertions)]
            if pm.check_count() > 1
                && declared_change.is_none()
                && let codewhale_core::prefix_cache::PrefixCheck::Drift { change }
                | codewhale_core::prefix_cache::PrefixCheck::Repinned { change, .. } = &outcome
            {
                debug_assert!(
                    false,
                    "prefix drift without a declared change (C5): the {} changed but no context update was stamped",
                    change.label()
                );
            }
            let pinned_hash = pm
                .pinned_fingerprint()
                .map(|fp| fp.combined_sha256.clone())
                .unwrap_or_default();
            let stability_pct = (pm.stability_ratio() * 100.0).round() as u32;
            let pin_reason = pm.pin_reason().unwrap_or_default().to_string();
            let last_miss_reason = pm.last_miss_reason().unwrap_or_default().to_string();
            let context_updates = pm.context_update_count();
            let event = match outcome {
                codewhale_core::prefix_cache::PrefixCheck::Stable => Event::PrefixCacheChange {
                    description: String::new(),
                    system_prompt_changed: false,
                    tools_changed: false,
                    stability_pct,
                    changed: false,
                    pinned_combined_hash: pinned_hash,
                    pin_reason,
                    last_miss_reason,
                    context_updates,
                },
                codewhale_core::prefix_cache::PrefixCheck::Repinned { reason, change } => {
                    // A declared header change re-pins under a logged
                    // reason: the miss is expected and attributable.
                    tracing::debug!(
                        target: "prefix_cache",
                        reason = %reason,
                        "prefix re-pinned: {}",
                        change.description()
                    );
                    Event::PrefixCacheChange {
                        description: format!("{reason} — {}", change.description()),
                        system_prompt_changed: change.system_changed,
                        tools_changed: change.tools_changed,
                        stability_pct,
                        changed: true,
                        pinned_combined_hash: pinned_hash,
                        pin_reason,
                        last_miss_reason,
                        context_updates,
                    }
                }
                codewhale_core::prefix_cache::PrefixCheck::Drift { change } => {
                    // Undeclared drift: the pin is kept so the same prefix
                    // keeps counting as a miss until an explicit op moves
                    // it. This should not happen after the mid-loop
                    // refresh removal — if it does it is a real bug.
                    tracing::warn!(
                        target: "prefix_cache",
                        "undeclared prefix drift (pin held): {}",
                        change.description()
                    );
                    Event::PrefixCacheChange {
                        description: format!("drift — {}", change.description()),
                        system_prompt_changed: change.system_changed,
                        tools_changed: change.tools_changed,
                        stability_pct,
                        changed: true,
                        pinned_combined_hash: pinned_hash,
                        pin_reason,
                        last_miss_reason,
                        context_updates,
                    }
                }
            };
            let _ = self.send_event(event).await;
        }

        // Three-zone prefix contract (#2264): freeze baseline on first
        // turn, verify against it on subsequent turns. Operates alongside
        // PrefixStabilityManager as an independent diagnostic layer.
        // Phase 3: emit a one-shot 'frozen' event on first turn.
        // Drift is logged (tracing::debug!) but not re-emitted —
        // PrefixStabilityManager already reports the change above.
        let system_text =
            codewhale_core::prefix_cache::system_prompt_text(self.session.system_prompt.as_ref());
        let current_tools: &[codewhale_models::Tool] = active_tools.as_deref().unwrap_or_default();

        match &self.session.frozen_prefix {
            Some(frozen) => {
                if let Err(drift) = frozen.verify(&system_text, current_tools) {
                    // Report drift; never replace the frozen baseline. The
                    // original freeze is the byte prefix the provider cache
                    // is keyed on — re-freezing here would make `/cache`
                    // look stable while the provider cache is already dead.
                    // A declared header change is re-pinned through the
                    // PrefixStabilityManager path above under a logged
                    // reason; the three-zone baseline stays put.
                    tracing::debug!(
                        target: "prefix_cache",
                        "three-zone drift (baseline held): {drift}"
                    );
                }
            }
            None => {
                let pinned =
                    PinnedPrefix::new(self.session.system_prompt.as_ref(), current_tools.to_vec());
                let frozen = pinned.freeze();
                let _ = self
                    .send_event(Event::PrefixCacheChange {
                        description: format!("frozen: {}", frozen.short_id()),
                        system_prompt_changed: false,
                        tools_changed: false,
                        stability_pct: 100,
                        changed: false,
                        pinned_combined_hash: frozen.hash().to_string(),
                        pin_reason: "initial".to_string(),
                        last_miss_reason: String::new(),
                        context_updates: 0,
                    })
                    .await;
                self.session.frozen_prefix = Some(frozen);
            }
        }

        let fleet_report_response = progress
            .fleet_denial_guard
            .as_ref()
            .is_some_and(FleetDenialGuard::report_only);
        // `take` is what keeps this request-scoped: the nudge is spent
        // here and never reaches `self.session.messages`.
        let request_nudge = progress.reasoning_only_nudge.take();
        let mut request = prepare_primary_turn_request(PrimaryTurnRequest {
            model: self.session.model.clone(),
            messages: {
                let mut messages = self.messages_with_turn_metadata();
                if let Some(nudge) = request_nudge.as_ref() {
                    messages.push(self.runtime_text_message_with_turn_metadata(
                        nudge.clone(),
                        UserInputProvenance::Runtime,
                    ));
                }
                messages
            },
            max_tokens: crate::route_budget::effective_max_output_tokens_for_turn(
                self.api_provider,
                &self.session.model,
                self.active_route_limits,
                turn.max_output_tokens,
            ),
            system: self.session.system_prompt.clone(),
            tools: active_tools.clone(),
            tool_choice: if active_tools.is_some() {
                if fleet_report_response || turn.budget_exhausted_final_report {
                    // Keep the pinned tool prefix; only this request's
                    // choice changes. Admission below also enforces this
                    // if a provider ignores the report-only request
                    // (C02-10: the step-budget final report included).
                    Some(json!("none"))
                } else if strict_tool_mode {
                    Some(json!("required"))
                } else {
                    Some(json!({ "type": "auto" }))
                }
            } else {
                None
            },
            reasoning_effort: effective_reasoning_effort,
        });
        if let Some(report) = self
            .child_host
            .as_ref()
            .and_then(|state| state.report.as_ref())
        {
            request.system = Some(report.system.clone());
            request.messages = report.messages.clone();
            request.max_tokens = report.output_tokens;
            request.tools = None;
            request.tool_choice = None;
        }
        if turn.max_output_tokens.is_some() {
            request.max_tokens = request
                .max_tokens
                .min(client.effective_max_output_tokens(&self.session.model));
        }
        // Normalize images against the route this request is actually
        // going to. Session history keeps the real image so that switching
        // to a vision-capable model later makes it visible again; only the
        // outbound copy is rewritten, and it is rewritten to text that says
        // why rather than being dropped.
        let fresh_images = crate::image_attach::images_since_last_user_prompt(&request.messages);
        let stripped_images = crate::image_attach::strip_images_when_unsupported(
            &mut request.messages,
            self.active_route_capabilities.image_input,
            &self.session.model,
        );
        if stripped_images > 0 {
            crate::logging::warn(format!(
                "{stripped_images} image block(s) replaced with text: model {} does not accept image input",
                self.session.model
            ));
            if fresh_images > 0 && !progress.image_omission_notified {
                progress.image_omission_notified = true;
                let status = codewhale_localization::tr(
                    codewhale_localization::resolve_locale(&self.config.locale_tag),
                    codewhale_localization::MessageId::ImageInputOmitted,
                )
                .replace("{model}", &self.session.model)
                .replace("{count}", &fresh_images.to_string());
                let _ = self.send_event(Event::status(status)).await;
            }
        }
        let tool_request_snapshot =
            crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request_with_surface(
                &turn.id,
                turn.step,
                request.tools.as_deref(),
                inspection_surface,
            );
        turn.last_request_snapshot = Some(tool_request_snapshot.clone());
        turn.stop_diagnostics.route_context_window_tokens = self
            .active_route_limits
            .and_then(|limits| limits.context_tokens);

        turn.stop_diagnostics.last_prepared_output_limit_tokens = Some(request.max_tokens);

        // Stream the response. Keep the request around (cloned into the
        // first call) so we can resend it on a transparent retry below
        // when the wire dies before any content was streamed (#103).
        let stream_request = request;
        let cache_receipt = client.cache_route_receipt();
        if self.child_host.is_none()
            && (client.wire_format()
                != Some(codewhale_config::provider::WireFormat::ChatCompletions)
                || client.model() != stream_request.model
                || cache_receipt.is_none())
            && let Ok(mut cell) = self.live_header.lock()
        {
            *cell = None;
        }

        if self.child_host.is_none()
            && crate::prompt_zones::fork_inherit_enabled()
            && client.wire_format() == Some(codewhale_config::provider::WireFormat::ChatCompletions)
            && client.model() == stream_request.model
            && cache_receipt.is_some()
            && let Some(tools_json) = crate::prompt_zones::wire_tool_catalog_json(
                stream_request.tools.as_deref().unwrap_or_default(),
            )
            && let Ok(mut cell) = self.live_header.lock()
        {
            let route = client.effective_route_envelope(&stream_request.model, chrono::Utc::now());
            let prior = cell.as_ref().filter(|old| {
                old.model == route.model
                    && old.provider == route.provider
                    && old.provider_identity == route.provider_identity
                    && old.endpoint_fingerprint == route.endpoint_fingerprint
            });
            let carried = crate::prompt_zones::carry_hot_forward(
                prior,
                &stream_request.system,
                &tools_json,
                &stream_request.messages,
                cache_receipt.as_ref(),
            );
            *cell = Some(crate::prompt_zones::LiveHeaderSnapshot {
                system: stream_request.system.clone(),
                tools_json,
                active_names: stream_request
                    .tools
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .map(|tool| tool.name.clone())
                    .collect(),
                model: route.model,
                provider: route.provider,
                provider_identity: route.provider_identity,
                endpoint_fingerprint: route.endpoint_fingerprint,
                route_receipt: cache_receipt,
                history: stream_request.messages.clone(),
                last_hit_tokens: carried,
            });
        }

        // Superfast Decision Gate (shadow mode, off by default). When
        // SUPERFAST_ENABLED is set, this spawns a detached task that asks
        // a small System One decision model about the user's turn and only
        // logs the recommendation. It never changes routing, never skips
        // the model call below, and never waits on the decision call.
        // Fired only on the first model request of the turn, where the raw
        // user message decides intent. See `crate::superfast`.
        if turn.step == 0 {
            // Detached on purpose: dropping the handle does not cancel it.
            drop(crate::superfast::spawn_shadow_gate(
                &self.api_config,
                &stream_request.messages,
                self.config.compaction.runtime_cost_owner.as_deref(),
                &self.cancel_token,
            ));
        }
        let _ = self
            .send_event(Event::ToolRequestSnapshot {
                snapshot: tool_request_snapshot,
            })
            .await;
        if let Some(nudge) = request_nudge {
            // The "Continuing — " prefix classifies the receipt as
            // internal: durable clients keep it, collapsed.
            let _ = self
                .send_event(Event::status(format!(
                    "{REQUEST_NUDGE_RECEIPT_PREFIX}{nudge}"
                )))
                .await;
        }
        if let Some(mut route) = turn.pending_route.take() {
            if let Some(billing) = route.billing.as_mut() {
                // Freeze the exact provider-live row at CodeWhale's
                // pre-permit application-dispatch boundary. This is an
                // admission contract, not provider invoice-time evidence;
                // a later cancellation/preparation failure has no usage
                // and therefore contributes no usage cost.
                let dispatched_at = chrono::Utc::now();
                billing.dispatched_at = dispatched_at;
                billing.provider_live_pricing = u64::try_from(dispatched_at.timestamp())
                    .ok()
                    .and_then(|dispatched_at_unix| {
                        crate::client::main_turn_pricing_quote_at(
                            self.codewhale_client.as_ref(),
                            route.provider,
                            &route.provider_identity,
                            &route.model,
                            billing.endpoint_fingerprint.as_deref()?,
                            dispatched_at_unix,
                        )
                    });
            }
            let _ = self
                .send_event(Event::RouteDispatched {
                    turn_id: turn.id.clone(),
                    route,
                })
                .await;
        }

        PhaseResult::Ready(PreparedModelStep {
            request: stream_request,
            zero_tool_turn,
            fleet_report_response,
        })
    }
    /// The same pure primary request builder and the same dispatch phase,
    /// with a whole bounded RLM history and no autonomous recovery producer.
    async fn prepare_rlm_model_step(
        &mut self,
        turn: &mut TurnContext,
        client: &SharedModelClient,
        inspection_surface: Option<&crate::tool_inspection::ToolSurfaceContext>,
    ) -> PhaseResult<PreparedModelStep> {
        let state = self.rlm_host.as_ref().expect("RLM host");
        if let Err(error) = state.caller.validate_live() {
            return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error.to_string())));
        }
        if self.cancel_token.is_cancelled() {
            return PhaseResult::Return((
                TurnOutcomeStatus::Interrupted,
                Some("RLM evaluation cancelled".into()),
            ));
        }
        if let Some(error) = self.turn_wall_clock_exhausted_error() {
            return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error)));
        }
        if turn.at_max_steps() {
            let state = self.rlm_host.as_mut().expect("RLM host");
            state.termination = Some(crate::rlm::turn::RlmTermination::Exhausted);
            return PhaseResult::Return((
                TurnOutcomeStatus::Failed,
                Some(format!(
                    "RLM exhausted {} iterations without FINAL",
                    crate::rlm::turn::MAX_RLM_ITERATIONS
                )),
            ));
        }
        self.record_current_constitution().await;
        self.record_current_extension_prompt_contributions().await;
        let estimated = turn
            .live_input_tokens_for_compaction(
                &self.session.messages,
                self.session.system_prompt.as_ref(),
                self.session.latest_parent_input_tokens,
            )
            .unwrap_or(0);
        if route_context_budget_for_route(
            self.api_provider,
            &self.session.model,
            self.active_route_limits,
            usize::try_from(estimated).unwrap_or(usize::MAX),
        )
        .is_some_and(|budget| estimated > budget.input_budget_ceiling)
        {
            return PhaseResult::Return((TurnOutcomeStatus::Failed, Some("RLM whole round history exceeds the captured route context budget; history was not truncated".into())));
        }
        let mut request = prepare_primary_turn_request(PrimaryTurnRequest {
            model: self.session.model.clone(),
            messages: self.messages_with_turn_metadata(),
            max_tokens: crate::route_budget::effective_max_output_tokens_for_turn(
                self.api_provider,
                &self.session.model,
                self.active_route_limits,
                turn.max_output_tokens,
            ),
            system: self.session.system_prompt.clone(),
            tools: None,
            tool_choice: None,
            reasoning_effort: resolve_auto_effort(
                self.session.reasoning_effort.as_deref(),
                self.api_provider,
                &self.api_config.active_route_base_url(),
                &self.config.model,
            ),
        });
        request.max_tokens = request
            .max_tokens
            .min(client.effective_max_output_tokens(&self.session.model));
        let snapshot =
            crate::tool_inspection::ToolInspectionSnapshot::from_prepared_request_with_surface(
                &turn.id,
                turn.step,
                None,
                inspection_surface,
            );
        turn.last_request_snapshot = Some(snapshot.clone());
        turn.stop_diagnostics.last_prepared_output_limit_tokens = Some(request.max_tokens);
        let _ = self
            .send_event(Event::ToolRequestSnapshot { snapshot })
            .await;
        PhaseResult::Ready(PreparedModelStep {
            request,
            zero_tool_turn: true,
            fleet_report_response: false,
        })
    }
}

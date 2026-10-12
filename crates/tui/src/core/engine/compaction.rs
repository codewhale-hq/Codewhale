//! Engine-owned compaction lifecycle, recovery and checkpoint installation.
//! Automatic, manual and emergency paths share the existing session and event authority.

use super::*;

pub(super) struct CompactionPass {
    pub trigger: &'static str,
    pub path: crate::compaction::CompactionPath,
    pub tokens_before: usize,
    pub threshold_tokens: usize,
    pub usage: Usage,
}

/// Which branch the turn loop takes after [`Engine::run_auto_compaction`].
/// The phase only reports it; `run_turn` keeps the `continue` and `return`.
pub(super) enum AutoCompactionStep {
    /// No pass was due, or a pass reached an outcome (compacted, skipped or
    /// failed with the conversation unchanged): build this step's request.
    Proceed,
    /// The pass was stopped on its own while the turn stays live: start the
    /// next loop iteration without sending a request.
    Restart,
    /// End the turn with this outcome: it was cancelled during the pass, or
    /// the pass spent the turn's wall-clock budget.
    EndTurn(TurnOutcomeStatus, Option<String>),
}

/// Engine-side sink for compaction downgrade notices.
///
/// Delivered as `Event::Status`, the same channel other engine status lines
/// use, so a long recovery says what it is doing while it runs. A full event
/// channel drops the notice instead of stalling the pass; the same sentence
/// is already in the log via `logging::warn`.
#[derive(Debug)]
struct EngineCompactionNoticeSink {
    tx: mpsc::Sender<Event>,
    child: Option<Arc<crate::tools::subagent::engine::ChildAuthority>>,
}

impl crate::compaction::CompactionNoticeSink for EngineCompactionNoticeSink {
    fn notice(&self, message: String) {
        let _ = self.tx.try_send(Event::Status { message });
    }
    fn accounting_origin(&self) -> Option<(crate::cost_status::CostScopeToken, String, String)> {
        self.child.as_ref().map(|child| child.accounting_origin())
    }
    fn settled_usage<'a>(
        &'a self,
        source: &'a str,
        route: &'a crate::cost_status::EffectiveRouteEnvelope,
        usage: &'a Usage,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            if let Some(child) = self.child.as_ref() {
                child.project_settled_response(source, route, usage).await;
            }
        })
    }
}

impl Engine {
    pub(super) async fn emit_compaction_started(
        &mut self,
        id: String,
        auto: bool,
        message: String,
    ) {
        let _ = self
            .send_event(Event::CompactionStarted { id, auto, message })
            .await;
    }

    pub(super) async fn emit_compaction_completed(
        &mut self,
        id: String,
        auto: bool,
        message: String,
        messages_before: Option<usize>,
        messages_after: Option<usize>,
        pass: CompactionPass,
    ) {
        let summary_prompt = self.rendered_compaction_summary();
        // Every call site runs after message replacement and checkpoint
        // commit. Reuse the same complete estimate as context pressure.
        let post_input_tokens = Some(self.estimated_input_tokens() as u64);
        let reduction_ratio = messages_before
            .zip(messages_after)
            .filter(|(before, _)| *before > 0)
            .map(|(before, after)| 1.0 - after as f64 / before as f64);
        self.record_compaction_event(
            "compaction.completed",
            serde_json::json!({
                "compaction_id": id,
                "trigger": pass.trigger,
                "path": match pass.path {
                    crate::compaction::CompactionPath::Summary => "summary",
                    crate::compaction::CompactionPath::PruneOnly => "pruning_only",
                },
                "messages_before": messages_before,
                "messages_after": messages_after,
                "estimated_tokens_before": pass.tokens_before,
                "estimated_tokens_after": post_input_tokens,
                "threshold_tokens": pass.threshold_tokens,
                "summarizer_usage": pass.usage,
                "reduction_ratio": reduction_ratio,
            }),
        )
        .await;
        let _ = self
            .send_event(Event::CompactionCompleted {
                id,
                auto,
                message,
                messages_before,
                messages_after,
                summary_prompt,
                post_input_tokens,
            })
            .await;
    }

    /// One audit producer shared by interactive, headless and Runtime hosts.
    /// No transcript content or credentials enter this diagnostic record.
    pub(super) async fn record_compaction_event(
        &self,
        event: &'static str,
        mut details: serde_json::Value,
    ) {
        details["session_id"] = serde_json::json!(self.session.id);
        details["thread_id"] = serde_json::json!(self.config.runtime_services.active_thread_id);
        details["model"] = serde_json::json!(self.config.model);
        #[cfg(test)]
        let env_ticket = crate::test_support::env_scope_ticket();
        if let Err(error) = tokio::task::spawn_blocking(move || {
            #[cfg(test)]
            let _membership = crate::test_support::join_env_scope(env_ticket);
            crate::audit::log_sensitive_event(event, details);
        })
        .await
        {
            tracing::warn!(%error, "compaction audit writer failed");
        }
    }

    pub(super) async fn emit_compaction_cancelled(
        &mut self,
        id: String,
        auto: bool,
        message: String,
    ) {
        let _ = self
            .send_event(Event::CompactionCancelled { id, auto, message })
            .await;
    }

    /// Render the accumulated compaction summary prompt to plain text so it
    /// can travel in events and be persisted by host layers. All emit sites
    /// run after `commit_compaction_checkpoint`, so this reflects the checkpoint
    /// state the engine will use for subsequent requests.
    pub(super) fn rendered_compaction_summary(&self) -> Option<String> {
        self.session
            .compaction_summary_prompt
            .as_ref()
            .map(|prompt| match prompt {
                SystemPrompt::Text(text) => text.clone(),
                SystemPrompt::Blocks(blocks) => blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            })
            .filter(|text| !text.trim().is_empty())
    }

    pub(super) async fn emit_compaction_failed(&mut self, id: String, auto: bool, message: String) {
        let _ = self
            .send_event(Event::CompactionFailed { id, auto, message })
            .await;
    }

    pub(super) fn claim_compaction(&self, id: &str) -> Option<CancellationToken> {
        self.compaction_cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .claim(id)
    }

    pub(super) fn finish_compaction(&self, id: &str) {
        self.compaction_cancellation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish(id);
    }

    /// Pressure and effective trigger in append-only turn metadata. Numeric
    /// estimates never modify the session-pinned system/tool prefix.
    pub(super) fn context_pressure_line(
        &self,
        current_text: &str,
        prompt_context: &NextTurnPromptContext,
        system_prompt: Option<&SystemPrompt>,
    ) -> Option<String> {
        // The engine owns automatic compaction. Asking the model to warn the
        // user here created a competing save/compact ceremony before the
        // automatic request-boundary guard could do its work (#5620).
        if self.config.compaction.enabled {
            return None;
        }
        let input_tokens = self.active_input_tokens_with_current_text(current_text, system_prompt);
        let budget = route_context_budget_for_route(
            prompt_context.provider,
            &prompt_context.model,
            prompt_context.route_limits,
            input_tokens,
        )?;
        context_pressure_message(budget.usage_percent()).map(|warning| format!(
            "{warning}. Estimated input: {input_tokens} tokens ({:.1}% of route budget). Making room automatically is off for this session. /compact saves the original conversation and its model-written handoff before replacing context.",
            budget.usage_percent(),
        ))
    }

    pub(super) fn prepare_compaction_envelope(
        &self,
        mut config: CompactionConfig,
    ) -> PreparedCompactionEnvelope {
        // Host-supplied configs may not carry the workspace; compaction needs
        // it only to re-state the user's `/anchor` file after the summary.
        config
            .workspace
            .get_or_insert_with(|| self.config.workspace.clone());
        let mut prepared = PreparedCompactionEnvelope::new(config);
        prepared.session_id = Some(self.session.id.clone());
        prepared.notice_sink = Some(std::sync::Arc::new(EngineCompactionNoticeSink {
            tx: self.tx_event.clone(),
            child: self
                .child_host
                .as_ref()
                .map(|child| child.authority.clone()),
        }));
        // The summary request must carry the reasoning tier the turn sends:
        // reasoning routes render it at the head of the prompt, so omitting
        // it forfeited the whole cached history prefix (#6540).
        prepared.reasoning_effort = super::turn_loop::resolve_auto_effort(
            self.session.reasoning_effort.as_deref(),
            self.api_provider,
            &self.api_config.active_route_base_url(),
            &self.config.model,
        );
        prepared
    }

    pub(super) async fn handle_manual_compaction_op(
        &mut self,
        id: String,
        route: ResolvedRuntimeRoute,
        compaction: CompactionConfig,
    ) {
        self.emit_compaction_started(id.clone(), false, "Making room…".to_string())
            .await;
        let Some(cancel_token) = self.claim_compaction(&id) else {
            let message = "Making room stopped before it started".to_string();
            self.emit_compaction_cancelled(id, false, message).await;
            let _ = self
                .send_event(Event::TurnComplete {
                    usage: Usage::default(),
                    parent_route_usage: Usage::default(),
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Interrupted,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            return;
        };
        if let Err(err) = self.install_resolved_runtime_route(route) {
            let message =
                format!("Cannot compact context because its provider route is not ready: {err}");
            self.finish_compaction(&id);
            self.emit_compaction_failed(id, false, message.clone())
                .await;
            let _ = self
                .send_event(Event::error(ErrorEnvelope::fatal_auth(message)))
                .await;
            return;
        }
        self.config.compaction = compaction;
        self.handle_manual_compaction(id, cancel_token).await;
    }

    pub(super) async fn emit_compaction_usage(&self, usage: &Usage, elapsed: Duration) {
        if *usage == Usage::default() {
            return;
        }
        let _ = self
            .send_event(Event::RoutedTurnUsage {
                usage: usage.clone(),
                duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
                first_token_ms: None,
                request_ms: None,
            })
            .await;
    }

    pub(super) async fn handle_manual_compaction(
        &mut self,
        id: String,
        cancel_token: CancellationToken,
    ) {
        let zero_usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            ..Usage::default()
        };
        let Some(client) = self.codewhale_client.clone() else {
            let message = "Can't make room: no model is connected".to_string();
            self.finish_compaction(&id);
            self.emit_compaction_failed(id, false, message.clone())
                .await;
            let _ = self
                .send_event(Event::error(ErrorEnvelope::fatal_auth(message.clone())))
                .await;
            let _ = self
                .send_event(Event::TurnComplete {
                    usage: zero_usage,
                    parent_route_usage: Usage::default(),
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Failed,
                    error: Some(message),
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            return;
        };

        let messages_before = self.session.messages.len();
        // Message counts alone do not show the win the user cares about: a
        // compaction that drops few but enormous messages reads as a no-op.
        // The emergency path already reports tokens; manual and auto now match.
        let tokens_before = self.estimated_input_tokens();
        let mut turn_status = TurnOutcomeStatus::Completed;
        let mut turn_error = None;

        let prepared = self.prepare_compaction_envelope(self.config.compaction.clone());

        let started = Instant::now();
        let mut compaction_usage = Usage::default();
        let compaction_result = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => None,
            result = compact_messages_safe(
                &client,
                &self.session.messages,
                self.session.system_prompt.as_ref(),
                &prepared,
                &mut compaction_usage,
            ) => Some(result),
        };
        self.session.total_usage.add(&compaction_usage);
        self.record_goal_usage_for_turn(&compaction_usage, started);
        self.emit_compaction_usage(&compaction_usage, started.elapsed())
            .await;

        let Some(compaction_result) = compaction_result else {
            self.finish_compaction(&id);
            self.emit_compaction_cancelled(
                id,
                false,
                "Making room stopped; the conversation was not changed".to_string(),
            )
            .await;
            let _ = self
                .send_event(Event::TurnComplete {
                    usage: compaction_usage,
                    parent_route_usage: Usage::default(),
                    routed_usage_dropped_records: 0,
                    status: TurnOutcomeStatus::Interrupted,
                    error: None,
                    tool_catalog: None,
                    base_url: None,
                })
                .await;
            return;
        };

        match compaction_result {
            Ok(mut result) => {
                if !result.messages.is_empty() || self.session.messages.is_empty() {
                    self.append_compaction_agent_topology(&mut result.messages)
                        .await;
                    if cancel_token.is_cancelled() {
                        self.finish_compaction(&id);
                        self.emit_compaction_cancelled(
                            id,
                            false,
                            "Making room stopped; the conversation was not changed".to_string(),
                        )
                        .await;
                        let _ = self
                            .send_event(Event::TurnComplete {
                                usage: compaction_usage,
                                parent_route_usage: Usage::default(),
                                routed_usage_dropped_records: 0,
                                status: TurnOutcomeStatus::Interrupted,
                                error: None,
                                tool_catalog: None,
                                base_url: None,
                            })
                            .await;
                        return;
                    }
                    let messages_after = result.messages.len();
                    let retries_used = result.retries_used;
                    let coverage_clause = result.coverage.receipt_clause();
                    let path = result.coverage.path;
                    if let Some(job) = self.child_job()
                        && let Err(error) = job
                            .before_replace(&self.session.messages, &result.messages)
                            .await
                    {
                        self.cancel_token.cancel();
                        tracing::error!(%error, "child compaction projection failed; original Session retained");
                        return;
                    }
                    self.session.replace_messages(result.messages);
                    if let Some(pm) = self.session.prefix_stability.as_mut() {
                        pm.note_history_reset("compaction");
                    }
                    self.commit_compaction_checkpoint(result.summary_prompt);
                    self.emit_session_updated().await;
                    let removed = messages_before.saturating_sub(messages_after);
                    let tokens_after = self.estimated_input_tokens();
                    let message = if retries_used > 0 {
                        format!(
                            "Made room: {messages_before} → {messages_after} messages ({removed} removed, {retries_used} retries), ~{tokens_before} → ~{tokens_after} tokens ({coverage_clause})"
                        )
                    } else {
                        format!(
                            "Made room: {messages_before} → {messages_after} messages ({removed} removed), ~{tokens_before} → ~{tokens_after} tokens ({coverage_clause})"
                        )
                    };
                    self.emit_compaction_completed(
                        id.clone(),
                        false,
                        message,
                        Some(messages_before),
                        Some(messages_after),
                        CompactionPass {
                            trigger: "manual",
                            path,
                            tokens_before,
                            threshold_tokens: prepared.config.token_threshold,
                            usage: compaction_usage.clone(),
                        },
                    )
                    .await;
                } else {
                    let message = "Making room skipped: the summary came back empty".to_string();
                    self.emit_compaction_failed(id.clone(), false, message.clone())
                        .await;
                    turn_status = TurnOutcomeStatus::Failed;
                    turn_error = Some(message);
                }
            }
            Err(err) => {
                let message = crate::compaction::report_compaction_failure(
                    "Making room failed",
                    &id,
                    false,
                    &err,
                );
                self.emit_compaction_failed(id.clone(), false, message.clone())
                    .await;
                let _ = self.send_event(Event::status(message.clone())).await;
                turn_status = TurnOutcomeStatus::Failed;
                turn_error = Some(message);
            }
        }

        self.finish_compaction(&id);

        let _ = self
            .send_event(Event::TurnComplete {
                usage: compaction_usage,
                parent_route_usage: Usage::default(),
                routed_usage_dropped_records: 0,
                status: turn_status,
                error: turn_error,
                tool_catalog: None,
                base_url: None,
            })
            .await;
    }

    /// The automatic compaction phase of `run_turn`, run once per loop
    /// iteration before the request is built.
    ///
    /// When context pressure has reached the trigger, this summarizes history
    /// with the tool prefix the next request will carry, installs the
    /// checkpoint and reports the pass; a refusal is named once per turn.
    /// `auto_compaction_suppressed` is the loop's turn-scoped latch: a failed,
    /// cancelled or empty pass, or one that leaves pressure high, sets it so
    /// the turn cannot become a paid summarization loop at every tool
    /// boundary. The bounded hard-limit recovery
    /// ([`Self::recover_context_overflow`]) stays available either way.
    ///
    /// The loop keeps its own control flow: the returned
    /// [`AutoCompactionStep`] names the branch, and `run_turn` takes it.
    pub(super) async fn run_auto_compaction(
        &mut self,
        client: &dyn crate::core::model_client::ModelClient,
        active_tools: Option<&[Tool]>,
        turn: &mut TurnContext,
        auto_compaction_suppressed: &mut bool,
    ) -> AutoCompactionStep {
        let auto_compaction_config = self.config.compaction.clone();
        // Billing usage accumulates every parent step and child-model
        // call. Only the most recent parent-route request describes the
        // live message list whose pressure we are checking here.
        let billed_input_tokens = turn.live_input_tokens_for_compaction(
            &self.session.messages,
            self.session.system_prompt.as_ref(),
            self.session.latest_parent_input_tokens,
        );
        let prepared = if !*auto_compaction_suppressed
            && auto_compaction_config.enabled
            && billed_input_tokens
                .and_then(|tokens| usize::try_from(tokens).ok())
                .unwrap_or(0)
                >= auto_compaction_config.token_threshold
        {
            let mut prepared = self.prepare_compaction_envelope(auto_compaction_config);
            prepared.tools = active_tools.map(<[Tool]>::to_vec);
            Some(prepared)
        } else {
            None
        };

        let compaction_go = match prepared.as_ref() {
            None => false,
            Some(prepared) => match crate::compaction::compaction_decision_with_billed(
                &self.session.messages,
                self.session.system_prompt.as_ref(),
                prepared,
                billed_input_tokens,
            ) {
                crate::compaction::CompactionDecision::Compact => true,
                crate::compaction::CompactionDecision::NotNeeded => false,
                crate::compaction::CompactionDecision::Refused(reason) => {
                    // A silent refusal looks like broken auto-compaction:
                    // the meter is full and nothing happens (#5577). Name
                    // the guard once per turn, in both the transcript
                    // status line and the trace.
                    if !turn.compaction_refusal_notified {
                        turn.compaction_refusal_notified = true;
                        let estimated_tokens_before = self.estimated_input_tokens();
                        self.record_compaction_event("compaction.refused", serde_json::json!({
                            "trigger": "auto",
                            "reason": match &reason {
                                crate::compaction::CompactionRefusal::TooFewMessages { .. } => "too_few_messages",
                                crate::compaction::CompactionRefusal::RetainedFloor { .. } => "retained_floor",
                            },
                            "messages_before": self.session.messages.len(),
                            "estimated_tokens_before": estimated_tokens_before,
                            "billed_input_tokens": billed_input_tokens,
                            "threshold_tokens": prepared.config.token_threshold,
                        })).await;
                        let message = match reason {
                            crate::compaction::CompactionRefusal::TooFewMessages { count } => {
                                format!(
                                    "Context is filling up, but there is nothing to make room from yet: only {count} messages"
                                )
                            }
                            crate::compaction::CompactionRefusal::RetainedFloor {
                                floor,
                                threshold,
                            } => format!(
                                "Context is filling up, but making room would not help: retained context (~{}K tokens) cannot fall below the {}K trigger — /compact to force a pass, or trim pinned context",
                                floor / 1000,
                                threshold / 1000
                            ),
                        };
                        tracing::warn!(
                            target: "compaction",
                            ?reason,
                            billed = ?billed_input_tokens,
                            "auto-compaction refused under pressure"
                        );
                        let _ = self.send_event(Event::status(message)).await;
                    }
                    false
                }
            },
        };
        if let Some(prepared) = prepared
            && compaction_go
        {
            let compaction_id = format!("compact_{}", &uuid::Uuid::new_v4().to_string()[..8]);
            turn.stop_diagnostics.automatic_compaction_attempts = turn
                .stop_diagnostics
                .automatic_compaction_attempts
                .saturating_add(1);
            let compaction_cancel = self
                .claim_compaction(&compaction_id)
                .expect("a fresh automatic compaction id cannot be pre-canceled");
            self.emit_compaction_started(compaction_id.clone(), true, "Making room…".to_string())
                .await;
            let auto_messages_before = self.session.messages.len();
            let auto_tokens_before = self.estimated_input_tokens();
            let turn_cancel = self.cancel_token.clone();
            let started = Instant::now();
            let mut compaction_usage = Usage::default();
            // Parked: the compaction pass owns its own bound.
            self.turn_heartbeat
                .enter(super::turn_heartbeat::TurnPhase::Compacting, None, None);
            let (compaction_result, turn_was_canceled) = tokio::select! {
                biased;
                _ = turn_cancel.cancelled() => (None, true),
                _ = compaction_cancel.cancelled() => (None, false),
                result = compact_messages_safe(
                    client,
                    &self.session.messages,
                    self.session.system_prompt.as_ref(),
                    &prepared,
                    &mut compaction_usage,
                ) => (Some(result), false),
            };
            turn.add_usage(&compaction_usage);
            self.emit_compaction_usage(&compaction_usage, started.elapsed())
                .await;
            let Some(compaction_result) = compaction_result else {
                *auto_compaction_suppressed = true;
                self.finish_compaction(&compaction_id);
                let message = if turn_was_canceled {
                    "Making room stopped with the turn; the conversation was not changed"
                } else {
                    "Making room stopped; the conversation was not changed"
                }
                .to_string();
                self.emit_compaction_cancelled(compaction_id, true, message)
                    .await;
                if turn_was_canceled {
                    return AutoCompactionStep::EndTurn(TurnOutcomeStatus::Interrupted, None);
                }
                return AutoCompactionStep::Restart;
            };

            match compaction_result {
                Ok(mut result) => {
                    // Only update if we got valid messages (never corrupt state)
                    if !result.messages.is_empty() || self.session.messages.is_empty() {
                        self.append_compaction_agent_topology(&mut result.messages)
                            .await;
                        let turn_was_canceled = turn_cancel.is_cancelled();
                        if turn_was_canceled || compaction_cancel.is_cancelled() {
                            *auto_compaction_suppressed = true;
                            self.finish_compaction(&compaction_id);
                            let message = if turn_was_canceled {
                                "Making room stopped with the turn; the conversation was not changed"
                            } else {
                                "Making room stopped; the conversation was not changed"
                            }
                            .to_string();
                            self.emit_compaction_cancelled(compaction_id, true, message)
                                .await;
                            if turn_was_canceled {
                                return AutoCompactionStep::EndTurn(
                                    TurnOutcomeStatus::Interrupted,
                                    None,
                                );
                            }
                            return AutoCompactionStep::Restart;
                        }
                        let auto_messages_after = result.messages.len();
                        let retries_used = result.retries_used;
                        let coverage_clause = result.coverage.receipt_clause();
                        let path = result.coverage.path;
                        if let Some(job) = self.child_job()
                            && let Err(error) = job
                                .before_replace(&self.session.messages, &result.messages)
                                .await
                        {
                            return AutoCompactionStep::EndTurn(
                                TurnOutcomeStatus::Failed,
                                Some(format!("child compaction projection failed: {error:#}")),
                            );
                        }
                        self.session.replace_messages(result.messages);
                        turn.clear_parent_input_tokens();
                        if let Some(pm) = self.session.prefix_stability.as_mut() {
                            pm.note_history_reset("compaction");
                        }
                        self.commit_compaction_checkpoint(result.summary_prompt);
                        *auto_compaction_suppressed =
                            crate::compaction::compaction_pressure_reached(
                                &self.session.messages,
                                self.session.system_prompt.as_ref(),
                                &self.config.compaction,
                            );
                        self.emit_session_updated().await;
                        let removed = auto_messages_before.saturating_sub(auto_messages_after);
                        let auto_tokens_after = self.estimated_input_tokens();
                        let status = if retries_used > 0 {
                            format!(
                                "Made room: {auto_messages_before} → {auto_messages_after} messages ({removed} removed, {retries_used} retries), ~{auto_tokens_before} → ~{auto_tokens_after} tokens ({coverage_clause})"
                            )
                        } else {
                            format!(
                                "Made room: {auto_messages_before} → {auto_messages_after} messages ({removed} removed), ~{auto_tokens_before} → ~{auto_tokens_after} tokens ({coverage_clause})"
                            )
                        };
                        self.emit_compaction_completed(
                            compaction_id.clone(),
                            true,
                            status.clone(),
                            Some(auto_messages_before),
                            Some(auto_messages_after),
                            CompactionPass {
                                trigger: "auto",
                                path,
                                tokens_before: auto_tokens_before,
                                threshold_tokens: prepared.config.token_threshold,
                                usage: compaction_usage.clone(),
                            },
                        )
                        .await;
                    } else {
                        *auto_compaction_suppressed = true;
                        let message =
                            "Making room skipped: the summary came back empty".to_string();
                        self.emit_compaction_failed(compaction_id.clone(), true, message.clone())
                            .await;
                        let _ = self.send_event(Event::status(message)).await;
                    }
                }
                Err(err) => {
                    *auto_compaction_suppressed = true;
                    // Log error but continue with original messages (never corrupt)
                    let message = crate::compaction::report_compaction_failure(
                        "Making room failed",
                        &compaction_id,
                        true,
                        &err,
                    );
                    self.emit_compaction_failed(compaction_id.clone(), true, message.clone())
                        .await;
                    let _ = self.send_event(Event::status(message)).await;
                }
            }
            self.finish_compaction(&compaction_id);
            // C02-06: a compaction pass has its own bound, not the
            // turn's. Recheck the wall clock before it can authorize the
            // provider request that follows this phase.
            if let Some(error) = self.turn_wall_clock_exhausted_error() {
                self.post_turn_budget_stop(&error).await;
                return AutoCompactionStep::EndTurn(TurnOutcomeStatus::Failed, Some(error));
            }
        }
        AutoCompactionStep::Proceed
    }

    pub(super) async fn recover_context_overflow(
        &mut self,
        client: &dyn crate::core::model_client::ModelClient,
        tools: Option<&[Tool]>,
        reason: &str,
        turn: &mut TurnContext,
    ) -> bool {
        let Some(target_budget) = context_input_budget_for_route(
            self.api_provider,
            &self.session.model,
            self.active_route_limits,
            0,
        ) else {
            return false;
        };
        // Nothing to summarize or prune: a pass cannot help, so do not make
        // the user wait on a model call before the failure the caller will
        // report anyway.
        if !crate::compaction::has_compactable_history(&self.session.messages) {
            return false;
        }

        let id = format!("compact_{}", &uuid::Uuid::new_v4().to_string()[..8]);
        turn.stop_diagnostics.emergency_compaction_attempts = turn
            .stop_diagnostics
            .emergency_compaction_attempts
            .saturating_add(1);
        let start_message = format!("Making room now ({reason})");
        self.emit_compaction_started(id.clone(), true, start_message)
            .await;
        let Some(compaction_cancel) = self.claim_compaction(&id) else {
            self.emit_compaction_cancelled(
                id,
                true,
                "Making room stopped before it started; the conversation was not changed"
                    .to_string(),
            )
            .await;
            return false;
        };
        let turn_cancel = self.cancel_token.clone();

        // Measured with the estimator `after_tokens` and the preflight guard
        // use, so `recovered` compares like with like and the receipt reads
        // ~before → ~after on one scale.
        let before_tokens = crate::compaction::estimate_input_tokens_for_pressure(
            &self.session.messages,
            self.session.system_prompt.as_ref(),
        );
        let before_count = self.session.messages.len();

        let mut forced_config = self.config.compaction.clone();
        forced_config.enabled = true;
        forced_config.token_threshold = forced_config
            .token_threshold
            .min(target_budget.saturating_sub(1))
            .max(1);
        let mut prepared = self.prepare_compaction_envelope(forced_config);
        prepared.tools = tools.map(<[Tool]>::to_vec);

        let started = Instant::now();
        let mut compaction_usage = Usage::default();
        let (compaction_result, turn_was_canceled) = tokio::select! {
            biased;
            _ = turn_cancel.cancelled() => (None, true),
            _ = compaction_cancel.cancelled() => (None, false),
            result = compact_messages_safe(
                client,
                &self.session.messages,
                self.session.system_prompt.as_ref(),
                &prepared,
                &mut compaction_usage,
            ) => (Some(result), false),
        };
        turn.add_usage(&compaction_usage);
        self.emit_compaction_usage(&compaction_usage, started.elapsed())
            .await;
        let Some(compaction_result) = compaction_result else {
            self.finish_compaction(&id);
            let message = if turn_was_canceled {
                "Making room stopped with the turn; the conversation was not changed"
            } else {
                "Making room stopped; the conversation was not changed"
            }
            .to_string();
            self.emit_compaction_cancelled(id, true, message).await;
            return false;
        };

        let result = match compaction_result {
            Ok(result) => result,
            Err(err) => {
                let message = if is_provider_rejection(&err) {
                    // The turn's error line carries the provider's answer;
                    // this receipt only closes the recovery attempt.
                    "Context recovery stopped: the provider rejected the request. Original conversation was preserved.".to_string()
                } else {
                    let reason = format!("{err:#}");
                    let reason = reason.trim_end().trim_end_matches('.');
                    if reason
                        .to_ascii_lowercase()
                        .contains("conversation was preserved")
                    {
                        format!("Context recovery failed: {reason}.")
                    } else {
                        format!(
                            "Context recovery failed: {reason}. Original conversation was preserved."
                        )
                    }
                };
                if is_provider_rejection(&err) {
                    turn.context_recovery_rejection = Some(err);
                }
                self.emit_compaction_failed(id.clone(), true, message).await;
                self.finish_compaction(&id);
                return false;
            }
        };
        let retries_used = result.retries_used;
        let summary_prompt = result.summary_prompt;
        let path = result.coverage.path;
        let mut compacted_messages = result.messages;

        let turn_was_canceled = turn_cancel.is_cancelled();
        if turn_was_canceled || compaction_cancel.is_cancelled() {
            self.finish_compaction(&id);
            let message = if turn_was_canceled {
                "Making room stopped with the turn; the conversation was not changed"
            } else {
                "Making room stopped; the conversation was not changed"
            }
            .to_string();
            self.emit_compaction_cancelled(id, true, message).await;
            return false;
        }

        if !compacted_messages.is_empty() || self.session.messages.is_empty() {
            self.append_compaction_agent_topology(&mut compacted_messages)
                .await;
            let turn_was_canceled = turn_cancel.is_cancelled();
            if turn_was_canceled || compaction_cancel.is_cancelled() {
                self.finish_compaction(&id);
                let message = if turn_was_canceled {
                    "Making room stopped with the turn; the conversation was not changed"
                } else {
                    "Making room stopped; the conversation was not changed"
                }
                .to_string();
                self.emit_compaction_cancelled(id, true, message).await;
                return false;
            }
        }
        // Validate the complete candidate before the only history swap. Bare
        // front-trimming after a failed summary silently lost user state and
        // could leave orphan tool results in an apparently recovered session.
        let after_tokens = crate::compaction::estimate_input_tokens_for_pressure(
            &compacted_messages,
            self.session.system_prompt.as_ref(),
        );
        let after_count = compacted_messages.len();
        let recovered = after_tokens <= target_budget && after_tokens < before_tokens;

        if recovered {
            if let Some(job) = self.child_job()
                && let Err(error) = job
                    .before_replace(&self.session.messages, &compacted_messages)
                    .await
            {
                self.cancel_token.cancel();
                tracing::error!(%error, "child recovery projection failed; original Session retained");
                return false;
            }
            self.session.replace_messages(compacted_messages);
            turn.clear_parent_input_tokens();
            if let Some(pm) = self.session.prefix_stability.as_mut() {
                pm.note_history_reset("compaction");
            }
            self.commit_compaction_checkpoint(summary_prompt);
            self.emit_session_updated().await;
            let removed = before_count.saturating_sub(after_count);
            let mut details = format!(
                "Made room: {before_count} → {after_count} messages ({removed} removed), ~{before_tokens} → ~{after_tokens} tokens"
            );
            if retries_used > 0 {
                details.push_str(&format!(" ({retries_used} retries)"));
            }
            self.emit_compaction_completed(
                id.clone(),
                true,
                details.clone(),
                Some(before_count),
                Some(after_count),
                CompactionPass {
                    trigger: "emergency",
                    path,
                    tokens_before: before_tokens,
                    threshold_tokens: prepared.config.token_threshold,
                    usage: compaction_usage.clone(),
                },
            )
            .await;
            let _ = self.send_event(Event::status(details)).await;
            self.finish_compaction(&id);
            return true;
        }

        // Two distinct failures were previously conflated into one banner.
        // When the provider rejected the request (its bill counts framing we
        // cannot see), our estimate may already sit within the budget while
        // the pass removed nothing — reporting that as "failed to reduce
        // below model limit" with an estimate printed *under* the budget
        // reads as self-contradictory. Name the actual outcome instead.
        let message = if after_tokens > target_budget {
            format!(
                "Making room failed: the request is still over the model limit \
                 (estimate ~{after_tokens} tokens, budget ~{target_budget}). Original conversation was preserved."
            )
        } else {
            format!(
                "Making room made no progress (estimate ~{after_tokens} tokens \
                 is already within the ~{target_budget} budget; the provider may count the \
                 request differently). Original conversation was preserved."
            )
        };
        self.emit_compaction_failed(id.clone(), true, message.clone())
            .await;
        let _ = self.send_event(Event::status(message)).await;
        self.finish_compaction(&id);
        false
    }

    /// Keep the rendered checkpoint for host persistence and repeat-compaction
    /// metadata. The model sees the checkpoint exactly once through ordinary
    /// conversation history; the stable system prefix never carries it.
    pub(super) fn commit_compaction_checkpoint(&mut self, summary_prompt: Option<SystemPrompt>) {
        let Some(summary_prompt) = summary_prompt else {
            return;
        };
        self.session.compaction_summary_prompt = Some(summary_prompt);
    }

    /// Capture the current session-owned Agent topology at the replacement
    /// history boundary. This is the Codewhale equivalent of Codex clearing
    /// its world-state reference after standalone compaction so the next turn
    /// receives fresh environment/subagent context instead of trusting the
    /// narrative summary as live process state.
    pub(super) async fn append_compaction_agent_topology(&self, messages: &mut Vec<Message>) {
        let snapshots = {
            let manager = self.subagent_manager.read().await;
            manager.list_for_session(&self.session.id)
        };
        crate::runtime_handoff::replace_agent_topology_checkpoint(messages, &snapshots);
    }
}

/// A context-recovery failure that came from the provider refusing the
/// request (capability, auth, reachability, quota) rather than from the
/// summary itself. Context-length rejections are excluded: those really are
/// the budget problem the caller already reports.
pub(super) fn is_provider_rejection(err: &anyhow::Error) -> bool {
    use crate::error_taxonomy::{ErrorCategory, classify_error_message};
    let text = format!("{err:#}");
    if super::context::is_context_length_error_message(&text)
        || matches!(
            err.downcast_ref::<crate::llm_client::LlmError>(),
            Some(crate::llm_client::LlmError::ContextLengthError(_))
        )
    {
        return false;
    }
    err.downcast_ref::<crate::llm_client::LlmError>().is_some()
        || matches!(
            classify_error_message(&text),
            ErrorCategory::Authentication
                | ErrorCategory::Authorization
                | ErrorCategory::Network
                | ErrorCategory::RateLimit
                | ErrorCategory::Timeout
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compaction::CompactionNoticeSink as _;

    /// The engine sink is the one link between a compaction downgrade and the
    /// person watching: the notice must land on the status line, not only in
    /// the log.
    #[tokio::test]
    async fn compaction_notice_sink_delivers_a_status_event() {
        let (tx, mut rx) = mpsc::channel(4);
        let sink = EngineCompactionNoticeSink { tx, child: None };
        sink.notice("Making room re-encoded 2 inline image(s)".to_string());
        match rx.recv().await {
            Some(Event::Status { message }) => {
                assert!(message.contains("re-encoded"), "{message}");
            }
            other => panic!("expected a Status event, got {other:?}"),
        }
    }
}

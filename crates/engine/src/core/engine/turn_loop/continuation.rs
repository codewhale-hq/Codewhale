//! One private phase of the existing Engine turn loop.

use super::*;

impl Engine {
    pub(super) async fn continue_model_step(
        &mut self,
        turn: &mut TurnContext,
        tool_policy: &ToolSurfacePolicy,
        progress: &mut TurnLoopProgress,
        client: &SharedModelClient,
        response: AcceptedModelStep,
    ) -> PhaseResult<AcceptedModelStep> {
        if self.rlm_host.is_some() {
            return self
                .continue_rlm_model_step(turn, tool_policy, progress, client, response)
                .await;
        }
        let tool_registry = Some(&tool_policy.registry);
        let AcceptedModelStep {
            current_text_visible,
            tool_uses,
            mut pending_steers,
            mut output_limit_truncated,
            zero_tool_turn,
            zero_tool_text_call,
            fleet_report_response,
            fleet_no_progress_report,
            has_sendable_assistant_content,
            has_provider_reasoning,
            no_sendable_assistant_content,
            stop_reason,
            stream_errors,
            prepared_output_tokens,
        } = response;
        // A truncated response with no tool call cannot continue through
        // tool execution: surface the truncation as a bounded observation
        // and resume the loop so the model can act on it instead of the
        // turn silently ending on a cut-off answer. Resume only when the
        // truncated response actually delivered partial content — a
        // reasoning-only length stop delivered nothing to continue from.
        // The default fails that turn; opt-in answer recovery is handled below.
        if output_limit_truncated.is_some()
            && !fleet_no_progress_report
            && tool_uses.is_empty()
            && has_sendable_assistant_content
        {
            let reason = output_limit_truncated
                .take()
                .expect("output_limit_truncated checked above");
            self.add_session_message(
                    self.runtime_text_message_with_turn_metadata(
                        format!(
                            "[runtime] The provider stopped generation at its output limit (`{reason}`) before completing. Your last response was cut off. Continue from where you left off; do not repeat content already delivered."
                        ),
                        UserInputProvenance::Runtime,
                    ),
                )
                .await;
            let _ = self
                .send_event(Event::status(
                    "Continuing — provider output limit reached; asking the model to continue"
                        .to_string(),
                ))
                .await;
            turn.next_step();
            return PhaseResult::Retry;
        }

        // If no tool uses, check for inline REPL blocks (paper §2) or
        // finish the turn. Honest ladder (NOTE-turn-loop-wrongness §3):
        // 1) pending steers → resume, 2) queued subagent completions →
        // resume, 3) REPL fences → run (empty cap may end), 4) goal
        // continuation if under cap → resume, 5) else end. Healthy
        // children continue in the background; their existence alone
        // does not authorize another parent model request.
        if tool_uses.is_empty() && !fleet_no_progress_report {
            if !pending_steers.is_empty() {
                if let Some(guard) = progress.fleet_denial_guard.as_mut() {
                    guard.reset();
                    turn.stop_diagnostics
                        .permission_denial_rounds_without_progress = 0;
                }
                for pending in pending_steers.drain(..) {
                    let steer = pending.commit().trim().to_string();
                    self.session
                        .working_set
                        .observe_user_message(&steer, &self.session.workspace);
                    self.add_session_message(self.user_text_message_with_turn_metadata(steer))
                        .await;
                }
                let _ = self
                    .send_event(Event::status("Continuing — queued steer input".to_string()))
                    .await;
                turn.next_step();
                return PhaseResult::Retry;
            }

            self.deliver_shell_completions("").await;

            // Sub-agent completion handoff (issue #756). Resuming when
            // queued completions exist is correct; #3216 says do not wait
            // indefinitely for every running child here. Healthy work
            // keeps running and reports by sentinel on a later turn.
            let subagent_completions = if self.is_acp_turn() {
                0
            } else {
                self.drain_subagent_completion_events("").await
            };
            if subagent_completions > 0 {
                let _ = self
                    .send_event(Event::status(format!(
                        "Continuing — {subagent_completions} sub-agent(s) completed"
                    )))
                    .await;
                turn.next_step();
                return PhaseResult::Retry;
            }

            match self
                .run_inline_repl_phase(
                    turn,
                    tool_policy,
                    progress,
                    client,
                    &current_text_visible,
                    has_sendable_assistant_content,
                )
                .await
            {
                PhaseResult::Ready(()) => {}
                PhaseResult::Retry => return PhaseResult::Retry,
                PhaseResult::Break => return PhaseResult::Break,
                PhaseResult::Return(outcome) => return PhaseResult::Return(outcome),
            }

            // Issue #1727: the turn is now genuinely finishing with no
            // sendable content. Control only reaches here when there were
            // no pending steers (`continue`d above) and no sub-agent
            // completions to resume with. Healthy running children do
            // not force another model request.
            // If the assistant produced ONLY a reasoning block, the prior
            // code fell straight through to this `break`, emitting nothing
            // and leaving the UI spinner hung. Surface a status now —
            // safe because the turn can no longer resume.
            // #1961: Before breaking, drain any sub-agent completions that
            // arrived between the last hold check and now. If a child finished
            // while we were running the thinking-only check, surface its
            // sentinel rather than delaying it to the next turn.
            self.deliver_shell_completions("late").await;

            if !self.is_acp_turn() && self.drain_subagent_completion_events("late").await > 0 {
                let _ = self
                    .send_event(Event::status(
                        "Continuing — late sub-agent completion".to_string(),
                    ))
                    .await;
                turn.next_step();
                return PhaseResult::Retry;
            }

            // A goal continuation is optional work on top of a productive
            // step. A response that produced nothing sendable and ran no
            // tools is a failed step (incomplete/length-stopped provider
            // response): continuing would re-issue the exact request that
            // just failed — for an output-length stop it can only
            // reproduce — instead of failing the turn honestly.
            let step_produced_nothing = no_sendable_assistant_content && tool_uses.is_empty();
            if !self.is_acp_turn()
                && !step_produced_nothing
                && let Some(continuation) = self
                    .goal_continuation_message_if_needed(
                        tool_registry,
                        &mut progress.goal_continuations_this_turn,
                        &turn.usage,
                    )
                    .await
            {
                // The model already delivered a complete answer this step;
                // the continuation is optional runtime work on top of it.
                // If the step budget then runs out, the turn is finished,
                // not failed.
                progress.step_budget_exhaustion_is_terminal = false;
                self.add_session_message(self.runtime_text_message_with_turn_metadata(
                    continuation,
                    UserInputProvenance::Runtime,
                ))
                .await;
                let _ = self
                    .send_event(Event::status(format!(
                        "Continuing — goal still active (pass {goal_continuations_this_turn})",
                        goal_continuations_this_turn = progress.goal_continuations_this_turn
                    )))
                    .await;
                turn.next_step();
                return PhaseResult::Retry;
            }

            let output_limit_stop = is_output_limit_stop_reason(stop_reason.as_deref());
            let opted_in_output_limit =
                self.config.reasoning_only_on_output_limit && output_limit_stop;
            if no_sendable_assistant_content
                && !zero_tool_text_call
                && (has_provider_reasoning || opted_in_output_limit)
                && should_fail_no_sendable_content(
                    tool_uses.is_empty(),
                    progress.turn_error.is_none(),
                    self.cancel_token.is_cancelled(),
                    !pending_steers.is_empty(),
                    false,
                )
                && (!output_limit_stop || opted_in_output_limit)
                && progress.reasoning_only_reprompts < self.config.reasoning_only_max_reprompts
            {
                progress.reasoning_only_reprompts += 1;
                turn.stop_diagnostics.reasoning_only_reprompts = progress.reasoning_only_reprompts;
                let attempt = progress.reasoning_only_reprompts;
                let max_reprompts = self.config.reasoning_only_max_reprompts;
                progress.reasoning_only_nudge = (attempt > 1 || output_limit_stop)
                    .then(|| {
                        self.config
                            .reasoning_only_reprompt_message
                            .clone()
                            .unwrap_or_else(|| {
                                codewhale_runtime::config::DEFAULT_REASONING_ONLY_REPROMPT_MESSAGE
                                    .to_string()
                            })
                    })
                    .filter(|text| !text.trim().is_empty());
                let nudged = progress.reasoning_only_nudge.is_some();
                let how = if nudged {
                    "re-requesting the answer with a nudge"
                } else {
                    "re-requesting the answer"
                };
                let reason = if output_limit_stop {
                    "output-limit reply"
                } else {
                    "reasoning-only reply"
                };
                codewhale_runtime::logging::warn(format!(
                    "Model returned {reason} with no answer or tool call (attempt {attempt}/{max_reprompts}); {how}"
                ));
                let locale = codewhale_localization::resolve_locale(&self.config.locale_tag);
                let action = codewhale_localization::tr(
                    locale,
                    if nudged {
                        codewhale_localization::MessageId::AnswerRetryActionNudge
                    } else {
                        codewhale_localization::MessageId::AnswerRetryActionBare
                    },
                );
                let body = codewhale_localization::tr(
                    locale,
                    if output_limit_stop {
                        codewhale_localization::MessageId::OutputLimitAnswerRetryBody
                    } else {
                        codewhale_localization::MessageId::ReasoningOnlyRetryBody
                    },
                )
                .replace("{attempt}", &attempt.to_string())
                .replace("{limit}", &max_reprompts.to_string())
                .replace("{action}", action.as_ref());
                let _ = self
                    .send_retry_status(format!("Retry attempt: {body}"))
                    .await;
                progress.turn_error = None;
                return PhaseResult::Retry;
            }

            // #6310: a clean terminal stop with no text, no reasoning and
            // no tool call. The stream finished without a transport error,
            // so the NoContentStreamDeath resume above never sees it; it
            // is the same transient failure all the same. Nothing was
            // persisted for this response, so the first retry re-issues
            // the identical request, the second carries the request-scoped
            // nudge, and after that the turn fails visibly below.
            let empty_clean_stop = no_sendable_assistant_content
                && !zero_tool_text_call
                && !has_provider_reasoning
                && stream_errors == 0
                && stop_reason.is_some()
                && !is_output_limit_stop_reason(stop_reason.as_deref())
                && should_fail_no_sendable_content(
                    tool_uses.is_empty(),
                    progress.turn_error.is_none(),
                    self.cancel_token.is_cancelled(),
                    !pending_steers.is_empty(),
                    false,
                );
            if empty_clean_stop
                && let Some(retry) = plan_empty_stop_retry(progress.empty_stop_retries)
            {
                progress.empty_stop_retries += 1;
                turn.stop_diagnostics.empty_stop_retries = progress.empty_stop_retries;
                let attempt = progress.empty_stop_retries;
                let reason = stop_reason_detail(stop_reason.as_deref());
                let how = match retry {
                    EmptyStopRetry::ExactPrefix => "re-requesting the answer",
                    EmptyStopRetry::Nudged => {
                        let text = self
                            .config
                            .reasoning_only_reprompt_message
                            .clone()
                            .unwrap_or_else(|| {
                                codewhale_runtime::config::DEFAULT_REASONING_ONLY_REPROMPT_MESSAGE
                                    .to_string()
                            });
                        if !text.trim().is_empty() {
                            progress.reasoning_only_nudge = Some(text);
                        }
                        "re-requesting the answer with a nudge"
                    }
                };
                codewhale_runtime::logging::warn(format!(
                    "Model returned terminal stop reason `{reason}` with no answer or tool call (attempt {attempt}/{EMPTY_STOP_MAX_RETRIES}); {how}"
                ));
                let _ = self.send_retry_status(format!(
                    "Retry attempt: empty-stop {attempt}/{EMPTY_STOP_MAX_RETRIES}; no answer, reasoning or tool call; {how}"
                ))
                .await;
                return PhaseResult::Retry;
            }

            if no_sendable_assistant_content
                && should_fail_no_sendable_content(
                    tool_uses.is_empty(),
                    progress.turn_error.is_none(),
                    self.cancel_token.is_cancelled(),
                    !pending_steers.is_empty(),
                    false,
                )
            {
                let message = if zero_tool_text_call {
                    "Model answered only with a tool call, and this turn offers no tools."
                        .to_string()
                } else if is_output_limit_stop_reason(stop_reason.as_deref()) {
                    // With or without streamed reasoning: a provider that
                    // keeps its reasoning to itself still spends the
                    // allowance on it (#6889).
                    format!(
                        "Model reached the response output limit with no answer or tool call (requested allowance: {} tokens, including reasoning).",
                        prepared_output_tokens
                    )
                } else if has_provider_reasoning {
                    let reason = codewhale_models::stop_reason_detail(stop_reason.as_deref());
                    format!(
                        "Model returned reasoning but no answer or tool call; the provider response was incomplete (stop reason: {}).",
                        reason
                            .chars()
                            .flat_map(char::escape_default)
                            .take(120)
                            .collect::<String>()
                    )
                } else if let Some(reason) = stop_reason.as_deref() {
                    if progress.empty_stop_retries > 0 {
                        format!(
                            "Model returned terminal stop reason `{reason}` with no answer or tool call (after {empty_stop_retries} retries).",
                            empty_stop_retries = progress.empty_stop_retries
                        )
                    } else {
                        format!(
                            "Model returned terminal stop reason `{reason}` with no answer or tool call."
                        )
                    }
                } else {
                    "Model stream ended with no answer or tool call.".to_string()
                };
                codewhale_runtime::logging::warn(&message);
                progress.turn_error = Some(message.clone());
                let _ = self
                    .send_event(Event::error(ErrorEnvelope::classify(message, true)))
                    .await;
            }

            if progress.turn_error.is_none() {
                if !turn.budget_exhausted_final_report {
                    turn.stop_diagnostics.reason = Some(TurnStopReason::ProviderNoToolCall);
                }
                // This branch received no calls and dispatches no tools.
                turn.stop_diagnostics.last_response_tool_calls_suppressed = Some(0);
            }
            return PhaseResult::Break;
        }

        PhaseResult::Ready(AcceptedModelStep {
            current_text_visible,
            tool_uses,
            pending_steers,
            output_limit_truncated,
            zero_tool_turn,
            zero_tool_text_call,
            fleet_report_response,
            fleet_no_progress_report,
            has_sendable_assistant_content,
            has_provider_reasoning,
            no_sendable_assistant_content,
            stop_reason,
            stream_errors,
            prepared_output_tokens,
        })
    }
}

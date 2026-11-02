//! One private phase of the existing Engine turn loop.

use super::*;
use crate::rlm::bridge::wait_for_deadline;

impl Engine {
    pub(super) async fn run_inline_repl_phase(
        &mut self,
        turn: &mut TurnContext,
        tool_policy: &ToolSurfacePolicy,
        progress: &mut TurnLoopProgress,
        client: &SharedModelClient,
        current_text_visible: &str,
        has_sendable_assistant_content: bool,
    ) -> PhaseResult<()> {
        let tool_registry = Some(&tool_policy.registry);
        // Inline ```repl execution — the normal Agent working kernel.
        // The kernel is session-scoped: refresh its inspectable context
        // for this model step, but preserve Python variables/imports
        // from earlier steps. That keeps the simple `repl` route useful
        // for sustained work instead of forcing the model through a
        // separate open/eval/configure control surface.

        // The kernel runs model-written Python, so it answers to the
        // same command gate as `code_execution`: a narrowed tool
        // surface (`exec --allowed-tools …`, or plain `exec`'s zero-tool
        // surface, #6510) must not execute code through a fence.
        // Plan mode withholds `code_execution` from the catalog, and a
        // fence is not a way around that: it runs only when the tool is
        // on this turn's surface, and only after the same approval.
        let repl_fence_present = has_sendable_assistant_content
            && codewhale_repl::sandbox::has_repl_block(current_text_visible);
        let repl_fence_offered =
            code_execution_offered(progress.mode, &progress.tool_catalog, tool_policy);
        let mut repl_fence_skip_reason = (repl_fence_present && !repl_fence_offered)
            .then(|| "code execution is not available on this turn".to_string());
        let repl_blocks = if repl_fence_present && repl_fence_offered {
            codewhale_repl::sandbox::extract_repl_blocks(current_text_visible)
        } else {
            Vec::new()
        };
        if !repl_blocks.is_empty() {
            let approval_id = format!("{}-repl-{}", turn.id, turn.step);
            repl_fence_skip_reason = if let Some(state) = self.rlm_host.as_ref() {
                self.rlm_round_refusal(&repl_blocks[0].code, state.gate.as_ref())
                    .await
            } else {
                self.repl_fence_blocked_reason(
                    &repl_blocks,
                    "the reply's ```repl block(s) in the session REPL kernel",
                    &approval_id,
                    client.as_ref(),
                    turn,
                    tool_policy,
                    &progress.tool_catalog,
                    tool_registry,
                    &mut progress.active_tool_names,
                    &mut progress.tool_call_budget,
                    progress.mode,
                    progress.fleet_denial_guard.as_ref(),
                )
                .await
            };
            // Admission may have applied a pending posture change.
            progress.mode = self.current_mode;
            if self.turn_wall_clock.exhausted() {
                let reason = "parent turn deadline exhausted before REPL execution".to_string();
                repl_fence_skip_reason = Some(reason.clone());
                progress.turn_error = Some(reason);
            }
        }
        if let Some(reason) = repl_fence_skip_reason.as_deref() {
            if self.rlm_host.is_some() {
                return PhaseResult::Return((
                    TurnOutcomeStatus::Failed,
                    Some(format!("RLM code was not run: {reason}")),
                ));
            }
            let _ = self
                .send_event(Event::status(format!("REPL block not run: {reason}")))
                .await;
        }
        if !repl_blocks.is_empty() && repl_fence_skip_reason.is_none() {
            let repl_deadline = self.nested_work_deadline();
            // A kernel left broken by a dropped turn refuses every round; kill it.
            drop(self.repl_kernel.take_if(|kernel| kernel.is_broken()));
            if self.repl_kernel.is_none() {
                let startup = tokio::select! {
                    biased;
                    () = self.cancel_token.cancelled() => {
                        Err("REPL startup cancelled".into())
                    }
                    () = wait_for_deadline(repl_deadline) => Err("parent turn deadline reached during REPL startup".into()),
                    result = codewhale_repl::runtime::PythonRuntime::new() => result,
                };
                self.repl_kernel = match startup {
                    Ok(runtime) => Some(runtime),
                    Err(e) => {
                        let _ = self
                            .send_event(Event::status(format!("REPL init failed: {e}")))
                            .await;
                        progress.turn_error = Some(format!("REPL init failed: {e}"));
                        return PhaseResult::Break;
                    }
                };
            }

            if self.rlm_host.is_none() {
                let kernel_context = self.repl_kernel_context();
                let refresh_result = tokio::select! {
                    biased;
                    () = self.cancel_token.cancelled() => {
                        Err("REPL context refresh cancelled".into())
                    }
                    () = wait_for_deadline(repl_deadline) => Err("parent turn deadline reached during REPL context refresh".into()),
                    result = self.repl_kernel
                        .as_mut()
                        .expect("REPL kernel initialized above")
                        .replace_context(&kernel_context) => result,
                };
                if let Err(e) = refresh_result {
                    // A broken subprocess cannot be trusted to retain
                    // state. Drop it so a later model step gets a clean,
                    // freshly bootstrapped kernel instead of repeating a
                    // hidden failure.
                    self.repl_kernel = None;
                    let _ = self
                        .send_event(Event::status(format!("REPL context refresh failed: {e}")))
                        .await;
                    progress.turn_error = Some(format!("REPL context refresh failed: {e}"));
                    return PhaseResult::Break;
                }
            }

            // Child queries use the same object-safe client as the
            // root turn. This follows the user-selected provider and
            // lets deterministic/injected hosts exercise the exact
            // same kernel contract, rather than quietly dropping
            // programmatic recursion outside the legacy DeepSeek
            // client path.
            //
            // Depth 0: the approval above covers the code in the
            // fence, not code a child model writes later. A nested
            // `rlm(...)` from a fence degrades to a one-shot child
            // completion (text back to Python) instead of starting a
            // sub-RLM whose code rounds would run unapproved.
            let captured = self
                .rlm_host
                .as_ref()
                .map(|state| Arc::clone(&state.caller))
                .or_else(|| {
                    self.live_tool_context(tool_registry)
                        .and_then(|context| context.rlm_caller.clone())
                });
            let bridge = captured.as_deref().map(|caller| {
                let depth = self.rlm_host.as_ref().map_or(0, |state| match state.mode {
                    crate::core::engine::rlm_host::RlmMode::Completion => 0,
                    crate::core::engine::rlm_host::RlmMode::Recursive { depth_remaining } => {
                        depth_remaining
                    }
                });
                let usage = self
                    .rlm_host
                    .as_ref()
                    .map_or_else(crate::rlm::bridge::RlmUsageAccumulator::new, |state| {
                        state.usage.clone()
                    });
                crate::rlm::RlmBridge::with_usage_accumulator(caller, depth, usage)
                    .with_events(self.tx_event.clone())
                    .with_deadline(repl_deadline)
                    .with_gate(self.rlm_host.as_ref().and_then(|state| state.gate.clone()))
            });
            let repl_started = Instant::now();

            let mut final_result: Option<String> = None;
            let mut kernel_failed = false;
            for (i, block) in repl_blocks.iter().enumerate() {
                let round_num = i + 1;
                let _ = self
                    .send_event(Event::status(format!(
                        "REPL round {round_num}: executing..."
                    )))
                    .await;

                // Dropping the cancelled round also stops its owned
                // RPC/forwarder futures. The ledger below still
                // accounts completed and pending provider requests;
                // the kernel is discarded after any round failure.
                let round_result = tokio::select! {
                    biased;
                    () = self.cancel_token.cancelled() => {
                        Err("REPL execution cancelled".into())
                    }
                    () = wait_for_deadline(repl_deadline) => Err("REPL execution reached the parent turn deadline".into()),
                    result = self.repl_kernel
                        .as_mut()
                        .expect("REPL kernel stays alive during a round")
                        .run(&block.code, bridge.as_ref()) => result,
                };

                match round_result {
                    Ok(round) => {
                        if let Some(state) = self.rlm_host.as_mut() {
                            state.total_rpcs = state.total_rpcs.saturating_add(round.rpc_count);
                            let round_output = if round.has_error {
                                format!("stdout:\n{}\nstderr:\n{}", round.stdout, round.stderr)
                            } else {
                                round.stdout.clone()
                            };
                            let stdout = crate::rlm::turn::truncate_text(
                                &round_output,
                                crate::rlm::turn::STDOUT_METADATA_PREVIEW_LEN,
                            );
                            state.trace.push(crate::rlm::turn::RlmRoundTrace {
                                round: state.model_rounds,
                                code_summary: crate::rlm::turn::summarize_code(&block.code),
                                stdout_preview: stdout.clone(),
                                had_error: round.has_error,
                                rpc_count: round.rpc_count,
                                elapsed_ms: u64::try_from(round.elapsed.as_millis())
                                    .unwrap_or(u64::MAX),
                            });
                            let feedback = crate::rlm::turn::metadata_text(
                                &state.prompt,
                                state.model_rounds,
                                Some(&block.code),
                                Some(&stdout),
                            );
                            if round.final_value.is_none() {
                                self.add_session_message(
                                    self.runtime_text_message_with_turn_metadata(
                                        feedback,
                                        UserInputProvenance::Runtime,
                                    ),
                                )
                                .await;
                            }
                        }
                        if let Some(val) = &round.final_value {
                            let _ = self
                                .send_event(Event::status(format!(
                                    "REPL round {round_num}: FINAL result obtained"
                                )))
                                .await;
                            final_result = Some(val.clone());
                            break;
                        }

                        let is_empty_round = !round.has_error
                            && round.stdout.trim().is_empty()
                            && round.stderr.trim().is_empty()
                            && round.rpc_count == 0;
                        if is_empty_round {
                            progress.consecutive_empty_repl_rounds =
                                progress.consecutive_empty_repl_rounds.saturating_add(1);
                            let feedback = format!(
                                "[Your emitted ```repl block (round {round_num}) produced no observable output. Inspect the state or print a result when needed, and finish when the task is complete.]\n[0 child query RPC(s)]"
                            );
                            self.add_session_message(self.runtime_text_message_with_turn_metadata(
                                feedback,
                                UserInputProvenance::Runtime,
                            ))
                            .await;
                        } else if self.rlm_host.is_some() {
                            progress.consecutive_empty_repl_rounds = 0;
                        } else {
                            progress.consecutive_empty_repl_rounds = 0;
                            let provenance_prefix =
                                format!("Your emitted ```repl block (round {round_num}) result:");
                            let feedback = if round.has_error {
                                format!(
                                    "{provenance_prefix} error\nstdout:\n{}\nstderr:\n{}",
                                    round.stdout, round.stderr
                                )
                            } else {
                                format!(
                                    "{provenance_prefix}\n[{} child query RPC(s)]\n{}",
                                    round.rpc_count, round.stdout
                                )
                            };
                            self.add_session_message(self.runtime_text_message_with_turn_metadata(
                                feedback,
                                UserInputProvenance::Runtime,
                            ))
                            .await;
                        }
                    }
                    Err(e) => {
                        let _ = self
                            .send_event(Event::status(format!(
                                "REPL round {round_num} failed: {e}"
                            )))
                            .await;
                        self.add_session_message(self.runtime_text_message_with_turn_metadata(
                            format!("[REPL round {round_num} execution failed]\n{e}"),
                            UserInputProvenance::Runtime,
                        ))
                        .await;
                        if self.rlm_host.is_some() {
                            progress.turn_error = Some(
                                if repl_deadline
                                    .is_some_and(|deadline| tokio::time::Instant::now() >= deadline)
                                {
                                    format!("RLM original wall-clock deadline exhausted: {e}")
                                } else {
                                    e.clone()
                                },
                            );
                        }
                        // A transport error or timeout means Python
                        // may still be executing unknown code. Do not
                        // send another block into that process or
                        // pretend its state is trustworthy.
                        kernel_failed = true;
                        break;
                    }
                }
            }

            if kernel_failed {
                self.repl_kernel = None;
                if self.rlm_host.is_some() {
                    return PhaseResult::Return((
                        TurnOutcomeStatus::Failed,
                        progress
                            .turn_error
                            .take()
                            .or_else(|| Some("RLM Python execution failed".into())),
                    ));
                }
            }

            // Programmatic child calls are real provider work, not
            // implementation detail. Fold their authoritative usage
            // into the parent turn exactly once, including failures
            // after a partial fan-out, so `/cost`, goals, and the
            // final receipt cannot undercount the working kernel.
            if self.rlm_host.is_none()
                && let Some(bridge) = bridge.as_ref()
            {
                let snapshot = bridge.usage_snapshot().await;
                turn.add_usage(&snapshot.usage);
                let residual_dropped_records = snapshot
                    .dropped_records
                    .saturating_sub(u64::try_from(snapshot.drop_records.len()).unwrap_or(u64::MAX));
                turn.add_routed_usage_dropped_records(residual_dropped_records);
                if usage_has_reported_data(&snapshot.usage) {
                    let _ = self
                        .send_event(Event::RoutedTurnUsage {
                            usage: snapshot.usage.clone(),
                            duration_ms: u64::try_from(repl_started.elapsed().as_millis())
                                .unwrap_or(u64::MAX),
                            first_token_ms: None,
                            request_ms: None,
                        })
                        .await;
                }
            }

            if let Some(final_val) = final_result {
                if let Some(state) = self.rlm_host.as_mut() {
                    state.final_answer = Some(final_val.clone());
                    state.termination = Some(crate::rlm::turn::RlmTermination::Final);
                }
                // Replace the assistant's text with the FINAL answer.
                if let Some(last_msg) = self.session.messages.last_mut()
                    && last_msg.role == "assistant"
                {
                    for block in &mut last_msg.content {
                        if let ContentBlock::Text { text, .. } = block {
                            *text = final_val;
                            break;
                        }
                    }
                }
                self.emit_session_updated().await;
                return PhaseResult::Break;
            }

            // No FINAL — let the model iterate with the feedback.
            let _ = self.send_event(Event::status(format!(
                            "Continuing — REPL round feedback (consecutive_empty={consecutive_empty_repl_rounds})", consecutive_empty_repl_rounds = progress.consecutive_empty_repl_rounds
                        )))
                        .await;
            turn.next_step();
            return PhaseResult::Retry;
        }

        PhaseResult::Ready(())
    }
    pub(super) async fn continue_rlm_model_step(
        &mut self,
        turn: &mut TurnContext,
        tool_policy: &ToolSurfacePolicy,
        progress: &mut TurnLoopProgress,
        client: &SharedModelClient,
        response: AcceptedModelStep,
    ) -> PhaseResult<AcceptedModelStep> {
        let text = response.current_text_visible;
        let state = self.rlm_host.as_mut().expect("RLM host");
        state.last_response = text.clone();
        if state.mode == crate::core::engine::rlm_host::RlmMode::Completion {
            if text.trim().is_empty() {
                return PhaseResult::Return((
                    TurnOutcomeStatus::Failed,
                    Some("empty LLM response".into()),
                ));
            }
            state.final_answer = Some(text);
            state.termination = Some(crate::rlm::turn::RlmTermination::Final);
            return PhaseResult::Break;
        }
        let text_final = crate::rlm::turn::parse_text_final(&text);
        if let Some(answer) = text_final.as_ref() {
            state.final_answer = Some(answer.clone());
            state.termination = Some(crate::rlm::turn::RlmTermination::Final);
            return PhaseResult::Break;
        }
        let Some(code) = crate::rlm::turn::extract_repl_code(&text) else {
            state.consecutive_no_code = state.consecutive_no_code.saturating_add(1);
            let reminder = "Continue the task with Python inside a ```repl … ``` fence when computation is needed. Inspect `_context` through bounded helpers when the answer depends on it. Return FINAL(value) or call finalize(value) when the task is complete.";
            self.add_session_message(self.runtime_text_message_with_turn_metadata(
                reminder.to_string(),
                UserInputProvenance::Runtime,
            ))
            .await;
            turn.next_step();
            return PhaseResult::Retry;
        };
        state.consecutive_no_code = 0;
        let canonical_fence = format!("```repl\n{code}\n``` ");
        match self
            .run_inline_repl_phase(turn, tool_policy, progress, client, &canonical_fence, true)
            .await
        {
            PhaseResult::Ready(()) => PhaseResult::Return((
                TurnOutcomeStatus::Failed,
                Some("RLM code was not available on the captured surface".into()),
            )),
            PhaseResult::Retry => PhaseResult::Retry,
            PhaseResult::Break => PhaseResult::Break,
            PhaseResult::Return(outcome) => PhaseResult::Return(outcome),
        }
    }

    async fn rlm_round_refusal(
        &self,
        code: &str,
        gate: Option<&crate::tools::codemode::NestedCallGate>,
    ) -> Option<String> {
        use crate::tools::codemode::NestedCallVerdict;
        let name = tool_catalog::CODE_EXECUTION_TOOL_NAME;
        let Some(gate) = gate else {
            return Some("no permission gate is serving this RLM turn".into());
        };
        match gate
            .ask(name.to_string(), serde_json::json!({"code":code}))
            .await
        {
            NestedCallVerdict::Run {
                name: admitted,
                input,
                ..
            } if admitted == name
                && input.get("code").and_then(serde_json::Value::as_str) == Some(code) =>
            {
                None
            }
            NestedCallVerdict::Refused { error, .. } => Some(error.to_string()),
            _ => Some("the admitted call was not this code".into()),
        }
    }
}

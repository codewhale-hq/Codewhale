//! One private phase of the existing Engine turn loop.

use super::*;

impl Engine {
    pub(super) async fn run_tool_batch_phase(
        &mut self,
        turn: &mut TurnContext,
        tool_policy: &ToolSurfacePolicy,
        progress: &mut TurnLoopProgress,
        client: &SharedModelClient,
        response: AcceptedModelStep,
    ) -> PhaseResult<()> {
        let tool_registry = Some(&tool_policy.registry);
        let AcceptedModelStep {
            current_text_visible,
            mut tool_uses,
            mut pending_steers,
            mut output_limit_truncated,
            fleet_report_response,
            fleet_no_progress_report,
            ..
        } = response;
        // A user can change Ask / Auto-Review / Full Access while the
        // provider is streaming. Apply the newest typed authority before
        // planning this tool batch; already-running tools are never
        // retroactively reclassified.
        let authority_changed_before_tools = self.apply_pending_runtime_authority().await;
        if authority_changed_before_tools {
            // A response requested as report-only never acquires execution
            // authority after it streamed. Reset only after pairing its
            // suppressed calls; the next response can use the new posture.
            if !fleet_report_response && let Some(guard) = progress.fleet_denial_guard.as_mut() {
                guard.reset();
                turn.stop_diagnostics
                    .permission_denial_rounds_without_progress = 0;
            }
            progress.mode = self.current_mode;
        }

        // Execute tools
        if self.shared_paused.lock().is_ok_and(|paused| *paused) {
            let _ = self.send_event(Event::status("Request was Paused")).await;
            self.add_interrupted_assistant_text(&current_text_visible)
                .await;
            return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
        }

        let tool_exec_lock = self.tool_exec_lock.clone();
        let mcp_pool = if !fleet_report_response
            && self.config.features.enabled(Feature::Mcp)
            && tool_uses.iter().any(|tool| {
                McpPool::is_mcp_tool(&tool.name) || tool.name == EXECUTE_TOOLS_TOOL_NAME
            }) {
            match self.ensure_mcp_pool().await {
                Ok(pool) => Some(pool),
                Err(err) => {
                    let _ = self.send_event(Event::status(err.to_string())).await;
                    None
                }
            }
        } else {
            None
        };

        // Tool discovery may be the first action after a model request
        // that overlapped MCP startup. Search the ready catalog now.
        self.refresh_boot_mcp_catalog(
            tool_policy,
            &mut progress.tool_catalog,
            &mut progress.active_tool_names,
        )
        .await;
        // Per-tool timeouts own a running tool, but the batch also covers the
        // engine's own post-tool bookkeeping, which no per-tool timeout sees.
        // The ceiling keeps that window owned by the heartbeat.
        self.turn_heartbeat.enter(
            super::turn_heartbeat::TurnPhase::Tools,
            Some(
                tool_uses
                    .iter()
                    .map(|tool| tool.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Some(super::turn_heartbeat::TOOLS_PHASE_BOUND),
        );
        let PlannedToolCalls {
            plans,
            hook_contexts,
            batch_sandbox_policy,
        } = self
            .plan_tool_calls(
                client.as_ref(),
                turn,
                tool_policy,
                &mut tool_uses,
                &progress.tool_catalog,
                tool_registry,
                &mut progress.active_tool_names,
                &mut progress.tool_call_budget,
                progress.mode,
                progress.fleet_denial_guard.as_ref(),
                ToolCallSource::Model,
            )
            .await;

        let origin_turn_id = turn.id.clone();
        let mut nested_gate_env = NestedGateEnv {
            client: client.as_ref(),
            turn: &mut *turn,
            tool_policy,
            tool_call_budget: &mut progress.tool_call_budget,
            fleet_denial_guard: progress.fleet_denial_guard.as_ref(),
            authority_changed: false,
        };
        let (outcomes, authority_changed_during_tools) = self
            .execute_planned_tools(
                plans,
                &origin_turn_id,
                &current_text_visible,
                &mut progress.tool_catalog,
                &mut progress.active_tool_names,
                tool_registry,
                tool_exec_lock,
                mcp_pool,
                &batch_sandbox_policy,
                &mut progress.mode,
                &mut nested_gate_env,
            )
            .await;

        let authority_changed = authority_changed_before_tools || authority_changed_during_tools;
        self.turn_heartbeat.enter(
            super::turn_heartbeat::TurnPhase::Preparing,
            None,
            Some(super::turn_heartbeat::PREPARING_PHASE_BOUND),
        );
        let denial_action = self
            .process_tool_results(
                client.as_ref(),
                tool_policy.strict_tool_mode,
                outcomes,
                turn,
                &mut progress.tool_catalog,
                &mut progress.active_tool_names,
                &hook_contexts,
                if authority_changed || fleet_report_response {
                    None
                } else {
                    progress.fleet_denial_guard.as_mut()
                },
            )
            .await;

        let accepted_steer_after_tools = !pending_steers.is_empty();
        if !pending_steers.is_empty() {
            for pending in pending_steers.drain(..) {
                let steer = pending.commit().trim().to_string();
                self.session
                    .working_set
                    .observe_user_message(&steer, &self.session.workspace);
                self.add_session_message(self.user_text_message_with_turn_metadata(steer))
                    .await;
            }
        }

        if let Some(stop) = turn.auto_review_denials.stop().map(str::to_string) {
            let _ = self.send_event(Event::status(stop.clone())).await;
            return PhaseResult::Return((TurnOutcomeStatus::Interrupted, Some(stop)));
        }

        if authority_changed || accepted_steer_after_tools {
            if let Some(guard) = progress.fleet_denial_guard.as_mut() {
                guard.reset();
                turn.stop_diagnostics
                    .permission_denial_rounds_without_progress = 0;
            }
        } else if fleet_no_progress_report {
            // Exactly one accepted report response, including empty,
            // reasoning-only, truncated or tool-producing responses.
            if self.cancel_token.is_cancelled() {
                return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
            }
            turn.stop_diagnostics.last_response_tool_calls_suppressed = Some(tool_uses.len());
            let error = if turn.budget_exhausted_final_report {
                // One response can serve both report requests; the
                // explicit budget retains its existing stop provenance.
                format!(
                    "Maximum model steps reached before completion (limit: {}, {})",
                    turn.max_steps,
                    turn.budget_source.key_label()
                )
            } else {
                turn.stop_diagnostics.reason = Some(TurnStopReason::NoProgress);
                FLEET_NO_PROGRESS_STOP.to_string()
            };
            if turn.budget_exhausted_final_report {
                self.post_turn_budget_stop(&error).await;
            } else {
                let _ = self.send_event(Event::status(error.clone())).await;
            }
            return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error)));
        } else {
            let notice = match denial_action {
                FleetDenialAction::Continue => None,
                FleetDenialAction::SwitchStrategy => {
                    turn.stop_diagnostics.permission_strategy_switches = turn
                        .stop_diagnostics
                        .permission_strategy_switches
                        .saturating_add(1);
                    Some(FLEET_STRATEGY_SWITCH_NOTICE)
                }
                FleetDenialAction::FinalReport => {
                    turn.stop_diagnostics.final_report_requested = true;
                    Some(FLEET_FINAL_REPORT_NOTICE)
                }
            };
            if let Some(notice) = notice {
                // Dynamic guard facts are append-only runtime history;
                // BASE_PROMPT and the session's pinned prefix stay intact.
                self.add_session_message(self.runtime_text_message_with_turn_metadata(
                    notice.to_string(),
                    UserInputProvenance::Runtime,
                ))
                .await;
            }
        }

        // Surface an output-limit truncation after the tool result so the
        // transcript stays well-formed (a `tool_result` must follow the
        // assistant `tool_use` directly) and the model can act on it.
        if let Some(reason) = output_limit_truncated.take() {
            self.add_session_message(
                    self.runtime_text_message_with_turn_metadata(
                        format!(
                            "[runtime] The provider stopped generation at its output limit (`{reason}`) before completing. Your last response was cut off. Continue from where you left off; do not repeat content already delivered."
                        ),
                        UserInputProvenance::Runtime,
                    ),
                )
                .await;
        }

        self.deliver_shell_completions("").await;

        // A successful tool step is productive progress, not a runaway
        // synthetic resume. Declared per-task tool budgets and max_steps
        // remain the explicit limits for tool-driven work.
        let _ = self
            .send_event(Event::status("Continuing — tool results".to_string()))
            .await;
        turn.next_step();

        PhaseResult::Ready(())
    }
}

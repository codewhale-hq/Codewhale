//! One private phase of the existing Engine turn loop.

use super::*;
use anyhow::anyhow;

impl Engine {
    pub(super) async fn run_model_step(
        &mut self,
        turn: &mut TurnContext,
        progress: &mut TurnLoopProgress,
        client: &SharedModelClient,
        prepared: PreparedModelStep,
    ) -> PhaseResult<AcceptedModelStep> {
        let PreparedModelStep {
            request: stream_request,
            zero_tool_turn,
            fleet_report_response,
        } = prepared;
        let child_job = self.child_job();
        if let Some(job) = child_job.as_ref() {
            if let Err(error) = job
                .project(
                    &self.session.messages,
                    job.steps(),
                    stream_request.system.as_ref(),
                )
                .await
            {
                return PhaseResult::Return((
                    TurnOutcomeStatus::Failed,
                    Some(format!(
                        "child checkpoint failed before dispatch: {error:#}"
                    )),
                ));
            }
            if job.authority.runtime.cancel_token.is_cancelled() {
                return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
            }
        }
        // Session metrics: the model call is measured from this dispatch
        // instant (connection setup included), and time-to-first-token is
        // the gap to the first content-bearing stream event.
        let request_dispatched_at = Instant::now();
        self.turn_heartbeat.enter(
            super::turn_heartbeat::TurnPhase::AwaitingModel,
            Some(format!(
                "{} / {}",
                self.api_provider_identity
                    .as_ref()
                    .map_or("unavailable", |identity| {
                        identity
                            .compatibility()
                            .map_or(identity.key.as_str(), |row| row.label)
                    }),
                stream_request.model
            )),
            Some(awaiting_model_bound(&self.config)),
        );
        let mut rlm_dispatch = None;
        let request_cancel = self.cancel_token.clone();
        let mut child_dispatch = None;
        let child_reporting = self.child_report();
        let child_logical_step = child_job.as_ref().map(|job| {
            if child_reporting {
                job.steps().saturating_add(1)
            } else {
                turn.steps_used().saturating_add(1)
            }
        });
        let retry_observation = self.request_retry_observation();
        let transport_retries = retry_observation.retries.clone();
        let stream_result = tokio::select! {
            biased;
            () = request_cancel.cancelled() => {
                turn.stop_diagnostics.transport_retries = turn.stop_diagnostics.transport_retries
                    .saturating_add(transport_retries.load(std::sync::atomic::Ordering::Relaxed));
                let _ = self.send_event(Event::status("Request cancelled")).await;
                return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
            }
            result = crate::llm_client::observe_request_retries(Some(retry_observation), async {
                rlm_dispatch = match self.rlm_provider_request(client, &stream_request).await {
                    Ok(request) => request,
                    Err(error) => return Err(anyhow!(error)),
                };
                turn.stop_diagnostics.model_requests_started = turn
                    .stop_diagnostics
                    .model_requests_started
                    .saturating_add(1);
                if let Some(job) = child_job.as_ref() {
                    let child = &job.authority;
                    let route = client.effective_route_envelope(&stream_request.model, chrono::Utc::now());
                    let source = format!("child:{}:turn:{}:request:{}:dispatch:{}", child.owner_agent_id, turn.id, turn.stop_diagnostics.model_requests_started, route.dispatched_at.timestamp_nanos_opt().unwrap_or_default());
                    child_dispatch = Some(job.dispatched(child_logical_step.expect("captured child step"), source, route));
                }
                if let Some(job) = child_job.as_ref() {
                    tokio::time::timeout(
                        job.authority.runtime.step_api_timeout,
                        client.create_message_stream(stream_request.clone()),
                    ).await.unwrap_or_else(|_| Err(anyhow::Error::new(LlmError::Timeout(
                        job.authority.runtime.step_api_timeout,
                    ))))
                } else {
                    client.create_message_stream(stream_request.clone()).await
                }
            }) => result,
        };
        turn.stop_diagnostics.transport_retries = turn
            .stop_diagnostics
            .transport_retries
            .saturating_add(transport_retries.load(std::sync::atomic::Ordering::Relaxed));
        let stream = match stream_result {
            Ok(s) => {
                // Physical-cache population, nested execution and result
                // projection alone do not establish model delivery.
                self.session
                    .working_set
                    .observe_navigation_delivery(&stream_request.messages);
                if let Some(job) = child_job.as_ref() {
                    job.provider_responded();
                }
                progress.context_recovery_attempts = 0;
                // The route answered, so any run of identical upstream
                // failures is over (#6889).
                self.repeated_upstream_failure = None;
                // A model has the question now; a later credential
                // failure in this turn (a token expiring mid-turn, say)
                // must not take it back (#6566).
                turn.unanswered_user_message = None;
                s
            }
            Err(e) => {
                if let Some(dispatch) = rlm_dispatch.as_mut() {
                    dispatch.settle_open_error(&e).await;
                }
                if let Some(job) = child_job.as_ref() {
                    job.provider_refused(&e);
                    job.response_settled(false);
                    if let Some(dispatch) = child_dispatch.as_mut() {
                        dispatch.settle_open_error(&e).await;
                    }
                    drop(child_dispatch.take());
                }
                if self.child_report() {
                    return PhaseResult::Return((
                        TurnOutcomeStatus::Failed,
                        Some("bounded Core report request failed".into()),
                    ));
                }
                // Replacement is permitted only if Core can still retract
                // this exact unsent user message. A runtime note or rewrite
                // cannot be silently replayed as the original task.
                if let Some(mark) = turn.unanswered_user_message
                    && self.can_retract_unanswered_user_message(mark)
                {
                    match self.admit_child_first_request_replacement(&e).await {
                        Ok(true) => match self.retract_child_unsent_message(mark).await {
                            Ok(true) => {
                                turn.unanswered_user_message = None;
                                self.emit_session_updated().await;
                                return PhaseResult::Return((
                                    TurnOutcomeStatus::Failed,
                                    Some(codewhale_config::persistence::redact_secrets(&format!(
                                        "{e:#}; approved captured replacement queued"
                                    ))),
                                ));
                            }
                            Ok(false) => {
                                self.clear_child_pending_route();
                            }
                            Err(error) => {
                                return PhaseResult::Return((
                                    TurnOutcomeStatus::Failed,
                                    Some(codewhale_config::persistence::redact_secrets(&format!(
                                        "{e:#}; unsent child checkpoint failed: {error:#}"
                                    ))),
                                ));
                            }
                        },
                        Err(error) => {
                            return PhaseResult::Return((
                                TurnOutcomeStatus::Failed,
                                Some(codewhale_config::persistence::redact_secrets(&format!(
                                    "{e:#}; {error:#}"
                                ))),
                            ));
                        }
                        Ok(false) => {}
                    }
                }
                if self.rlm_host.is_none()
                    && let Some(rejection) =
                        crate::llm_client::image_count_rejection(&format!("{e:#}").to_lowercase())
                    && let Some(result) = self
                        .recover_image_count_rejection(progress, &stream_request, rejection, &e)
                        .await
                {
                    return result;
                }
                if let Some(recovery) = self.recover_child_request(progress, &e).await {
                    return recovery;
                }
                // Recovery/classification keeps its existing input. Expanding
                // diagnostics must not introduce another model request.
                let message = self.decorate_auth_error_message(e.to_string());
                if self.rlm_host.is_none()
                    && is_context_length_error_message(&message)
                    && progress.context_recovery_attempts < MAX_CONTEXT_RECOVERY_ATTEMPTS
                    && self
                        .recover_context_overflow(
                            client.as_ref(),
                            stream_request.tools.as_deref(),
                            "provider context-length rejection",
                            turn,
                        )
                        .await
                {
                    progress.context_recovery_attempts =
                        progress.context_recovery_attempts.saturating_add(1);
                    return PhaseResult::Retry;
                }
                if self.rlm_host.is_none()
                    && is_image_input_rejection_message(&message)
                    && self.active_route_capabilities.image_input != CapabilityState::Unsupported
                    && !progress.image_rejection_recovered
                    && crate::image_attach::count_request_images(&stream_request.messages) > 0
                {
                    return self.resend_without_images(progress).await;
                }
                let display_message = self.decorate_auth_error_message(
                    initial_stream_error_user_message(&self.config.locale_tag, &e),
                );
                // Classified from the error's types across its whole
                // context chain, before `e` moves into the envelope: an
                // adapter's outer context must not hide a connect error,
                // and a provider's HTTP rejection must not pass for one
                // because its body text mentions a timeout (#6711).
                let open_transport_failure = crate::client::is_stream_open_transport_failure(&e);
                // #6889: read the upstream status before `e` moves, so a 5xx
                // that keeps repeating on this model can be told apart from
                // a transient one below.
                let upstream_status =
                    e.chain()
                        .find_map(|cause| match cause.downcast_ref::<LlmError>() {
                            Some(LlmError::ServerError { status, .. }) => Some(*status),
                            _ => None,
                        });
                let mut envelope =
                    crate::error_taxonomy::envelope_for_llm_error(e, message.clone());
                // #6699: the request never became a stream (connect
                // failure, response-header stall). The transport layer
                // already spent its own retries; re-issue the identical
                // request from here through the same bounded resume
                // budget every other stream failure spends. Nothing
                // streamed, so there is no fragment to keep or discard,
                // and no error event is emitted for an attempt that is
                // retried — an exhausted budget falls through to the
                // normal failure below. Only a failure with no response
                // headers qualifies; a provider rejection never does.
                if self.rlm_host.is_none()
                    && open_transport_failure
                    && !self.cancel_token.is_cancelled()
                    && let Some(attempt) = progress.stream_retry_budget.authorize()
                {
                    turn.stop_diagnostics.stream_resumes =
                        turn.stop_diagnostics.stream_resumes.saturating_add(1);
                    let _ = self.send_retry_status(format!(
                        "Retry attempt: stream-open {attempt}/{}; connection failed before response headers",
                        progress.stream_retry_budget.limit()
                    )).await;
                    crate::logging::warn(format!(
                        "Stream failed to open (attempt {attempt}/{}); retrying request: {message}",
                        progress.stream_retry_budget.limit()
                    ));
                    return PhaseResult::Retry;
                }
                if open_transport_failure && progress.stream_retry_budget.spent() > 0 {
                    let _ = self.send_retry_status(format!(
                        "Retry exhaustion: stream-open stopped after {} retries; connection failed before response headers",
                        progress.stream_retry_budget.spent()
                    )).await;
                }
                // #6889: this request is not being re-issued. When the same
                // model has now failed the same way more than once, say so
                // and name the way out; the provider's own text stays first.
                let display_message = match upstream_status {
                    Some(status) => match super::streaming::note_upstream_failure(
                        &mut self.repeated_upstream_failure,
                        &stream_request.model,
                        status,
                    ) {
                        Some(notice) => format!("{display_message}\n{notice}"),
                        None => display_message,
                    },
                    None => {
                        self.repeated_upstream_failure = None;
                        display_message
                    }
                };
                envelope.message = display_message.clone();
                // #6566: no model saw the question. Take it back out of
                // the session before reporting, so the next request does
                // not send it twice and a resumed session does not show
                // it twice; the code tells the host to hand the text back.
                if envelope.category == ErrorCategory::Authentication
                    && let Some(mark) = turn.unanswered_user_message.take()
                {
                    match self.retract_child_unsent_message(mark).await {
                        Ok(true) => {
                            envelope.code =
                                crate::error_taxonomy::CREDENTIAL_REJECTED_UNSENT_CODE.to_string();
                            self.emit_session_updated().await;
                        }
                        Ok(false) => {}
                        Err(error) => {
                            return PhaseResult::Return((
                                TurnOutcomeStatus::Failed,
                                Some(format!(
                                    "{display_message}; unsent child checkpoint failed: {error:#}"
                                )),
                            ));
                        }
                    }
                }
                progress.turn_error = Some(display_message);
                if envelope.severity == crate::error_taxonomy::ErrorSeverity::Warning {
                    envelope.severity = crate::error_taxonomy::ErrorSeverity::Error;
                }
                let _ = self.send_event(Event::error(envelope)).await;
                return PhaseResult::Return((
                    TurnOutcomeStatus::Failed,
                    progress.turn_error.take(),
                ));
            }
        };
        let StreamOutcome {
            current_text_raw,
            current_text_visible,
            current_thinking,
            current_thinking_signature,
            current_thinking_state,
            mut completed_thinking,
            mut tool_uses,
            usage,
            usage_reported,
            stop_reason,
            pending_message_complete,
            last_text_index,
            stream_errors,
            terminal_stream_error,
            pending_steers,
            pending_resume,
            stream_start,
            first_token_at,
            request_dispatched_at,
            stream_error,
            frame_error,
        } = self
            .process_stream(
                client.as_ref(),
                stream,
                &stream_request,
                request_dispatched_at,
                progress.stream_retry_budget.spent(),
                &mut turn.stop_diagnostics,
            )
            .await;
        self.turn_heartbeat.enter(
            super::turn_heartbeat::TurnPhase::Preparing,
            None,
            Some(super::turn_heartbeat::PREPARING_PHASE_BOUND),
        );
        if let Some(dispatch) = rlm_dispatch.as_mut() {
            dispatch
                .settle(&usage, stream_error.is_none() && stream_errors == 0)
                .await;
        }
        // C02-05: a response whose stream failed — a provider error frame,
        // a transport error, a stall, or a cap — is not a complete
        // response. Unless the retry below re-issues the request, nothing
        // it collected may execute or continue the turn.
        let response_stream_failed = stream_error.is_some();
        progress.turn_error = progress.turn_error.take().or(stream_error.clone());
        turn.stop_diagnostics
            .observe_provider_response(stop_reason.as_deref(), tool_uses.len());
        // Counts and terminal metadata only: never log messages, tool
        // arguments, credentials, or raw provider bodies.
        tracing::debug!(
            target: "provider_response_diagnostics",
            model_request = turn.stop_diagnostics.model_requests_started,
            prepared_output_limit_tokens = stream_request.max_tokens,
            finish_reason = ?turn.stop_diagnostics.last_provider_finish_reason,
            reported_usage = usage_reported,
            input_tokens = usage.input_tokens,
            output_tokens = usage.output_tokens,
            cached_input_tokens = ?usage.prompt_cache_hit_tokens,
            reasoning_tokens = ?usage.reasoning_tokens,
            decoded_tool_calls = tool_uses.len(),
            visible_text_chars = current_text_visible.chars().count(),
            "parent model response settled"
        );
        // #6889: a Chat Completions `stop` whose own usage equals the output
        // ceiling this request asked for was cut at that ceiling. From here
        // on it is the `length` stop it should have been, so it takes the
        // one output-limit path below (continue from the partial answer, or
        // fail by name when there is none) instead of passing for a finished
        // answer or being re-requested unchanged. The diagnostics above keep
        // the reason the provider sent.
        let provider_finish_reason = stop_reason.clone();
        let stop_reason = if stop_contradicts_output_ceiling(
            stop_reason.as_deref(),
            usage_reported.then_some(usage.output_tokens),
            stream_request.max_tokens,
        ) {
            crate::logging::warn(format!(
                "Provider reported stop reason `stop`, but its usage shows the whole requested output allowance was used ({} tokens, reasoning tokens: {:?}); treating the response as cut at the output limit.",
                usage.output_tokens, usage.reasoning_tokens
            ));
            Some(OUTPUT_CEILING_STOP_REASON.to_string())
        } else {
            stop_reason
        };
        // These belong to post-stream response assembly, not stream
        // consumption: blocks are built from the completed stream state,
        // and truncation is derived from its terminal stop reason below.
        let mut content_blocks: Vec<ContentBlock> = Vec::new();
        let mut output_limit_truncated: Option<String> = None;

        // Account for every provider response before deciding whether to
        // retry or accept it. A terminal stop reason followed by a
        // transport error is still a billed, incomplete response; it must
        // not be discarded and re-issued.
        if let Some(dispatch) = child_dispatch.as_mut() {
            dispatch
                .settle(
                    &usage,
                    !response_stream_failed && !self.cancel_token.is_cancelled(),
                )
                .await;
        }
        drop(child_dispatch.take());
        if let Some(job) = child_job.as_ref() {
            job.response_settled(!response_stream_failed);
        }
        turn.add_parent_usage(&usage);
        if let Some(child) = self.child_host.as_mut()
            && child.fork_cache_checked
            && !child.fork_trial_first_response_logged
            && !child_reporting
        {
            child.fork_trial_first_response_logged = true;
            #[allow(clippy::print_stderr)]
            if crate::prompt_zones::fork_trial_diag_enabled() {
                eprintln!(
                    "{}",
                    serde_json::json!({
                        "codewhale_fork_trial": "first_response",
                        "agent_id": child.authority.owner_agent_id,
                        "inherited": child.fork_cache_inherited.is_some(),
                        "usage_reported": usage_reported,
                        "stream_failed": response_stream_failed,
                        "input": usage.input_tokens,
                        "hit": usage.prompt_cache_hit_tokens,
                        "miss": usage.prompt_cache_miss_tokens,
                        "system_bytes": serde_json::to_vec(&stream_request.system).ok().map(|bytes| bytes.len()),
                        "tools_bytes": serde_json::to_vec(&stream_request.tools).ok().map(|bytes| bytes.len()),
                        "history_bytes": serde_json::to_vec(&stream_request.messages).ok().map(|bytes| bytes.len()),
                    })
                );
            }
        }
        if self.child_host.is_none()
            && let Ok(mut cell) = self.live_header.lock()
            && let Some(snapshot) = cell.as_mut()
        {
            snapshot.last_hit_tokens = usage.prompt_cache_hit_tokens;
        }
        turn.note_parent_prompt_len(self.session.messages.len());
        self.session.latest_parent_input_tokens = turn.latest_parent_input_tokens;
        if usage_reported {
            let _ = self
                .send_event(Event::TurnUsage {
                    max_output_tokens: turn.max_output_tokens.map(|_| stream_request.max_tokens),
                    usage: usage.clone(),
                    duration_ms: u64::try_from(stream_start.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                    first_token_ms: first_token_at.map(|at| {
                        u64::try_from(
                            at.saturating_duration_since(request_dispatched_at)
                                .as_millis(),
                        )
                        .unwrap_or(u64::MAX)
                    }),
                    request_ms: Some(
                        u64::try_from(request_dispatched_at.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                    ),
                    finish_reason: provider_finish_reason,
                })
                .await;
        }

        if let Some(state) = self.rlm_host.as_mut() {
            state.usage.record_nested_event(serde_json::json!({"run_id": state.run_id, "depth_remaining": match state.mode { rlm_host::RlmMode::Completion => 0, rlm_host::RlmMode::Recursive { depth_remaining } => depth_remaining }, "kind":"code", "content":current_text_visible})).await;
            if is_incomplete_stop_reason(stop_reason.as_deref())
                || stream_error.is_some()
                || stream_errors > 0
                || !tool_uses.is_empty()
            {
                let error = if !tool_uses.is_empty() {
                    "RLM provider emitted a structured tool call outside its pure Python surface"
                        .to_string()
                } else {
                    format!(
                        "Model response incomplete: {}",
                        stream_error
                            .as_deref()
                            .unwrap_or_else(|| stop_reason_detail(stop_reason.as_deref()))
                    )
                };
                self.add_interrupted_assistant_text(&current_text_visible)
                    .await;
                self.post_held_frame_error(&frame_error).await;
                return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error)));
            }
            // Rejected fragments remain in the interrupted Session/code receipt,
            // but cannot replace the last admitted answer or execute FINAL/REPL.
            state.last_response = current_text_visible.clone();
        }
        if response_stream_failed && self.child_host.is_some() && !self.child_report() {
            let error = if progress
                .turn_error
                .as_deref()
                .is_some_and(|reason| reason.contains("child step API request timed out"))
            {
                anyhow::Error::new(LlmError::Timeout(
                    child_job
                        .as_ref()
                        .expect("captured child")
                        .authority
                        .runtime
                        .step_api_timeout,
                ))
            } else {
                anyhow!(
                    "{}",
                    progress
                        .turn_error
                        .as_deref()
                        .unwrap_or("child stream failed")
                )
            };
            if !terminal_stream_error
                && let Some(recovery) = self.recover_child_request(progress, &error).await
            {
                return recovery;
            }
        }
        if !response_stream_failed {
            progress.child_request_retries = Default::default();
        }

        // The injected Child transport owns its response grammar, including
        // an explicitly configured Custom wire or an approved replacement.
        let protocol = self
            .child_request_protocol()
            .or_else(|| {
                self.active_route_endpoint
                    .as_ref()
                    .map(|endpoint| endpoint.protocol)
            })
            .unwrap_or(codewhale_config::provider::WireFormat::ChatCompletions);
        // No tool observation or replayable tool history is published before
        // this whole-response admission. Usage and visible text remain real.
        if let Err(error) = crate::client::validate_tool_call_ids_for_protocol(
            protocol,
            tool_uses.iter().map(|tool| tool.id.as_str()),
        ) {
            self.add_interrupted_assistant_text(&current_text_visible)
                .await;
            return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error.to_string())));
        }

        if self.cancel_token.is_cancelled() {
            let _ = self.send_event(Event::status("Request cancelled")).await;
            self.add_interrupted_assistant_text(&current_text_visible)
                .await;
            return PhaseResult::Return((TurnOutcomeStatus::Interrupted, None));
        }

        if is_incomplete_stop_reason(stop_reason.as_deref()) {
            let reason = stop_reason_detail(stop_reason.as_deref());
            if self.child_host.is_none()
                && is_output_limit_stop_reason(stop_reason.as_deref())
                && stream_errors == 0
            {
                // Degrade, don't kill the turn — but only when the stream
                // finished cleanly. A `max_tokens` stop followed by a
                // transport error is a billed incomplete response: charge
                // it and fail closed instead of continuing into a second
                // request. A generation limit on a complete stream is a
                // normal provider outcome, not an unrecoverable error:
                // accept whatever complete tool call or content was
                // produced and continue. The truncation is surfaced as a
                // bounded observation after the partial assistant message
                // is committed (and, for a tool-call response, after the
                // tool result is appended) so the transcript stays
                // well-formed.
                crate::logging::warn(format!(
                    "Model output truncated: provider stop reason `{reason}`; accepting partial response and continuing the turn."
                ));
                output_limit_truncated = Some(reason.to_string());
                // Fall through to the normal content/tool dispatch below.
            } else {
                self.settle_unadmitted_tool_calls(&tool_uses, &incomplete_tool_result(reason))
                    .await;
                // Do not emit MessageComplete: hosts must retain the visible
                // fragment as interrupted/failed rather than recording it as
                // a completed assistant item.
                self.add_interrupted_assistant_text(&current_text_visible)
                    .await;
                let error = if self.child_host.is_some() {
                    crate::tools::subagent::incomplete_subagent_response_failure(
                        stop_reason.as_deref(),
                    )
                } else {
                    format!(
                        "Model response incomplete: provider stop reason `{reason}`; no complete response or tool call was accepted."
                    )
                };
                crate::logging::warn(&error);
                self.post_held_frame_error(&frame_error).await;
                return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error)));
            }
        }

        // #103 Phase 3 — transparent retry. The inner loop above bails
        // when reqwest yields chunk decode errors three times in a row;
        // most of the time those are recoverable proxy / HTTP/2 issues
        // and the request can simply be re-issued. Re-issue silently up
        // to MAX_STREAM_RETRIES, but only when the stream produced
        // nothing actionable — if any tool call landed or text was
        // streamed, ship the partial state to the rest of the turn
        // pipeline so we don't double-bill the user by re-running it.
        // The post-content exceptions to that rule are the #2990
        // sleep-resume and the mid-stream network-drop resumes: those
        // discard the uncommitted fragment unless an operator watched
        // visible text land (see `StreamResume::InteractiveNetworkDrop`).
        //
        // The resume itself is typed state, consumed here by value, so
        // one drop schedules exactly one retry; and no resume path
        // appends a synthetic user message to the persisted
        // conversation — the retried request is the persisted
        // conversation re-issued, nothing else.
        if self.child_report() {
            let refusal = if !tool_uses.is_empty() {
                Some("provider returned a tool call; no call executed")
            } else if output_limit_truncated.is_some() {
                Some("report did not finish: provider output limit")
            } else if response_stream_failed {
                Some("provider call failed; no complete report accepted")
            } else if current_text_visible.trim().is_empty() {
                Some("report did not finish: empty response")
            } else {
                None
            };
            if let Some(refusal) = refusal {
                self.post_held_frame_error(&frame_error).await;
                return PhaseResult::Return((
                    TurnOutcomeStatus::Failed,
                    Some(format!("bounded Core report refused: {refusal}")),
                ));
            }
        }
        // A flushed redacted block or non-blank thinking is received content,
        // the same rule `current_thinking` follows.
        let stream_died_with_nothing = stream_errors > 0
            && !terminal_stream_error
            && tool_uses.is_empty()
            && current_text_visible.trim().is_empty()
            && current_thinking.trim().is_empty()
            && completed_thinking.iter().all(|block| {
                matches!(
                    block,
                    ContentBlock::Thinking { thinking, redacted_data: None, .. }
                        if thinking.trim().is_empty()
                )
            })
            && !pending_message_complete;
        let pending_resume = match pending_resume {
            Some(resume) => Some(resume),
            None if stream_died_with_nothing => Some(StreamResume::NoContentStreamDeath),
            None => None,
        };
        if self.rlm_host.is_none()
            && self.child_host.is_none()
            && !terminal_stream_error
            && let Some(resume) = pending_resume
            && let Some(attempt) = progress.stream_retry_budget.authorize()
        {
            let limit = progress.stream_retry_budget.limit();
            turn.stop_diagnostics.stream_resumes =
                turn.stop_diagnostics.stream_resumes.saturating_add(1);
            let reason = match resume {
                StreamResume::AfterSleep => "system sleep interrupted the stream",
                StreamResume::HeadlessNetworkDrop => "network drop; incomplete reply discarded",
                StreamResume::InteractiveNetworkDrop => {
                    "network drop; continuing the admitted reply"
                }
                StreamResume::NoContentStreamDeath => {
                    "stream ended before a response was completed"
                }
            };
            let _ = self
                .send_retry_status(format!(
                    "Retry attempt: stream-resume {attempt}/{limit}; {reason}"
                ))
                .await;
            match resume {
                StreamResume::AfterSleep => {
                    crate::logging::warn(format!(
                        "Resuming after system sleep (attempt {attempt}/{limit}); discarding partial output and retrying request"
                    ));
                    // Finalize any partially-rendered assistant cell so
                    // the retried stream renders fresh instead of
                    // appending to the pre-sleep fragment.
                    if pending_message_complete {
                        let index = last_text_index.unwrap_or(0);
                        let _ = self.send_event(Event::MessageComplete { index }).await;
                    }
                }
                StreamResume::HeadlessNetworkDrop => {
                    crate::logging::warn(format!(
                        "Resuming headless turn after mid-stream network drop (attempt {attempt}/{limit}); discarding partial output and retrying request"
                    ));
                }
                StreamResume::InteractiveNetworkDrop => {
                    // Commit the partial assistant message so the retried
                    // request sees the prefix as already delivered. Build
                    // the blocks inline; the outer `content_blocks`
                    // variable is still empty at this point and will be
                    // rebuilt on the next round.
                    let mut resume_blocks: Vec<ContentBlock> = Vec::new();
                    // Thinking blocks the stream already terminated stay in
                    // the retry prefix, in wire order, ahead of the block
                    // that was still open when the stream died.
                    resume_blocks.append(&mut completed_thinking);
                    // A wire-only placeholder must not ride into the
                    // retry prefix as stored reasoning either.
                    let thinking_is_placeholder_only =
                        crate::client::is_reasoning_replay_placeholder(&current_thinking);
                    if (!current_thinking.is_empty() && !thinking_is_placeholder_only)
                        || current_thinking_state.is_some()
                    {
                        resume_blocks.push(ContentBlock::Thinking {
                            thinking: current_thinking.clone(),
                            signature: current_thinking_signature.clone(),
                            state: current_thinking_state.clone(),
                            redacted_data: None,
                        });
                    }
                    if !current_text_visible.is_empty() {
                        resume_blocks.push(ContentBlock::Text {
                            text: current_text_visible.clone(),
                            cache_control: None,
                        });
                    }
                    for tool in &tool_uses {
                        resume_blocks.push(ContentBlock::ToolUse {
                            execution_id: Some(tool.execution_id.clone()),
                            id: tool.id.clone(),
                            name: tool.name.clone(),
                            input: tool.input.clone(),
                            caller: tool.caller.clone(),
                            thought_signature: tool.thought_signature.clone(),
                        });
                    }
                    let has_sendable_assistant_content = resume_blocks.iter().any(|block| {
                        matches!(
                            block,
                            ContentBlock::Text { .. } | ContentBlock::ToolUse { .. }
                        )
                    });
                    if !has_sendable_assistant_content {
                        // Thinking-only drop: nothing visible streamed, so
                        // nothing is preserved and nothing is committed.
                        // The re-issued request is identical to the one
                        // that died. Neither the log line nor the status
                        // copy may claim a partial reply was preserved —
                        // that claim is what minted the fake `[runtime]`
                        // user turn in session 1589c05d.
                        crate::logging::warn(format!(
                            "Resuming interactive turn after mid-stream network drop (attempt {attempt}/{limit}); only hidden reasoning streamed — no partial reply to preserve, retrying request"
                        ));
                    } else {
                        crate::logging::warn(format!(
                            "Resuming interactive turn after mid-stream network drop (attempt {attempt}/{limit}); preserving partial reply and retrying request"
                        ));
                        // Finalize the partial text cell so the UI stops
                        // streaming and the retried content lands in a
                        // fresh cell instead of appending to an
                        // unfinished one.
                        if let Some(index) = last_text_index {
                            let _ = self.send_event(Event::MessageComplete { index }).await;
                        }
                        // Persist the fragment the operator already saw —
                        // exactly one assistant cell for it, and no
                        // synthetic user turn after it. The retried
                        // request therefore ends with this fragment, which
                        // is the provider-neutral "continue from here"
                        // contract; retry status receipts stay outside the
                        // session messages and provider request history.
                        // They never manufacture a user turn.
                        self.add_session_message(Message {
                            role: Role::Assistant,
                            content: resume_blocks,
                        })
                        .await;
                    }
                }
                StreamResume::NoContentStreamDeath => {
                    crate::logging::warn(format!(
                        "Stream died with no content (attempt {attempt}/{limit}); retrying request"
                    ));
                }
            }
            // Don't preserve the per-stream `turn_error` — we're
            // about to retry, and a successful retry should not
            // surface the transient error as the turn outcome.
            progress.turn_error = None;
            return PhaseResult::Retry;
        }
        // #6795: the request is not being re-issued, so a retryable error
        // frame that was held back is now the turn's outcome. Post it as the
        // non-recoverable envelope it was built as (severity Error), once.
        self.post_held_frame_error(&frame_error).await;
        if pending_resume.is_some() {
            if progress.stream_retry_budget.spent() > 0 {
                let _ = self
                    .send_retry_status(format!(
                        "Retry exhaustion: stream-resume stopped after {} retries; stream interrupted",
                        progress.stream_retry_budget.spent()
                    ))
                    .await;
            }
            crate::logging::warn(format!(
                "Stream retry budget exhausted ({} attempts); failing turn",
                progress.stream_retry_budget.spent()
            ));
        } else if stream_errors == 0 {
            if progress.stream_retry_budget.spent() > 0 && pending_message_complete {
                let _ = self
                    .send_retry_status(format!(
                        "Retry recovery: stream recovered after {} retries",
                        progress.stream_retry_budget.spent()
                    ))
                    .await;
            }
            // Healthy round → reset retry budget so we don't carry over
            // state from a previous bad round.
            progress.stream_retry_budget.reset();
        }

        let mut final_text = current_text_visible.clone();
        if tool_uses.is_empty() && tool_parser::has_tool_call_markers(&current_text_raw) {
            let parsed = tool_parser::parse_tool_calls(&current_text_raw);
            if parsed.tool_calls.len() > super::streaming::MAX_TOOL_CALLS_PER_RESPONSE {
                let envelope = super::streaming::tool_call_limit_error();
                let error = envelope.message.clone();
                turn.stop_diagnostics.last_response_tool_calls_suppressed =
                    Some(parsed.tool_calls.len());
                let _ = self.send_stream_event(Event::error(envelope)).await;
                self.add_interrupted_assistant_text(&current_text_visible)
                    .await;
                return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error)));
            }
            final_text = parsed.clean_text;
            for call in parsed.tool_calls {
                tool_uses.push(ToolUseState {
                    execution_id: self.new_tool_execution_id(),
                    id: call.id,
                    name: call.name,
                    input: call.args,
                    caller: None,
                    thought_signature: None,
                    input_buffer: String::new(),
                    input_parse_error: None,
                });
            }
            if let Err(error) = crate::client::validate_tool_call_ids_for_protocol(
                protocol,
                tool_uses.iter().map(|tool| tool.id.as_str()),
            ) {
                self.add_interrupted_assistant_text(&current_text_visible)
                    .await;
                return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error.to_string())));
            }
        }

        // C02-05: the one admission authority for a failed stream. Calls
        // collected before the failure are announced with an explicit
        // not-executed result and never reach planning, approval or a
        // handler; the visible text is kept as an interrupted fragment
        // and no tool_use enters history, so the transcript stays paired.
        // A retry never gets here: it `continue`d above with this batch
        // dropped, and the re-issued request streams its own calls.
        if response_stream_failed && !tool_uses.is_empty() {
            let error = progress
                .turn_error
                .clone()
                .unwrap_or_else(|| "provider stream failed".to_string());
            self.settle_unadmitted_tool_calls(
                &tool_uses,
                &stream_failed_tool_result(&summarize_text(&error, 200)),
            )
            .await;
            self.add_interrupted_assistant_text(&current_text_visible)
                .await;
            turn.stop_diagnostics.last_response_tool_calls_suppressed = Some(tool_uses.len());
            return PhaseResult::Return((TurnOutcomeStatus::Failed, Some(error)));
        }

        for tool in &tool_uses {
            let _ = self
                .send_event(Event::ToolCallStarted {
                    id: tool.execution_id.clone(),
                    model_call: Some(tool.model_call()),
                    name: tool.name.clone(),
                    input: final_tool_input(tool),
                })
                .await;
        }

        // Persist only reasoning the provider actually emitted. Some chat
        // wires require a non-empty `reasoning_content` field when an
        // assistant message carries tool calls; the route serializer adds
        // that compatibility value to the outgoing JSON only. Persisting
        // it here leaked an invented "(reasoning omitted)" block into the
        // transcript and every provider-neutral session replay.
        // `completed_thinking` holds every thinking/redacted block the
        // stream already terminated, in wire order; the guard below keeps
        // a trailing block whose stop event never arrived. Both must
        // survive — Anthropic replays the whole thinking sequence
        // unmodified.
        content_blocks.append(&mut completed_thinking);
        let thinking_is_placeholder_only =
            crate::client::is_reasoning_replay_placeholder(&current_thinking);
        if (!current_thinking.is_empty() && !thinking_is_placeholder_only)
            || current_thinking_state.is_some()
        {
            content_blocks.push(ContentBlock::Thinking {
                thinking: current_thinking.clone(),
                signature: current_thinking_signature.clone(),
                state: current_thinking_state.clone(),
                redacted_data: None,
            });
        }

        // A worker may cooperate with the strategy notice by immediately
        // reporting its blocker. No intervening useful work means that
        // report must not become a false Completed result.
        let fleet_no_progress_report = fleet_report_response
            || tool_uses.is_empty()
                && progress
                    .fleet_denial_guard
                    .as_ref()
                    .is_some_and(FleetDenialGuard::awaiting_strategy_change);

        // A protocol-level tool stop promises a call, unlike ordinary
        // text that merely describes an intended action. Keep that
        // distinction factual; never synthesize a tool or another request.
        if tool_uses.is_empty()
            && !fleet_no_progress_report
            && progress.turn_error.is_none()
            && matches!(stop_reason.as_deref(), Some("tool_calls" | "tool_use"))
        {
            turn.stop_diagnostics.reason = Some(TurnStopReason::ProviderToolCallMissing);
            turn.stop_diagnostics.last_response_tool_calls_suppressed = Some(0);
            self.add_interrupted_assistant_text(&current_text_visible)
                .await;
            let reason = stop_reason.as_deref().expect("matched tool stop");
            return PhaseResult::Return((
                TurnOutcomeStatus::Failed,
                Some(
                    codewhale_localization::tr(
                        codewhale_localization::resolve_locale(&self.config.locale_tag),
                        codewhale_localization::MessageId::ProviderToolCallMissing,
                    )
                    .replace("{reason}", reason),
                ),
            ));
        }

        for tool in &mut tool_uses {
            let Some(schema) = progress
                .tool_catalog
                .iter()
                .find(|candidate| candidate.name == tool.name)
                .map(|candidate| &candidate.input_schema)
            else {
                continue;
            };
            normalize_schema_json_containers(&mut tool.input, schema);
        }

        // A zero-tool turn (plain `exec`) has no tool channel, yet a model
        // can still answer with nothing but a tool call written as text
        // (DeepSeek's DSML). The stream filter strips the markup and leaves
        // at most whitespace, which is not an answer: persisting it would
        // end the run "successfully" on a blank line, and re-requesting
        // only reproduces the call. It is failed once below, by name.
        let zero_tool_text_call = zero_tool_turn
            && tool_uses.is_empty()
            && final_text.trim().is_empty()
            && contains_fake_tool_wrapper(&current_text_raw);
        if !final_text.is_empty() && !zero_tool_text_call {
            content_blocks.push(ContentBlock::Text {
                text: final_text,
                cache_control: None,
            });
        }
        for tool in &tool_uses {
            content_blocks.push(ContentBlock::ToolUse {
                execution_id: Some(tool.execution_id.clone()),
                id: tool.id.clone(),
                name: tool.name.clone(),
                input: tool.input.clone(),
                caller: tool.caller.clone(),
                thought_signature: tool.thought_signature.clone(),
            });
        }

        if pending_message_complete {
            let index = last_text_index.unwrap_or(0);
            let _ = self.send_event(Event::MessageComplete { index }).await;
        }

        // RLM is a structured tool call (`rlm_query`) handled by the
        // normal tool dispatch path; inline ```repl blocks (paper §2)
        // are executed below when tool_uses is empty.
        // DeepSeek chat API rejects assistant messages that contain only
        // Keep thinking for UI stream events, but persist only sendable
        // assistant turns in the conversation state.
        let has_sendable_assistant_content = content_blocks.iter().any(|block| {
            matches!(
                block,
                ContentBlock::Text { .. } | ContentBlock::ToolUse { .. }
            )
        });
        let has_provider_reasoning = content_blocks.iter().any(|block| {
            matches!(
                block,
                ContentBlock::Thinking {
                    thinking,
                    state,
                    redacted_data,
                    ..
                } if !thinking.trim().is_empty() || state.is_some() || redacted_data.is_some()
            )
        });

        // Issue #1727: did this turn produce ONLY a reasoning/thinking
        // block — empty content, no tool calls (e.g. gpt-oss via ollama's
        // harmony→OpenAI shim mapping to `reasoning_content`)? We do NOT
        // surface anything here: after this point the same turn can still
        // CONTINUE for pending steers (~below) or sub-agent completions,
        // and emitting now would show a spurious "turn ended" notice right
        // before the turn resumes. Capture the fact and decide later, at
        // the point the turn is certain to be finishing with no sendable
        // content (see the `tool_uses.is_empty()` tail).
        let no_sendable_assistant_content = !has_sendable_assistant_content;

        // Add assistant message to session
        if has_sendable_assistant_content {
            self.add_session_message(Message {
                role: Role::Assistant,
                content: content_blocks,
            })
            .await;
        }

        // C02-05: a failed stream that carried no tool call keeps its text
        // (above) and ends the turn with the stream's error. It never
        // runs a ```repl fence from the failed text, and never authorizes
        // another request for an output-limit continuation, a steer, a
        // sub-agent completion or a goal continuation.
        if response_stream_failed {
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
            prepared_output_tokens: stream_request.max_tokens,
        })
    }
    async fn resend_without_images(
        &mut self,
        progress: &mut TurnLoopProgress,
    ) -> PhaseResult<AcceptedModelStep> {
        progress.image_rejection_recovered = true;
        // This path tells the user itself; the resend must
        // not announce the same omission a second time.
        progress.image_omission_notified = true;
        self.active_route_capabilities.image_input = CapabilityState::Unsupported;
        crate::logging::warn(format!(
            "model {} rejected image content; resending with images replaced by text",
            self.session.model
        ));
        let status = codewhale_localization::tr(
            codewhale_localization::resolve_locale(&self.config.locale_tag),
            codewhale_localization::MessageId::ImageInputRejectedResent,
        )
        .replace("{model}", &self.session.model);
        let _ = self.send_event(Event::status(status)).await;
        PhaseResult::Retry
    }

    async fn recover_image_count_rejection(
        &mut self,
        progress: &mut TurnLoopProgress,
        request: &codewhale_models::MessageRequest,
        rejection: crate::llm_client::ImageCountRejection,
        error: &anyhow::Error,
    ) -> Option<PhaseResult<AcceptedModelStep>> {
        let sent = crate::image_attach::count_request_images(&request.messages);
        let model = self.session.model.clone();
        let cap = self.learn_image_cap(&model, sent, rejection)?;
        if cap == 0 {
            if self.active_route_capabilities.image_input != CapabilityState::Unsupported
                && !progress.image_rejection_recovered
            {
                return Some(self.resend_without_images(progress).await);
            }
        } else if progress.image_count_recoveries < MAX_IMAGE_COUNT_RECOVERIES {
            progress.image_count_recoveries = progress.image_count_recoveries.saturating_add(1);
            return Some(PhaseResult::Retry);
        }
        let hint = codewhale_localization::tr(
            codewhale_localization::resolve_locale(&self.config.locale_tag),
            codewhale_localization::MessageId::ImageCountRejected,
        )
        .replace("{model}", &model)
        .replace("{count}", &sent.to_string());
        let detail = self.decorate_auth_error_message(initial_stream_error_user_message(
            &self.config.locale_tag,
            error,
        ));
        let display_message = format!("{hint}\n{detail}");
        progress.turn_error = Some(display_message.clone());
        let _ = self
            .send_event(Event::error(
                crate::error_taxonomy::ErrorEnvelope::image_count_exceeded(display_message),
            ))
            .await;
        Some(PhaseResult::Return((
            TurnOutcomeStatus::Failed,
            progress.turn_error.take(),
        )))
    }

    async fn recover_child_request(
        &mut self,
        progress: &mut TurnLoopProgress,
        error: &anyhow::Error,
    ) -> Option<PhaseResult<AcceptedModelStep>> {
        use crate::tools::subagent::engine::ChildRequestRecovery;
        let job = self.child_job()?;
        if self.child_report() {
            return None;
        }
        let decision = progress
            .child_request_retries
            .decide(&job.authority.runtime, error)?;
        match decision {
            ChildRequestRecovery::Interrupted {
                checkpoint_reason,
                message,
            } => {
                self.child_host
                    .as_mut()
                    .expect("captured child recovery")
                    .request_stop_reason = Some(checkpoint_reason);
                Some(PhaseResult::Return((
                    TurnOutcomeStatus::Interrupted,
                    Some(
                        job.authority
                            .runtime
                            .client
                            .redact_model_bound_text(&message),
                    ),
                )))
            }
            ChildRequestRecovery::Retry { delay, note } => {
                let _ = self
                    .send_event(Event::status(
                        job.authority.runtime.client.redact_model_bound_text(&note),
                    ))
                    .await;
                tokio::select! {
                    biased;
                    () = self.cancel_token.cancelled() => Some(PhaseResult::Return((TurnOutcomeStatus::Interrupted, None))),
                    () = job.authority.runtime.cancel_token.cancelled() => Some(PhaseResult::Return((TurnOutcomeStatus::Interrupted, None))),
                    () = tokio::time::sleep(delay) => {
                        progress.turn_error = None;
                        Some(PhaseResult::Retry)
                    }
                }
            }
        }
    }
}

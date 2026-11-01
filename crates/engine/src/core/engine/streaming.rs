//! Streaming response state and guardrails.
//!
//! This module owns the local state used while decoding one model stream:
//! content block kind tracking, streamed tool-use buffers, transparent retry
//! policy, and scrubbers for text that looks like a forged tool-call wrapper.

use crate::core::events::TurnOutcomeStatus;
use codewhale_models::ToolCaller;
use std::time::Duration;

/// A send that did not enter the existing event queue. Cancellation is not
/// evidence that the consumer closed, and neither is user-visible delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventSendError {
    Cancelled,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EventReservationPolicy {
    /// Cancellation wins before admitting work or a new stream observation.
    Strict,
    /// Preserve a completed observation when capacity is already available.
    Receipt,
}

/// Reserve from the one event queue. Lifecycle and terminal handoff permits
/// are held locally; ordinary receipts consume theirs immediately. Receipts
/// may enter available capacity after cancellation, but cancellation always
/// releases a wait on a full queue. Idle sends have no turn token to cancel.
pub(super) async fn reserve_event_capacity(
    tx: &tokio::sync::mpsc::Sender<super::Event>,
    cancel: Option<&tokio_util::sync::CancellationToken>,
    policy: EventReservationPolicy,
) -> Result<tokio::sync::mpsc::OwnedPermit<super::Event>, EventSendError> {
    if policy == EventReservationPolicy::Receipt {
        match tx.clone().try_reserve_owned() {
            Ok(permit) => return Ok(permit),
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                return Err(EventSendError::Closed);
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
        }
    }
    let reserve = tx.clone().reserve_owned();
    match cancel {
        Some(cancel) => tokio::select! {
            biased;
            () = cancel.cancelled() => Err(EventSendError::Cancelled),
            result = reserve => result.map_err(|_| EventSendError::Closed),
        },
        None => reserve.await.map_err(|_| EventSendError::Closed),
    }
}

/// Selective quiet affects only attempt observations; completed summaries and
/// counters survive. Admission and delivery use the same existing queue guard.
async fn emit_retry_status(
    tx: &tokio::sync::mpsc::Sender<super::Event>,
    cancel: &tokio_util::sync::CancellationToken,
    quiet: bool,
    message: String,
) -> Result<(), EventSendError> {
    let attempt = message.starts_with("Retry attempt:");
    if quiet && attempt {
        return Ok(());
    }
    let policy = if attempt {
        EventReservationPolicy::Strict
    } else {
        EventReservationPolicy::Receipt
    };
    let permit = reserve_event_capacity(tx, Some(cancel), policy).await?;
    permit.send(super::Event::status(message));
    Ok(())
}

impl super::Engine {
    pub(super) fn request_retry_observation(&self) -> crate::llm_client::RequestRetryObservation {
        let tx = self.tx_event.clone();
        let cancel = self.cancel_token.clone();
        let quiet = self.api_config.notifications_config().quiet;
        crate::llm_client::RequestRetryObservation {
            wire_observations: Default::default(),
            retries: std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0)),
            emit: std::sync::Arc::new(move |message| {
                let tx = tx.clone();
                let cancel = cancel.clone();
                Box::pin(async move {
                    let _ = emit_retry_status(&tx, &cancel, quiet, message).await;
                })
            }),
        }
    }

    // The existing turn owns these cumulative counters. Closing observations
    // describe its actual outcome, without attributing a later tool failure
    // to an earlier provider response or changing any recovery budget.
    pub(super) async fn send_answer_retry_summary(
        &self,
        diagnostics: &crate::tool_inspection::TurnStopDiagnostics,
        status: TurnOutcomeStatus,
    ) {
        let (prefix, outcome) = match status {
            TurnOutcomeStatus::Completed => ("Retry recovery", "turn completed"),
            TurnOutcomeStatus::Failed => ("Retry stopped", "turn failed"),
            TurnOutcomeStatus::Interrupted => ("Retry interrupted", "turn interrupted"),
        };
        let reasoning_kind = if self.config.reasoning_only_on_output_limit {
            codewhale_localization::tr(
                codewhale_localization::resolve_locale(&self.config.locale_tag),
                codewhale_localization::MessageId::AnswerRetryCounter,
            )
            .into_owned()
        } else {
            "reasoning-only".to_string()
        };
        for (kind, retries, limit) in [
            (
                reasoning_kind.as_str(),
                diagnostics.reasoning_only_reprompts,
                self.config.reasoning_only_max_reprompts,
            ),
            (
                "empty-stop",
                diagnostics.empty_stop_retries,
                super::turn_loop::EMPTY_STOP_MAX_RETRIES,
            ),
        ] {
            if retries > 0 {
                let _ = self
                    .send_retry_status(format!(
                        "{prefix}: per-kind counter {kind} used {retries}/{limit} retries; {outcome}"
                    ))
                    .await;
            }
        }
    }

    pub(super) async fn send_retry_status(&self, message: String) -> Result<(), EventSendError> {
        emit_retry_status(
            &self.tx_event,
            &self.cancel_token,
            self.api_config.notifications_config().quiet,
            message,
        )
        .await
    }

    /// Stream observations always belong to the decoder's current turn,
    /// including direct test/embedding calls that do not enqueue an Op.
    pub(super) async fn send_stream_event(&self, event: super::Event) -> bool {
        match reserve_event_capacity(
            &self.tx_event,
            Some(&self.cancel_token),
            EventReservationPolicy::Strict,
        )
        .await
        {
            Ok(permit) => {
                permit.send(event);
                true
            }
            Err(_) => false,
        }
    }

    /// Every instance emitter uses the same queue authority. Available
    /// capacity preserves post-cancel usage/status receipts; cancellation
    /// releases a wait for capacity. An idle refresh must not inherit the
    /// token of an earlier interrupted turn. Stream/admission/terminal handoff
    /// reservations retain their strict cancellation floor.
    pub(super) async fn send_event(&self, event: super::Event) -> Result<(), EventSendError> {
        let cancel = self
            .turn_controls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .as_ref()
            .map(|control| control.cancel.clone());
        let permit = reserve_event_capacity(
            &self.tx_event,
            cancel.as_ref(),
            EventReservationPolicy::Receipt,
        )
        .await?;
        permit.send(event);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ContentBlockKind {
    Text,
    Thinking,
    ToolUse,
}

#[derive(Debug, Clone)]
pub(super) struct ToolUseState {
    pub(super) id: String,
    pub(super) execution_id: String,
    pub(super) name: String,
    pub(super) input: serde_json::Value,
    pub(super) caller: Option<ToolCaller>,
    /// Google thought signature captured on the tool call; replayed with the
    /// assistant tool-call message on later turns.
    pub(super) thought_signature: Option<String>,
    pub(super) input_buffer: String,
    pub(super) input_parse_error: Option<String>,
}

impl ToolUseState {
    pub(super) fn model_call(&self) -> crate::core::events::ModelToolCall {
        crate::core::events::ModelToolCall {
            provider_id: self.id.clone(),
            caller: self.caller.clone(),
            thought_signature: self.thought_signature.clone(),
        }
    }
}

/// A response can contain many empty tool starts without spending the byte
/// budget. Bound that batch before any call is retained or admitted. A lower
/// configured per-turn tool budget remains authoritative at execution.
pub(super) const MAX_TOOL_CALLS_PER_RESPONSE: usize = 256;

pub(super) fn tool_call_limit_error() -> codewhale_error_taxonomy::ErrorEnvelope {
    codewhale_error_taxonomy::ErrorEnvelope::new(
        codewhale_error_taxonomy::ErrorCategory::InvalidInput,
        codewhale_error_taxonomy::ErrorSeverity::Error,
        false,
        "response_tool_call_limit",
        format!(
            "Model response exceeded the maximum of {MAX_TOOL_CALLS_PER_RESPONSE} tool calls; no call from this response was executed"
        ),
    )
}

/// Decide whether a stream error is eligible for a transparent retry.
///
/// True only when ALL three conditions hold:
/// 1. No content has been received on the current attempt. Reissuing after
///    visible partial deltas needs a separate recovery policy. This content
///    check is not evidence that the provider consumed or billed zero tokens.
/// 2. We still have transparent-retry budget remaining (`max_attempts`,
///    `[tui].stream_max_transparent_retries`, default
///    [`codewhale_runtime::turn_budget::DEFAULT_STREAM_MAX_TRANSPARENT_RETRIES`]).
/// 3. The turn has not been cancelled.
///
/// Extracted as a pure function so the four #103 retry cases can be exercised
/// in unit tests without booting the full engine state machine.
pub(super) fn should_transparently_retry_stream(
    any_content_received: bool,
    transparent_attempts: u32,
    max_attempts: u32,
    cancelled: bool,
) -> bool {
    !any_content_received && transparent_attempts < max_attempts && !cancelled
}

/// Typed, engine-internal state for one mid-stream drop recovery.
///
/// This enum **is** the retry mechanism. A resumed turn used to append a
/// synthetic `[runtime]` *user* message to the persisted conversation, which
/// polluted the transcript and — when only hidden reasoning had streamed —
/// promised a preserved partial answer that never existed (0.9.10
/// regression). The retry is now modeled as this value, carried out of the
/// stream decoder in [`StreamOutcome`] and consumed exactly once per drop:
///
/// * it is never persisted to the user transcript, and
/// * nothing it triggers is serialized into the provider request history as
///   a user role — the retried request is simply the persisted conversation
///   re-issued, ending (when a visible fragment was preserved) with that
///   assistant fragment so the provider continues from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StreamResume {
    /// The stream died before anything actionable was streamed (#103
    /// Phase 3): discard the fragment and re-issue the identical request.
    NoContentStreamDeath,
    /// The host slept mid-stream (#2990): the partial output predates the
    /// sleep and no operator watched it — discard and re-issue.
    AfterSleep,
    /// Mid-stream network drop on a headless host (v0.9.4 Terminal-Bench
    /// P0): the fragment was never committed and no tool from it ran, so
    /// discard and re-issue the identical request.
    HeadlessNetworkDrop,
    /// Mid-stream network drop in the interactive TUI. A fragment with
    /// sendable content is preserved as the trailing assistant message and
    /// the request is re-issued; a thinking-only fragment has nothing
    /// visible to preserve, so it is discarded exactly like the headless
    /// resume and the copy must never claim otherwise.
    InteractiveNetworkDrop,
}

/// Bounded authorization for drop-resume retries.
///
/// Mechanism, not comment: [`StreamRetryBudget::authorize`] is the only way
/// to spend a resume and it returns `None` once `limit` resumes (default
/// [`DEFAULT_STREAM_MAX_RESUMES`]) have been issued, so no call site can loop past
/// the budget even if a guard predicate is relaxed. A healthy stream round
/// resets it.
#[derive(Debug)]
pub(super) struct StreamRetryBudget {
    spent: u32,
    limit: u32,
}

impl Default for StreamRetryBudget {
    fn default() -> Self {
        Self::with_limit(codewhale_runtime::turn_budget::DEFAULT_STREAM_MAX_RESUMES)
    }
}

impl StreamRetryBudget {
    /// A fresh budget allowing at most `limit` resumes.
    pub(super) fn with_limit(limit: u32) -> Self {
        Self { spent: 0, limit }
    }

    /// The configured resume ceiling.
    pub(super) fn limit(&self) -> u32 {
        self.limit
    }

    /// Drop-resumes already issued without a healthy round in between.
    pub(super) fn spent(&self) -> u32 {
        self.spent
    }

    /// Spend one resume and return its 1-based attempt number, or `None`
    /// when the budget is exhausted.
    pub(super) fn authorize(&mut self) -> Option<u32> {
        if self.spent >= self.limit {
            return None;
        }
        self.spent = self.spent.saturating_add(1);
        Some(self.spent)
    }

    /// A healthy round clears the chain: the next drop starts a fresh,
    /// still-bounded budget.
    pub(super) fn reset(&mut self) {
        self.spent = 0;
    }
}

/// Failures in a row, on one model with one upstream HTTP status, at which the
/// error starts saying so (#6889). Every counted failure has already spent its
/// transport retries, so two means two full rounds of them.
pub(super) const REPEATED_UPSTREAM_FAILURE_NOTICE_AT: u32 = 2;

/// Model requests that failed the same way with nothing succeeding in
/// between: same model, same upstream HTTP status (#6889). One 502 is a
/// transient fault worth retrying. The same 502 on every request is a model
/// that is listed but not serving, and no amount of retrying fixes that.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RepeatedUpstreamFailure {
    model: String,
    status: u16,
    count: u32,
}

/// Record one upstream failure for `model`. Returns the line to add to its
/// error once the same failure has repeated
/// [`REPEATED_UPSTREAM_FAILURE_NOTICE_AT`] times. A different model or status
/// starts a new count; the caller clears the streak when a request succeeds
/// or fails some other way.
pub(super) fn note_upstream_failure(
    streak: &mut Option<RepeatedUpstreamFailure>,
    model: &str,
    status: u16,
) -> Option<String> {
    let count = match streak {
        Some(prior) if prior.model == model && prior.status == status => {
            prior.count = prior.count.saturating_add(1);
            prior.count
        }
        _ => {
            *streak = Some(RepeatedUpstreamFailure {
                model: model.to_string(),
                status,
                count: 1,
            });
            1
        }
    };
    (count >= REPEATED_UPSTREAM_FAILURE_NOTICE_AT).then(|| {
        format!(
            "`{model}` has now failed {count} times in a row with HTTP {status}, after retries each time. A provider can list a model that is not serving requests. If this keeps happening, choose another model with /model."
        )
    })
}

#[cfg(test)]
mod repeated_upstream_failure_tests {
    use super::{REPEATED_UPSTREAM_FAILURE_NOTICE_AT, note_upstream_failure};

    #[test]
    fn the_same_upstream_failure_is_named_once_it_repeats() {
        let mut streak = None;
        for _ in 1..REPEATED_UPSTREAM_FAILURE_NOTICE_AT {
            assert_eq!(note_upstream_failure(&mut streak, "model-a", 502), None);
        }
        let notice = note_upstream_failure(&mut streak, "model-a", 502)
            .expect("a repeated identical failure is named");
        assert!(notice.contains("`model-a`"), "{notice}");
        assert!(notice.contains("HTTP 502"), "{notice}");
        assert!(
            notice.contains(&format!(
                "{REPEATED_UPSTREAM_FAILURE_NOTICE_AT} times in a row"
            )),
            "{notice}"
        );
        assert!(notice.contains("/model"), "{notice}");

        // It keeps counting while nothing changes.
        let next = note_upstream_failure(&mut streak, "model-a", 502).expect("still repeating");
        assert!(
            next.contains(&format!(
                "{} times in a row",
                REPEATED_UPSTREAM_FAILURE_NOTICE_AT + 1
            )),
            "{next}"
        );
    }

    #[test]
    fn a_different_model_or_status_or_a_success_starts_over() {
        let mut streak = None;
        assert_eq!(note_upstream_failure(&mut streak, "model-a", 502), None);
        // Another status is another failure, not a repeat of the first.
        assert_eq!(note_upstream_failure(&mut streak, "model-a", 503), None);
        // So is another model.
        assert_eq!(note_upstream_failure(&mut streak, "model-b", 503), None);
        // The caller clears the streak on a success.
        streak = None;
        assert_eq!(note_upstream_failure(&mut streak, "model-b", 503), None);
        assert!(note_upstream_failure(&mut streak, "model-b", 503).is_some());
    }
}

/// Wall-clock vs monotonic divergence above which we conclude the host slept
/// mid-stream (#2990). `Instant` pauses during system sleep (CLOCK_UPTIME_RAW
/// on macOS, CLOCK_MONOTONIC on Linux) while `SystemTime` keeps advancing, so
/// a large positive gap can only come from a suspend/resume cycle — ordinary
/// network flakes never produce one. Windows `Instant` may keep ticking
/// through sleep, in which case this simply never fires (no behavior change).
pub(super) const SLEEP_GAP_THRESHOLD: Duration = Duration::from_secs(10);

/// True when the gap between wall-clock and monotonic elapsed time since the
/// last stream progress says the host was suspended.
pub(super) fn sleep_gap_detected(monotonic_elapsed: Duration, wallclock_elapsed: Duration) -> bool {
    wallclock_elapsed.saturating_sub(monotonic_elapsed) > SLEEP_GAP_THRESHOLD
}

/// Decide whether a failed stream should be silently re-issued because the
/// host slept mid-turn (#2990).
///
/// Unlike the transparent retry (#103), this fires even after content has
/// streamed: the partial output predates the sleep, the user was not
/// watching, and re-running the identical request is the correct
/// user-visible behavior. The double-billing concern that blocks ordinary
/// post-content retries is accepted here because the alternative is a dead
/// turn the user must re-prompt (and pay for) anyway.
pub(super) fn should_resume_after_sleep(
    sleep_detected: bool,
    retry_attempts: u32,
    retry_limit: u32,
    cancelled: bool,
) -> bool {
    sleep_detected && retry_attempts < retry_limit && !cancelled
}

/// Decide whether a failed stream should be re-issued after a mid-stream
/// network drop in a headless host (`exec` / stream-json / app-server), even
/// though content already streamed.
///
/// This extends the #2990 sleep-resume contract to ordinary transport drops
/// for hosts with no operator watching: the partial assistant fragment has
/// not been committed to the conversation and no tool call from the
/// incomplete response has executed, so discarding the fragment and
/// re-issuing the identical request cannot duplicate side effects. The
/// double-billing risk that blocks post-content retries in the interactive
/// TUI (#103) is accepted here because the alternative is a dead turn that
/// forfeits the entire headless run — the exact tradeoff #2990 already makes
/// for sleep-resume. Interactive sessions keep the #103 surface-the-warning
/// behavior: the user saw the partial deltas, and replaying would render the
/// same prefix twice.
pub(super) fn should_resume_after_network_drop(
    headless_host: bool,
    network_class_error: bool,
    retry_attempts: u32,
    retry_limit: u32,
    cancelled: bool,
) -> bool {
    headless_host && network_class_error && retry_attempts < retry_limit && !cancelled
}

/// Decide whether an interactive TUI stream should be re-issued after a
/// mid-stream network drop, preserving a visible partial reply.
///
/// Unlike the headless resume, this keeps a sendable fragment: the user has
/// already seen the deltas, so the assistant message is committed and the
/// re-issued request ends with that fragment, which is the provider-neutral
/// continuation contract. No synthetic user turn is appended — the retry is
/// typed state ([`StreamResume::InteractiveNetworkDrop`]), invisible to the
/// transcript and to the provider request history as a user role. A
/// thinking-only fragment preserves nothing and must not be described as a
/// preserved reply. Tool calls are never resumed because an incomplete tool
/// call could be re-issued and duplicate side effects. Bounded by
/// `DEFAULT_STREAM_MAX_RESUMES` and gated on a network/timeout-class error so
/// model/parse/auth failures still surface normally.
pub(super) fn should_resume_interactive_after_network_drop(
    terminal_chrome_enabled: bool,
    network_class_error: bool,
    any_content_received: bool,
    tool_uses_empty: bool,
    retry_attempts: u32,
    retry_limit: u32,
    cancelled: bool,
) -> bool {
    terminal_chrome_enabled
        && network_class_error
        && any_content_received
        && tool_uses_empty
        && retry_attempts < retry_limit
        && !cancelled
}

/// Convert low-level reqwest/hyper stream read errors into an operator-facing
/// message. The raw provider error remains attached, but the lead sentence
/// explains why Codewhale may retry before any output and why it must surface
/// the warning once partial output has already streamed.
pub(super) fn stream_read_error_user_message(message: &str, any_content_received: bool) -> String {
    let lower = message.to_ascii_lowercase();
    let is_stream_read = lower.contains("stream read error")
        || lower.contains("error decoding response body")
        || lower.contains("chunk decode error")
        || lower.contains("body decode");
    if !is_stream_read {
        return message.to_string();
    }

    let retry_note = if any_content_received {
        "Some output had already streamed, so Codewhale is surfacing the warning instead of replaying the request and risking duplicated output."
    } else {
        "No output had streamed yet, so Codewhale will retry automatically while retry budget remains."
    };
    format!(
        "Provider stream connection dropped while reading the response body. {retry_note} Details: {message}"
    )
}

/// Wrapper shapes a model may emit as plain text instead of using the API tool
/// channel. Each pair is `(start, end)`; the tables below are projections of
/// this one and must stay in sync with it.
///
/// Three families are covered:
///
/// 1. Generic/Anthropic-style (`[TOOL_CALL]`, `<invoke …>`, `<function_calls>`).
/// 2. DSML wrappers, in fullwidth `｜` (U+FF5C) and ASCII `|` delimiters, upper
///    and lower case. DeepSeek also emits a doubled-delimiter form
///    (`<｜｜DSML｜｜ calls>`) when a request offers no tools; one-shot
///    `codewhale exec` printed it verbatim as the answer.
/// 3. **DeepSeek's native tool-call tokens** (#3880). DeepSeek's chat template
///    separates words with `▁` (U+2581 LOWER ONE EIGHTH BLOCK), not a space or
///    underscore, so `<｜tool▁calls▁begin｜>` does not match any DSML entry and
///    leaked into visible output. Both the `▁` and `_` separators are listed
///    because a partially-normalizing tokenizer can emit either, and both
///    delimiter forms because the ASCII fallback shows up in some renderings.
///
/// When adding a shape, add it here and to the two marker tables below.
/// `marker_tables_are_consistent` enforces that they agree.
pub const TOOL_CALL_MARKER_PAIRS: [(&str, &str); 30] = [
    ("[TOOL_CALL]", "[/TOOL_CALL]"),
    ("<codewhale:tool_call", "</codewhale:tool_call>"),
    ("<tool_call", "</tool_call>"),
    ("<invoke ", "</invoke>"),
    ("<function_calls>", "</function_calls>"),
    ("<｜DSML｜tool_calls>", "</｜DSML｜tool_calls>"),
    ("<｜DSML｜invoke ", "</｜DSML｜invoke>"),
    ("<|DSML|tool_calls>", "</|DSML|tool_calls>"),
    ("<|DSML|invoke ", "</|DSML|invoke>"),
    ("<|dsml|tool_calls>", "</|dsml|tool_calls>"),
    ("<|dsml|invoke ", "</|dsml|invoke>"),
    ("<｜｜DSML｜｜ calls>", "</｜｜DSML｜｜ calls>"),
    ("<｜｜DSML｜｜ invoke ", "</｜｜DSML｜｜ invoke>"),
    ("<|tool_calls>", "</|tool_calls>"),
    // DeepSeek native, fullwidth delimiters, U+2581 separator.
    ("<｜tool▁calls▁begin｜>", "<｜tool▁calls▁end｜>"),
    ("<｜tool▁call▁begin｜>", "<｜tool▁call▁end｜>"),
    ("<｜tool▁outputs▁begin｜>", "<｜tool▁outputs▁end｜>"),
    ("<｜tool▁output▁begin｜>", "<｜tool▁output▁end｜>"),
    // DeepSeek native, ASCII delimiters, U+2581 separator.
    ("<|tool▁calls▁begin|>", "<|tool▁calls▁end|>"),
    ("<|tool▁call▁begin|>", "<|tool▁call▁end|>"),
    ("<|tool▁outputs▁begin|>", "<|tool▁outputs▁end|>"),
    ("<|tool▁output▁begin|>", "<|tool▁output▁end|>"),
    // DeepSeek native, underscore separator.
    ("<｜tool_calls_begin｜>", "<｜tool_calls_end｜>"),
    ("<｜tool_call_begin｜>", "<｜tool_call_end｜>"),
    ("<｜tool_outputs_begin｜>", "<｜tool_outputs_end｜>"),
    ("<｜tool_output_begin｜>", "<｜tool_output_end｜>"),
    ("<|tool_calls_begin|>", "<|tool_calls_end|>"),
    ("<|tool_call_begin|>", "<|tool_call_end|>"),
    ("<|tool_outputs_begin|>", "<|tool_outputs_end|>"),
    ("<|tool_output_begin|>", "<|tool_output_end|>"),
];

pub const TOOL_CALL_START_MARKERS: [&str; 30] = [
    "[TOOL_CALL]",
    "<codewhale:tool_call",
    "<tool_call",
    "<invoke ",
    "<function_calls>",
    "<｜DSML｜tool_calls>",
    "<｜DSML｜invoke ",
    "<|DSML|tool_calls>",
    "<|DSML|invoke ",
    "<|dsml|tool_calls>",
    "<|dsml|invoke ",
    "<｜｜DSML｜｜ calls>",
    "<｜｜DSML｜｜ invoke ",
    "<|tool_calls>",
    "<｜tool▁calls▁begin｜>",
    "<｜tool▁call▁begin｜>",
    "<｜tool▁outputs▁begin｜>",
    "<｜tool▁output▁begin｜>",
    "<|tool▁calls▁begin|>",
    "<|tool▁call▁begin|>",
    "<|tool▁outputs▁begin|>",
    "<|tool▁output▁begin|>",
    "<｜tool_calls_begin｜>",
    "<｜tool_call_begin｜>",
    "<｜tool_outputs_begin｜>",
    "<｜tool_output_begin｜>",
    "<|tool_calls_begin|>",
    "<|tool_call_begin|>",
    "<|tool_outputs_begin|>",
    "<|tool_output_begin|>",
];

pub const TOOL_CALL_END_MARKERS: [&str; 30] = [
    "[/TOOL_CALL]",
    "</codewhale:tool_call>",
    "</tool_call>",
    "</invoke>",
    "</function_calls>",
    "</｜DSML｜tool_calls>",
    "</｜DSML｜invoke>",
    "</|DSML|tool_calls>",
    "</|DSML|invoke>",
    "</|dsml|tool_calls>",
    "</|dsml|invoke>",
    "</｜｜DSML｜｜ calls>",
    "</｜｜DSML｜｜ invoke>",
    "</|tool_calls>",
    "<｜tool▁calls▁end｜>",
    "<｜tool▁call▁end｜>",
    "<｜tool▁outputs▁end｜>",
    "<｜tool▁output▁end｜>",
    "<|tool▁calls▁end|>",
    "<|tool▁call▁end|>",
    "<|tool▁outputs▁end|>",
    "<|tool▁output▁end|>",
    "<｜tool_calls_end｜>",
    "<｜tool_call_end｜>",
    "<｜tool_outputs_end｜>",
    "<｜tool_output_end｜>",
    "<|tool_calls_end|>",
    "<|tool_call_end|>",
    "<|tool_outputs_end|>",
    "<|tool_output_end|>",
];

#[derive(Debug, Default)]
pub struct ToolCallDeltaFilterState {
    in_tool_call: bool,
    marker_carry: String,
    active_end_marker: Option<&'static str>,
}

/// Compact one-shot notice emitted when a model attempts to forge a tool-call
/// wrapper in plain text instead of using the API tool channel. The visible
/// content is still scrubbed; this exists so the user can see why their text
/// shrank.
pub const FAKE_WRAPPER_NOTICE: &str =
    "Stripped non-API tool-call wrapper from model output (use the API tool channel)";

/// True if `text` contains any of the known fake-wrapper start markers. Used by
/// the streaming loop to decide whether to emit `FAKE_WRAPPER_NOTICE`.
pub fn contains_fake_tool_wrapper(text: &str) -> bool {
    TOOL_CALL_START_MARKERS.iter().any(|m| text.contains(m))
}

fn find_first_marker(text: &str, markers: &[&str]) -> Option<(usize, usize)> {
    markers
        .iter()
        .filter_map(|marker| text.find(marker).map(|idx| (idx, marker.len())))
        .min_by_key(|(idx, _)| *idx)
}

fn find_first_start_marker(text: &str) -> Option<(usize, usize, &'static str)> {
    TOOL_CALL_MARKER_PAIRS
        .iter()
        .filter_map(|(start, end)| text.find(start).map(|idx| (idx, start.len(), *end)))
        .min_by_key(|(idx, _, _)| *idx)
}

/// Cheap rejection: every marker prefix ends with the marker's own first
/// byte, so a text without any marker's first byte cannot end with one.
/// Every tool-call marker starts with `<` or `[`, so this is one scan of a
/// usually short delta and keeps per-token `ends_with` probing off the hot
/// path for plain prose deltas.
fn fast_reject_marker_text(text: &str) -> bool {
    !text.bytes().any(|b| b == b'<' || b == b'[')
}

fn trailing_marker_prefix_len(text: &str, markers: &[&str]) -> usize {
    if fast_reject_marker_text(text) {
        return 0;
    }
    markers
        .iter()
        .flat_map(|marker| {
            marker
                .char_indices()
                .map(|(idx, _)| idx)
                .filter(|idx| *idx > 0)
                .chain(std::iter::once(marker.len()))
                .filter(|idx| *idx < marker.len())
                .filter(|idx| {
                    let prefix = &marker[..*idx];
                    text.ends_with(prefix)
                })
        })
        .max()
        .unwrap_or(0)
}

fn trailing_start_marker_prefix_len(text: &str) -> usize {
    if fast_reject_marker_text(text) {
        return 0;
    }
    TOOL_CALL_MARKER_PAIRS
        .iter()
        .flat_map(|(marker, _)| {
            marker
                .char_indices()
                .map(|(idx, _)| idx)
                .filter(|idx| *idx > 0)
                .chain(std::iter::once(marker.len()))
                .filter(|idx| *idx < marker.len())
                .filter(|idx| {
                    let prefix = &marker[..*idx];
                    text.ends_with(prefix)
                })
        })
        .max()
        .unwrap_or(0)
}

#[cfg(any(test, feature = "test-support"))]
pub fn filter_tool_call_delta(delta: &str, in_tool_call: &mut bool) -> String {
    let mut state = ToolCallDeltaFilterState {
        in_tool_call: *in_tool_call,
        ..ToolCallDeltaFilterState::default()
    };
    let output = filter_tool_call_delta_with_state(delta, &mut state);
    *in_tool_call = state.in_tool_call;
    output
}

pub fn filter_tool_call_delta_with_state(
    delta: &str,
    state: &mut ToolCallDeltaFilterState,
) -> String {
    if delta.is_empty() {
        return String::new();
    }

    let chunk;
    let mut rest = if state.marker_carry.is_empty() {
        delta
    } else {
        chunk = format!("{}{delta}", state.marker_carry);
        state.marker_carry.clear();
        &chunk
    };
    let mut output = String::new();

    loop {
        if state.in_tool_call {
            // Close only on the opener's own end marker when it is known.
            // Falling back to any end marker let a nested closer inside a
            // DSML block (`</｜DSML｜invoke>`) end the block whenever a delta
            // lacked the outer closer, leaking `</｜DSML｜tool_calls>`.
            let active_end_marker = state.active_end_marker;
            let found = match active_end_marker {
                Some(marker) => rest.find(marker).map(|idx| (idx, marker.len())),
                None => find_first_marker(rest, &TOOL_CALL_END_MARKERS),
            };
            let Some((idx, len)) = found else {
                let keep = active_end_marker.map_or_else(
                    || trailing_marker_prefix_len(rest, &TOOL_CALL_END_MARKERS),
                    |marker| trailing_marker_prefix_len(rest, &[marker]),
                );
                if keep > 0 {
                    state.marker_carry.push_str(&rest[rest.len() - keep..]);
                }
                break;
            };
            rest = &rest[idx + len..];
            state.in_tool_call = false;
            state.active_end_marker = None;
        } else {
            let Some((idx, len, end_marker)) = find_first_start_marker(rest) else {
                let keep = trailing_start_marker_prefix_len(rest);
                if keep > 0 {
                    let split = rest.len() - keep;
                    output.push_str(&rest[..split]);
                    state.marker_carry.push_str(&rest[split..]);
                } else {
                    output.push_str(rest);
                }
                break;
            };
            output.push_str(&rest[..idx]);
            rest = &rest[idx + len..];
            state.in_tool_call = true;
            state.active_end_marker = Some(end_marker);
        }
    }

    output
}

pub fn flush_tool_call_delta_state(state: &mut ToolCallDeltaFilterState) -> String {
    if state.in_tool_call {
        state.marker_carry.clear();
        return String::new();
    }
    std::mem::take(&mut state.marker_carry)
}

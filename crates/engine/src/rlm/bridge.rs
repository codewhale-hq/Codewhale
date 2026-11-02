//! RPC bridge that services `llm_query` / `rlm_query` calls coming back
//! from the long-lived Python REPL during an RLM turn.
//!
//! This is the spiritual successor to the HTTP sidecar from earlier
//! versions — except instead of binding a localhost port and routing
//! through `urllib`, requests come in through stdin/stdout and we just
//! submit an admitted call to the canonical Engine.
//!
//! The bridge tracks cumulative token usage and the recursion budget. For
//! `Rlm` / `RlmBatch` requests it submits an admitted Core turn at depth-1.
//! Python and its persistent session never retain this borrowed dispatcher.

use codewhale_models::Usage;
use codewhale_repl::runtime::{BatchResp, RpcDispatcher, RpcRequest, RpcResponse, SingleResp};
use futures_util::future::join_all;
use std::sync::Arc;
use std::sync::Mutex;
#[cfg(test)]
use std::time::Duration;
use uuid::Uuid;

pub const MAX_BATCH: usize = 16;

/// One pre-dispatch reservation in the shared routed-usage ledger.
#[derive(Debug, Clone, Copy)]
pub struct RlmUsageReservation {
    index: usize,
}

#[derive(Debug, Default)]
struct RlmUsageState {
    ledger_id: String,
    usage: Usage,
    records: Vec<Option<RlmUsageSlot>>,
    drop_records: Vec<crate::cost_status::RuntimeUsageDropRecord>,
    dropped_records: u64,
    nested_events: Vec<serde_json::Value>,
    nested_runs: usize,
}

#[derive(Debug)]
struct RlmUsageSlot {
    record: crate::cost_status::RuntimeUsageRecord,
    completed: bool,
}

/// Shared, bounded provider-call ledger for one complete RLM tree.
///
/// Every root, child, batch member, and recursive call reserves one slot
/// before invoking a provider. A distinct call is never coalesced merely
/// because it used the same route: its dispatch instant and frozen quote are
/// independent accounting evidence. Sharing one accumulator across recursion
/// makes the bound global instead of allowing every nested bridge to reset it.
#[derive(Debug, Clone)]
pub struct RlmUsageAccumulator {
    state: Arc<Mutex<RlmUsageState>>,
}

/// Atomic snapshot returned after all RPC work for a round has settled.
#[derive(Debug, Clone, Default)]
pub struct RlmUsageSnapshot {
    pub usage: Usage,
    pub records: Vec<crate::cost_status::RuntimeUsageRecord>,
    /// Exact frozen routes for provider-success responses that did not carry
    /// authoritative usage. Keeping these separate prevents a missing payload
    /// from becoming a priced zero-usage receipt.
    pub drop_records: Vec<crate::cost_status::RuntimeUsageDropRecord>,
    /// Calls whose execution/usage became ambiguous (for example a timeout).
    /// They are never represented as authoritative zero-usage responses.
    pub dropped_records: u64,
    /// Producer-owned nested loop events, retained in the enclosing tool body
    /// so every session host persists them through its existing result path.
    pub nested_events: Vec<serde_json::Value>,
}

impl RlmUsageAccumulator {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RlmUsageState {
                ledger_id: Uuid::new_v4().simple().to_string(),
                ..RlmUsageState::default()
            })),
        }
    }

    /// Reserve durable accounting capacity before a provider request.
    /// Definite transport failure cancels the slot; ambiguous cancellation is
    /// explicit incomplete coverage. Reaching the cap rejects before any
    /// unreceipted provider work can occur.
    pub async fn reserve(
        &self,
        route: crate::cost_status::EffectiveRouteEnvelope,
    ) -> std::result::Result<RlmUsageReservation, String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.records.len() == crate::cost_status::MAX_CHILD_USAGE_RECORDS {
            return Err(format!(
                "RLM provider-call receipt limit reached ({}); request rejected before dispatch",
                crate::cost_status::MAX_CHILD_USAGE_RECORDS
            ));
        }
        let index = state.records.len();
        let source_id = format!("rlm:{}:request:{index}", state.ledger_id);
        state.records.push(Some(RlmUsageSlot {
            record: crate::cost_status::RuntimeUsageRecord {
                source_id,
                usage: crate::cost_status::EffectiveRouteUsage {
                    route: route.sanitized_for_persistence(),
                    usage: Usage::default(),
                },
            },
            completed: false,
        }));
        Ok(RlmUsageReservation { index })
    }

    pub async fn source_id(&self, reservation: RlmUsageReservation) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .records
            .get(reservation.index)
            .and_then(Option::as_ref)
            .map(|slot| slot.record.source_id.clone())
    }

    /// Attach a provider's reported usage to its already-reserved exact route.
    pub async fn complete(&self, reservation: RlmUsageReservation, usage: &Usage) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let completed = if let Some(Some(slot)) = state.records.get_mut(reservation.index)
            && !slot.completed
        {
            crate::core::turn::add_usage_to(&mut slot.record.usage.usage, usage);
            slot.completed = true;
            true
        } else {
            false
        };
        if completed {
            crate::core::turn::add_usage_to(&mut state.usage, usage);
        }
    }

    /// Remove a reservation that never produced provider-reported usage.
    /// Ambiguous execution increments explicit incomplete coverage instead of
    /// being persisted as a priced-zero response.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn cancel(&self, reservation: RlmUsageReservation, coverage_unknown: bool) {
        self.cancel_sync(
            reservation,
            coverage_unknown,
            crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown,
        );
    }

    pub fn cancel_sync(
        &self,
        reservation: RlmUsageReservation,
        coverage_unknown: bool,
        reason: crate::cost_status::RuntimeUsageMissingReason,
    ) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cancelled = state.records.get_mut(reservation.index).and_then(|slot| {
            if slot.as_ref().is_some_and(|slot| !slot.completed) {
                slot.take()
            } else {
                None
            }
        });
        if let Some(slot) = cancelled
            && coverage_unknown
        {
            state
                .drop_records
                .push(crate::cost_status::RuntimeUsageDropRecord {
                    reason,
                    source_id: slot.record.source_id,
                    route: slot.record.usage.route,
                });
            state.dropped_records = state.dropped_records.saturating_add(1);
        }
    }

    /// Bound even nested runs that fail before their first provider request.
    /// Without this reservation repeated setup failures could grow event
    /// receipts while never consuming the existing provider-call bound.
    pub async fn reserve_nested_turn(&self) -> std::result::Result<(), String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.nested_runs == crate::cost_status::MAX_CHILD_USAGE_RECORDS {
            return Err("RLM nested-turn receipt limit reached before dispatch".into());
        }
        state.nested_runs += 1;
        Ok(())
    }

    pub async fn record_nested_event(&self, event: serde_json::Value) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .nested_events
            .push(event);
    }

    pub async fn snapshot(&self) -> RlmUsageSnapshot {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending = state
            .records
            .iter()
            .flatten()
            .filter(|slot| !slot.completed)
            .map(|slot| crate::cost_status::RuntimeUsageDropRecord {
                reason: crate::cost_status::RuntimeUsageMissingReason::RequestOutcomeUnknown,
                source_id: slot.record.source_id.clone(),
                route: slot.record.usage.route.clone(),
            })
            .collect::<Vec<_>>();
        let mut drop_records = state.drop_records.clone();
        drop_records.extend(pending.iter().cloned());
        RlmUsageSnapshot {
            nested_events: state.nested_events.clone(),
            usage: state.usage.clone(),
            records: state
                .records
                .iter()
                .flatten()
                .filter(|slot| slot.completed)
                .map(|slot| slot.record.clone())
                .collect(),
            drop_records,
            dropped_records: state
                .dropped_records
                .saturating_add(u64::try_from(pending.len()).unwrap_or(u64::MAX)),
        }
    }
}

pub async fn wait_for_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

/// A dispatcher borrowed by one Python round. Persistent kernels never own
/// this caller, its services or an Engine. All nested work ends with this borrow.
pub struct RlmBridge<'call> {
    caller: &'call crate::core::engine::rlm_host::CapturedRlmCaller,
    depth_remaining: u32,
    usage: RlmUsageAccumulator,
    events: Option<tokio::sync::mpsc::Sender<crate::core::events::Event>>,
    deadline: Option<tokio::time::Instant>,
    gate: Option<crate::tools::codemode::NestedCallGate>,
}

impl<'call> RlmBridge<'call> {
    pub fn new(
        caller: &'call crate::core::engine::rlm_host::CapturedRlmCaller,
        depth_remaining: u32,
    ) -> Self {
        Self::with_usage_accumulator(caller, depth_remaining, RlmUsageAccumulator::new())
    }

    pub fn with_usage_accumulator(
        caller: &'call crate::core::engine::rlm_host::CapturedRlmCaller,
        depth_remaining: u32,
        usage: RlmUsageAccumulator,
    ) -> Self {
        Self {
            caller,
            depth_remaining,
            usage,
            events: None,
            deadline: caller.deadline(),
            gate: None,
        }
    }

    pub fn with_gate(mut self, gate: Option<crate::tools::codemode::NestedCallGate>) -> Self {
        self.gate = gate;
        self
    }

    pub fn with_deadline(mut self, deadline: Option<tokio::time::Instant>) -> Self {
        self.deadline = self.deadline.into_iter().chain(deadline).min();
        self
    }

    pub fn deadline(&self) -> Option<tokio::time::Instant> {
        self.deadline
    }

    pub fn with_events(
        mut self,
        events: tokio::sync::mpsc::Sender<crate::core::events::Event>,
    ) -> Self {
        self.events = Some(events);
        self
    }

    pub async fn usage_snapshot(&self) -> RlmUsageSnapshot {
        self.usage.snapshot().await
    }

    async fn invoke(
        &self,
        prompt: String,
        mode: crate::core::engine::rlm_host::RlmMode,
        max_tokens: Option<u32>,
        system: Option<String>,
    ) -> SingleResp {
        let result = self
            .caller
            .dispatch(crate::core::engine::rlm_host::RlmInvocation {
                prompt,
                mode,
                max_tokens,
                task_instructions: system,
                deadline: self.deadline,
                gate: self.gate.clone(),
                events: self.events.clone(),
                usage: self.usage.clone(),
            })
            .await;
        SingleResp {
            text: result.answer,
            error: result.error,
        }
    }

    async fn dispatch_llm(
        &self,
        prompt: String,
        _model: Option<String>,
        max_tokens: Option<u32>,
        system: Option<String>,
    ) -> SingleResp {
        self.invoke(
            prompt,
            crate::core::engine::rlm_host::RlmMode::Completion,
            max_tokens,
            system,
        )
        .await
    }

    async fn dispatch_llm_batch(
        &self,
        prompts: Vec<String>,
        _model: Option<String>,
        dependency_mode: Option<String>,
    ) -> BatchResp {
        if let Some(resp) = batch_guard(prompts.len(), dependency_mode.as_deref()) {
            return resp;
        }
        BatchResp {
            results: join_all(
                prompts
                    .into_iter()
                    .map(|prompt| self.dispatch_llm(prompt, None, None, None)),
            )
            .await,
        }
    }

    pub async fn dispatch_rlm(&self, prompt: String, _model: Option<String>) -> SingleResp {
        if self.depth_remaining == 0 {
            return self.dispatch_llm(prompt, None, None, None).await;
        }
        self.invoke(
            prompt,
            crate::core::engine::rlm_host::RlmMode::Recursive {
                depth_remaining: self.depth_remaining.saturating_sub(1),
            },
            None,
            None,
        )
        .await
    }

    async fn dispatch_rlm_batch(
        &self,
        prompts: Vec<String>,
        _model: Option<String>,
        dependency_mode: Option<String>,
    ) -> BatchResp {
        if let Some(resp) = batch_guard(prompts.len(), dependency_mode.as_deref()) {
            return resp;
        }
        BatchResp {
            results: join_all(
                prompts
                    .into_iter()
                    .map(|prompt| self.dispatch_rlm(prompt, None)),
            )
            .await,
        }
    }
}

/// One nested sub-RLM event as a parent-stream status line, or `None` for an
/// event kind the nested loop does not produce.
pub fn nested_rlm_status_line(event: crate::core::events::Event, depth: u32) -> Option<String> {
    use crate::core::events::Event;
    let body = match event {
        Event::Status { message } => message,
        Event::MessageDelta { content, .. } => content.trim().to_string(),
        _ => return None,
    };
    Some(format!("{NESTED_RLM_STATUS_PREFIX}{depth}): {body}"))
}

/// Status-line prefix for forwarded nested sub-RLM events;
/// `core::events::status_visibility` classifies these as internal receipts.
pub const NESTED_RLM_STATUS_PREFIX: &str = "sub-RLM (depth ";

fn batch_guard(prompt_count: usize, dependency_mode: Option<&str>) -> Option<BatchResp> {
    if prompt_count == 0 {
        return Some(BatchResp { results: vec![] });
    }
    if prompt_count > MAX_BATCH {
        return Some(BatchResp {
            results: (0..prompt_count)
                .map(|_| SingleResp {
                    text: String::new(),
                    error: Some(format!("batch too large: {prompt_count} > {MAX_BATCH}")),
                })
                .collect(),
        });
    }
    let mode = dependency_mode
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_");
    if !matches!(
        mode.as_str(),
        "independent" | "parallel_safe" | "map_reduce"
    ) {
        return Some(BatchResp {
            results: (0..prompt_count)
                .map(|_| SingleResp {
                    text: String::new(),
                    error: Some(
                        "batch requires dependency_mode='independent'; use sub_query_sequence or sequential sub_query calls for dependent work"
                            .to_string(),
                    ),
                })
                .collect(),
        });
    }
    None
}

impl RpcDispatcher for RlmBridge<'_> {
    fn dispatch<'a>(
        &'a self,
        req: RpcRequest,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = RpcResponse> + Send + 'a>> {
        Box::pin(async move {
            match req {
                RpcRequest::Llm {
                    prompt,
                    model,
                    max_tokens,
                    system,
                } => {
                    RpcResponse::Single(self.dispatch_llm(prompt, model, max_tokens, system).await)
                }
                RpcRequest::LlmBatch {
                    prompts,
                    model,
                    dependency_mode,
                    safety_note: _,
                } => RpcResponse::Batch(
                    self.dispatch_llm_batch(prompts, model, dependency_mode)
                        .await,
                ),
                RpcRequest::Rlm { prompt, model } => {
                    RpcResponse::Single(self.dispatch_rlm(prompt, model).await)
                }
                RpcRequest::RlmBatch {
                    prompts,
                    model,
                    dependency_mode,
                    safety_note: _,
                } => RpcResponse::Batch(
                    self.dispatch_rlm_batch(prompts, model, dependency_mode)
                        .await,
                ),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::tests::rlm_host::{BridgeFixture, Replies};
    use crate::llm_client::mock::MockLlmClient;
    use anyhow::Result;
    use codewhale_models::{ContentBlock, MessageRequest, MessageResponse};
    use std::future::Future;
    use std::pin::Pin;

    fn mock_response_with_usage(text: &str, usage: Usage) -> MessageResponse {
        MessageResponse {
            id: "mock_msg".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
            model: "mock-model".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            usage,
        }
    }

    fn mock_response(text: &str, input_tokens: u32, output_tokens: u32) -> MessageResponse {
        mock_response_with_usage(
            text,
            Usage {
                input_tokens,
                output_tokens,
                ..Usage::default()
            },
        )
    }

    fn bridge_for(mock: Arc<MockLlmClient>, depth_remaining: u32) -> BridgeFixture {
        let client: Arc<dyn Replies> = mock;
        BridgeFixture::new(client, "child-model".to_string(), depth_remaining).with_gate(Some(
            crate::tools::codemode::NestedCallGate::admitting_for_test(),
        ))
    }

    #[tokio::test]
    async fn expired_parent_deadline_and_nested_receipt_bound_refuse_before_work() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        for depth in [0, 1] {
            let bridge =
                bridge_for(mock.clone(), depth).with_deadline(Some(tokio::time::Instant::now()));
            let response = bridge.dispatch_rlm("must not dispatch".into(), None).await;
            assert!(response.error.unwrap().contains("deadline exhausted"));
            assert_eq!(mock.call_count(), 0);
            assert!(bridge.usage_snapshot().await.nested_events.is_empty());
        }
        let bridge = bridge_for(mock.clone(), 1);
        for _ in 0..crate::cost_status::MAX_CHILD_USAGE_RECORDS {
            bridge.usage.reserve_nested_turn().await.unwrap();
        }
        let response = bridge
            .dispatch_rlm("must not spawn Python".into(), None)
            .await;
        assert!(response.error.unwrap().contains("receipt limit"));
        assert_eq!(mock.call_count(), 0);
        assert!(bridge.usage_snapshot().await.nested_events.is_empty());
    }

    struct PendingClient(MockLlmClient);

    impl Replies for PendingClient {
        fn effective_route_envelope(
            &self,
            model: &str,
            at: chrono::DateTime<chrono::Utc>,
        ) -> crate::cost_status::EffectiveRouteEnvelope {
            Replies::effective_route_envelope(&self.0, model, at)
        }
        fn effective_max_output_tokens(&self, model: &str) -> u32 {
            Replies::effective_max_output_tokens(&self.0, model)
        }
        fn create_message_boxed(
            &self,
            _: MessageRequest,
        ) -> Pin<Box<dyn Future<Output = Result<MessageResponse>> + Send + '_>> {
            Box::pin(std::future::pending())
        }
    }

    #[tokio::test]
    async fn parent_deadline_interrupts_plain_and_recursive_model_calls() {
        for depth in [0, 1] {
            let bridge = BridgeFixture::new(
                Arc::new(PendingClient(MockLlmClient::new(Vec::new()))),
                "child-model".into(),
                depth,
            )
            .with_deadline(Some(
                tokio::time::Instant::now() + Duration::from_millis(500),
            ))
            .with_gate(Some(
                crate::tools::codemode::NestedCallGate::admitting_for_test(),
            ));
            let response = tokio::time::timeout(
                Duration::from_secs(5),
                bridge.dispatch_rlm("bounded nested context".into(), None),
            )
            .await
            .expect("a child must not replace the inherited budget");
            assert!(
                response
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("deadline")),
                "{:?}",
                response.error
            );
            if depth == 0 {
                assert_eq!(
                    bridge.usage_snapshot().await.dropped_records,
                    1,
                    "canceled provider work has unknown usage, never priced zero"
                );
            }
        }
    }

    #[test]
    fn batch_guard_allows_non_empty_batches_at_the_cap() {
        assert!(batch_guard(MAX_BATCH, Some("independent")).is_none());
    }

    #[test]
    fn batch_guard_returns_empty_response_for_empty_batches() {
        let response = batch_guard(0, None).expect("empty batch should be handled");
        assert!(response.results.is_empty());
    }

    #[test]
    fn batch_guard_returns_one_error_per_oversized_prompt() {
        let response = batch_guard(MAX_BATCH + 2, Some("independent"))
            .expect("oversized batch should be handled");
        assert_eq!(response.results.len(), MAX_BATCH + 2);
        assert!(response.results.iter().all(|result| {
            result.text.is_empty()
                && result
                    .error
                    .as_deref()
                    .is_some_and(|err| err.contains("batch too large"))
        }));
    }

    #[test]
    fn batch_guard_requires_explicit_independence_for_parallel_work() {
        let response = batch_guard(2, None).expect("missing dependency mode should be handled");
        assert_eq!(response.results.len(), 2);
        assert!(response.results.iter().all(|result| {
            result.text.is_empty()
                && result
                    .error
                    .as_deref()
                    .is_some_and(|err| err.contains("dependency_mode='independent'"))
        }));

        let response = batch_guard(2, Some("sequential"))
            .expect("dependent dependency mode should be handled");
        assert!(response.results.iter().all(|result| {
            result
                .error
                .as_deref()
                .is_some_and(|err| err.contains("sub_query_sequence"))
        }));
    }

    #[tokio::test]
    async fn llm_dispatch_pins_configured_child_model() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(mock_response("child answer", 7, 11));
        let bridge = bridge_for(Arc::clone(&mock), 1);

        let response = bridge
            .dispatch(RpcRequest::Llm {
                prompt: "child prompt".to_string(),
                model: Some("override-model".to_string()),
                max_tokens: Some(123),
                system: Some("child system".to_string()),
            })
            .await;

        match response {
            RpcResponse::Single(single) => {
                assert_eq!(single.text, "child answer");
                assert!(single.error.is_none());
            }
            other => panic!("expected single response, got {other:?}"),
        }

        let captured = mock.captured_requests();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].model, "child-model");
        assert_eq!(captured[0].max_tokens, 123);
        let system = serde_json::to_string(&captured[0].system).unwrap();
        assert!(system.contains("Captured operator Core policy"));
        assert!(system.contains("child system"));

        let snapshot = bridge.usage_snapshot().await;
        assert_eq!(snapshot.usage.input_tokens, 7);
        assert_eq!(snapshot.usage.output_tokens, 11);
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.records[0].usage.usage, snapshot.usage);
        assert!(snapshot.drop_records.is_empty());
        assert_eq!(snapshot.dropped_records, 0);
    }

    #[tokio::test]
    async fn llm_dispatch_keeps_semantic_success_but_marks_missing_usage_once() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(mock_response_with_usage(
            "usable child answer",
            Usage::default(),
        ));
        let bridge = bridge_for(Arc::clone(&mock), 1);

        let response = bridge
            .dispatch(RpcRequest::Llm {
                prompt: "child prompt".to_string(),
                model: None,
                max_tokens: None,
                system: None,
            })
            .await;

        let RpcResponse::Single(response) = response else {
            panic!("expected single response");
        };
        assert_eq!(response.text, "usable child answer");
        assert!(response.error.is_none());

        let first = bridge.usage_snapshot().await;
        let replay = bridge.usage_snapshot().await;
        assert_eq!(first.usage, Usage::default());
        assert!(first.records.is_empty());
        assert_eq!(first.drop_records.len(), 1);
        assert_eq!(first.dropped_records, 1);
        assert_eq!(replay.drop_records, first.drop_records);
        assert_eq!(replay.dropped_records, 1);
        assert_eq!(first.drop_records[0].route.model, "child-model");
        assert!(first.drop_records[0].source_id.starts_with("rlm:"));
    }

    #[tokio::test]
    async fn repeated_reservation_settlement_cannot_duplicate_usage_or_missing_coverage() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        let bridge = bridge_for(Arc::clone(&mock), 1);
        let route =
            Replies::effective_route_envelope(mock.as_ref(), "child-model", chrono::Utc::now());

        let usage_reservation = bridge
            .usage
            .reserve(route.clone())
            .await
            .expect("usage reservation");
        let reported = Usage {
            input_tokens: 3,
            output_tokens: 5,
            ..Usage::default()
        };
        bridge.usage.complete(usage_reservation, &reported).await;
        bridge.usage.complete(usage_reservation, &reported).await;

        let missing_reservation = bridge
            .usage
            .reserve(route)
            .await
            .expect("missing reservation");
        bridge.usage.cancel(missing_reservation, true).await;
        bridge.usage.cancel(missing_reservation, true).await;

        let snapshot = bridge.usage_snapshot().await;
        let mut expected_reported = reported.clone();
        expected_reported.request_usages = vec![reported.clone()];
        assert_eq!(snapshot.usage, expected_reported);
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.drop_records.len(), 1);
        assert_eq!(snapshot.dropped_records, 1);
    }

    #[tokio::test]
    async fn llm_dispatch_preserves_prompt_cache_usage() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(mock_response_with_usage(
            "cached child answer",
            Usage {
                input_tokens: 1000,
                output_tokens: 100,
                prompt_cache_hit_tokens: Some(800),
                prompt_cache_miss_tokens: Some(200),
                ..Usage::default()
            },
        ));
        let bridge = bridge_for(Arc::clone(&mock), 1);

        let response = bridge
            .dispatch(RpcRequest::Llm {
                prompt: "child prompt".to_string(),
                model: None,
                max_tokens: None,
                system: None,
            })
            .await;

        match response {
            RpcResponse::Single(single) => {
                assert_eq!(single.text, "cached child answer");
                assert!(single.error.is_none());
            }
            other => panic!("expected single response, got {other:?}"),
        }

        let usage = bridge.usage_snapshot().await.usage;
        assert_eq!(usage.input_tokens, 1000);
        assert_eq!(usage.output_tokens, 100);
        assert_eq!(usage.prompt_cache_hit_tokens, Some(800));
        assert_eq!(usage.prompt_cache_miss_tokens, Some(200));
    }

    #[tokio::test]
    async fn llm_dispatch_rejects_max_tokens_partial_output_after_charging_usage() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        let usage = Usage {
            input_tokens: 23,
            output_tokens: 4096,
            reasoning_tokens: Some(4000),
            ..Usage::default()
        };
        let mut response = mock_response_with_usage(
            "FINAL('partial answer')\n```repl\nFINAL('also partial')\n```",
            usage.clone(),
        );
        response.stop_reason = Some("max_tokens".to_string());
        mock.push_message_response(response);
        let bridge = bridge_for(Arc::clone(&mock), 1);

        let response = bridge
            .dispatch(RpcRequest::Llm {
                prompt: "child prompt".to_string(),
                model: None,
                max_tokens: None,
                system: None,
            })
            .await;

        match response {
            RpcResponse::Single(single) => {
                assert!(
                    single.text.is_empty(),
                    "partial output must not be accepted"
                );
                let error = single.error.expect("truncation must surface as an error");
                assert!(error.contains("incomplete"), "{error}");
                assert!(error.contains("max_tokens"), "{error}");
            }
            other => panic!("expected single response, got {other:?}"),
        }

        let snapshot = bridge.usage_snapshot().await;
        let mut expected_usage = usage.clone();
        expected_usage.request_usages = vec![usage.clone()];
        assert_eq!(snapshot.usage, expected_usage);
        assert_eq!(snapshot.records.len(), 1);
        assert_eq!(snapshot.records[0].usage.usage, expected_usage);
        let rejected_fragments: Vec<_> = snapshot
            .nested_events
            .iter()
            .filter(|event| {
                event["kind"] == "code"
                    && event["content"]
                        == "FINAL('partial answer')\n```repl\nFINAL('also partial')\n```"
            })
            .collect();
        assert_eq!(
            rejected_fragments.len(),
            1,
            "retain the real rejected diagnostic fragment"
        );
        assert_eq!(mock.call_count(), 1, "truncation must not retry");
    }

    #[tokio::test]
    async fn llm_batch_dispatch_pins_configured_child_model() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(mock_response("one", 1, 2));
        mock.push_message_response(mock_response("two", 3, 4));
        mock.push_message_response(mock_response("three", 5, 6));
        let bridge = bridge_for(Arc::clone(&mock), 1);

        let response = bridge
            .dispatch(RpcRequest::LlmBatch {
                prompts: vec!["a".to_string(), "b".to_string(), "c".to_string()],
                model: Some("batch-model".to_string()),
                dependency_mode: Some("independent".to_string()),
                safety_note: Some("test prompts are independent".to_string()),
            })
            .await;

        match response {
            RpcResponse::Batch(batch) => {
                let texts: Vec<_> = batch
                    .results
                    .iter()
                    .map(|result| result.text.as_str())
                    .collect();
                assert_eq!(texts, ["one", "two", "three"]);
                assert!(batch.results.iter().all(|result| result.error.is_none()));
            }
            other => panic!("expected batch response, got {other:?}"),
        }

        let captured = mock.captured_requests();
        assert_eq!(captured.len(), 3);
        assert!(
            captured
                .iter()
                .all(|request| request.model == "child-model")
        );

        let snapshot = bridge.usage_snapshot().await;
        assert_eq!(snapshot.usage.input_tokens, 9);
        assert_eq!(snapshot.usage.output_tokens, 12);
        assert_eq!(snapshot.records.len(), 3);
        assert_ne!(
            snapshot.records[0].source_id, snapshot.records[1].source_id,
            "distinct provider calls must keep distinct stable identities"
        );
    }

    #[tokio::test]
    async fn shared_accumulator_rejects_the_first_unreceipted_request_before_provider_work() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        let client: Arc<dyn Replies> = mock.clone();
        let usage = RlmUsageAccumulator::new();
        let bridge = BridgeFixture::with_usage_accumulator(
            Arc::clone(&client),
            "child-model".to_string(),
            1,
            usage.clone(),
        );
        let nested_bridge =
            BridgeFixture::with_usage_accumulator(client, "child-model".to_string(), 1, usage);
        let route =
            Replies::effective_route_envelope(mock.as_ref(), "child-model", chrono::Utc::now());
        for _ in 0..crate::cost_status::MAX_CHILD_USAGE_RECORDS {
            let reservation = bridge
                .usage
                .reserve(route.clone())
                .await
                .expect("receipt slot below cap");
            bridge
                .usage
                .complete(
                    reservation,
                    &Usage {
                        input_tokens: 1,
                        ..Usage::default()
                    },
                )
                .await;
        }

        let response = nested_bridge
            .dispatch(RpcRequest::Llm {
                prompt: "must not reach provider".to_string(),
                model: None,
                max_tokens: None,
                system: None,
            })
            .await;
        let RpcResponse::Single(response) = response else {
            panic!("expected single response");
        };
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("rejected before dispatch"))
        );
        assert_eq!(mock.call_count(), 0);
        let snapshot = bridge.usage_snapshot().await;
        assert_eq!(
            snapshot.records.len(),
            crate::cost_status::MAX_CHILD_USAGE_RECORDS
        );
        assert_eq!(snapshot.dropped_records, 0);
        assert!(snapshot.drop_records.is_empty());
    }

    /// #6511: a nested sub-RLM's events used to go to a drain task, so its
    /// model calls never reached the parent's record.
    #[tokio::test]
    async fn nested_rlm_events_reach_the_parent_stream() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(mock_response("```repl\nFINAL('nested answer')\n```", 3, 4));
        let (tx, mut rx) = tokio::sync::mpsc::channel(256);
        let bridge = bridge_for(Arc::clone(&mock), 1).with_events(tx);

        let response = bridge
            .dispatch_rlm("nested context".to_string(), None)
            .await;
        assert_eq!(response.text, "nested answer");
        assert!(response.error.is_none(), "{:?}", response.error);

        let mut lines = Vec::new();
        while let Ok(event) = rx.try_recv() {
            match event {
                crate::core::events::Event::Status { message } => lines.push(message),
                other => panic!("nested events must arrive as status lines, got {other:?}"),
            }
        }
        assert!(
            lines
                .iter()
                .all(|line| line.starts_with(NESTED_RLM_STATUS_PREFIX)),
            "{lines:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("FINAL('nested answer')")),
            "the nested code round is part of the record: {lines:#?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("RLM finished: Final")),
            "{lines:#?}"
        );
        let receipts = bridge.usage_snapshot().await.nested_events;
        assert_eq!(
            receipts.len(),
            2,
            "one complete model/code reply and one terminal receipt"
        );
        assert!(receipts.iter().all(|entry| {
            let content = entry["content"]
                .as_str()
                .expect("canonical receipt content");
            lines.iter().any(|line| line.contains(content))
        }));
        assert_eq!(
            receipts
                .iter()
                .filter(|entry| entry["kind"] == "code")
                .count(),
            1
        );
        assert!(receipts.iter().any(|entry| {
            entry["content"]
                .as_str()
                .is_some_and(|text| text.contains("RLM finished: Final"))
        }));
        assert_eq!(
            crate::core::events::status_visibility(&lines[0]),
            crate::core::events::StatusVisibility::Internal
        );
    }

    #[tokio::test]
    async fn rlm_dispatch_at_depth_zero_pins_configured_child_model() {
        let mock = Arc::new(MockLlmClient::new(Vec::new()));
        mock.push_message_response(mock_response("fallback answer", 3, 5));
        let bridge = bridge_for(Arc::clone(&mock), 0);

        let response = bridge
            .dispatch(RpcRequest::Rlm {
                prompt: "nested prompt".to_string(),
                model: Some("override-model".to_string()),
            })
            .await;

        match response {
            RpcResponse::Single(single) => {
                assert_eq!(single.text, "fallback answer");
                assert!(single.error.is_none());
            }
            other => panic!("expected single response, got {other:?}"),
        }

        let usage = bridge.usage_snapshot().await.usage;
        assert_eq!(usage.input_tokens, 3);
        assert_eq!(usage.output_tokens, 5);

        let captured = mock.captured_requests();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].model, "child-model");
    }
}

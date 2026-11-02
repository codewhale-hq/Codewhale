//! `MockLlmClient` — a queue-driven `LlmClient` implementation for tests.
//!
//! This client implements the [`LlmClient`](super::LlmClient) trait by replaying a
//! pre-loaded queue of canned responses (one per turn). It captures every
//! request the runtime sends so tests can assert on the outgoing payload —
//! e.g. confirming that prior `reasoning_content` is replayed in DeepSeek V4
//! thinking-mode tool-calling turns (V4 §5.1.1; the bug that broke
//! v0.4.9-v0.5.1).
//!
//! # Mocking strategy
//!
//! Tests mock at the **trait boundary** (`LlmClient`), never at the `reqwest`
//! HTTP layer. The trait is the durable abstraction — internal HTTP plumbing
//! changes frequently and is not part of the public engine contract.
//!
//! # Example
//!
//! ```ignore
//! use crate::llm_client::mock::{MockLlmClient, canned};
//! use crate::llm_client::LlmClient;
//!
//! // One canned turn that emits "hello world" as two text deltas, then
//! // finishes with stop_reason = "end_turn".
//! let turn = vec![
//!     canned::message_start("msg_1"),
//!     canned::text_delta(0, "hello "),
//!     canned::text_delta(0, "world"),
//!     canned::message_stop(),
//! ];
//!
//! let mock = MockLlmClient::new(vec![turn]);
//! let stream = mock.create_message_stream(/* ... */).await.unwrap();
//! // ... drain the stream, assert deltas ...
//! assert_eq!(mock.call_count(), 1);
//! assert_eq!(mock.captured_requests().len(), 1);
//! ```

// This module ships methods + builder helpers that integration tests rely on
// individually. Not every helper is exercised by unit tests — that's expected
// (the goal is a usable mock surface for downstream tests), so we silence
// per-item dead-code warnings at the module level.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Result, anyhow};
use async_stream::try_stream;
use futures_util::Stream;

use codewhale_models::{
    ContentBlock, MessageDelta, MessageRequest, MessageResponse, StreamEvent, Usage,
};

use super::{LlmClient, StreamEventBox};
#[cfg(test)]
use codewhale_models::Role;

/// A pre-recorded "turn" the mock will replay on the next streaming call.
///
/// `MessageStop` does *not* need to be the final element — the mock will
/// auto-emit one if missing, mirroring the real client's behaviour. Likewise
/// the mock does not require `MessageStart` to be present.
pub type CannedTurn = Vec<StreamEvent>;

/// A queued mock response step.
pub enum FauxStep {
    Canned(CannedTurn),
    /// Build a canned turn from the live outgoing request.
    ///
    /// Tests can assert DeepSeek V4's thinking-mode tool-call invariant here:
    /// on the assistant turn that produced the previous tool call, the next
    /// outgoing request must still carry `reasoning_content` (represented in
    /// this model as a [`ContentBlock::Thinking`] block). If it is missing,
    /// DeepSeek V4 returns HTTP 400 on the follow-up turn. This guards the
    /// [v0.4.9-v0.5.1 regression range](https://github.com/codewhale-hq/CodeWhale/compare/v0.4.9...v0.5.1)
    /// where that content was dropped.
    Factory(Box<dyn Fn(&MessageRequest) -> CannedTurn + Send + Sync>),
    /// Fail the request itself with this message, as a provider that refuses
    /// it (a 401 for a bad key, say) does before any stream starts.
    Error(String),
}

/// A queue-driven mock LLM client.
///
/// The mock holds a FIFO queue of canned response turns. Each call to
/// [`LlmClient::create_message_stream`] dequeues the next turn and replays its
/// events as a stream. If the queue is exhausted, the call returns an error
/// — tests should ensure they push exactly as many turns as the runtime will
/// consume.
///
/// The mock also captures the [`MessageRequest`] passed to every call so tests
/// can assert on the outgoing payload (e.g. that prior `reasoning_content` is
/// preserved across turns).
pub struct MockLlmClient {
    canned: Mutex<VecDeque<FauxStep>>,
    captured_requests: Mutex<Vec<MessageRequest>>,
    calls: AtomicUsize,
    provider_name: &'static str,
    model: String,
    /// If set, [`LlmClient::create_message`] returns this verbatim. Otherwise
    /// it falls back to streaming + collection. Useful for non-streaming
    /// compaction-style calls.
    canned_messages: Mutex<VecDeque<MessageResponse>>,
}

impl MockLlmClient {
    /// Construct a mock that will replay the given canned turns in order.
    #[must_use]
    pub fn new(canned: Vec<CannedTurn>) -> Self {
        Self {
            canned: Mutex::new(canned.into_iter().map(FauxStep::Canned).collect()),
            captured_requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            provider_name: "mock",
            model: "mock-model".to_string(),
            canned_messages: Mutex::new(VecDeque::new()),
        }
    }

    /// Set the provider-name string returned by [`LlmClient::provider_name`].
    #[must_use]
    pub fn with_provider(mut self, name: &'static str) -> Self {
        self.provider_name = name;
        self
    }

    /// Set the model identifier returned by [`LlmClient::model`].
    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Push a canned turn onto the back of the queue.
    pub fn push_turn(&self, turn: CannedTurn) {
        self.canned
            .lock()
            .expect("MockLlmClient.canned mutex poisoned")
            .push_back(FauxStep::Canned(turn));
    }

    /// Push a factory step onto the back of the queue.
    ///
    /// The closure receives the live outgoing [`MessageRequest`] before the
    /// response stream is built, so assertions panic directly from the client
    /// call rather than later while polling the returned stream.
    pub fn push_factory<F>(&self, factory: F)
    where
        F: Fn(&MessageRequest) -> CannedTurn + Send + Sync + 'static,
    {
        self.canned
            .lock()
            .expect("MockLlmClient.canned mutex poisoned")
            .push_back(FauxStep::Factory(Box::new(factory)));
    }

    /// Push a request failure onto the back of the queue.
    pub fn push_error(&self, message: impl Into<String>) {
        self.canned
            .lock()
            .expect("MockLlmClient.canned mutex poisoned")
            .push_back(FauxStep::Error(message.into()));
    }

    /// Push a canned non-streaming `MessageResponse`. Consumed by
    /// [`LlmClient::create_message`] (FIFO).
    pub fn push_message_response(&self, response: MessageResponse) {
        self.canned_messages
            .lock()
            .expect("MockLlmClient.canned_messages mutex poisoned")
            .push_back(response);
    }

    /// Number of completed calls to either `create_message` or
    /// `create_message_stream`.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    /// Number of canned turns still queued.
    #[must_use]
    pub fn remaining_turns(&self) -> usize {
        self.canned
            .lock()
            .expect("MockLlmClient.canned mutex poisoned")
            .len()
    }

    /// Snapshot of every request the mock has been asked to handle, in order.
    #[must_use]
    pub fn captured_requests(&self) -> Vec<MessageRequest> {
        self.captured_requests
            .lock()
            .expect("MockLlmClient.captured_requests mutex poisoned")
            .clone()
    }

    /// Convenience: return the most recently captured request, or `None` if
    /// the mock has not been called yet.
    #[must_use]
    pub fn last_request(&self) -> Option<MessageRequest> {
        self.captured_requests
            .lock()
            .expect("MockLlmClient.captured_requests mutex poisoned")
            .last()
            .cloned()
    }

    fn record_request(&self, request: &MessageRequest) {
        self.captured_requests
            .lock()
            .expect("MockLlmClient.captured_requests mutex poisoned")
            .push(request.clone());
        self.calls.fetch_add(1, Ordering::SeqCst);
    }

    fn pop_step(&self) -> Option<FauxStep> {
        self.canned
            .lock()
            .expect("MockLlmClient.canned mutex poisoned")
            .pop_front()
    }

    fn turn_from_step(&self, step: FauxStep, request: &MessageRequest) -> Result<CannedTurn> {
        match step {
            FauxStep::Canned(turn) => Ok(turn),
            FauxStep::Factory(factory) => Ok(factory(request)),
            FauxStep::Error(message) => Err(anyhow!(message)),
        }
    }

    fn pop_message(&self) -> Option<MessageResponse> {
        self.canned_messages
            .lock()
            .expect("MockLlmClient.canned_messages mutex poisoned")
            .pop_front()
    }
}

impl LlmClient for MockLlmClient {
    fn provider_name(&self) -> &'static str {
        self.provider_name
    }

    fn model(&self) -> &str {
        &self.model
    }

    async fn create_message(&self, request: MessageRequest) -> Result<MessageResponse> {
        self.record_request(&request);

        if let Some(canned) = self.pop_message() {
            return Ok(canned);
        }

        // Fallback: synthesize a MessageResponse from the next streaming turn.
        let Some(step) = self.pop_step() else {
            return Err(anyhow!(
                "MockLlmClient: create_message called but no canned response queued (request #{})",
                self.calls.load(Ordering::SeqCst)
            ));
        };

        let turn = self.turn_from_step(step, &request)?;
        Ok(synthesize_message_response(turn, &self.model))
    }

    async fn create_message_stream(&self, request: MessageRequest) -> Result<StreamEventBox> {
        self.record_request(&request);

        let Some(step) = self.pop_step() else {
            return Err(anyhow!(
                "MockLlmClient: create_message_stream called but no canned turn queued (call #{})",
                self.calls.load(Ordering::SeqCst)
            ));
        };

        let turn = self.turn_from_step(step, &request)?;
        Ok(stream_from_canned(turn))
    }

    async fn health_check(&self) -> Result<bool> {
        Ok(true)
    }
}

/// Wrap a canned event vector as a stream that yields each event in order and
/// auto-appends `MessageStop` if the trailing event is not already one.
fn stream_from_canned(turn: CannedTurn) -> StreamEventBox {
    let s = try_stream! {
        let has_stop = matches!(turn.last(), Some(StreamEvent::MessageStop));
        for ev in turn {
            yield ev;
        }
        if !has_stop {
            yield StreamEvent::MessageStop;
        }
    };
    Box::pin(s) as Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send + 'static>>
}

/// Best-effort: collapse a streaming turn into a non-streaming
/// `MessageResponse` by concatenating text deltas. Used only as a fallback
/// when callers `create_message` without a queued `MessageResponse`.
fn synthesize_message_response(turn: CannedTurn, model: &str) -> MessageResponse {
    use codewhale_models::Delta;

    let mut text = String::new();
    let mut stop_reason: Option<String> = None;

    for ev in turn {
        match ev {
            StreamEvent::ContentBlockDelta {
                delta: Delta::TextDelta { text: t },
                ..
            } => text.push_str(&t),
            StreamEvent::MessageDelta {
                delta: MessageDelta {
                    stop_reason: sr, ..
                },
                ..
            } => stop_reason = sr,
            _ => {}
        }
    }

    MessageResponse {
        id: "mock_msg".to_string(),
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![ContentBlock::Text {
            text,
            cache_control: None,
        }],
        model: model.to_string(),
        stop_reason: stop_reason.or_else(|| Some("end_turn".to_string())),
        stop_sequence: None,
        container: None,
        usage: Usage::default(),
    }
}

/// Builders for common canned-event patterns. Re-exported so tests can build
/// realistic streams without wiring `StreamEvent` shapes by hand.
pub mod canned {
    use serde_json::Value;

    use codewhale_models::{
        ContentBlockStart, Delta, MessageDelta, MessageResponse, StreamEvent, Usage,
    };

    /// `MessageStart` event with a synthetic message envelope.
    #[must_use]
    pub fn message_start(id: &str) -> StreamEvent {
        StreamEvent::MessageStart {
            message: MessageResponse {
                id: id.to_string(),
                r#type: "message".to_string(),
                role: "assistant".to_string(),
                content: vec![],
                model: "mock-model".to_string(),
                stop_reason: None,
                stop_sequence: None,
                container: None,
                usage: Usage::default(),
            },
        }
    }

    /// Open a text content block at `index`.
    #[must_use]
    pub fn text_block_start(index: u32) -> StreamEvent {
        StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlockStart::Text {
                text: String::new(),
            },
        }
    }

    /// Append `text` to the content block at `index`.
    #[must_use]
    pub fn text_delta(index: u32, text: &str) -> StreamEvent {
        StreamEvent::ContentBlockDelta {
            index,
            delta: Delta::TextDelta {
                text: text.to_string(),
            },
        }
    }

    /// Append a thinking-content delta at `index`.
    #[must_use]
    pub fn thinking_delta(index: u32, thinking: &str) -> StreamEvent {
        StreamEvent::ContentBlockDelta {
            index,
            delta: Delta::ThinkingDelta {
                thinking: thinking.to_string(),
            },
        }
    }

    /// Open a tool_use content block at `index`.
    #[must_use]
    pub fn tool_use_block_start(index: u32, id: &str, name: &str) -> StreamEvent {
        StreamEvent::ContentBlockStart {
            index,
            content_block: ContentBlockStart::ToolUse {
                id: id.to_string(),
                name: name.to_string(),
                input: Value::Null,
                caller: None,
                thought_signature: None,
            },
        }
    }

    /// Stream partial JSON for a tool's input arguments.
    #[must_use]
    pub fn tool_input_delta(index: u32, partial_json: &str) -> StreamEvent {
        StreamEvent::ContentBlockDelta {
            index,
            delta: Delta::InputJsonDelta {
                partial_json: partial_json.to_string(),
            },
        }
    }

    /// Close the content block at `index`.
    #[must_use]
    pub fn block_stop(index: u32) -> StreamEvent {
        StreamEvent::ContentBlockStop { index }
    }

    /// Emit a `message_delta` carrying `stop_reason` and optional `usage`.
    #[must_use]
    pub fn message_delta(stop_reason: &str, usage: Option<Usage>) -> StreamEvent {
        StreamEvent::MessageDelta {
            delta: MessageDelta {
                stop_reason: Some(stop_reason.to_string()),
                stop_sequence: None,
            },
            usage,
        }
    }

    /// Final `message_stop` sentinel.
    #[must_use]
    pub fn message_stop() -> StreamEvent {
        StreamEvent::MessageStop
    }

    /// Convenience: a complete "assistant emits this text" turn ending with
    /// `stop_reason = "end_turn"`.
    #[must_use]
    pub fn simple_text_turn(text: &str) -> Vec<StreamEvent> {
        vec![
            message_start("mock_msg_1"),
            text_block_start(0),
            text_delta(0, text),
            block_stop(0),
            message_delta("end_turn", None),
            message_stop(),
        ]
    }

    /// Convenience: a turn that emits one assistant tool_call and stops.
    #[must_use]
    pub fn tool_call_turn(call_id: &str, tool_name: &str, args_json: &str) -> Vec<StreamEvent> {
        vec![
            message_start("mock_msg_tool"),
            tool_use_block_start(0, call_id, tool_name),
            tool_input_delta(0, args_json),
            block_stop(0),
            message_delta("tool_use", None),
            message_stop(),
        ]
    }
}

// === Tests ===

#[cfg(test)]
mod tests {
    use futures_util::StreamExt;

    use super::*;
    use crate::llm_client::LlmClient;
    use codewhale_models::{Delta, Message, MessageRequest, StreamEvent};

    fn empty_request() -> MessageRequest {
        MessageRequest {
            model: "mock-model".to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![],
            }],
            max_tokens: 1024,
            system: None,
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: None,
            stream: Some(true),
            temperature: None,
            top_p: None,
        }
    }

    #[tokio::test]
    async fn replays_canned_turn_via_stream() {
        let mock = MockLlmClient::new(vec![canned::simple_text_turn("hello world")]);

        let mut stream = mock
            .create_message_stream(empty_request())
            .await
            .expect("stream should open");

        let mut text = String::new();
        let mut saw_stop = false;
        while let Some(ev) = stream.next().await {
            match ev.expect("event") {
                StreamEvent::ContentBlockDelta {
                    delta: Delta::TextDelta { text: t },
                    ..
                } => text.push_str(&t),
                StreamEvent::MessageStop => {
                    saw_stop = true;
                    break;
                }
                _ => {}
            }
        }

        assert_eq!(text, "hello world");
        assert!(saw_stop);
        assert_eq!(mock.call_count(), 1);
        assert_eq!(mock.captured_requests().len(), 1);
        assert_eq!(mock.remaining_turns(), 0);
    }

    #[tokio::test]
    async fn errors_when_queue_exhausted() {
        let mock = MockLlmClient::new(Vec::new());
        let result = mock.create_message_stream(empty_request()).await;
        match result {
            Ok(_) => panic!("should error on empty queue"),
            Err(err) => assert!(format!("{err}").contains("no canned")),
        }
    }

    #[tokio::test]
    async fn captures_request_payload_for_assertions() {
        let mock = MockLlmClient::new(vec![canned::simple_text_turn("ok")]);
        let mut req = empty_request();
        req.temperature = Some(0.42);
        let _ = mock.create_message_stream(req).await.unwrap();

        let captured = mock.last_request().expect("should have captured");
        assert_eq!(captured.temperature, Some(0.42));
    }

    #[tokio::test]
    async fn stream_auto_appends_message_stop() {
        // Queue a turn missing MessageStop — mock should append one.
        let turn = vec![canned::text_block_start(0), canned::text_delta(0, "x")];
        let mock = MockLlmClient::new(vec![turn]);

        let mut stream = mock.create_message_stream(empty_request()).await.unwrap();
        let mut saw_stop = false;
        while let Some(ev) = stream.next().await {
            if matches!(ev.expect("event"), StreamEvent::MessageStop) {
                saw_stop = true;
            }
        }
        assert!(saw_stop, "auto MessageStop missing");
    }

    #[tokio::test]
    async fn create_message_uses_canned_message_response_first() {
        let mock = MockLlmClient::new(vec![canned::simple_text_turn("from stream")]);
        mock.push_message_response(MessageResponse {
            id: "preset".to_string(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: vec![ContentBlock::Text {
                text: "from preset".to_string(),
                cache_control: None,
            }],
            model: "mock-model".to_string(),
            stop_reason: Some("end_turn".to_string()),
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        });

        let resp = mock.create_message(empty_request()).await.unwrap();
        assert_eq!(resp.id, "preset");
    }

    #[tokio::test]
    async fn create_message_synthesizes_from_streaming_turn_when_no_message_queued() {
        let mock = MockLlmClient::new(vec![canned::simple_text_turn("synthesized")]);
        let resp = mock.create_message(empty_request()).await.unwrap();
        let text = match &resp.content[0] {
            ContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(text, "synthesized");
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
    }

    #[tokio::test]
    async fn create_message_synthesizes_from_factory_turn() {
        let mock = MockLlmClient::new(Vec::new());
        mock.push_factory(|request| {
            assert_eq!(request.model, "mock-model");
            canned::simple_text_turn("from factory")
        });

        let resp = mock.create_message(empty_request()).await.unwrap();
        let text = match &resp.content[0] {
            ContentBlock::Text { text, .. } => text.clone(),
            _ => panic!("expected text"),
        };
        assert_eq!(text, "from factory");
    }

    #[tokio::test]
    async fn provider_and_model_are_overridable() {
        let mock = MockLlmClient::new(vec![canned::simple_text_turn("x")])
            .with_provider("test-provider")
            .with_model("test-model");
        assert_eq!(mock.provider_name(), "test-provider");
        assert_eq!(mock.model(), "test-model");
    }

    #[tokio::test]
    async fn tool_call_turn_serializes_correctly() {
        let mock = MockLlmClient::new(vec![canned::tool_call_turn(
            "call_1",
            "list_dir",
            r#"{"path":"/tmp"}"#,
        )]);
        let mut stream = mock.create_message_stream(empty_request()).await.unwrap();

        let mut saw_tool_use = false;
        let mut json_seen = String::new();
        while let Some(ev) = stream.next().await {
            match ev.unwrap() {
                StreamEvent::ContentBlockStart { content_block, .. } => {
                    use codewhale_models::ContentBlockStart;
                    if let ContentBlockStart::ToolUse { name, .. } = content_block {
                        assert_eq!(name, "list_dir");
                        saw_tool_use = true;
                    }
                }
                StreamEvent::ContentBlockDelta {
                    delta: Delta::InputJsonDelta { partial_json },
                    ..
                } => json_seen.push_str(&partial_json),
                _ => {}
            }
        }
        assert!(saw_tool_use, "expected tool_use start event");
        assert!(json_seen.contains("/tmp"));
    }

    #[tokio::test]
    async fn multiple_turns_consumed_in_order() {
        let mock = MockLlmClient::new(vec![
            canned::simple_text_turn("turn-one"),
            canned::simple_text_turn("turn-two"),
        ]);
        for expected in ["turn-one", "turn-two"] {
            let mut stream = mock.create_message_stream(empty_request()).await.unwrap();
            let mut text = String::new();
            while let Some(ev) = stream.next().await {
                if let StreamEvent::ContentBlockDelta {
                    delta: Delta::TextDelta { text: t },
                    ..
                } = ev.unwrap()
                {
                    text.push_str(&t);
                }
            }
            assert_eq!(text, expected);
        }
        assert_eq!(mock.call_count(), 2);
        assert_eq!(mock.remaining_turns(), 0);
    }
}

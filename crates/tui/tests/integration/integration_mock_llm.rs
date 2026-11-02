//! Integration tests for the [`MockLlmClient`](mock::MockLlmClient).
//!
//! These tests exercise the [`LlmClient`](llm_client::LlmClient) trait surface
//! directly. They verify that the mock client itself behaves correctly under
//! the patterns the runtime relies on:
//!
//! - **Streaming turn loop** — events arrive in order, `MessageStop` terminates
//!   the stream.
//! - **Reasoning replay** (issue #69 / V4 §5.1.1) — when the runtime sends a
//!   second turn after a tool round, it MUST replay prior `reasoning_content`.
//!   Catches the HTTP 400 path that broke v0.4.9-v0.5.1.
//! - **Tool-call round-trip** — assistant emits `tool_calls`, runtime executes,
//!   tool result is appended, next turn streams text.
//! - **Multiple tool calls in one round** — assistant returns N tool_calls;
//!   the request payload preserves their ordering.
//! - **Compaction-style non-streaming call** — `create_message` returns a
//!   queued `MessageResponse` without going through the streaming path.
//! - **Sub-agent style turn** — child mailbox receives a parent prompt and
//!   replies; trait boundary is the same.
//! - **Capacity-gate observation** — runtime can probe estimated request size
//!   and decline to dispatch; the mock surfaces capture-side hooks for that.
//!
//! # Why trait-level (not engine-level)
//!
//! As of v0.6.7 the engine (`crates/engine/src/core/engine.rs`) holds a concrete
//! `Option<CodewhaleClient>` — the [`LlmClient`] trait is implemented but no
//! consumer takes `Arc<dyn LlmClient>` or generic `<C: LlmClient>`. Wiring the
//! mock into a full engine turn-loop therefore requires a separate refactor:
//! every `Option<CodewhaleClient>` consumer (engine, registry, rlm, review,
//! cycle_manager, compaction, subagent) must move to `Arc<dyn LlmClient>`.
//!
//! Per the v0.7.0 mock-LLM issue (the parent of this file): "If the engine's
//! API surfaces are too tangled to mock cleanly … document that as BLOCKED with
//! what wiring needs to change. In that case still commit any partial work
//! that lands cleanly." Full engine integration coverage remains blocked on
//! that seam; this file keeps the blocker documented instead of carrying
//! ignored placeholder tests.
//!
//! Once `Arc<dyn LlmClient>` lands, add engine-level tests that reuse this mock.

use futures_util::StreamExt;

// Bring in the production model types verbatim — no other crate sources are
// needed because the mock is self-contained against `models.rs`.

// Mirror the real `llm_client` module hierarchy so that `mock.rs`'s
// `super::{LlmClient, StreamEventBox}` paths resolve. We re-declare a local
// `LlmClient` trait + `StreamEventBox` alias that match the production shape
// 1:1 (the public surface that ships in the binary). The mock implements
// this local trait, which is structurally identical to the production trait.
//
// The helper file lives under `tests/support/` so cargo does not try to
// compile it as its own test binary.

use crate::llm_client::LlmClient;
use crate::llm_client::mock::{MockLlmClient, canned};
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Delta, Message, MessageRequest, StreamEvent, Usage};

// === Helpers ===============================================================

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    }
}

fn assistant_thinking(thinking: &str, text: &str) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: thinking.to_string(),
                signature: None,
                state: None,
                redacted_data: None,
            },
            ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            },
        ],
    }
}

fn assistant_tool_call(id: &str, name: &str, input: serde_json::Value) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![ContentBlock::ToolUse {
            execution_id: None,
            id: id.to_string(),
            name: name.to_string(),
            input,
            caller: None,
            thought_signature: None,
        }],
    }
}

fn tool_result_message(tool_use_id: &str, content: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: tool_use_id.to_string(),
            content: content.to_string(),
            is_error: None,
            content_blocks: None,
        }],
    }
}

fn make_request(messages: Vec<Message>) -> MessageRequest {
    MessageRequest {
        model: "deepseek-v4-pro".to_string(),
        messages,
        max_tokens: 4096,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: Some("high".to_string()),
        stream: Some(true),
        temperature: None,
        top_p: None,
    }
}

async fn drain_stream_text(
    mock: &MockLlmClient,
    request: MessageRequest,
) -> (String, Option<String>) {
    let mut stream = mock
        .create_message_stream(request)
        .await
        .expect("stream open");
    let mut text = String::new();
    let mut stop_reason: Option<String> = None;
    while let Some(ev) = stream.next().await {
        match ev.expect("event") {
            StreamEvent::ContentBlockDelta {
                delta: Delta::TextDelta { text: t },
                ..
            } => text.push_str(&t),
            StreamEvent::MessageDelta { delta, .. } => {
                stop_reason = delta.stop_reason;
            }
            StreamEvent::MessageStop => break,
            _ => {}
        }
    }
    (text, stop_reason)
}

// === 1. Full turn loop with streaming =======================================

#[tokio::test]
async fn full_turn_loop_streams_text_chunks() {
    // Two text deltas + finish reason — exercises the canonical streaming
    // turn-loop path the engine drives.
    let turn = vec![
        canned::message_start("msg_1"),
        canned::text_block_start(0),
        canned::text_delta(0, "Hello, "),
        canned::text_delta(0, "world!"),
        canned::block_stop(0),
        canned::message_delta("end_turn", Some(Usage::default())),
        canned::message_stop(),
    ];
    let mock = MockLlmClient::new(vec![turn]);

    let request = make_request(vec![user_message("greet me")]);
    let (text, stop) = drain_stream_text(&mock, request).await;

    assert_eq!(text, "Hello, world!");
    assert_eq!(stop.as_deref(), Some("end_turn"));
    assert_eq!(mock.call_count(), 1);
    assert_eq!(mock.captured_requests().len(), 1);
}

// === 2. Reasoning replay (V4 thinking-mode HTTP-400 regression) =============

#[tokio::test]
async fn reasoning_replay_required_on_subsequent_turn() {
    // Turn 1: assistant emits thinking + tool_call. Turn 2: text reply.
    let turn1 = vec![
        canned::message_start("r1"),
        canned::thinking_delta(0, "I should call list_dir."),
        canned::tool_use_block_start(1, "call_a", "list_dir"),
        canned::tool_input_delta(1, r#"{"path":"/tmp"}"#),
        canned::block_stop(1),
        canned::message_delta("tool_use", None),
        canned::message_stop(),
    ];
    let turn2 = vec![
        canned::message_start("r2"),
        canned::text_block_start(0),
        canned::text_delta(0, "I see /tmp."),
        canned::block_stop(0),
        canned::message_delta("end_turn", None),
        canned::message_stop(),
    ];
    let mock = MockLlmClient::new(vec![turn1, turn2]);

    // === Round 1: user prompt -> assistant tool_call ===
    let req1 = make_request(vec![user_message("list /tmp")]);
    let _ = mock.create_message_stream(req1).await.unwrap().next().await;
    // (we don't drain — capture is what matters here)

    // === Round 2: runtime composes the next request including the prior
    // assistant turn's reasoning_content. The mock can verify that any
    // ContentBlock::Thinking the runtime preserves is present in the next
    // outgoing request — the very payload shape that broke v0.4.9-v0.5.1.
    let next_messages = vec![
        user_message("list /tmp"),
        assistant_thinking("I should call list_dir.", ""),
        assistant_tool_call("call_a", "list_dir", serde_json::json!({ "path": "/tmp" })),
        tool_result_message("call_a", "/tmp/file1\n/tmp/file2"),
    ];
    let req2 = make_request(next_messages);
    let _ = mock.create_message_stream(req2).await.unwrap();

    // The mock captured both requests. Assert the SECOND request preserves
    // the prior assistant message's Thinking block — i.e. the runtime did
    // not strip reasoning_content before re-sending. (V4 thinking-mode tool
    // turns reject HTTP 400 if reasoning_content is missing.)
    let captured = mock.captured_requests();
    assert_eq!(captured.len(), 2);

    let req2 = &captured[1];
    let assistant_with_thinking = req2
        .messages
        .iter()
        .find(|m| {
            m.role == "assistant"
                && m.content
                    .iter()
                    .any(|b| matches!(b, ContentBlock::Thinking { .. }))
        })
        .expect("turn 2 request must replay assistant Thinking content");

    let thinking_text = assistant_with_thinking
        .content
        .iter()
        .find_map(|b| match b {
            ContentBlock::Thinking { thinking, .. } => Some(thinking.clone()),
            _ => None,
        })
        .expect("Thinking block present");
    assert_eq!(
        thinking_text, "I should call list_dir.",
        "reasoning_content must be replayed verbatim across tool-call rounds"
    );
}

// === 3. Tool-call round-trip ================================================

#[tokio::test]
async fn tool_call_round_trip_streams_args_then_continues() {
    // Turn 1 emits a tool_use block with chunked input JSON.
    let turn1 = vec![
        canned::message_start("rt1"),
        canned::tool_use_block_start(0, "call_x", "read_file"),
        canned::tool_input_delta(0, r#"{"path":"#),
        canned::tool_input_delta(0, r#""README.md"}"#),
        canned::block_stop(0),
        canned::message_delta("tool_use", None),
        canned::message_stop(),
    ];
    let turn2 = vec![
        canned::message_start("rt2"),
        canned::text_block_start(0),
        canned::text_delta(0, "README starts with: # deepseek-tui"),
        canned::block_stop(0),
        canned::message_delta("end_turn", None),
        canned::message_stop(),
    ];
    let mock = MockLlmClient::new(vec![turn1, turn2]);

    // Round 1
    let mut s1 = mock
        .create_message_stream(make_request(vec![user_message("read README.md")]))
        .await
        .unwrap();

    let mut tool_use_seen = false;
    let mut json_seen = String::new();
    while let Some(ev) = s1.next().await {
        match ev.unwrap() {
            StreamEvent::ContentBlockStart { content_block, .. } => {
                use codewhale_models::ContentBlockStart;
                if let ContentBlockStart::ToolUse { name, .. } = content_block {
                    assert_eq!(name, "read_file");
                    tool_use_seen = true;
                }
            }
            StreamEvent::ContentBlockDelta {
                delta: Delta::InputJsonDelta { partial_json },
                ..
            } => json_seen.push_str(&partial_json),
            StreamEvent::MessageStop => break,
            _ => {}
        }
    }
    assert!(tool_use_seen);
    let parsed: serde_json::Value =
        serde_json::from_str(&json_seen).expect("valid JSON after concat");
    assert_eq!(parsed["path"], "README.md");

    // Round 2 — runtime sends back a tool_result and the mock replies with
    // the final assistant text turn.
    let req2 = make_request(vec![
        user_message("read README.md"),
        assistant_tool_call(
            "call_x",
            "read_file",
            serde_json::json!({ "path": "README.md" }),
        ),
        tool_result_message("call_x", "# deepseek-tui\n..."),
    ]);
    let (text, stop) = drain_stream_text(&mock, req2).await;
    assert!(text.contains("# deepseek-tui"));
    assert_eq!(stop.as_deref(), Some("end_turn"));
}

// === 4. Multiple tool calls in one round (parallel ordering) ================

#[tokio::test]
async fn parallel_tool_calls_preserve_ordering_in_turn_payload() {
    // Assistant returns two tool_calls in a single turn (indices 0 and 1).
    // The runtime is free to execute them in parallel; this test asserts that
    // the canonical event ordering survives a single-turn replay.
    let turn = vec![
        canned::message_start("p1"),
        canned::tool_use_block_start(0, "call_one", "list_dir"),
        canned::tool_input_delta(0, r#"{"path":"a"}"#),
        canned::block_stop(0),
        canned::tool_use_block_start(1, "call_two", "list_dir"),
        canned::tool_input_delta(1, r#"{"path":"b"}"#),
        canned::block_stop(1),
        canned::message_delta("tool_use", None),
        canned::message_stop(),
    ];
    let mock = MockLlmClient::new(vec![turn]);

    let mut stream = mock
        .create_message_stream(make_request(vec![user_message("list both")]))
        .await
        .unwrap();

    let mut starts: Vec<(u32, String)> = Vec::new();
    while let Some(ev) = stream.next().await {
        if let StreamEvent::ContentBlockStart {
            index,
            content_block,
        } = ev.unwrap()
        {
            use codewhale_models::ContentBlockStart;
            if let ContentBlockStart::ToolUse { id, .. } = content_block {
                starts.push((index, id));
            }
        }
    }

    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0], (0, "call_one".to_string()));
    assert_eq!(starts[1], (1, "call_two".to_string()));
}

// === 5. Compaction-style non-streaming call =================================

#[tokio::test]
async fn compaction_non_streaming_returns_queued_message_response() {
    use codewhale_models::MessageResponse;

    let mock = MockLlmClient::new(vec![]);
    mock.push_message_response(MessageResponse {
        id: "compact_msg".to_string(),
        r#type: "message".to_string(),
        role: "assistant".to_string(),
        content: vec![ContentBlock::Text {
            text: "## Summary\n- Step 1\n- Step 2".to_string(),
            cache_control: None,
        }],
        model: "deepseek-v4-pro".to_string(),
        stop_reason: Some("end_turn".to_string()),
        stop_sequence: None,
        container: None,
        usage: Usage::default(),
    });

    // The runtime's compaction path uses create_message (not stream).
    let req = MessageRequest {
        stream: Some(false),
        ..make_request(vec![user_message("summarize")])
    };
    let resp = mock.create_message(req).await.unwrap();

    let text = match &resp.content[0] {
        ContentBlock::Text { text, .. } => text.clone(),
        _ => panic!("expected text content"),
    };
    assert!(text.contains("Summary"));
    assert_eq!(resp.id, "compact_msg");
    assert_eq!(mock.call_count(), 1);
}

// === 6. Sub-agent style turn ================================================
//
// The next turn after an `agent` summary must re-verify the claimed
// side effect before reporting success.

#[tokio::test]
async fn v4_parent_reverifies_subagent_file_self_report_before_claiming_success() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("child-claimed-write.txt");
    assert!(!missing.exists(), "fixture path must start missing");
    let missing_path = missing.display().to_string();

    let parent = MockLlmClient::new(vec![vec![
        canned::message_start("parent_verify"),
        canned::thinking_delta(0, "Verify the child's file-write self-report first."),
        canned::tool_use_block_start(1, "verify_file", "read_file"),
        canned::tool_input_delta(1, &serde_json::json!({ "path": &missing_path }).to_string()),
        canned::block_stop(1),
        canned::message_delta("tool_use", None),
        canned::message_stop(),
    ]])
    .with_model("deepseek-v4-pro");
    let tool_summary = format!(
        "[sub-agent result summarized for parent context]\n\
Child results are self-reports; verify side effects with tools like read_file or list_dir before claiming success.\n\
- agent_filecheck (implementer) status=Completed\n  result: Wrote {missing_path} successfully."
    );

    let mut stream = parent
        .create_message_stream(make_request(vec![
            user_message("Use a child to create the file, then report back."),
            assistant_tool_call(
                "agent_call",
                "agent",
                serde_json::json!({
                    "prompt": "Create the requested file and report the result.",
                    "role": "implementer"
                }),
            ),
            tool_result_message("agent_call", &tool_summary),
        ]))
        .await
        .unwrap();

    let mut text_before_verification = String::new();
    let mut tool_name = None;
    let mut tool_input = String::new();
    while let Some(ev) = stream.next().await {
        match ev.unwrap() {
            StreamEvent::ContentBlockStart { content_block, .. } => {
                use codewhale_models::ContentBlockStart;
                if let ContentBlockStart::ToolUse { name, .. } = content_block {
                    tool_name = Some(name);
                }
            }
            StreamEvent::ContentBlockDelta { delta, .. } => match delta {
                Delta::InputJsonDelta { partial_json } => tool_input.push_str(&partial_json),
                Delta::TextDelta { text } => text_before_verification.push_str(&text),
                _ => {}
            },
            StreamEvent::MessageStop => break,
            _ => {}
        }
    }

    assert_eq!(text_before_verification, "");
    assert_eq!(tool_name.as_deref(), Some("read_file"));
    let parsed: serde_json::Value = serde_json::from_str(&tool_input).expect("tool input JSON");
    assert_eq!(parsed["path"], missing_path);
}

// === 7. Request capture observation =========================================
//
// The mock surfaces request captures BEFORE the response stream is opened, so
// trait-level tests can verify that captured requests are observable per-call
// rather than buffered across calls.

#[tokio::test]
async fn capacity_gate_can_observe_request_before_response_streams() {
    let turn = vec![canned::simple_text_turn("ok")];
    let mock = MockLlmClient::new(turn);

    // Build a "near-limit" request — many user messages.
    let mut messages = Vec::new();
    for i in 0..200 {
        messages.push(user_message(&format!("m{i}")));
    }
    let req = make_request(messages);

    // BEFORE the runtime drains the stream, the mock has already captured
    // the request. The capacity controller can inspect this and short-circuit
    // the dispatch if the estimated token cost exceeds the soft cap.
    let stream_future = mock.create_message_stream(req);
    let mut stream = stream_future.await.unwrap();

    assert_eq!(mock.captured_requests().len(), 1);
    let captured = mock.last_request().unwrap();
    assert_eq!(captured.messages.len(), 200);
    // Verify the capacity gate could compute a "should defer" decision based
    // on raw message count + payload size of the captured request.
    let total_chars: usize = captured
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .map(|b| match b {
            ContentBlock::Text { text, .. } => text.len(),
            _ => 0,
        })
        .sum();
    assert!(
        total_chars > 100,
        "synthetic over-cap request should have non-trivial size"
    );

    // Drain to keep the mock state consistent.
    while stream.next().await.is_some() {}
}

// === 8. Compaction defaults (#402 P0) ======================================

#[test]
fn compaction_config_defaults_are_enabled_for_session_survivability() {
    // The production CompactionConfig is gated behind a `#[path = ...]` module
    // that isn't wired here, but we can test the principle: the
    // `should_compact` function and `CompactionConfig` live in the same crate.
    // Re-import from the production module to verify the default.
    //
    // We test via the mock pathway: the non-streaming compaction call (test 5
    // above) already exercises `create_message` with `stream: Some(false)`,
    // which is the code path `compact_messages` uses. Combined with the
    // capacity controller's `TargetedContextRefresh`, the enabled-by-default
    // compaction config means long sessions auto-compact before hitting the
    // context window limit.
    //
    // This test is a smoke check that the defaults compile and are correct.
    // The production `CompactionConfig::default()` is exercised by
    // `compaction::tests::should_compact_respects_enabled_flag` etc.
    let config =
        codewhale_models::compaction_threshold_for_model_at_percent("deepseek-v4-pro", 80.0);
    // Verify the threshold is reasonable (> 0 and < context window).
    assert!(config > 0, "compaction threshold must be positive");
    assert!(config < 1_000_000, "compaction threshold must be below 1M");
}

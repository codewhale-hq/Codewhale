//! Redacted thinking capture end to end: a safeguard redacted block arrives
//! complete on its start event, must be parked in wire order next to the
//! readable thinking blocks, and must ride the follow-up request byte-exact.

use super::*;
use crate::llm_client::mock::{MockLlmClient, canned};
use crate::tools::spec::{ApprovalRequirement, ToolCapability, ToolSpec};

/// A no-op mutating-free tool that auto-approves, so the streamed call is
/// admitted and the follow-up request is issued.
struct RedactedFixtureTool;

#[async_trait::async_trait]
impl ToolSpec for RedactedFixtureTool {
    fn name(&self) -> &str {
        "redacted_fixture_read"
    }
    fn description(&self) -> &str {
        "Fixture tool that closes a thinking-heavy turn."
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn capabilities(&self) -> Vec<ToolCapability> {
        Vec::new()
    }
    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }
    async fn execute(&self, _: Value, _: &ToolContext) -> Result<ToolResult, ToolError> {
        Ok(ToolResult::success("read"))
    }
}

#[tokio::test]
async fn redacted_thinking_persists_in_wire_order_into_the_followup_request() {
    let workspace = tempdir().unwrap();
    let mock = Arc::new(MockLlmClient::new(vec![
        vec![
            canned::message_start("redacted-turn"),
            // Block 0: readable signed thinking, finished by its stop event.
            StreamEvent::ContentBlockStart {
                index: 0,
                content_block: ContentBlockStart::Thinking {
                    thinking: "first block".to_string(),
                },
            },
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta::ThinkingDelta {
                    thinking: " continued".to_string(),
                },
            },
            StreamEvent::ContentBlockDelta {
                index: 0,
                delta: Delta::SignatureDelta {
                    signature: "sig-1".to_string(),
                },
            },
            canned::block_stop(0),
            // Block 1: the redacted block — complete at start, no deltas.
            StreamEvent::ContentBlockStart {
                index: 1,
                content_block: ContentBlockStart::RedactedThinking {
                    data: "ENC_REDACTED_1".to_string(),
                },
            },
            canned::block_stop(1),
            // The tool call closes the turn and forces the follow-up request.
            canned::tool_use_block_start(2, "toolu_redacted", "redacted_fixture_read"),
            canned::tool_input_delta(2, "{}"),
            canned::block_stop(2),
            canned::message_delta("tool_use", None),
            canned::message_stop(),
        ],
        canned::simple_text_turn("done"),
    ]));
    let (mut engine, _handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        mock.clone(),
    );
    engine.session.auto_approve = true;
    let mut registry = crate::tools::ToolRegistry::new(ToolContext::new(workspace.path()));
    registry.register(Arc::new(RedactedFixtureTool));
    let tools = Some(registry.to_api_tools_with_cache(true));
    let surface = test_tool_surface(&engine, registry, tools, AppMode::Agent);
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;

    assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
    // The persisted assistant turn keeps the whole sequence in wire order.
    let tool_call_turn = |message: &&Message| {
        message.role == Role::Assistant
            && message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
    };
    let persisted = engine
        .session
        .messages
        .iter()
        .rev()
        .find(tool_call_turn)
        .expect("the turn persists the tool-call assistant message");
    let mut blocks = persisted.content.iter();
    match (blocks.next(), blocks.next(), blocks.next(), blocks.next()) {
        (
            Some(ContentBlock::Thinking {
                thinking,
                signature,
                state: None,
                redacted_data: None,
                ..
            }),
            Some(ContentBlock::Thinking {
                thinking: empty,
                signature: None,
                state: None,
                redacted_data: Some(data),
            }),
            Some(ContentBlock::ToolUse { .. }),
            None,
        ) => {
            assert_eq!(thinking, "first block continued");
            assert_eq!(signature.as_deref(), Some("sig-1"));
            assert!(
                empty.is_empty(),
                "a redacted block carries no readable text"
            );
            assert_eq!(data, "ENC_REDACTED_1");
        }
        other => panic!("unexpected persisted assistant blocks: {other:?}"),
    }

    // The follow-up request replays the same sequence, redacted payload
    // included byte-exact.
    assert!(
        mock.call_count() >= 2,
        "the admitted tool call forces a follow-up request"
    );
    let requests = mock.captured_requests();
    let followup = requests[1]
        .messages
        .iter()
        .rev()
        .find(tool_call_turn)
        .expect("follow-up request carries the prior assistant turn");
    let mut replayed = followup.content.iter();
    match (replayed.next(), replayed.next(), replayed.next()) {
        (
            Some(ContentBlock::Thinking {
                thinking,
                signature,
                ..
            }),
            Some(ContentBlock::Thinking {
                redacted_data: Some(data),
                ..
            }),
            Some(ContentBlock::ToolUse { .. }),
        ) => {
            assert_eq!(thinking, "first block continued");
            assert_eq!(signature.as_deref(), Some("sig-1"));
            assert_eq!(data, "ENC_REDACTED_1", "payload replays byte-exact");
        }
        other => panic!("unexpected replayed assistant blocks: {other:?}"),
    }
}

/// A stream that parks a redacted block and then dies without a completion
/// must not be silently re-issued: the parked block is received content, so
/// the death surfaces its error instead of double-billing for the turn.
#[tokio::test]
async fn stream_dying_after_a_redacted_block_is_not_treated_as_empty() {
    let workspace = tempdir().unwrap();
    let mock = Arc::new(MockLlmClient::new(vec![vec![
        canned::message_start("redacted-then-die"),
        StreamEvent::ContentBlockStart {
            index: 0,
            content_block: ContentBlockStart::RedactedThinking {
                data: "ENC_REDACTED_DIE".to_string(),
            },
        },
        canned::block_stop(0),
        StreamEvent::Error {
            error: json!({ "message": "Model not exist." }),
        },
    ]]));
    let (mut engine, _handle) = Engine::new_with_model_client(
        deterministic_engine_config(workspace.path()),
        &Config::default(),
        mock.clone(),
    );
    let surface = test_tool_surface(
        &engine,
        crate::tools::ToolRegistry::new(ToolContext::new(workspace.path())),
        None,
        AppMode::Agent,
    );
    let mut turn = crate::core::turn::TurnContext::new(4);

    let (status, error) = engine.run_turn(&mut turn, surface, None, None).await;

    assert_eq!(status, TurnOutcomeStatus::Failed);
    assert!(
        error
            .as_deref()
            .is_some_and(|e| e.contains("Model not exist.")),
        "{error:?}"
    );
    assert_eq!(
        mock.call_count(),
        1,
        "a stream that captured a redacted block authorizes no silent re-run"
    );
}

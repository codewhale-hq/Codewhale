use futures_util::StreamExt;

use crate::llm_client::LlmClient;
use crate::llm_client::mock::{MockLlmClient, canned};
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message, MessageRequest};

fn user_message(text: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
            cache_control: None,
        }],
    }
}

fn assistant_thinking_tool_call(
    thinking: &str,
    id: &str,
    name: &str,
    input: serde_json::Value,
) -> Message {
    Message {
        role: Role::Assistant,
        content: vec![
            ContentBlock::Thinking {
                thinking: thinking.to_string(),
                signature: None,
                state: None,
                redacted_data: None,
            },
            ContentBlock::ToolUse {
                execution_id: None,
                id: id.to_string(),
                name: name.to_string(),
                input,
                caller: None,
                thought_signature: None,
            },
        ],
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
        safety_check: None,
        title: None,
    }
}

#[tokio::test]
async fn reasoning_content_is_replayed_after_thinking_tool_call() {
    let mock = MockLlmClient::new(vec![]);

    mock.push_turn(vec![
        canned::message_start("r1"),
        canned::thinking_delta(0, "I should inspect /tmp before answering."),
        canned::tool_use_block_start(1, "call_a", "list_dir"),
        canned::tool_input_delta(1, r#"{"path":"/tmp"}"#),
        canned::block_stop(1),
        canned::message_delta("tool_use", None),
        canned::message_stop(),
    ]);

    mock.push_factory(|request| {
        let assistant = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "assistant")
            .expect("follow-up request must include the prior assistant tool-call turn");

        assert!(
            assistant
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::Thinking { .. })),
            "DeepSeek V4 follow-up requests must replay reasoning_content on the assistant tool-call turn"
        );

        canned::simple_text_turn("I see the /tmp entries.")
    });

    let mut first = mock
        .create_message_stream(make_request(vec![user_message("list /tmp")]))
        .await
        .expect("first stream opens");
    while first.next().await.is_some() {}

    let mut second = mock
        .create_message_stream(make_request(vec![
            user_message("list /tmp"),
            assistant_thinking_tool_call(
                "I should inspect /tmp before answering.",
                "call_a",
                "list_dir",
                serde_json::json!({ "path": "/tmp" }),
            ),
            tool_result_message("call_a", "/tmp/file1\n/tmp/file2"),
        ]))
        .await
        .expect("second stream opens");
    while second.next().await.is_some() {}
}

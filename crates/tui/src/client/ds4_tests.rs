use super::CodewhaleClient;
use super::chat::{parse_chat_message, parse_sse_chunk};
use anyhow::Result;
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Delta, Message, MessageRequest, StreamEvent, Tool};
use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig};
use serde_json::{Value, json};

fn ds4_client() -> CodewhaleClient {
    let mut providers = ProvidersConfig::default();
    providers.custom.insert(
        "ds4".to_string(),
        ProviderConfig {
            kind: Some("openai-compatible".to_string()),
            base_url: Some("http://127.0.0.1:8000/v1".to_string()),
            model: Some("deepseek-v4-flash".to_string()),
            context_window: Some(100_000),
            auth_mode: Some("none".to_string()),
            ..Default::default()
        },
    );
    CodewhaleClient::new(&{
        let __cw_config_init_0_0 = Some("ds4".to_string());
        let __cw_config_init_0_1 = Some(providers);
        let mut __cw_config_init_0_value: Config = Default::default();
        __cw_config_init_0_value.provider = __cw_config_init_0_0;
        __cw_config_init_0_value.providers = __cw_config_init_0_1;
        __cw_config_init_0_value
    })
    .expect("DS4 client")
}

fn request(effort: &str) -> MessageRequest {
    MessageRequest {
        model: "deepseek-v4-flash".to_string(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        }],
        max_tokens: 128,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: Some(effort.to_string()),
        stream: None,
        temperature: None,
        top_p: None,
    }
}

#[test]
fn named_ds4_route_uses_deepseek_reasoning_controls() -> Result<()> {
    let client = ds4_client();

    let off = client.prepare_outbound_request(request("off"), true)?;
    assert_eq!(off.body["thinking"]["type"], "disabled");
    assert!(off.body.get("reasoning_effort").is_none());

    let max = client.prepare_outbound_request(request("max"), true)?;
    assert_eq!(max.body["thinking"]["type"], "enabled");
    assert_eq!(max.body["reasoning_effort"], "max");
    Ok(())
}

#[test]
fn named_ds4_route_removes_unsupported_strict_tool_flag() -> Result<()> {
    let client = ds4_client();
    let mut request = request("high");
    request.tools = Some(vec![Tool {
        tool_type: Some("function".to_string()),
        name: "read_file".to_string(),
        description: "Read one file".to_string(),
        input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        allowed_callers: None,
        defer_loading: None,
        input_examples: None,
        strict: Some(true),
        cache_control: None,
    }]);

    let outbound = client.prepare_outbound_request(request, true)?;
    assert!(
        outbound.body["tools"][0]["function"]
            .get("strict")
            .is_none()
    );
    Ok(())
}

#[test]
fn non_streaming_fixture_preserves_tool_call_and_usage() -> Result<()> {
    // Recorded DS4/OpenAI-compatible response shape. Keep this fixture
    // provider-free: DS4 should stay on the shared parser contract.
    let response = parse_chat_message(&json!({
        "id": "chatcmpl-ds4-tool",
        "model": "deepseek-v4-flash",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "index": 0,
                    "id": "call_ds4_0",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": "{\"path\":\"src/main.rs\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 23,
            "completion_tokens": 7,
            "total_tokens": 30
        }
    }))?;

    assert!(matches!(
        response.content.as_slice(),
        [ContentBlock::ToolUse { id, name, input, ..}]
            if id == "call_ds4_0"
                && name == "read_file"
                && input == &json!({"path": "src/main.rs"})
    ));
    assert_eq!(response.usage.input_tokens, 23);
    assert_eq!(response.usage.output_tokens, 7);
    Ok(())
}

#[test]
fn malformed_tool_arguments_remain_visible_for_feedback() -> Result<()> {
    let response = parse_chat_message(&json!({
        "id": "chatcmpl-ds4-malformed",
        "model": "deepseek-v4-flash",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_bad",
                    "type": "function",
                    "function": {"name": "read_file", "arguments": "{bad json"}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    }))?;

    assert!(matches!(
        response.content.as_slice(),
        [ContentBlock::ToolUse { input: Value::String(raw), ..}] if raw == "{bad json"
    ));
    Ok(())
}

#[test]
fn replay_placeholder_echo_is_dropped_from_ingest() {
    // GLM-5.x mirrors the serializer's outgoing `(reasoning omitted)`
    // placeholder back as a live reasoning delta. Ingesting it persisted a
    // fake thinking block into the transcript and rendered it live. Only the
    // exact transport echo is dropped; genuine reasoning passes through.
    let mut content_index = 0;
    let mut text_started = false;
    let mut thinking_started = false;
    let mut tool_indices = std::collections::HashMap::new();
    let mut reasoning_detail_buffers = std::collections::HashMap::new();
    let chunks = [
        json!({
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "(reasoning omitted)"},
                "finish_reason": null
            }]
        }),
        json!({
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "  (reasoning omitted) \n"},
                "finish_reason": null
            }]
        }),
        json!({
            "choices": [{
                "index": 0,
                "delta": {"reasoning_content": "the user wants a table"},
                "finish_reason": null
            }]
        }),
    ];
    let events = chunks
        .iter()
        .flat_map(|chunk| {
            parse_sse_chunk(
                chunk,
                &mut content_index,
                &mut text_started,
                &mut thinking_started,
                &mut tool_indices,
                &mut reasoning_detail_buffers,
                true,
            )
        })
        .collect::<Vec<_>>();
    let thinking_deltas = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ContentBlockDelta {
                delta: Delta::ThinkingDelta { thinking },
                ..
            } => Some(thinking.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        thinking_deltas,
        vec!["the user wants a table"],
        "placeholder echoes must be dropped, real reasoning kept"
    );
}

#[test]
fn streaming_fixture_accepts_delayed_tool_arguments_and_usage_tail() {
    let mut content_index = 0;
    let mut text_started = false;
    let mut thinking_started = false;
    let mut tool_indices = std::collections::HashMap::new();
    let mut reasoning_detail_buffers = std::collections::HashMap::new();
    let chunks = [
        json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_ds4_0",
                        "type": "function",
                        "function": {"name": "read_file", "arguments": "{\"path\":"}
                    }]
                },
                "finish_reason": null
            }]
        }),
        json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {"arguments": "\"src/main.rs\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }),
        json!({
            "choices": [],
            "usage": {"prompt_tokens": 23, "completion_tokens": 7, "total_tokens": 30}
        }),
    ];

    let events = chunks
        .iter()
        .flat_map(|chunk| {
            parse_sse_chunk(
                chunk,
                &mut content_index,
                &mut text_started,
                &mut thinking_started,
                &mut tool_indices,
                &mut reasoning_detail_buffers,
                false,
            )
        })
        .collect::<Vec<_>>();

    let argument_deltas = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ContentBlockDelta {
                delta: Delta::InputJsonDelta { partial_json },
                ..
            } => Some(partial_json.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(argument_deltas, "{\"path\":\"src/main.rs\"}");
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::MessageDelta { usage: Some(usage), .. }
            if usage.input_tokens == 23 && usage.output_tokens == 7
    )));
}

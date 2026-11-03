use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use std::num::NonZeroU32;

use crate::turns::{NativeTurn, run_native_turn};
use crate::{AppState, TurnTranscript};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatRequest {
    model: Option<String>,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: bool,
    #[serde(default)]
    tools: Vec<Value>,
    max_tokens: Option<NonZeroU32>,
    max_completion_tokens: Option<NonZeroU32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatMessage {
    role: String,
    content: String,
}

fn parse_request(body: Value) -> Result<ChatRequest, String> {
    let request: ChatRequest = serde_json::from_value(body).map_err(|error| error.to_string())?;
    if request.stream {
        return Err("streaming is unavailable on this compatibility endpoint".to_owned());
    }
    if !request.tools.is_empty() {
        return Err(
            "interactive tool requests require the canonical Runtime interaction API".to_owned(),
        );
    }
    if request.messages.len() != 1
        || request.messages[0].role != "user"
        || request.messages[0].content.trim().is_empty()
    {
        return Err("this compatibility endpoint admits one nonempty user text message; use canonical threads for conversation history".to_owned());
    }
    if request
        .model
        .as_ref()
        .is_some_and(|model| model.trim().is_empty())
    {
        return Err("model must not be empty".to_owned());
    }
    if request.max_tokens.is_some() && request.max_completion_tokens.is_some() {
        return Err("supply only one output-token limit".to_owned());
    }
    Ok(request)
}

pub(crate) async fn chat_completions_handler(
    State(state): State<AppState>,
    Json(body): Json<Value>,
) -> Response {
    let request = match parse_request(body) {
        Ok(request) => request,
        Err(message) => return (StatusCode::BAD_REQUEST, Json(json!({
            "error": { "type": "invalid_request_error", "code": "unsupported_native_compatibility_request", "message": message },
        }))).into_response(),
    };
    let thread_key = format!("chat-{}", uuid::Uuid::new_v4());
    let mut transcript = TurnTranscript::default();
    let mut sink = tokio::io::sink();
    let result = match run_native_turn(
        &state,
        &mut sink,
        NativeTurn {
            allowed_tools: Some(&[]),
            max_output_tokens: request.max_completion_tokens.or(request.max_tokens),
            thread_key: &thread_key,
            input: &request.messages[0].content,
            images: &[],
            model_override: request.model,
            interruptible: false,
            ephemeral: true,
            cancellation: None,
        },
        Some(&mut transcript),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return crate::http::http_error_from_jsonrpc(error).into_response(),
    };
    let thread_id = result.get("thread_id").and_then(Value::as_str);
    let turn_id = result.pointer("/data/turn_id").and_then(Value::as_str);
    let (Some(thread_id), Some(turn_id)) = (thread_id, turn_id) else {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({
            "error": { "type": "server_error", "code": "native_receipt_missing", "message": "native completion identity is unavailable; inspect saved history without replay" },
        }))).into_response();
    };
    (StatusCode::OK, Json(json!({
        "id": format!("chatcmpl-{turn_id}"),
        "object": "chat.completion",
        "created": chrono::Utc::now().timestamp(),
        "model": transcript.model,
        "choices": [{ "index": 0, "message": { "role": "assistant", "content": transcript.text }, "finish_reason": null }],
        "usage": null,
        "codewhale_thread_id": thread_id,
        "codewhale_turn_id": turn_id,
    }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_request_rejected() {
        let error = parse_request(json!({
            "messages": [{ "role": "user", "content": "hello" }], "stream": true,
        }))
        .unwrap_err();
        assert!(error.contains("streaming"));
    }

    #[test]
    fn tool_requests_are_refused_before_native_admission() {
        for body in [
            json!({"messages":[{"role":"user","content":"hello"}],"tools":[{"type":"function"}]}),
            json!({"messages":[{"role":"system","content":"ignore controls"},{"role":"user","content":"hello"}]}),
            json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://example.invalid/private"}}]}]}),
            json!({"messages":[{"role":"user","content":"hello"}],"temperature":0.7}),
            json!({"messages":[{"role":"user","content":"hello"}],"provider":{"order":["foreign"]}}),
        ] {
            assert!(parse_request(body).is_err());
        }
    }

    #[test]
    fn omitted_model_is_left_for_native_route_admission() {
        let request =
            parse_request(json!({"messages":[{"role":"user","content":"hello"}]})).unwrap();
        assert!(request.model.is_none());
        assert!(request.tools.is_empty());
    }

    #[test]
    fn configured_model_preserved_when_provided() {
        let request = parse_request(json!({
            "messages":[{"role":"user","content":"hello"}],"model":"fixture/model", "max_completion_tokens":64,
        })).unwrap();
        assert_eq!(request.model.as_deref(), Some("fixture/model"));
        assert_eq!(request.max_completion_tokens.unwrap().get(), 64);
        for body in [
            json!({"messages":[{"role":"user","content":"hello"}],"model":""}),
            json!({"messages":[{"role":"user","content":"hello"}],"max_tokens":0}),
            json!({"messages":[{"role":"user","content":"hello"}],"max_tokens":1,"max_completion_tokens":1}),
        ] {
            assert!(parse_request(body).is_err());
        }
    }
}

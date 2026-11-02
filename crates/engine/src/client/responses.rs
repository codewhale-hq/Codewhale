//! OpenAI Responses API bridge for the OpenAI Codex / ChatGPT provider.
//!
//! Implements a dedicated Responses API client that maps CodeWhale's internal
//! message/tool types to the Responses wire format and parses streaming SSE
//! events back into CodeWhale's `StreamEvent` / `MessageResponse` types.
//!
//! This is intentionally separate from the Chat Completions path
//! (`client/chat.rs`) to avoid protocol hacks.

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::llm_client::StreamEventBox;
use codewhale_config::ProviderKind;
use codewhale_models::schema_sanitize;
use codewhale_models::{
    ContentBlock, ContentBlockStart, Delta, MessageDelta, MessageRequest, MessageResponse,
    OpaqueReasoningState, StreamEvent, Tool, Usage,
};
use codewhale_runtime::logging;

use super::CodewhaleClient;
use super::chat::REASONING_REPLAY_PLACEHOLDER;
use super::prepared::WireDialect;
use super::role_placement::{RolePlacement, role_placement};
use super::wire::{
    ERROR_BODY_MAX_BYTES, bounded_error_text, from_api_tool_name, system_to_instructions,
    to_api_tool_name,
};
use super::wire::{extract_sse_data_value, next_sse_line, push_sse_event_data};
use crate::llm_client::LlmError;

const CHATGPT_TOOL_NAMESPACE: &str = "codewhale";

/// Build the Responses API request body from a `MessageRequest`.
#[cfg(test)]
pub(super) fn build_responses_body(request: &MessageRequest) -> Value {
    build_responses_body_for_provider(request, ProviderKind::OpenaiCodex, None)
}

/// Build a provider-aware Responses API request body.
///
/// DeepSeek-V4-Flash-0731 implements the Responses wire shape but is stateless
/// and exposes plain reasoning text rather than OpenAI encrypted summaries.
/// Keep those exact-route differences here instead of leaking them into the
/// provider-neutral message model.
pub(super) fn build_responses_body_for_provider(
    request: &MessageRequest,
    provider: ProviderKind,
    reasoning_api: Option<&str>,
) -> Value {
    let is_deepseek = matches!(provider, ProviderKind::Deepseek);
    // Concentrate documents `model`, `input`, `stream`, `max_output_tokens`,
    // `tools` / `tool_choice` / `parallel_tool_calls`, and `reasoning.effort`;
    // `store`, `include`, `instructions`, and `reasoning.summary` are absent
    // from its parameter reference, so this route sends only documented
    // fields and carries the system prompt as a leading `system` message item
    // (a documented input role) instead of `instructions`.
    // https://concentrate.ai/docs/api-reference/endpoint/request-parameters
    let is_concentrate = provider == ProviderKind::Concentrate;
    let model = &request.model;
    let mut body = json!({
        "model": model,
        "stream": true,
    });
    if !is_deepseek && !is_concentrate {
        body["store"] = json!(false);
    }
    // Every Responses route receives the same resolved request envelope as
    // Chat and Messages. Omitting this field let auxiliary Responses calls
    // escape the central route cap and made preview unable to prove the wire
    // allowance. The official ChatGPT plan preview contract disallows
    // max_output_tokens, so that route carries no client-side output cap.
    if request.max_tokens > 0 && provider != ProviderKind::OpenaiCodex {
        body["max_output_tokens"] = json!(request.max_tokens);
    }
    if is_deepseek {
        if let Some(temperature) = request.temperature {
            body["temperature"] = json!(temperature);
        }
        if let Some(top_p) = request.top_p {
            body["top_p"] = json!(top_p);
        }
    }

    // Supply minimal instructions when the caller did not provide a system prompt.
    let instructions = system_to_instructions(request.system.clone())
        .filter(|text| !text.trim().is_empty())
        .unwrap_or_else(|| "You are a helpful assistant.".to_string());

    // Convert messages to Responses input items.
    let mut input = convert_messages_to_responses_input(request, provider, reasoning_api);
    if is_concentrate {
        input.insert(
            0,
            json!({
                "type": "message",
                "role": "system",
                "content": [{ "type": "input_text", "text": instructions }],
            }),
        );
    } else {
        body["instructions"] = json!(instructions);
    }
    body["input"] = json!(input);

    // Convert tools to Responses function tools.
    if let Some(tools) = request.tools.as_ref() {
        let responses_tools: Vec<Value> = tools.iter().map(tool_to_responses_function).collect();
        if !responses_tools.is_empty() {
            body["tools"] = if provider == ProviderKind::OpenaiCodex {
                json!([{ "type": "namespace", "name": CHATGPT_TOOL_NAMESPACE,
                    "description": "Codewhale tools running under the user's local permissions.",
                    "tools": responses_tools }])
            } else {
                json!(responses_tools)
            };
            let tool_choice = request.tool_choice.as_ref().and_then(Value::as_str);
            body["tool_choice"] = if tool_choice == Some("none") {
                json!("none")
            } else {
                json!("auto")
            };
            // The plan preview decoder tracks one active function-call block.
            body["parallel_tool_calls"] = json!(provider != ProviderKind::OpenaiCodex);
        }
    }

    // Preserve the selected Codex tier through the final wire boundary. The
    // roster owns each model's available levels; this pure builder must not
    // collapse newer tiers to an older model's xhigh ceiling. Other Responses
    // providers retain their own compatibility vocabulary.
    if let Some(raw) = request.reasoning_effort.as_deref()
        && let Some(effort) = if provider == ProviderKind::OpenaiCodex {
            codex_responses_reasoning_effort(raw)
        } else {
            responses_reasoning_effort(raw, is_deepseek)
        }
    {
        body["reasoning"] = if is_deepseek || is_concentrate {
            json!({ "effort": effort })
        } else {
            json!({
                "effort": effort,
                "summary": "auto",
            })
        };
    }

    // OpenAI Codex can replay encrypted reasoning. DeepSeek exposes plain
    // `reasoning_text` and does not support `include`.
    if !is_deepseek && !is_concentrate {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }

    body
}

impl CodewhaleClient {
    /// Handle a streaming Responses request for the resolved provider route.
    pub(super) async fn handle_responses_stream(
        &self,
        prepared: &super::PreparedOutboundRequest,
    ) -> Result<StreamEventBox> {
        // Body, endpoint, and route shape all come from the shared
        // prepared-request seam (`prepare_outbound_request`).
        let body = &prepared.body;
        let is_chatgpt = self.api_provider == ProviderKind::OpenaiCodex;
        let url = prepared.endpoint.url.clone();
        // The synthetic MessageStart below is emitted from inside the stream
        // closure, which outlives `prepared`. Clone the wire model — the id
        // actually placed on the body by the shared seam, after route
        // remapping — rather than borrowing the request that no longer exists
        // at this layer.
        let wire_model = prepared.wire_model.clone();
        let reasoning_api = if is_chatgpt {
            self.chatgpt_reasoning_api.as_deref()
        } else {
            Some("openai-responses")
        };
        let reasoning_origin = reasoning_api.map(|api| {
            (
                self.api_provider.as_str().to_string(),
                api.to_string(),
                wire_model.clone(),
            )
        });

        // The bearer Authorization header is already installed as a default
        // header on both HTTP clients, so it must not be duplicated here.
        // The public API does not use Codex backend identity or beta headers.
        //
        // Provider backoff stays outside each response-header wait. The
        // returned SSE body carries no reqwest total timeout.
        let request_body =
            serde_json::to_vec(&body).context("Failed to serialize Responses API request body")?;
        let response = self
            .send_stream_open_with_retry(|policy| {
                super::stream_entry::client_for_policy(
                    &self.http_client,
                    self.http1_fallback_client(),
                    policy,
                )
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Accept", "text/event-stream")
                .body(request_body.clone())
            })
            .await
            .context("Responses API request failed")?;

        let status = response.status();
        crate::client::record_provider_response(self.api_provider, status.as_u16());
        if !status.is_success() {
            let raw = bounded_error_text(response, ERROR_BODY_MAX_BYTES).await;
            let raw = self.redact_model_bound_text(&raw);
            return Err(LlmError::from_http_response(status.as_u16(), &raw).into());
        }

        let stream_idle_timeout = self.stream_idle_timeout;
        let first_byte = super::stream_entry::first_byte_timeout(stream_idle_timeout);
        let provider_label = self.api_provider.provider().display_name();
        let error_secrets = self.model_bound_secret_values.clone();
        let byte_stream = response.bytes_stream();

        let stream = async_stream::stream! {
            use futures_util::StreamExt;

            // Emit synthetic MessageStart.
            yield Ok(StreamEvent::MessageStart {
                message: MessageResponse {
                    id: String::new(),
                    r#type: "message".to_string(),
                    role: "assistant".to_string(),
                    content: vec![],
                    model: wire_model.clone(),
                    stop_reason: None,
                    stop_sequence: None,
                    container: None,
                    usage: Usage::default(),
                },
            });

            let mut current_block_index: Option<u32> = None;
            // Whether reasoning text has already been emitted for the current
            // reasoning block. Used to insert a paragraph break between
            // consecutive summary parts, which the wire protocol delivers
            // back-to-back with no separator.
            let mut reasoning_text_emitted = false;
            let mut saw_tool_call = false;
            let mut usage_data: Option<Usage> = None;
            // Raw byte buffer: decode only COMPLETE lines (or the stream-end
            // tail) via the shared take_sse_line / flush_sse_line helpers so a
            // multi-byte UTF-8 char split across HTTP/2 DATA is never
            // corrupted to U+FFFD. Genuine invalid bytes fail closed.
            let mut buffer: Vec<u8> = Vec::new();
            // `data:` fields of the event being assembled, dispatched at the
            // blank line that ends it (or at stream end).
            let mut event_data = String::new();
            let mut done = false;
            let mut ended = false;
            let mut content_block_counter: u32 = 0;
            let stream_start = std::time::Instant::now();
            let mut last_chunk_at = std::time::Instant::now();
            let mut bytes_received: usize = 0;

            tokio::pin!(byte_stream);

            while !done {
                if !ended {
                    let wait = super::stream_entry::next_chunk_timeout(
                        stream_idle_timeout,
                        first_byte,
                        bytes_received,
                    );
                    match tokio::time::timeout(wait, byte_stream.next()).await {
                        Ok(Some(Ok(chunk))) => {
                            bytes_received += chunk.len();
                            last_chunk_at = std::time::Instant::now();
                            buffer.extend_from_slice(&chunk);
                        }
                        Ok(Some(Err(e))) => {
                            yield Err(anyhow::anyhow!("Stream read error: {e}"));
                            return;
                        }
                        Ok(None) => ended = true,
                        Err(_) => {
                            yield Err(anyhow::anyhow!(super::stream_entry::body_timeout_message(
                                wait,
                                bytes_received,
                                stream_start.elapsed(),
                                last_chunk_at.elapsed(),
                                provider_label,
                            )));
                            return;
                        }
                    }
                }

                // Process complete SSE lines, and the unterminated tail at stream end.
                loop {
                    let data = match next_sse_line(&mut buffer, ended) {
                        // A blank line ends the event.
                        Ok(Some(line)) if line.is_empty() => std::mem::take(&mut event_data),
                        Ok(Some(line)) => {
                            if line.starts_with(':') {
                                // SSE comment keep-alive: the provider is alive (#6184).
                                yield Ok(StreamEvent::Ping);
                            } else if let Some(value) = extract_sse_data_value(&line)
                                && let Err(err) = push_sse_event_data(&mut event_data, value)
                            {
                                if is_chatgpt {
                                    yield Err(LlmError::ParseError(err.to_string()).into());
                                    return;
                                }
                                yield Err(anyhow::anyhow!("{err}"));
                                return;
                            }
                            continue;
                        }
                        // The final event may arrive without its blank line.
                        Ok(None) if ended && !event_data.is_empty() => {
                            std::mem::take(&mut event_data)
                        }
                        Ok(None) => break,
                        Err(err) => {
                            if is_chatgpt {
                                yield Err(LlmError::ParseError(err.to_string()).into());
                                return;
                            }
                            yield Err(anyhow::anyhow!("{err}"));
                            return;
                        }
                    };
                    if data.is_empty() {
                        continue;
                    }

                    {
                        let data = data.as_str();
                        if data == "[DONE]" {
                            if !is_chatgpt { done = true; }
                            break;
                        }

                        let event: Value = match serde_json::from_str(data) {
                            Ok(v) => v,
                            Err(e) => {
                                if is_chatgpt {
                                    yield Err(LlmError::ParseError("Invalid ChatGPT response event".into()).into());
                                    return;
                                }
                                logging::warn(format!(
                                    "Failed to parse Responses SSE event: {e}"
                                ));
                                continue;
                            }
                        };

                        let event_type =
                            event.get("type").and_then(|t| t.as_str()).unwrap_or("");

                        match event_type {
                            "response.output_item.added" => {
                                if let Some(item) = event.get("item") {
                                    let item_type = item
                                        .get("type")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("");

                                    match item_type {
                                        "message" => {
                                            content_block_counter += 1;
                                            yield Ok(StreamEvent::ContentBlockStart {
                                                index: content_block_counter - 1,
                                                content_block: ContentBlockStart::Text {
                                                    text: String::new(),
                                                },
                                            });
                                            current_block_index =
                                                Some(content_block_counter - 1);
                                        }
                                        "function_call" => {
                                            if is_chatgpt && item.get("namespace").and_then(Value::as_str) != Some(CHATGPT_TOOL_NAMESPACE) {
                                                yield Err(LlmError::ParseError("ChatGPT returned a function outside the Codewhale tool namespace".into()).into());
                                                return;
                                            }
                                            let call_id = item
                                                .get("call_id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let item_id = item
                                                .get("id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            let name = item
                                                .get("name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("")
                                                .to_string();
                                            saw_tool_call = true;
                                            // call_id and item_id are folded
                                            // into a composite tool-use id so
                                            // the function_call_output can be
                                            // routed back to the right call.
                                            let composite_id =
                                                format!("{call_id}|{item_id}");
                                            content_block_counter += 1;
                                            yield Ok(StreamEvent::ContentBlockStart {
                                                index: content_block_counter - 1,
                                                content_block:
                                                    ContentBlockStart::ToolUse {
                                                        id: composite_id,
                                                        name: from_api_tool_name(&name),
                                                        input: json!({}),
                                                        caller: None,
                                                    thought_signature: None,
                                                },
                                            });
                                            current_block_index =
                                                Some(content_block_counter - 1);
                                        }
                                        "reasoning" => {
                                            reasoning_text_emitted = false;
                                            content_block_counter += 1;
                                            yield Ok(StreamEvent::ContentBlockStart {
                                                index: content_block_counter - 1,
                                                content_block:
                                                    ContentBlockStart::Thinking {
                                                        thinking: String::new(),
                                                    },
                                            });
                                            current_block_index =
                                                Some(content_block_counter - 1);
                                        }
                                        // DeepSeek can run server-side web
                                        // search on this route, but Codewhale
                                        // does not yet replay `web_search_call`
                                        // items or their citations (the
                                        // offering keeps
                                        // `server_side_web_search: Unknown`).
                                        // Surface a visible notice instead of
                                        // dropping the item silently so the
                                        // user is not handed an ungrounded
                                        // answer with no explanation.
                                        "web_search_call" => {
                                            content_block_counter += 1;
                                            yield Ok(StreamEvent::ContentBlockStart {
                                                index: content_block_counter - 1,
                                                content_block:
                                                    ContentBlockStart::Text {
                                                        text: "[Web search ran server-side; results are not replayed on this route.]".to_string(),
                                                    },
                                            });
                                            current_block_index =
                                                Some(content_block_counter - 1);
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            "response.output_text.delta" => {
                                if let Some(delta_text) =
                                    event.get("delta").and_then(|d| d.as_str())
                                    && let Some(idx) = current_block_index
                                {
                                    yield Ok(StreamEvent::ContentBlockDelta {
                                        index: idx,
                                        delta: Delta::TextDelta {
                                            text: delta_text.to_string(),
                                        },
                                    });
                                }
                            }
                            "response.function_call_arguments.delta" => {
                                if let Some(delta_text) =
                                    event.get("delta").and_then(|d| d.as_str())
                                    && let Some(idx) = current_block_index
                                {
                                    yield Ok(StreamEvent::ContentBlockDelta {
                                        index: idx,
                                        delta: Delta::InputJsonDelta {
                                            partial_json: delta_text.to_string(),
                                        },
                                    });
                                }
                            }
                            "response.reasoning_summary_text.delta"
                            | "response.reasoning_text.delta" => {
                                if let Some(delta_text) =
                                    event.get("delta").and_then(|d| d.as_str())
                                    && let Some(idx) = current_block_index
                                {
                                    if !delta_text.is_empty() {
                                        reasoning_text_emitted = true;
                                    }
                                    yield Ok(StreamEvent::ContentBlockDelta {
                                        index: idx,
                                        delta: Delta::ThinkingDelta {
                                            thinking: delta_text.to_string(),
                                        },
                                    });
                                }
                            }
                            "response.reasoning_summary_part.added" => {
                                // Consecutive summary parts arrive with no
                                // separator in the text deltas, so without a
                                // boundary they concatenate as
                                // "…done.**Next Phase**…". Insert a paragraph
                                // break before every part after the first.
                                if reasoning_text_emitted
                                    && let Some(idx) = current_block_index
                                {
                                    yield Ok(StreamEvent::ContentBlockDelta {
                                        index: idx,
                                        delta: Delta::ThinkingDelta {
                                            thinking: "\n\n".to_string(),
                                        },
                                    });
                                }
                            }
                            "response.output_item.done" => {
                                if let Some(idx) = current_block_index {
                                    if let (Some((provider, api, model)), Some(item)) =
                                        (reasoning_origin.as_ref(), event.get("item"))
                                        && item.get("type").and_then(Value::as_str)
                                            == Some("reasoning")
                                        && let Some(encrypted_content) = item
                                            .get("encrypted_content")
                                            .and_then(Value::as_str)
                                            .filter(|value| !value.is_empty())
                                    {
                                        yield Ok(StreamEvent::ContentBlockDelta {
                                            index: idx,
                                            delta: Delta::ReasoningStateDelta {
                                                state: OpaqueReasoningState {
                                                    provider: provider.clone(),
                                                    api: api.clone(),
                                                    model: model.clone(),
                                                    id: item
                                                        .get("id")
                                                        .and_then(Value::as_str)
                                                        .map(str::to_string),
                                                    encrypted_content: encrypted_content.to_string(),
                                                },
                                            },
                                        });
                                    }
                                    yield Ok(StreamEvent::ContentBlockStop { index: idx });
                                    current_block_index = None;
                                }
                            }
                            "response.completed" | "response.incomplete" => {
                                if is_chatgpt && !event.get("response").is_some_and(Value::is_object) {
                                    yield Err(LlmError::ParseError("ChatGPT completion event has no response".into()).into());
                                    return;
                                }
                                if is_chatgpt && event_type == "response.completed" && string_at(&event, "/response/status") != Some("completed") {
                                    yield Err(LlmError::ParseError("ChatGPT completion event has an unsuccessful status".into()).into());
                                    return;
                                }
                                if is_chatgpt && event_type == "response.incomplete" {
                                    if let Some(usage_val) = event.pointer("/response/usage") {
                                        yield Ok(StreamEvent::MessageDelta {
                                            delta: MessageDelta { stop_reason: None, stop_sequence: None },
                                            usage: Some(parse_responses_usage(usage_val)),
                                        });
                                    }
                                    let (code, msg) = responses_event_error_details(&event);
                                    let code = super::redact_model_bound_text(&code, &error_secrets);
                                    let msg = super::redact_model_bound_text(&msg, &error_secrets);
                                    yield Err(LlmError::ParseError(format!("ChatGPT response incomplete [{code}]: {msg}")).into());
                                    return;
                                }
                                if let Some(resp) = event.get("response") {
                                    if let Some(usage_val) = resp.get("usage") {
                                        usage_data =
                                            Some(parse_responses_usage(usage_val));
                                    }
                                    let stop_reason = responses_stop_reason(resp, saw_tool_call);
                                    yield Ok(StreamEvent::MessageDelta {
                                        delta: MessageDelta {
                                            stop_reason: Some(stop_reason),
                                            stop_sequence: None,
                                        },
                                        usage: usage_data.take(),
                                    });
                                }
                                // DeepSeek terminates semantic Responses
                                // streams with this event and deliberately does
                                // not send `data: [DONE]`.
                                done = true;
                            }
                            "error" | "response.failed" => {
                                let (code, msg) = responses_event_error_details(&event);
                                if is_chatgpt {
                                    if let Some(usage_val) = event.pointer("/response/usage") {
                                        yield Ok(StreamEvent::MessageDelta {
                                            delta: MessageDelta { stop_reason: None, stop_sequence: None },
                                            usage: Some(parse_responses_usage(usage_val)),
                                        });
                                    }
                                    if let Some(error) = LlmError::from_subscription_sharing_error_code(&code) {
                                        yield Err(error.into());
                                        return;
                                    }
                                }
                                let code = super::redact_model_bound_text(&code, &error_secrets);
                                let msg = super::redact_model_bound_text(&msg, &error_secrets);
                                if is_chatgpt {
                                    yield Err(LlmError::ModelError(format!("Responses API error [{code}]: {msg}")).into());
                                    return;
                                }
                                yield Err(anyhow::anyhow!(
                                    "Responses API error [{code}]: {msg}"
                                ));
                                return;
                            }
                            _ => {
                                // Ignore unknown event types.
                            }
                        }
                    }
                }

                if ended {
                    break;
                }
            }

            // ChatGPT requires a successful response.completed event; other
            // Responses routes also accept their documented terminal markers.
            // A bare HTTP EOF is truncation, and
            // a MessageStop here would hand the turn a partial answer as done.
            if !done {
                if is_chatgpt {
                    yield Err(LlmError::ParseError("ChatGPT Responses stream closed before a successful completion event".into()).into());
                    return;
                }
                yield Err(anyhow::anyhow!(
                    "Responses stream closed before a successful completion event"
                ));
                return;
            }
            yield Ok(StreamEvent::MessageStop);
        };

        Ok(Box::pin(stream))
    }

    /// Non-streaming Responses request: drive the streaming handler and fold
    /// its events into a single `MessageResponse`.
    ///
    /// The official ChatGPT plan preview requires streaming. The blocking
    /// entry point (`create_message`, used by `exec`) folds that same wire path.
    pub(super) async fn handle_responses_message(
        &self,
        prepared: &super::PreparedOutboundRequest,
    ) -> Result<MessageResponse> {
        use futures_util::StreamExt;

        let model = prepared.wire_model.clone();
        let mut stream = self.handle_responses_stream(prepared).await?;

        let mut response = MessageResponse {
            id: String::new(),
            r#type: "message".to_string(),
            role: "assistant".to_string(),
            content: Vec::new(),
            model,
            stop_reason: None,
            stop_sequence: None,
            container: None,
            usage: Usage::default(),
        };
        // Accumulated tool-call argument JSON, parallel to `response.content`.
        let mut tool_args: Vec<String> = Vec::new();

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::MessageStart { message } => {
                    response.id = message.id;
                    response.usage = message.usage;
                }
                StreamEvent::ContentBlockStart { content_block, .. } => {
                    let block = match content_block {
                        ContentBlockStart::Text { text } => ContentBlock::Text {
                            text,
                            cache_control: None,
                        },
                        ContentBlockStart::Thinking { thinking } => ContentBlock::Thinking {
                            thinking,
                            signature: None,
                            state: None,
                            redacted_data: None,
                        },
                        // Redacted thinking does not occur on Responses
                        // routes (encrypted reasoning rides the reasoning
                        // item's `encrypted_content` instead); keep the
                        // match total without inventing a block shape.
                        ContentBlockStart::RedactedThinking { .. } => {
                            continue;
                        }
                        ContentBlockStart::ToolUse {
                            id,
                            name,
                            input,
                            caller,
                            thought_signature,
                        } => ContentBlock::ToolUse {
                            execution_id: None,
                            id,
                            name,
                            input,
                            caller,
                            thought_signature,
                        },
                        ContentBlockStart::ServerToolUse { id, name, input } => {
                            ContentBlock::ServerToolUse { id, name, input }
                        }
                    };
                    response.content.push(block);
                    tool_args.push(String::new());
                }
                StreamEvent::ContentBlockDelta { index, delta } => {
                    let i = index as usize;
                    match delta {
                        Delta::TextDelta { text } => {
                            if let Some(ContentBlock::Text { text: existing, .. }) =
                                response.content.get_mut(i)
                            {
                                existing.push_str(&text);
                            }
                        }
                        Delta::ThinkingDelta { thinking } => {
                            if let Some(ContentBlock::Thinking {
                                thinking: existing, ..
                            }) = response.content.get_mut(i)
                            {
                                existing.push_str(&thinking);
                            }
                        }
                        Delta::InputJsonDelta { partial_json } => {
                            if let Some(buf) = tool_args.get_mut(i) {
                                buf.push_str(&partial_json);
                            }
                        }
                        Delta::SignatureDelta { .. } => {
                            // Anthropic-native signature deltas never occur on
                            // the Responses bridge (#3014).
                        }
                        Delta::ReasoningStateDelta { state } => {
                            if let Some(ContentBlock::Thinking {
                                state: existing, ..
                            }) = response.content.get_mut(i)
                            {
                                *existing = Some(state);
                            }
                        }
                    }
                }
                StreamEvent::ContentBlockStop { index } => {
                    let i = index as usize;
                    if let Some(buf) = tool_args.get(i)
                        && !buf.trim().is_empty()
                        && let Ok(parsed) = serde_json::from_str::<Value>(buf)
                        && let Some(ContentBlock::ToolUse { input, .. }) =
                            response.content.get_mut(i)
                    {
                        *input = parsed;
                    }
                }
                StreamEvent::MessageDelta { delta, usage } => {
                    if let Some(stop_reason) = delta.stop_reason {
                        response.stop_reason = Some(stop_reason);
                    }
                    if let Some(usage) = usage {
                        response.usage = usage;
                    }
                }
                StreamEvent::MessageStop => break,
                _ => {}
            }
        }

        Ok(response)
    }
}

pub(super) fn responses_tool_output(content: &str, content_blocks: Option<&[Value]>) -> Value {
    let (image, omitted) = crate::image_attach::provider_tool_result_image_refs(content_blocks);
    let content = codewhale_models::image_window::tool_result_text_with_omission(content, omitted);
    let Some((mime_type, data)) = image else {
        return json!(content);
    };
    let mut output = Vec::with_capacity(2);
    if !content.is_empty() {
        output.push(json!({ "type": "input_text", "text": content }));
    }
    output.push(json!({
        "type": "input_image",
        "image_url": format!("data:{mime_type};base64,{data}"),
        "detail": "auto",
    }));
    json!(output)
}

/// Convert Codewhale messages to Responses API input items.
pub(super) fn convert_messages_to_responses_input(
    request: &MessageRequest,
    provider: ProviderKind,
    reasoning_api: Option<&str>,
) -> Vec<Value> {
    let is_deepseek = matches!(provider, ProviderKind::Deepseek);
    let mut items = Vec::new();

    for msg in &request.messages {
        // Channel selection lives in the shared placement table; this adapter
        // owns only the shape of each channel's items.
        let placement = role_placement(&msg.role, WireDialect::OpenAiResponses);
        match placement {
            RolePlacement::User => {
                let mut content_items = Vec::new();
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            content_items.push(json!({
                                "type": "input_text",
                                "text": text,
                            }));
                        }
                        ContentBlock::ImageUrl { image_url } => {
                            content_items.push(json!({
                                "type": "input_image",
                                "image_url": image_url.url,
                            }));
                        }
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                            content_blocks,
                            ..
                        } => {
                            if !content_items.is_empty() {
                                items.push(json!({
                                    "type": "message",
                                    "role": "user",
                                    "content": content_items,
                                }));
                                content_items = Vec::new();
                            }
                            let (call_id, _item_id) = parse_tool_use_id(tool_use_id);
                            items.push(json!({
                                "type": "function_call_output",
                                "call_id": call_id,
                                "output": responses_tool_output(content, content_blocks.as_deref()),
                            }));
                        }
                        _ => {}
                    }
                }
                if !content_items.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "user",
                        "content": content_items,
                    }));
                }
            }
            RolePlacement::Assistant | RolePlacement::InterruptedAssistant => {
                let reasoning_item = |block: &ContentBlock| -> Option<Value> {
                    let ContentBlock::Thinking {
                        thinking, state, ..
                    } = block
                    else {
                        return None;
                    };
                    if let Some(state) = state {
                        let matches = state.provider == provider.as_str()
                            && if provider == ProviderKind::OpenaiCodex {
                                reasoning_api.is_some_and(|api| state.api == api)
                            } else {
                                state.api == "openai-responses"
                            }
                            && state.model == request.model;
                        if !matches {
                            return None;
                        }
                        let mut item = json!({
                            "type": "reasoning",
                            "summary": [],
                            "encrypted_content": state.encrypted_content,
                        });
                        if let Some(id) = state.id.as_ref() {
                            item["id"] = json!(id);
                        }
                        return Some(item);
                    }
                    is_deepseek.then(|| {
                        let text = if thinking.trim().is_empty() {
                            REASONING_REPLAY_PLACEHOLDER
                        } else {
                            thinking.as_str()
                        };
                        json!({
                            "type": "reasoning",
                            "content": [{
                                "type": "reasoning_text",
                                "text": text,
                            }],
                        })
                    })
                };
                if is_deepseek {
                    let before = items.len();
                    items.extend(msg.content.iter().filter_map(&reasoning_item));
                    if items.len() == before
                        && msg
                            .content
                            .iter()
                            .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
                    {
                        items.push(json!({
                            "type": "reasoning",
                            "content": [{
                                "type": "reasoning_text",
                                "text": REASONING_REPLAY_PLACEHOLDER,
                            }],
                        }));
                    }
                }
                for block in &msg.content {
                    match block {
                        ContentBlock::Text { text, .. } => {
                            let text = if placement == RolePlacement::InterruptedAssistant {
                                format!(
                                    "{}{}",
                                    codewhale_models::INTERRUPTED_ASSISTANT_CONTEXT_PREFIX,
                                    text
                                )
                            } else {
                                text.clone()
                            };
                            items.push(json!({
                                "type": "message",
                                "role": "assistant",
                                "content": [{
                                    "type": "output_text",
                                    "text": text,
                                }],
                            }));
                        }
                        ContentBlock::ToolUse {
                            id, name, input, ..
                        } => {
                            let (call_id, _item_id) = parse_tool_use_id(id);
                            let mut item = json!({
                                "type": "function_call",
                                "call_id": call_id,
                                "name": to_api_tool_name(name),
                                "arguments": serde_json::to_string(input).unwrap_or_default(),
                            });
                            if provider == ProviderKind::OpenaiCodex {
                                item["namespace"] = json!(CHATGPT_TOOL_NAMESPACE);
                            }
                            items.push(item);
                        }
                        ContentBlock::Thinking { .. } if !is_deepseek => {
                            if let Some(item) = reasoning_item(block) {
                                items.push(item);
                            }
                        }
                        _ => {}
                    }
                }
            }
            // `System` and `Developer` are typed placements for load-bearing
            // in-history context. `Omitted` also receives compatible transcript
            // spellings that predate the closed Role enum; preserve the
            // representable `tool`, `system`, and `developer` wire shapes
            // instead of silently deleting them.
            RolePlacement::System | RolePlacement::Developer | RolePlacement::Omitted => {
                match msg.role.as_str() {
                    "tool" => {
                        for block in &msg.content {
                            if let ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                                content_blocks,
                                ..
                            } = block
                            {
                                let (call_id, _item_id) = parse_tool_use_id(tool_use_id);
                                items.push(json!({
                                    "type": "function_call_output",
                                    "call_id": call_id,
                                    "output": responses_tool_output(
                                        content,
                                        content_blocks.as_deref(),
                                    ),
                                }));
                            }
                        }
                    }
                    role @ ("system" | "developer") => {
                        let content_items: Vec<Value> = msg
                            .content
                            .iter()
                            .filter_map(|block| match block {
                                ContentBlock::Text { text, .. } => Some(json!({
                                    "type": "input_text",
                                    "text": text,
                                })),
                                _ => None,
                            })
                            .collect();
                        if !content_items.is_empty() {
                            items.push(json!({
                                "type": "message",
                                "role": if provider == ProviderKind::OpenaiCodex && role == "system" { "developer" } else { role },
                                "content": content_items,
                            }));
                        }
                    }
                    other => {
                        logging::warn(format!(
                            "Responses adapter dropped a message with unsupported role {other:?}"
                        ));
                    }
                }
            }
            // The outbound seam refuses rejected pairs before body building;
            // keeping this arm empty is fail-closed defense in depth.
            RolePlacement::Rejected => {}
        }
    }

    items
}

/// Convert a CodeWhale tool definition to a Responses API function tool.
fn tool_to_responses_function(tool: &Tool) -> Value {
    let mut parameters = tool.input_schema.clone();
    let constraint_note = schema_sanitize::sanitize_for_responses(&mut parameters);
    let description = match constraint_note {
        Some(note) if tool.description.trim().is_empty() => note,
        Some(note) => format!("{}\n\n{}", tool.description.trim(), note),
        None => tool.description.clone(),
    };
    json!({
        "type": "function",
        "name": to_api_tool_name(&tool.name),
        "description": description,
        "parameters": parameters,
        "strict": false,
    })
}

fn codex_responses_reasoning_effort(raw: &str) -> Option<&'static str> {
    crate::reasoning_preference::ReasoningEffort::parse_strict(raw)
        .unwrap_or(crate::reasoning_preference::ReasoningEffort::Medium)
        .api_value_for_provider(ProviderKind::OpenaiCodex)
}

fn compatible_responses_reasoning_effort(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" | "disabled" | "none" | "false" => Some("low"),
        "minimal" => Some("low"),
        "low" => Some("low"),
        "high" => Some("high"),
        "xhigh" | "max" | "maximum" | "ultra" | "ultracode" => Some("xhigh"),
        _ => Some("medium"),
    }
}

/// DeepSeek's Responses wire spelling of the shared tier table
/// (`client::deepseek_effort`), which is the single annotated source for the
/// tier ladder — including the documented `"none"` off tier, so the picker's
/// Off entry stays off instead of collapsing into a still-thinking low.
///
/// Unknown tiers normalize to the table's default tier.
pub(super) fn responses_reasoning_effort(raw: &str, is_deepseek: bool) -> Option<&'static str> {
    if !is_deepseek {
        return compatible_responses_reasoning_effort(raw);
    }
    if raw.trim().eq_ignore_ascii_case("auto") {
        return None;
    }
    Some(super::deepseek_effort::deepseek_effort_tier_or_default(raw).responses_effort())
}

fn responses_event_error_details(event: &Value) -> (String, String) {
    let event_type = string_at(event, "/type").unwrap_or("error");
    let code = first_string_at(
        event,
        &[
            "/code",
            "/error/code",
            "/response/error/code",
            "/response/incomplete_details/reason",
            "/response/status",
        ],
    )
    .unwrap_or("unknown");
    let message = LlmError::from_subscription_sharing_error_code(code).map_or_else(
        || {
            first_string_at(
                event,
                &[
                    "/message",
                    "/error/message",
                    "/response/error/message",
                    "/response/incomplete_details/reason",
                ],
            )
            .map_or_else(
                || format!("{event_type} event received"),
                |message| {
                    if message == code && event_type == "response.incomplete" {
                        format!("response incomplete: {message}")
                    } else {
                        message.to_string()
                    }
                },
            )
        },
        |error| error.to_string(),
    );
    (code.to_string(), message)
}

fn responses_stop_reason(response: &Value, saw_tool_call: bool) -> String {
    match string_at(response, "/status").unwrap_or("completed") {
        "completed" if saw_tool_call => "tool_use".to_string(),
        "completed" => "end_turn".to_string(),
        "incomplete" => format!(
            "incomplete:{}",
            string_at(response, "/incomplete_details/reason").unwrap_or("max_tokens")
        ),
        _ => "end_turn".to_string(),
    }
}

fn first_string_at<'a>(value: &'a Value, paths: &[&str]) -> Option<&'a str> {
    paths.iter().find_map(|path| string_at(value, path))
}

fn string_at<'a>(value: &'a Value, path: &str) -> Option<&'a str> {
    value.pointer(path).and_then(Value::as_str).and_then(|s| {
        let trimmed = s.trim();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

/// Parse a composite tool_use_id back to (call_id, item_id).
/// Composite format: "call_id|item_id"
pub(super) fn parse_tool_use_id(id: &str) -> (String, String) {
    if let Some(pipe_pos) = id.find('|') {
        (id[..pipe_pos].to_string(), id[pipe_pos + 1..].to_string())
    } else {
        (id.to_string(), String::new())
    }
}

/// Parse usage from a Responses API usage object.
fn parse_responses_usage(val: &Value) -> Usage {
    let input = val
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .map_or(0, super::saturating_u32);
    let output = val
        .get("output_tokens")
        .and_then(|v| v.as_u64())
        .map_or(0, super::saturating_u32);
    // Cache telemetry arrives in two dialects. DeepSeek's Responses payload
    // uses the same top-level `prompt_cache_hit_tokens` /
    // `prompt_cache_miss_tokens` fields as its Chat-Completions endpoint,
    // while OpenAI-style payloads nest `cached_tokens` under
    // `input_tokens_details`. Prefer the top-level hit, falling back to the
    // nested form when the payload only reports that.
    let nested_cached_tokens = val
        .get("input_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64());
    let prompt_cache_hit_tokens = val
        .get("prompt_cache_hit_tokens")
        .and_then(|v| v.as_u64())
        .or(nested_cached_tokens)
        .map(super::saturating_u32);
    // DeepSeek reports the miss explicitly; otherwise mirror the
    // Chat-Completions parser: derive the miss as input minus the cached hit
    // when the payload reported cached input tokens. Responses nests
    // reasoning under `output_tokens_details` (not `completion_tokens_details`).
    let prompt_cache_miss_tokens = val
        .get("prompt_cache_miss_tokens")
        .and_then(|v| v.as_u64())
        .map(super::saturating_u32)
        .or_else(|| prompt_cache_hit_tokens.map(|hit| input.saturating_sub(hit)));
    // Cache-creation tokens, kept as their own class so pricing can apply the
    // write rate where the provider publishes one. DeepSeek-style payloads
    // nest these under `input_tokens_details`; accept a top-level spelling
    // too for providers that flatten the object.
    let prompt_cache_write_tokens = val
        .get("prompt_cache_write_tokens")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            val.get("input_tokens_details")
                .and_then(|d| d.get("cache_write_tokens"))
                .and_then(|v| v.as_u64())
        })
        .map(super::saturating_u32);
    // `output_tokens` is already the total billable completion count, with
    // reasoning a subset of it. A payload reporting more reasoning than output
    // violates that, so the value is rejected as invalid telemetry rather than
    // being trusted or turned into extra billable output (#4318).
    let reasoning_tokens = val
        .get("output_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64())
        .map(super::saturating_u32)
        .filter(|reasoning| *reasoning <= output);
    // `input_tokens` stays the provider-reported *total* (cache-hit + miss +
    // write + uncategorized): `token_usage_for_pricing` partitions it into
    // billable classes and the context budget measures the window with it.
    // Reducing it here to miss-only would double-subtract at those surfaces.
    Usage {
        input_tokens: input,
        output_tokens: output,
        prompt_cache_hit_tokens,
        prompt_cache_miss_tokens,
        prompt_cache_write_tokens,
        reasoning_tokens,
        reasoning_replay_tokens: None,
        server_tool_use: None,

        input_audio_tokens: val
            .pointer("/input_tokens_details/audio_tokens")
            .and_then(Value::as_u64)
            .map(super::saturating_u32),
        output_audio_tokens: val
            .pointer("/output_tokens_details/audio_tokens")
            .and_then(Value::as_u64)
            .map(super::saturating_u32),

        request_usages: Vec::new(),
    }
}

#[cfg(test)]
mod tests;

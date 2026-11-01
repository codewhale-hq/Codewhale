//! Native Anthropic Messages API adapter (#3014).
//!
//! CodeWhale's internal wire types are already Anthropic-shaped (the harness
//! speaks Messages internally and translates *out* to OpenAI dialects), so
//! this adapter is mostly native serialization plus an SSE pass-through:
//! `StreamEvent` deserializes Anthropic's `message_start` /
//! `content_block_*` / `message_delta` / `message_stop` / `ping` events
//! directly. What the adapter adds on top:
//!
//! - request shaping: adaptive thinking + `output_config.effort` from
//!   CodeWhale's `reasoning_effort` tiers, sampling-parameter rules for
//!   models that reject them, and `cache_control` breakpoint placement
//!   aligned with the prefix-zone model in `prefix_cache.rs`;
//! - usage normalization (#2961 / #4318): `prompt_cache_hit_tokens` comes from
//!   `cache_read_input_tokens`, `prompt_cache_write_tokens` from
//!   `cache_creation_input_tokens`, `prompt_cache_miss_tokens` is the raw
//!   non-cached `input_tokens`, and the normalized `input_tokens` is the sum
//!   of all three (total prompt, the DeepSeek convention);
//! - signed-thinking handling: `signature_delta` is captured into
//!   [`codewhale_models::Delta::SignatureDelta`] and assistant thinking blocks
//!   replay verbatim (signature included); unsigned thinking blocks are
//!   dropped from replay because the API rejects them.
//!
//! Modeled on `client/responses.rs` (separate file per dialect, no protocol
//! hacks in the shared paths).

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::llm_client::StreamEventBox;
use codewhale_config::ProviderKind;
use codewhale_models::schema_sanitize;
use codewhale_models::{ContentBlock, MessageRequest, MessageResponse, StreamEvent, Usage};
use codewhale_runtime::config::wire_model_for_provider_route;
use codewhale_runtime::logging;

use super::CodewhaleClient;
use super::prepared::WireDialect;
use super::role_placement::{RolePlacement, role_placement};
use super::wire::{extract_sse_data_value, next_sse_line, push_sse_event_data};

/// Maximum `cache_control` breakpoints Anthropic accepts per request.
const MAX_CACHE_BREAKPOINTS: usize = 4;

impl CodewhaleClient {
    /// Build the native Messages API request body from a [`MessageRequest`].
    pub(super) fn build_anthropic_body(&self, request: &MessageRequest, stream: bool) -> Value {
        let model =
            wire_model_for_provider_route(self.api_provider, &self.base_url, &request.model);
        let mut body = json!({
            "model": model,
            "max_tokens": request.max_tokens,
            "stream": stream,
        });

        if let Some(system) = request.system.as_ref() {
            body["system"] = match system {
                codewhale_models::SystemPrompt::Text(text) => json!(text),
                codewhale_models::SystemPrompt::Blocks(blocks) => json!(
                    blocks
                        .iter()
                        .map(|block| {
                            let mut value = json!({
                                "type": "text",
                                "text": block.text,
                            });
                            if let Some(cache) = block.cache_control.as_ref() {
                                value["cache_control"] = json!({ "type": cache.cache_type });
                            }
                            value
                        })
                        .collect::<Vec<_>>()
                ),
            };
        }

        let mut messages: Vec<Value> = request
            .messages
            .iter()
            .filter_map(message_to_anthropic)
            .collect();
        merge_split_tool_results(&mut messages);
        repair_dangling_tool_uses(&mut messages);
        body["messages"] = Value::Array(messages);

        if let Some(tools) = request.tools.as_ref()
            && !tools.is_empty()
        {
            body["tools"] = json!(
                tools
                    .iter()
                    .map(|tool| {
                        // Sanitize the tool's input_schema the same way the
                        // OpenAI Responses adapter does: strip top-level
                        // oneOf/anyOf/allOf (which Anthropic rejects), merge
                        // alternative properties into the root, and surface
                        // the dropped constraint as a description note so the
                        // model still knows which parameters are expected.
                        let mut schema = tool.input_schema.clone();
                        let constraint_note = schema_sanitize::sanitize_for_responses(&mut schema);
                        let description = match constraint_note {
                            Some(note) if tool.description.trim().is_empty() => note,
                            Some(note) => format!("{}\n\n{}", tool.description.trim(), note),
                            None => tool.description.clone(),
                        };
                        let mut value = json!({
                            "name": tool.name,
                            "description": description,
                            "input_schema": schema,
                        });
                        if let Some(strict) = tool.strict {
                            value["strict"] = json!(strict);
                        }
                        if let Some(cache) = tool.cache_control.as_ref() {
                            value["cache_control"] = json!({ "type": cache.cache_type });
                        }
                        value
                    })
                    .collect::<Vec<_>>()
            );
        }

        if let Some(tool_choice) = request.tool_choice.as_ref() {
            body["tool_choice"] = anthropic_tool_choice(tool_choice, &model);
        }

        // Thinking + effort shaping. MiniMax supports adaptive/disabled but
        // not Anthropic's output_config effort field; native Anthropic routes
        // keep the existing effort mapping. Other Messages-compatible
        // gateways (#4978, e.g. Sensenova) only accept the documented
        // enabled/disabled/auto thinking types, so non-native routes get the
        // portable `{"type":"enabled","budget_tokens":N}` shape instead.
        let thinking_capable = self
            .route_facts
            .as_ref()
            .filter(|facts| facts.wire_model_id == request.model)
            .and_then(|facts| facts.reasoning)
            .unwrap_or_else(|| codewhale_models::model_supports_reasoning(&model));
        let is_minimax_provider = self.api_provider == ProviderKind::MinimaxAnthropic;
        let is_minimax = codewhale_runtime::config::is_exact_minimax_anthropic_m3_route(
            self.api_provider,
            &self.base_url,
            &model,
        );
        let is_deepseek = self.api_provider == ProviderKind::DeepseekAnthropic;
        // Model Studio's Anthropic-compatible endpoint documents the portable
        // `{"type":"enabled","budget_tokens":N}` shape AND `{"type":"disabled"}`
        // (alibabacloud.com/help/en/model-studio/anthropic-api-messages), so
        // an explicit "off" can be honored on the wire instead of silently
        // falling through to the server default (which is thinking-ON for the
        // qwen3.x families).
        let is_modelstudio = matches!(
            self.api_provider,
            ProviderKind::ModelstudioTokenPlan
                | ProviderKind::ModelstudioTokenPlanAnthropic
                | ProviderKind::ModelstudioCodingPlan
                | ProviderKind::ModelstudioCodingPlanAnthropic
        );
        // MiniMax's exact M3 route and DeepSeek's Messages dialect both
        // document adaptive support; everything else needs the native host.
        let supports_adaptive =
            is_native_anthropic_base_url(&self.base_url) || is_minimax || is_deepseek;
        let effort = request
            .reasoning_effort
            .as_deref()
            .map(|raw| raw.trim().to_ascii_lowercase());
        match effort.as_deref() {
            _ if is_minimax_provider && !is_minimax => {}
            Some("off" | "disabled" | "none" | "false")
                if (is_minimax || is_deepseek || is_modelstudio) && thinking_capable =>
            {
                // Deliberately includes thinking-only Model Studio models
                // (qwen3.8-max family): unlike the chat dialect's
                // enable_thinking switch, the Messages endpoint documents the
                // portable {"type":"disabled"} shape for them
                // (alibabacloud.com/help/en/model-studio/anthropic-api-messages)
                // — pinned by modelstudio_messages_body_requests_thinking_
                // with_budget. Re-checked 2026-08-04.
                body["thinking"] = json!({ "type": "disabled" });
            }
            Some("off" | "disabled" | "none" | "false") => {}
            Some(level) if thinking_capable && supports_adaptive => {
                body["thinking"] = json!({ "type": "adaptive" });
                if !is_minimax {
                    let mapped = match level {
                        "low" | "minimal" => "low",
                        "medium" | "mid" => "medium",
                        "max" | "xhigh" | "highest" => "max",
                        _ => "high",
                    };
                    body["output_config"] = json!({ "effort": mapped });
                }
            }
            None if thinking_capable && supports_adaptive => {
                body["thinking"] = json!({ "type": "adaptive" });
            }
            _ if thinking_capable => {
                if let Some(mut budget) =
                    compat_thinking_budget(effort.as_deref(), request.max_tokens)
                {
                    let mut minimum = u64::from(MIN_THINKING_BUDGET_TOKENS);
                    if let Some(facts) = self
                        .route_facts
                        .as_ref()
                        .filter(|facts| facts.wire_model_id == request.model)
                        && let Some(option) = facts.reasoning_options.iter().find(|option| {
                            matches!(
                                option.get("type").and_then(Value::as_str),
                                Some("budget" | "budget_tokens")
                            )
                        })
                    {
                        minimum = option.get("min").and_then(Value::as_u64).unwrap_or(minimum);
                        let maximum = option
                            .get("max")
                            .and_then(Value::as_u64)
                            .unwrap_or(u64::from(request.max_tokens.saturating_sub(1)));
                        budget = budget.min(u32::try_from(maximum).unwrap_or(u32::MAX));
                    }
                    if u64::from(budget) >= minimum {
                        body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
                    } else {
                        tracing::warn!(
                            "The output allowance cannot represent the offering's minimum reasoning budget; thinking was omitted"
                        );
                    }
                }
            }
            _ => {}
        }

        // Anthropic documents `redacted_thinking.data` as opaque encrypted
        // content to pass back unchanged and says nothing about third-party
        // gateways understanding the block type, and we cannot tell a
        // terminating gateway from a forwarding proxy to the first-party
        // API (which would handle the block). Withholding on every
        // non-native host is the conservative call: a strict terminating
        // gateway would reject the unknown block type outright, while a
        // forwarding proxy only loses the block's continuity (a
        // redacted-only current tool turn can still 400 there). Native
        // `*.anthropic.com` routes keep the payload byte-exact.
        if !is_native_anthropic_base_url(&self.base_url)
            && let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut)
        {
            messages.retain_mut(|message| {
                let is_assistant = message.get("role").and_then(Value::as_str) == Some("assistant");
                let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
                    return true;
                };
                blocks.retain(|block| {
                    block.get("type").and_then(Value::as_str) != Some("redacted_thinking")
                });
                // A message that only carried redacted blocks would
                // otherwise go out with an empty `content: []`, which
                // Messages-compatible endpoints reject — the same
                // every-retry 400 this withhold exists to prevent. Dropping
                // the message is safe: a redacted-only assistant turn
                // carries no `tool_use`, so no `tool_result` can dangle.
                // Reachable only from foreign/imported sessions; the live
                // engine never persists a reasoning-only turn.
                !(is_assistant && blocks.is_empty())
            });
        }

        // Sampling parameters: every model from Claude 4.7 on (including the
        // Claude 5 generation and Haiku 5.5) rejects non-default
        // temperature/top_p; earlier models reject the two together. Send at
        // most one (temperature wins), or neither for models that forbid
        // them.
        if !anthropic_model_rejects_sampling(&request.model) {
            if let Some(temperature) = request.temperature {
                body["temperature"] = json!(temperature);
            } else if let Some(top_p) = request.top_p {
                body["top_p"] = json!(top_p);
            }
        }

        apply_anthropic_cache_breakpoints(&mut body);
        body
    }

    /// Non-streaming send through the shared typed retry path
    /// (`send_with_retry`): 429/5xx/transport failures retry with backoff and
    /// honor `Retry-After`, and a final failure stays a downcastable
    /// `LlmError` so the engine can classify auth, rate-limit, context and
    /// invalid-request failures like every other wire.
    async fn send_anthropic_request(&self, url: &str, body: &Value) -> Result<reqwest::Response> {
        let url = self.messages_transport_url(url);
        let request_body =
            serde_json::to_vec(body).context("Failed to serialize Anthropic Messages request")?;
        self.send_with_retry(|| {
            self.http_client
                .post(&url)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .header("Accept", "text/event-stream")
                .body(request_body.clone())
        })
        .await
        .context("Anthropic Messages API request failed")
    }

    /// Open Messages through the shared provider retry and stream-entry
    /// policy. Each physical request has its own header wait and at most one
    /// HTTP/1.1 fallback; provider backoff stays outside that wait. Request
    /// construction remains at this wire-specific edge.
    async fn open_anthropic_stream_response(
        &self,
        url: &str,
        body: &Value,
    ) -> Result<reqwest::Response> {
        let url = self.messages_transport_url(url);
        let request_body =
            serde_json::to_vec(body).context("Failed to serialize Anthropic Messages request")?;
        self.send_stream_open_with_retry(|policy| {
            super::stream_entry::client_for_policy(
                &self.http_client,
                self.http1_fallback_client(),
                policy,
            )
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("Accept", "text/event-stream")
            .body(request_body.clone())
        })
        .await
        .context("Anthropic Messages API request failed")
    }

    /// Handle a streaming Messages API request.
    pub(super) async fn handle_anthropic_stream(
        &self,
        prepared: &super::PreparedOutboundRequest,
    ) -> Result<StreamEventBox> {
        // Body and endpoint come from the shared prepared-request seam
        // (`prepare_outbound_request`), never from a second builder.
        let body = &prepared.body;
        let response = self
            .open_anthropic_stream_response(&prepared.endpoint.url, body)
            .await?;

        let stream_idle_timeout = self.stream_idle_timeout;
        let first_byte = super::stream_entry::first_byte_timeout(stream_idle_timeout);
        let provider_label = self.api_provider.provider().display_name();
        let suppress_error_details = self.claude_oauth_config.is_some();
        let byte_stream = response.bytes_stream();

        let stream = async_stream::stream! {
            use futures_util::StreamExt;

            // Raw byte buffer: decode only COMPLETE lines (or the stream-end
            // tail) via the shared take_sse_line / flush_sse_line helpers so a
            // multi-byte UTF-8 char (CJK/emoji) split across HTTP/2 DATA is
            // never corrupted to U+FFFD. Genuine invalid bytes fail closed.
            let mut buffer: Vec<u8> = Vec::new();
            // `data:` fields of the event being assembled, dispatched at the
            // blank line that ends it (or at stream end).
            let mut event_data = String::new();
            let stream_start = std::time::Instant::now();
            let mut last_chunk_at = std::time::Instant::now();
            let mut bytes_received: usize = 0;
            let mut ended = false;
            tokio::pin!(byte_stream);

            loop {
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

                loop {
                    let data = match next_sse_line(&mut buffer, ended) {
                        // A blank line ends the event.
                        Ok(Some(line)) if line.is_empty() => std::mem::take(&mut event_data),
                        Ok(Some(line)) => {
                            // `event:` lines are redundant (the data payload
                            // carries `type`) and comment/heartbeat lines are
                            // ignorable.
                            if let Some(value) = extract_sse_data_value(&line)
                                && let Err(err) = push_sse_event_data(&mut event_data, value)
                            {
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
                            yield Err(anyhow::anyhow!("{err}"));
                            return;
                        }
                    };
                    if data.is_empty() {
                        continue;
                    }

                    match convert_anthropic_sse_data(&data) {
                        Some(Ok(StreamEvent::Error { error })) => {
                            if suppress_error_details {
                                yield Err(anyhow::anyhow!("Claude subscription stream failed"));
                                return;
                            }
                            let (error_type, message) = anthropic_error_fields(&error);
                            yield Err(anyhow::anyhow!(
                                "Anthropic stream error ({error_type}): {message}"
                            ));
                            return;
                        }
                        Some(Ok(event)) => {
                            let is_stop = matches!(event, StreamEvent::MessageStop);
                            yield Ok(event);
                            if is_stop {
                                return;
                            }
                        }
                        Some(Err(e)) => {
                            if suppress_error_details {
                                logging::warn("Failed to parse Claude subscription stream event");
                            } else {
                                logging::warn(format!("Failed to parse Anthropic SSE event: {e}"));
                            }
                        }
                        None => {}
                    }
                }

                if ended {
                    break;
                }
            }
            // Only `message_stop` (returned above) or a provider error proves
            // the response is whole. A bare HTTP EOF is truncation, and ending
            // the stream quietly would hand the turn a partial answer as done.
            yield Err(anyhow::anyhow!(
                "Anthropic Messages stream closed before message_stop"
            ));
        };

        Ok(Box::pin(stream))
    }

    /// Handle a non-streaming Messages API request.
    pub(super) async fn handle_anthropic_message(
        &self,
        prepared: &super::PreparedOutboundRequest,
    ) -> Result<MessageResponse> {
        let response = self
            .send_anthropic_request(&prepared.endpoint.url, &prepared.body)
            .await?;
        let value: Value = response
            .json()
            .await
            .context("Failed to parse Anthropic Messages response")?;
        decode_anthropic_message(value)
    }
}

fn discard_provider_execution_ids(value: &mut Value) {
    // Shared history retains execution ids on disk. Incoming provider content
    // cannot choose one, including a value that would not deserialize as the
    // host field. Live execution must mint its own correlation instead.
    if let Some(blocks) = value.get_mut("content").and_then(Value::as_array_mut) {
        for block in blocks {
            if let Some(block) = block.as_object_mut() {
                block.remove("execution_id");
            }
        }
    }
}

fn decode_anthropic_message(mut value: Value) -> Result<MessageResponse> {
    discard_provider_execution_ids(&mut value);
    if let Some(usage) = value.get_mut("usage") {
        *usage = json!(parse_anthropic_usage(usage));
    }
    normalize_redacted_thinking_blocks(&mut value);
    serde_json::from_value(value).context("Failed to decode Anthropic Messages response")
}

/// Rewrite non-stream `redacted_thinking` content blocks into the
/// provider-neutral shape (`Thinking` with `redacted_data`) before decoding.
///
/// The provider-neutral content model has no `redacted_thinking` tag, so an
/// unknown block type would fail the whole response decode and lose the turn
/// — including the tool calls. Streaming responses carry the same blocks as
/// typed `ContentBlockStart::RedactedThinking` events and need no shim.
/// A block whose `data` is missing or not a string violates the contract;
/// it is still rewritten (with an empty payload) so the turn decodes and the
/// failure surfaces at replay with a diagnosable shape instead of as a
/// whole-response decode error.
fn normalize_redacted_thinking_blocks(value: &mut Value) {
    let Some(content) = value.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for block in content.iter_mut() {
        if block.get("type").and_then(Value::as_str) == Some("redacted_thinking") {
            let data = block.get("data").and_then(Value::as_str).unwrap_or("");
            if data.is_empty() {
                logging::warn(
                    "Response carried a redacted thinking block with a missing or non-string payload; capturing it with an empty payload",
                );
            }
            *block = json!({
                "type": "thinking",
                "thinking": "",
                "redacted_data": data,
            });
        }
    }
}

/// Build the `/v1/messages` endpoint URL, tolerating base URLs that already
/// carry a `/v1` suffix.
pub(super) fn anthropic_messages_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    if trimmed.ends_with("/v1") {
        format!("{trimmed}/messages")
    } else {
        format!("{trimmed}/v1/messages")
    }
}

/// Whether the route targets first-party Anthropic (`api.anthropic.com`),
/// where the `{"type":"adaptive"}` thinking control is valid. Strict
/// Anthropic-compatible gateways reject it (#4978).
fn is_native_anthropic_base_url(base_url: &str) -> bool {
    let rest = base_url
        .trim()
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host = rest
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    host == "api.anthropic.com" || host.ends_with(".anthropic.com")
}

/// Minimum `budget_tokens` the Messages API accepts for extended thinking.
const MIN_THINKING_BUDGET_TOKENS: u32 = 1024;

/// Effort-tier `budget_tokens` for gateways that only accept the documented
/// `{"type":"enabled","budget_tokens":N}` thinking shape (#4978). The wire
/// contract requires `budget_tokens >= 1024` and `< max_tokens`, so requests
/// too small to fit the minimum budget send no thinking block at all.
fn compat_thinking_budget(effort: Option<&str>, max_tokens: u32) -> Option<u32> {
    let tier: u32 = match effort {
        Some("low" | "minimal") => 4_096,
        Some("medium" | "mid") => 8_192,
        Some("max" | "xhigh" | "highest") => 32_768,
        // "high" and unspecified effort share the adaptive default tier.
        _ => 16_384,
    };
    let budget = tier.min(max_tokens.checked_sub(1)?);
    (budget >= MIN_THINKING_BUDGET_TOKENS).then_some(budget)
}

/// Fold a user turn that carries `tool_result`s into the user turn before it
/// (#6378).
///
/// The engine records each tool result as its own user message, so a parallel
/// tool-call batch arrives here as `assistant{use_a, use_b}`, `user{result_a}`,
/// `user{result_b}`. Anthropic wants every result in the user turn right after
/// the `tool_use`s, and the repair below reads only that turn: it would answer
/// `use_b` with an error placeholder while the real result sits in the next
/// message. Only turns carrying a `tool_result` are folded, so any other
/// consecutive user turns keep their shape; inside the merged turn the results
/// stay ahead of other content so they still lead it.
fn merge_split_tool_results(messages: &mut Vec<Value>) {
    let is_user = |message: &Value| message.get("role").and_then(Value::as_str) == Some("user");
    let is_tool_result =
        |block: &Value| block.get("type").and_then(Value::as_str) == Some("tool_result");
    let carries_tool_result = |message: &Value| {
        message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| blocks.iter().any(is_tool_result))
    };
    let mut merged: Vec<Value> = Vec::with_capacity(messages.len());
    for mut message in messages.drain(..) {
        if is_user(&message)
            && carries_tool_result(&message)
            && let Some(previous) = merged.last_mut()
            && is_user(previous)
        {
            let mut blocks = match previous["content"].take() {
                Value::Array(blocks) => blocks,
                other => vec![other],
            };
            match message["content"].take() {
                Value::Array(incoming) => blocks.extend(incoming),
                other => blocks.push(other),
            }
            // Stable sort: results keep their order and lead the turn.
            blocks.sort_by_key(|block| !is_tool_result(block));
            previous["content"] = Value::Array(blocks);
        } else {
            merged.push(message);
        }
    }
    *messages = merged;
}

/// Placeholder body for a `tool_use` that never produced a `tool_result`.
const UNEXECUTED_TOOL_RESULT: &str = "tool call was not executed";

/// Defensive wire repair (#5002): every assistant `tool_use` must be answered
/// by a `tool_result` in the immediately following user message, or the API
/// rejects the whole conversation with a 400 on every retry. Pre-dispatch
/// failure paths (e.g. the model calling an unavailable tool) can strand an
/// orphaned `tool_use` in history, so missing results get an explicit
/// error placeholder instead of poisoning the session.
fn repair_dangling_tool_uses(messages: &mut Vec<Value>) {
    let mut index = 0;
    while index < messages.len() {
        let ids = assistant_tool_use_ids(&messages[index]);
        if ids.is_empty() {
            index += 1;
            continue;
        }
        let next_is_user = messages
            .get(index + 1)
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            == Some("user");
        if !next_is_user {
            messages.insert(index + 1, json!({ "role": "user", "content": [] }));
        }
        if let Some(blocks) = messages[index + 1]
            .get_mut("content")
            .and_then(Value::as_array_mut)
        {
            let answered: std::collections::HashSet<String> = blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
                .filter_map(|block| block.get("tool_use_id").and_then(Value::as_str))
                .map(str::to_string)
                .collect();
            // tool_result blocks must lead the user turn, so placeholders are
            // prepended in tool_use order.
            for (offset, id) in ids
                .iter()
                .filter(|id| !answered.contains(id.as_str()))
                .enumerate()
            {
                blocks.insert(
                    offset,
                    json!({
                        "type": "tool_result",
                        "tool_use_id": id,
                        "content": UNEXECUTED_TOOL_RESULT,
                        "is_error": true,
                    }),
                );
            }
        }
        index += 1;
    }
}

fn assistant_tool_use_ids(message: &Value) -> Vec<String> {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return Vec::new();
    }
    message
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                .filter_map(|block| block.get("id").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Models that reject non-default `temperature` / `top_p` outright (every
/// model released after Opus 4.6). The Messages API reference states those
/// models "do not support" setting `temperature` / `top_p` beyond their
/// backwards-compatible defaults (`temperature: 1.0`, `top_p >= 0.99`), and
/// the migration guides restate it per family: "Setting `temperature`,
/// `top_p`, or `top_k` to any non-default value on Claude Opus 4.7 and
/// later models, including Claude Opus 5.5, returns a 400 error". That
/// covers the Claude 5 generation (`claude-opus-5(-5)`, `claude-sonnet-5(-5)`)
/// and Haiku 5.5 (which admits only its defaults — `temperature: 1`, a lone
/// `top_p: 0.99`; `top_p: 1`, any `top_k`, or both params together 400),
/// besides the Fable/Mythos lines. Dropping the params entirely is always
/// safe; sending a forbidden one bricks the request. Re-checked 2026-10-08.
fn anthropic_model_rejects_sampling(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    lower.contains("opus-4-7")
        || lower.contains("opus-4-8")
        || ["opus-5", "sonnet-5", "haiku-5"]
            .iter()
            .any(|family| lower.contains(family))
        || lower.contains("fable")
        || lower.contains("mythos")
}

/// Convert the engine's `tool_choice` value (OpenAI-style string or object)
/// to the Anthropic object form. The Messages API tool-use reference lists
/// Opus 5.5, Sonnet 5.5, Fable 5.1 and Mythos 5.1 as answering `any`/`tool`
/// with a 400 error, so those models fall back to the server default `auto`
/// (`none` still works and is passed through unchanged).
fn anthropic_tool_choice(tool_choice: &Value, model: &str) -> Value {
    let lower = model.to_ascii_lowercase();
    let rejects_forced = lower.contains("opus-5-5")
        || lower.contains("sonnet-5-5")
        || lower.contains("fable")
        || lower.contains("mythos");
    let forced = match tool_choice.as_str() {
        Some("any" | "required") => true,
        Some(name) => !matches!(name, "auto" | "none"),
        None => matches!(
            tool_choice.get("type").and_then(Value::as_str),
            Some("any" | "required" | "tool" | "function")
        ),
    };
    if forced && rejects_forced {
        return json!({ "type": "auto" });
    }
    match tool_choice.as_str() {
        Some("auto") => json!({ "type": "auto" }),
        Some("none") => json!({ "type": "none" }),
        Some("any" | "required") => json!({ "type": "any" }),
        Some(name) => json!({ "type": "tool", "name": name }),
        None => tool_choice.clone(),
    }
}

/// Convert one internal message to the Anthropic wire shape. Returns `None`
/// when no blocks survive conversion (Anthropic rejects empty content) or
/// when the role has no Anthropic channel.
///
/// The wire role used to be `message.role` forwarded verbatim, which is how a
/// `system` message ended up on the wire for the provider to 400 on. It now
/// comes from the shared placement table, and pairs the table rejects are
/// refused at the outbound seam before this function ever runs.
pub(super) fn message_to_anthropic(message: &codewhale_models::Message) -> Option<Value> {
    let placement = role_placement(&message.role, WireDialect::AnthropicMessages);
    let wire_role = match placement {
        RolePlacement::User | RolePlacement::Developer => "user",
        RolePlacement::Assistant | RolePlacement::InterruptedAssistant => "assistant",
        // Unreachable in production: `reject_unsupported_roles` refuses these
        // pairs at the seam. Failing closed here keeps a future caller that
        // skips the seam from putting an unrepresentable role on the wire.
        RolePlacement::System | RolePlacement::Omitted | RolePlacement::Rejected => return None,
    };
    let mut blocks: Vec<Value> = message
        .content
        .iter()
        .filter_map(content_block_to_anthropic)
        .collect();
    if blocks.is_empty() {
        return None;
    }
    if placement == RolePlacement::InterruptedAssistant
        && let Some(text) = blocks
            .iter_mut()
            .find(|block| block.get("type").and_then(Value::as_str) == Some("text"))
    {
        let existing = text
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        text["text"] = json!(format!(
            "{}{}",
            codewhale_models::INTERRUPTED_ASSISTANT_CONTEXT_PREFIX,
            existing
        ));
    }
    Some(json!({ "role": wire_role, "content": blocks }))
}

/// Project the shared `ImageUrl` block onto Anthropic's tagged image source.
///
/// The OpenAI dialects carry an image as a single URL string, so that is what
/// [`ContentBlock::ImageUrl`] stores. Anthropic instead models the source as a
/// tagged union, and — this is the part that used to be wrong here — it does
/// **not** accept a `data:` URL under `{"type":"url"}`. Sending a local
/// screenshot that way earns an opaque provider-side 400, which is exactly the
/// confusing failure this whole path exists to avoid, so the data URL is taken
/// back apart into `{"type":"base64", media_type, data}`.
fn anthropic_image_block(url: &str) -> Value {
    if let Some((media_type, data)) = codewhale_models::image_window::parse_data_url(url) {
        return json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data },
        });
    }
    if codewhale_models::image_window::is_remote_image_url(url) {
        return json!({
            "type": "image",
            "source": { "type": "url", "url": url },
        });
    }
    // Anything else (a bare path, a `file://`, a truncated data URL) has no
    // Anthropic representation. Degrade to visible text rather than emitting a
    // source the API will reject: the turn survives and the model can see that
    // something was meant to be here.
    json!({
        "type": "text",
        "text": format!("[unsupported image reference: {url}]"),
    })
}

pub(super) fn anthropic_tool_result_content(
    content: &str,
    content_blocks: Option<&[Value]>,
) -> Value {
    let (image, omitted) = crate::image_attach::provider_tool_result_image_refs(content_blocks);
    let content = codewhale_models::image_window::tool_result_text_with_omission(content, omitted);
    let Some((mime_type, data)) = image else {
        return json!(content);
    };
    let mut blocks = Vec::with_capacity(2);
    if !content.is_empty() {
        blocks.push(json!({ "type": "text", "text": content }));
    }
    blocks.push(json!({
        "type": "image",
        "source": { "type": "base64", "media_type": mime_type, "data": data },
    }));
    json!(blocks)
}

fn content_block_to_anthropic(block: &ContentBlock) -> Option<Value> {
    match block {
        ContentBlock::Text {
            text,
            cache_control,
        } => {
            let mut value = json!({ "type": "text", "text": text });
            if let Some(cache) = cache_control {
                value["cache_control"] = json!({ "type": cache.cache_type });
            }
            Some(value)
        }
        ContentBlock::Thinking {
            thinking,
            signature,
            redacted_data,
            ..
        } => {
            // A redacted thinking block is provider-encrypted content with no
            // readable text: replay it byte-exact so the API can decrypt it,
            // exactly like a signed block. Anthropic requires the complete
            // unmodified block in tool loops and decrypts it to restore the
            // turn's reasoning.
            if let Some(data) = redacted_data {
                return Some(json!({
                    "type": "redacted_thinking",
                    "data": data,
                }));
            }
            // Anthropic rejects unsigned thinking blocks on replay (and the
            // DeepSeek-era "(reasoning omitted)" placeholders mean nothing to
            // it), so only signed blocks are replayed — verbatim, signature
            // included.
            signature.as_ref().map(|signature| {
                json!({
                    "type": "thinking",
                    "thinking": thinking,
                    "signature": signature,
                })
            })
        }
        ContentBlock::ToolUse {
            id, name, input, ..
        } => Some(json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        })),
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
            content_blocks,
            ..
        } => {
            let mut value = json!({
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": anthropic_tool_result_content(content, content_blocks.as_deref()),
            });
            if let Some(is_error) = is_error {
                value["is_error"] = json!(is_error);
            }
            Some(value)
        }
        ContentBlock::ImageUrl { image_url } => Some(anthropic_image_block(&image_url.url)),
        // Server-tool block types are DeepSeek/internal concepts with no
        // Anthropic client-side wire equivalent.
        ContentBlock::ServerToolUse { .. }
        | ContentBlock::ToolSearchToolResult { .. }
        | ContentBlock::CodeExecutionToolResult { .. } => None,
    }
}

/// Enforce the prefix-zone breakpoint policy (#3014):
/// 1. the last tool in the catalog (or, with no tools, the last system
///    block) — caches the immutable prefix;
/// 2. the last content block of the most recent user turn — caches the
///    append-only history.
///
/// Caller-provided breakpoints are preserved, but the total is capped at
/// [`MAX_CACHE_BREAKPOINTS`] by dropping the earliest markers first (the
/// latest markers cover the longest prefixes).
fn apply_anthropic_cache_breakpoints(body: &mut Value) {
    // Place breakpoint 1: prefer the last tool; otherwise last system block.
    let mut placed_prefix = false;
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut)
        && let Some(last) = tools.last_mut()
    {
        last["cache_control"] = json!({ "type": "ephemeral" });
        placed_prefix = true;
    }
    if !placed_prefix
        && let Some(system) = body.get_mut("system").and_then(Value::as_array_mut)
        && let Some(last) = system.last_mut()
    {
        last["cache_control"] = json!({ "type": "ephemeral" });
    }

    // Place breakpoint 2: last content block of the latest user message.
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut)
        && let Some(last_user) = messages
            .iter_mut()
            .rev()
            .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        && let Some(last_block) = last_user
            .get_mut("content")
            .and_then(Value::as_array_mut)
            .and_then(|blocks| blocks.last_mut())
    {
        last_block["cache_control"] = json!({ "type": "ephemeral" });
    }

    // Cap at MAX_CACHE_BREAKPOINTS in render order (tools → system →
    // messages), dropping the earliest extras.
    let mut marked: Vec<*mut Value> = Vec::new();
    let collect = |value: Option<&mut Value>| {
        let Some(array) = value.and_then(Value::as_array_mut) else {
            return Vec::new();
        };
        array
            .iter_mut()
            .filter(|item| item.get("cache_control").is_some())
            .map(|item| item as *mut Value)
            .collect::<Vec<_>>()
    };
    marked.extend(collect(body.get_mut("tools")));
    marked.extend(collect(body.get_mut("system")));
    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages.iter_mut() {
            if let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) {
                marked.extend(
                    blocks
                        .iter_mut()
                        .filter(|block| block.get("cache_control").is_some())
                        .map(|block| block as *mut Value),
                );
            }
        }
    }
    if marked.len() > MAX_CACHE_BREAKPOINTS {
        let excess = marked.len() - MAX_CACHE_BREAKPOINTS;
        for pointer in marked.into_iter().take(excess) {
            // SAFETY: the pointers were collected from `body`, which is
            // exclusively borrowed for the duration of this function, and
            // each pointer targets a distinct JSON node.
            unsafe {
                if let Some(map) = (*pointer).as_object_mut() {
                    map.remove("cache_control");
                }
            }
        }
    }
}

/// Provider event types [`convert_anthropic_sse_data`] accepts. Anything else
/// with a string `type` is tolerated as `None` (future additions); note
/// `tool_projection_warning` is deliberately absent — it is local-only and
/// must never decode from provider SSE.
fn is_known_sse_type(event_type: &str) -> bool {
    matches!(
        event_type,
        "message_start"
            | "content_block_start"
            | "content_block_delta"
            | "content_block_stop"
            | "message_delta"
            | "message_stop"
            | "ping"
            | "error"
    )
}

/// Peek at an SSE payload's `type` without building a DOM.
#[derive(Deserialize)]
struct SseTagPeek<'a> {
    #[serde(borrow)]
    r#type: Option<&'a str>,
}

/// Convert one SSE `data:` payload into a [`StreamEvent`], normalizing usage
/// objects to the #2961 convention. Returns `None` for ignorable payloads.
///
/// #6213 T7: the per-token path deserializes directly into the tagged
/// [`StreamEvent`] instead of building a `Value` DOM and converting it.
/// Usage-bearing events (two per stream) keep the exact legacy path — the
/// usage rewrite reads wire fields the normalized [`Usage`] cannot
/// represent — and decode failures keep their exact legacy outcomes.
fn convert_anthropic_sse_data(data: &str) -> Option<Result<StreamEvent>> {
    let trimmed = data.trim();
    if trimmed.is_empty() {
        return None;
    }
    let usage_event = matches!(
        serde_json::from_str::<SseTagPeek>(trimmed).map(|peek| peek.r#type),
        Ok(Some("message_start" | "message_delta"))
    );
    if usage_event {
        return convert_anthropic_sse_usage_event(trimmed);
    }
    match serde_json::from_str::<StreamEvent>(trimmed) {
        // Local-only receipt: the legacy path ignored it (not a provider
        // type), so it stays ignored rather than decoding.
        Ok(StreamEvent::ToolProjectionWarning { .. }) => None,
        Ok(event) => Some(Ok(event)),
        Err(error) => {
            // Cold path, reached only when direct decode fails: invalid JSON
            // and unknown types keep their exact legacy outcomes.
            let value: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(e) => return Some(Err(anyhow::anyhow!("invalid SSE JSON: {e}"))),
            };
            match value.get("type").and_then(Value::as_str) {
                // Tolerate unknown event types (e.g. future additions) silently.
                Some(known) if !is_known_sse_type(known) => None,
                _ => Some(Err(anyhow::anyhow!("unrecognized SSE event: {error}"))),
            }
        }
    }
}

/// Legacy `Value` path for `message_start`/`message_delta`: the usage
/// rewrite reads wire fields the normalized [`Usage`] cannot represent, so
/// these two events normalize before decoding, exactly as before.
fn convert_anthropic_sse_usage_event(trimmed: &str) -> Option<Result<StreamEvent>> {
    let mut value: Value = match serde_json::from_str(trimmed) {
        Ok(value) => value,
        Err(e) => return Some(Err(anyhow::anyhow!("invalid SSE JSON: {e}"))),
    };

    match value.get("type").and_then(Value::as_str) {
        Some("message_start") => {
            if let Some(message) = value.get_mut("message") {
                discard_provider_execution_ids(message);
                if let Some(usage) = message.get_mut("usage") {
                    *usage = json!(parse_anthropic_usage(usage));
                }
            }
        }
        Some("message_delta") => {
            if let Some(usage) = value.get_mut("usage") {
                *usage = json!(parse_anthropic_usage(usage));
            }
        }
        // Tolerate unknown event types (e.g. future additions) silently.
        Some(known) if !is_known_sse_type(known) => {
            return None;
        }
        _ => {}
    }

    Some(serde_json::from_value(value).map_err(|e| anyhow::anyhow!("unrecognized SSE event: {e}")))
}

/// Map Anthropic's usage payload onto the normalized [`Usage`] convention
/// (#2961 / #4318): hit = cache reads, write = cache creation, miss = raw
/// uncached input, `input_tokens` = the total prompt across all three.
fn parse_anthropic_usage(usage: &Value) -> Usage {
    let field = |name: &str| {
        usage
            .get(name)
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0)
    };
    let input_raw = field("input_tokens");
    let cache_creation = field("cache_creation_input_tokens");
    let cache_read = field("cache_read_input_tokens");
    let output = field("output_tokens");

    Usage {
        input_tokens: input_raw
            .saturating_add(cache_creation)
            .saturating_add(cache_read),
        output_tokens: output,
        prompt_cache_hit_tokens: Some(cache_read),
        prompt_cache_miss_tokens: Some(input_raw),
        prompt_cache_write_tokens: Some(cache_creation),
        reasoning_tokens: None,
        reasoning_replay_tokens: None,
        server_tool_use: None,

        input_audio_tokens: None,
        output_audio_tokens: None,

        request_usages: Vec::new(),
    }
}

/// Extract `error.type` / `error.message` from an Anthropic error envelope
/// (`{"type":"error","error":{"type":...,"message":...}}`), falling back to
/// the raw body so nothing is swallowed.
fn anthropic_error_fields(error: &Value) -> (String, String) {
    let error_type = error
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| error.to_string());
    (error_type, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::Role;
    use codewhale_models::{CacheControl, Message, SystemBlock, SystemPrompt, Tool};

    #[test]
    fn provider_messages_cannot_supply_host_execution_identity() {
        for supplied in [json!("forged-local"), json!({"invalid": "host id"})] {
            let value = json!({
                "id": "provider-message", "type": "message", "role": "assistant",
                "model": "claude-sonnet-4-6", "stop_reason": "tool_use",
                "content": [
                    {"type": "tool_use", "id": "wire-call", "name": "read",
                     "input": {"execution_id": "ordinary argument"},
                     "execution_id": supplied,
                     "caller": {"type": "code_execution", "tool_id": "parent-wire"},
                     "thought_signature": "provider-signature"},
                    {"type": "tool_result", "tool_use_id": "wire-call", "content": "result",
                     "execution_id": supplied},
                ],
                "usage": {"input_tokens": 3, "output_tokens": 2},
            });
            let decoded = decode_anthropic_message(value.clone()).unwrap();
            let event = convert_anthropic_sse_data(
                &json!({
                    "type": "message_start", "message": value,
                })
                .to_string(),
            )
            .unwrap()
            .unwrap();
            let StreamEvent::MessageStart { message: streamed } = event else {
                panic!("expected message start")
            };
            for message in [decoded, streamed] {
                let ContentBlock::ToolUse {
                    id,
                    input,
                    execution_id,
                    caller,
                    thought_signature,
                    ..
                } = &message.content[0]
                else {
                    panic!("expected tool use")
                };
                assert!(execution_id.is_none());
                assert_eq!(id, "wire-call");
                assert_eq!(input["execution_id"], "ordinary argument");
                assert_eq!(
                    caller.as_ref().unwrap().tool_id.as_deref(),
                    Some("parent-wire")
                );
                assert_eq!(thought_signature.as_deref(), Some("provider-signature"));
                assert!(matches!(
                    &message.content[1],
                    ContentBlock::ToolResult {
                        execution_id: None,
                        ..
                    }
                ));
                assert_eq!(message.usage.input_tokens, 3);
            }
        }
    }

    fn request_with(
        model: &str,
        reasoning_effort: Option<&str>,
        temperature: Option<f32>,
        top_p: Option<f32>,
    ) -> MessageRequest {
        MessageRequest {
            model: model.to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "hello".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 1024,
            system: Some(SystemPrompt::Blocks(vec![SystemBlock {
                block_type: "text".to_string(),
                text: "be helpful".to_string(),
                cache_control: Some(CacheControl {
                    cache_type: "ephemeral".to_string(),
                }),
            }])),
            tools: None,
            tool_choice: None,
            metadata: None,
            thinking: None,
            reasoning_effort: reasoning_effort.map(str::to_string),
            stream: Some(true),
            temperature,
            top_p,
        }
    }

    fn test_client() -> CodewhaleClient {
        anthropic_test_client(None)
    }

    /// #6378: the engine stores each tool result as its own user message, so
    /// a parallel batch reaches the wire as `assistant{a, b}`, `user{a}`,
    /// `user{b}`. Both results must land in the one user turn after the batch,
    /// and the dangling-use repair must not answer `b` a second time.
    #[test]
    fn parallel_tool_results_split_across_user_turns_are_answered_once() {
        let client = test_client();
        let mut request = request_with("claude-sonnet-4-6", None, None, None);
        let tool_use = |id: &str, path: &str| ContentBlock::ToolUse {
            execution_id: None,
            id: id.to_string(),
            name: "read".to_string(),
            input: json!({ "path": path }),
            caller: None,
            thought_signature: None,
        };
        let tool_result = |id: &str, content: &str| ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: id.to_string(),
            content: content.to_string(),
            is_error: None,
            content_blocks: None,
        };
        request.messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "Read a.txt and b.txt".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "I will read both files in parallel.".to_string(),
                        cache_control: None,
                    },
                    tool_use("toolu_a", "a.txt"),
                    tool_use("toolu_b", "b.txt"),
                ],
            },
            Message {
                role: Role::User,
                content: vec![tool_result("toolu_a", "content of file A")],
            },
            Message {
                role: Role::User,
                content: vec![tool_result("toolu_b", "content of file B")],
            },
        ];

        let body = client.build_anthropic_body(&request, true);
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(
            messages.len(),
            3,
            "both results share one user turn: {body}"
        );
        let results = messages[2]["content"].as_array().expect("user content");
        assert_eq!(
            results
                .iter()
                .map(|block| (block["tool_use_id"].as_str(), block["content"].as_str()))
                .collect::<Vec<_>>(),
            vec![
                (Some("toolu_a"), Some("content of file A")),
                (Some("toolu_b"), Some("content of file B")),
            ],
            "{body}"
        );
        assert!(
            results.iter().all(|block| block.get("is_error").is_none()),
            "{body}"
        );
        assert!(!body.to_string().contains(UNEXECUTED_TOOL_RESULT), "{body}");
    }

    fn anthropic_test_client(base_url: Option<&str>) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = {
            let __cw_config_init_0_0 = Some("anthropic".to_string());
            let __cw_config_init_0_1 = Some(codewhale_runtime::config::ProvidersConfig {
                anthropic: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("test-key".to_string()),
                    base_url: base_url.map(str::to_string),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_0_value: codewhale_runtime::config::Config =
                Default::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        CodewhaleClient::new(&config).expect("anthropic client constructs")
    }

    fn minimax_test_client() -> CodewhaleClient {
        minimax_test_client_for(
            codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_ANTHROPIC_BASE_URL,
        )
    }

    fn minimax_test_client_for(base_url: &str) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = {
            let __cw_config_init_1_0 = Some("minimax-anthropic".to_string());
            let __cw_config_init_1_1 = Some(codewhale_runtime::config::ProvidersConfig {
                minimax_anthropic: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("test-key".to_string()),
                    base_url: Some(base_url.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_1_value: codewhale_runtime::config::Config =
                Default::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        };
        CodewhaleClient::new(&config).expect("MiniMax Messages client constructs")
    }

    fn deepseek_test_client(base_url: &str) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = {
            let __cw_config_init_2_0 = Some("deepseek-anthropic".to_string());
            let __cw_config_init_2_1 = Some(codewhale_runtime::config::ProvidersConfig {
                deepseek_anthropic: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("test-key".to_string()),
                    base_url: Some(base_url.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_2_value: codewhale_runtime::config::Config =
                Default::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        };
        CodewhaleClient::new(&config).expect("DeepSeek Messages client constructs")
    }

    fn modelstudio_test_client(base_url: &str) -> CodewhaleClient {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = {
            let __cw_config_init_3_0 = Some("modelstudio-token-plan-anthropic".to_string());
            let __cw_config_init_3_1 = Some(codewhale_runtime::config::ProvidersConfig {
                // Durable secret-store keys share a family slot, but a literal
                // config key belongs to the selected route's own table.
                modelstudio_token_plan_anthropic: codewhale_runtime::config::ProviderConfig {
                    api_key: Some("test-key".to_string()),
                    base_url: Some(base_url.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut __cw_config_init_3_value: codewhale_runtime::config::Config =
                Default::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value.providers = __cw_config_init_3_1;
            __cw_config_init_3_value
        };
        CodewhaleClient::new(&config).expect("Model Studio Messages client constructs")
    }

    #[test]
    fn body_keeps_native_cache_control_on_system_and_tools() {
        let client = test_client();
        let mut request = request_with("claude-sonnet-4-6", Some("high"), None, None);
        request.tools = Some(vec![Tool {
            tool_type: None,
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            input_schema: json!({"type": "object", "additionalProperties": false}),
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: Some(true),
            cache_control: None,
        }]);

        let body = client.build_anthropic_body(&request, true);

        assert_eq!(
            body.pointer("/system/0/cache_control/type")
                .and_then(Value::as_str),
            Some("ephemeral"),
            "system cache_control must survive natively: {body}"
        );
        assert_eq!(
            body.pointer("/tools/0/strict").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            body.pointer("/tools/0/cache_control/type")
                .and_then(Value::as_str),
            Some("ephemeral"),
            "breakpoint 1 lands on the last tool: {body}"
        );
        // Breakpoint 2 lands on the latest user turn's last block.
        assert_eq!(
            body.pointer("/messages/0/content/0/cache_control/type")
                .and_then(Value::as_str),
            Some("ephemeral")
        );
    }

    #[test]
    fn body_maps_reasoning_effort_to_adaptive_thinking_and_effort() {
        let client = test_client();

        let body = client.build_anthropic_body(
            &request_with("claude-sonnet-4-6", Some("high"), None, None),
            true,
        );
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("adaptive")
        );
        assert_eq!(
            body.pointer("/output_config/effort")
                .and_then(Value::as_str),
            Some("high")
        );

        let body = client.build_anthropic_body(
            &request_with("claude-opus-4-8", Some("xhigh"), None, None),
            true,
        );
        assert_eq!(
            body.pointer("/output_config/effort")
                .and_then(Value::as_str),
            Some("max")
        );

        let body = client.build_anthropic_body(
            &request_with("claude-sonnet-4-6", Some("off"), None, None),
            true,
        );
        assert!(body.get("thinking").is_none(), "off omits thinking: {body}");
        assert!(body.get("output_config").is_none());

        // A model the reviewed catalog does not carry is not thinking-capable:
        // no thinking, no effort. `claude-3-5-haiku-latest` (and Haiku 4.5,
        // which the reviewed catalog now marks `reasoning: true`) are not
        // interchangeable here.
        let body = client.build_anthropic_body(
            &request_with("claude-3-5-haiku-latest", Some("high"), None, None),
            true,
        );
        assert!(body.get("thinking").is_none(), "{body}");
        assert!(body.get("output_config").is_none(), "{body}");
    }

    #[test]
    fn compat_gateway_sends_enabled_budget_thinking_instead_of_adaptive() {
        // #4978: strict Anthropic-compatible gateways (e.g. Sensenova) reject
        // {"type":"adaptive"} with a 400; non-native routes must send the
        // documented enabled+budget shape and no output_config.
        let client = anthropic_test_client(Some("https://api.sensenova.example/v1"));

        let mut request = request_with("claude-sonnet-4-6", Some("high"), None, None);
        request.max_tokens = 64_000;
        let body = client.build_anthropic_body(&request, true);
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("enabled"),
            "{body}"
        );
        assert_eq!(
            body.pointer("/thinking/budget_tokens")
                .and_then(Value::as_u64),
            Some(16_384)
        );
        assert!(body.get("output_config").is_none(), "{body}");

        // Effort tiers map onto budgets, capped below max_tokens.
        let mut request = request_with("claude-sonnet-4-6", Some("max"), None, None);
        request.max_tokens = 64_000;
        let body = client.build_anthropic_body(&request, true);
        assert_eq!(
            body.pointer("/thinking/budget_tokens")
                .and_then(Value::as_u64),
            Some(32_768)
        );
        let mut request = request_with("claude-sonnet-4-6", Some("max"), None, None);
        request.max_tokens = 8_000;
        let body = client.build_anthropic_body(&request, true);
        assert_eq!(
            body.pointer("/thinking/budget_tokens")
                .and_then(Value::as_u64),
            Some(7_999),
            "budget stays below max_tokens: {body}"
        );

        // Unspecified effort defaults to the "high" tier.
        let mut request = request_with("claude-sonnet-4-6", None, None, None);
        request.max_tokens = 64_000;
        let body = client.build_anthropic_body(&request, true);
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("enabled")
        );
        assert_eq!(
            body.pointer("/thinking/budget_tokens")
                .and_then(Value::as_u64),
            Some(16_384)
        );

        // "off" and requests too small for the 1024-token minimum budget
        // omit thinking entirely.
        let mut request = request_with("claude-sonnet-4-6", Some("off"), None, None);
        request.max_tokens = 64_000;
        let body = client.build_anthropic_body(&request, true);
        assert!(body.get("thinking").is_none(), "{body}");
        let body = client.build_anthropic_body(
            &request_with("claude-sonnet-4-6", Some("high"), None, None),
            true,
        );
        assert!(
            body.get("thinking").is_none(),
            "max_tokens=1024 cannot fit the minimum budget: {body}"
        );

        // The native route keeps adaptive; the compat shape is only for
        // non-anthropic.com hosts.
        let native = test_client().build_anthropic_body(
            &request_with("claude-sonnet-4-6", Some("high"), None, None),
            true,
        );
        assert_eq!(
            native.pointer("/thinking/type").and_then(Value::as_str),
            Some("adaptive")
        );
    }

    #[test]
    fn dangling_tool_use_gets_placeholder_tool_result() {
        // #5002: an orphaned tool_use with no matching tool_result poisons
        // the conversation with repeated 400s; request preparation must
        // repair it with an explicit placeholder result.
        let client = test_client();
        let mut request = request_with("claude-sonnet-4-6", None, None, None);
        request.messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "run both tools".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "toolu_ok".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "a.txt"}),
                        caller: None,
                        thought_signature: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "toolu_orphan".to_string(),
                        name: "task".to_string(),
                        input: json!({}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            // Pre-dispatch failure left only one tool_result behind.
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "toolu_ok".to_string(),
                    content: "contents".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
            // Trailing assistant tool_use with no user turn at all.
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    execution_id: None,
                    id: "toolu_tail".to_string(),
                    name: "task".to_string(),
                    input: json!({}),
                    caller: None,
                    thought_signature: None,
                }],
            },
        ];

        let body = client.build_anthropic_body(&request, true);
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 5, "a repair turn is appended: {body}");

        // The orphaned id gets a leading placeholder; the answered one is
        // untouched (no duplicate result).
        let repaired = messages[2]["content"].as_array().expect("user content");
        assert_eq!(repaired.len(), 2, "{body}");
        assert_eq!(repaired[0]["type"].as_str(), Some("tool_result"));
        assert_eq!(repaired[0]["tool_use_id"].as_str(), Some("toolu_orphan"));
        assert_eq!(
            repaired[0]["content"].as_str(),
            Some(UNEXECUTED_TOOL_RESULT)
        );
        assert_eq!(repaired[0]["is_error"].as_bool(), Some(true));
        assert_eq!(repaired[1]["tool_use_id"].as_str(), Some("toolu_ok"));
        assert_eq!(repaired[1]["content"].as_str(), Some("contents"));

        // The trailing tool_use gains a synthesized user turn.
        assert_eq!(messages[4]["role"].as_str(), Some("user"));
        let tail = messages[4]["content"].as_array().expect("tail content");
        assert_eq!(tail.len(), 1, "{body}");
        assert_eq!(tail[0]["type"].as_str(), Some("tool_result"));
        assert_eq!(tail[0]["tool_use_id"].as_str(), Some("toolu_tail"));
        assert_eq!(tail[0]["content"].as_str(), Some(UNEXECUTED_TOOL_RESULT));

        // A fully answered history is left alone.
        request.messages.truncate(3);
        request.messages[1].content.retain(
            |block| !matches!(block, ContentBlock::ToolUse { id, ..} if id == "toolu_orphan"),
        );
        let body = client.build_anthropic_body(&request, true);
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(messages.len(), 3, "no repair turn appended: {body}");
        let untouched = messages[2]["content"].as_array().expect("user content");
        assert_eq!(untouched.len(), 1, "{body}");
        assert_eq!(untouched[0]["tool_use_id"].as_str(), Some("toolu_ok"));
    }

    #[test]
    fn modelstudio_messages_body_requests_thinking_with_budget() {
        // Model Studio's Anthropic-compatible endpoint documents the portable
        // {"type":"enabled","budget_tokens":N} shape plus {"type":"disabled"}
        // (alibabacloud.com/help/en/model-studio/anthropic-api-messages).
        let client = modelstudio_test_client(
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic",
        );

        let mut request = request_with("qwen3.8-max", Some("high"), None, None);
        request.max_tokens = 64_000;
        let body = client.build_anthropic_body(&request, true);
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("enabled"),
            "{body}"
        );
        assert!(
            body.pointer("/thinking/budget_tokens")
                .and_then(Value::as_u64)
                .is_some(),
            "{body}"
        );
        assert!(body.get("output_config").is_none(), "{body}");
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("qwen3.8-max"),
            "{body}"
        );

        // An explicit "off" is honored on the wire instead of silently
        // falling through to the server default (thinking-ON for qwen3.x).
        let mut request = request_with("qwen3.8-max", Some("off"), None, None);
        request.max_tokens = 64_000;
        let body = client.build_anthropic_body(&request, true);
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("disabled"),
            "{body}"
        );
    }

    #[test]
    fn deepseek_messages_body_retires_aliases_and_keeps_thinking_control() {
        let client = deepseek_test_client(
            codewhale_config::descriptors::defaults::DEFAULT_DEEPSEEK_ANTHROPIC_BASE_URL,
        );

        let chat = client.build_anthropic_body(
            &request_with("deepseek-chat", Some("off"), None, None),
            true,
        );
        assert_eq!(
            chat.get("model").and_then(Value::as_str),
            Some(codewhale_config::catalog::reviewed::constants::DEEPSEEK_ALIAS_REPLACEMENT)
        );
        assert_eq!(
            chat.pointer("/thinking/type").and_then(Value::as_str),
            Some("disabled")
        );

        let reasoner = client.build_anthropic_body(
            &request_with("deepseek-reasoner", Some("high"), None, None),
            true,
        );
        assert_eq!(
            reasoner.get("model").and_then(Value::as_str),
            Some(codewhale_config::catalog::reviewed::constants::DEEPSEEK_ALIAS_REPLACEMENT)
        );
        assert_eq!(
            reasoner.pointer("/thinking/type").and_then(Value::as_str),
            Some("adaptive")
        );
        assert_eq!(
            reasoner
                .pointer("/output_config/effort")
                .and_then(Value::as_str),
            Some("high")
        );

        let custom = deepseek_test_client("https://messages.example/v1");
        let custom_body = custom.build_anthropic_body(
            &request_with("deepseek-reasoner", Some("high"), None, None),
            true,
        );
        assert_eq!(
            custom_body.get("model").and_then(Value::as_str),
            Some("deepseek-reasoner")
        );
    }

    #[test]
    fn omitted_alias_effort_is_migrated_into_deepseek_messages_body() {
        for (alias, expected_effort, expected_thinking) in [
            ("deepseek-chat", "off", "disabled"),
            ("deepseek-reasoner", "high", "adaptive"),
        ] {
            let mut config = {
                let __cw_config_init_4_0 = Some("deepseek-anthropic".to_string());
                let __cw_config_init_4_1 = Some(codewhale_runtime::config::ProvidersConfig {
                    deepseek_anthropic: codewhale_runtime::config::ProviderConfig {
                        api_key: Some("test-key".to_string()),
                        model: Some(alias.to_string()),
                        ..Default::default()
                    },
                    ..Default::default()
                });
                let mut __cw_config_init_4_value: codewhale_runtime::config::Config =
                    Default::default();
                __cw_config_init_4_value.provider = __cw_config_init_4_0;
                __cw_config_init_4_value.providers = __cw_config_init_4_1;
                __cw_config_init_4_value
            };
            assert!(
                config.reasoning_effort().is_none(),
                "fixture must omit effort"
            );

            codewhale_runtime::config::normalize_model_config_for_test(&mut config);
            let client = CodewhaleClient::new(&config).expect("DeepSeek Messages client");
            let model = config.default_model();
            let body = client.build_anthropic_body(
                &request_with(&model, config.reasoning_effort(), None, None),
                true,
            );

            assert_eq!(
                body.get("model").and_then(Value::as_str),
                Some(codewhale_config::catalog::reviewed::constants::DEEPSEEK_ALIAS_REPLACEMENT),
                "{alias}: {body}"
            );
            assert_eq!(config.reasoning_effort(), Some(expected_effort));
            assert_eq!(
                body.pointer("/thinking/type").and_then(Value::as_str),
                Some(expected_thinking),
                "{alias}: {body}"
            );
            if alias == "deepseek-reasoner" {
                assert_eq!(
                    body.pointer("/output_config/effort")
                        .and_then(Value::as_str),
                    Some("high"),
                    "{body}"
                );
            } else {
                assert!(body.get("output_config").is_none(), "{body}");
            }
        }
    }

    #[test]
    fn minimax_body_uses_supported_thinking_controls() {
        let client = minimax_test_client();
        let body =
            client.build_anthropic_body(&request_with("MiniMax-M3", Some("off"), None, None), true);
        assert_eq!(
            body.pointer("/thinking/type").and_then(Value::as_str),
            Some("disabled")
        );
        assert!(body.get("output_config").is_none(), "{body}");

        let mut enabled_bodies = Vec::new();
        for effort in ["high", "max"] {
            let body = client
                .build_anthropic_body(&request_with("MiniMax-M3", Some(effort), None, None), true);
            assert_eq!(
                body.pointer("/thinking/type").and_then(Value::as_str),
                Some("adaptive"),
                "{effort}: {body}"
            );
            assert!(body.get("output_config").is_none(), "{effort}: {body}");
            enabled_bodies.push(body);
        }
        assert_eq!(
            enabled_bodies[0].get("thinking"),
            enabled_bodies[1].get("thinking"),
            "MiniMax high/max select the same untiered adaptive wire control"
        );
    }

    #[test]
    fn minimax_messages_reasoning_controls_require_exact_first_party_m3_route() {
        for (base_url, model) in [
            (
                "https://gateway.example/anthropic",
                codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_MODEL,
            ),
            (
                codewhale_config::descriptors::defaults::DEFAULT_MINIMAX_ANTHROPIC_BASE_URL,
                "MiniMax-M2",
            ),
        ] {
            let client = minimax_test_client_for(base_url);
            for effort in ["off", "high", "max"] {
                let body = client
                    .build_anthropic_body(&request_with(model, Some(effort), None, None), true);
                assert!(
                    body.get("thinking").is_none(),
                    "{base_url} {model} {effort}: {body}"
                );
                assert!(
                    body.get("output_config").is_none(),
                    "{base_url} {model} {effort}: {body}"
                );
            }
        }
    }

    #[test]
    fn body_drops_sampling_params_for_models_that_reject_them() {
        let client = test_client();

        let body = client.build_anthropic_body(
            &request_with("claude-opus-4-8", None, Some(0.7), Some(0.9)),
            true,
        );
        assert!(body.get("temperature").is_none(), "{body}");
        assert!(body.get("top_p").is_none(), "{body}");

        // The Claude 5 generation and Haiku 5.5 reject sampling too. The
        // datestamped spelling pins the substring match against snapshot
        // suffixes.
        for model in [
            "claude-opus-5",
            "claude-opus-5-5",
            "claude-opus-5-20260101",
            "claude-sonnet-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5",
            "claude-mythos-5",
        ] {
            let body =
                client.build_anthropic_body(&request_with(model, None, Some(0.7), Some(0.9)), true);
            assert!(
                body.get("temperature").is_none() && body.get("top_p").is_none(),
                "{model} must not receive sampling params: {body}"
            );
        }

        // Older models accept ONE of temperature / top_p (temperature wins).
        // The pre-4.7 generations stay ungated, so a future over-broad arm
        // like `contains("opus-4-")` cannot slip through.
        for model in ["claude-sonnet-4-6", "claude-opus-4-6", "claude-haiku-4-5"] {
            let body =
                client.build_anthropic_body(&request_with(model, None, Some(0.7), Some(0.9)), true);
            assert_eq!(
                body.get("temperature").and_then(Value::as_f64),
                Some(f64::from(0.7f32)),
                "{model} keeps temperature"
            );
            assert!(
                body.get("top_p").is_none(),
                "{model}: never send both: {body}"
            );
        }
        let body = client.build_anthropic_body(
            &request_with("claude-sonnet-4-6", None, None, Some(0.9)),
            true,
        );
        assert_eq!(
            body.get("top_p").and_then(Value::as_f64),
            Some(f64::from(0.9f32)),
            "top_p alone passes through: {body}"
        );
    }

    #[test]
    fn body_replays_signed_thinking_and_drops_unsigned_placeholders() {
        let client = test_client();
        let mut request = request_with("claude-sonnet-4-6", None, None, None);
        request.messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "do the thing".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "signed reasoning".to_string(),
                        signature: Some("sig-abc".to_string()),
                        state: None,
                        redacted_data: None,
                    },
                    ContentBlock::Thinking {
                        thinking: "(reasoning omitted)".to_string(),
                        signature: None,
                        state: None,
                        redacted_data: None,
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "toolu_1".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "a.txt"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "toolu_1".to_string(),
                    content: "contents".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];

        let body = client.build_anthropic_body(&request, true);
        let assistant = &body["messages"][1]["content"];
        assert_eq!(assistant.as_array().map(Vec::len), Some(2));
        assert_eq!(
            assistant[0]["signature"].as_str(),
            Some("sig-abc"),
            "signed thinking replays verbatim: {assistant}"
        );
        assert_eq!(assistant[1]["type"].as_str(), Some("tool_use"));
        assert!(
            assistant[1].get("caller").is_none(),
            "internal caller metadata must not reach the wire"
        );
        assert_eq!(
            body["messages"][2]["content"][0]["type"].as_str(),
            Some("tool_result")
        );
    }

    #[test]
    fn body_replays_redacted_thinking_blocks_byte_exact() {
        let client = test_client();
        let mut request = request_with("claude-sonnet-4-6", None, None, None);
        request.messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "do the thing".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: String::new(),
                        signature: None,
                        state: None,
                        redacted_data: Some("ENC_REDACTED_PAYLOAD".to_string()),
                    },
                    ContentBlock::ToolUse {
                        execution_id: None,
                        id: "toolu_1".to_string(),
                        name: "read_file".to_string(),
                        input: json!({"path": "a.txt"}),
                        caller: None,
                        thought_signature: None,
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: "toolu_1".to_string(),
                    content: "contents".to_string(),
                    is_error: None,
                    content_blocks: None,
                }],
            },
        ];

        let body = client.build_anthropic_body(&request, true);
        let assistant = &body["messages"][1]["content"];
        assert_eq!(assistant.as_array().map(Vec::len), Some(2));
        assert_eq!(assistant[0]["type"].as_str(), Some("redacted_thinking"));
        assert_eq!(assistant[0]["data"].as_str(), Some("ENC_REDACTED_PAYLOAD"));
        assert_eq!(assistant[1]["type"].as_str(), Some("tool_use"));
    }

    #[test]
    fn non_native_gateways_never_receive_redacted_thinking_blocks() {
        // Conservative withholding: the docs promise nothing about
        // third-party gateways understanding `redacted_thinking`, and a
        // strict Messages-compatible gateway would reject the unknown type.
        let client = anthropic_test_client(Some("https://gateway.example.com/anthropic"));
        let mut request = request_with("claude-sonnet-4-6", None, None, None);
        request.messages = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: None,
                    state: None,
                    redacted_data: Some("ENC_REDACTED".to_string()),
                },
                ContentBlock::ToolUse {
                    execution_id: None,
                    id: "toolu_1".to_string(),
                    name: "read_file".to_string(),
                    input: json!({}),
                    caller: None,
                    thought_signature: None,
                },
            ],
        }];

        let body = client.build_anthropic_body(&request, true);
        let wire = body.to_string();
        assert!(
            !wire.contains("redacted_thinking") && !wire.contains("ENC_REDACTED"),
            "non-native gateway must not receive redacted blocks: {body}"
        );
        let assistant = &body["messages"][0]["content"];
        assert_eq!(assistant.as_array().map(Vec::len), Some(1));

        // First-party Anthropic keeps the block: it can decrypt it.
        let native = test_client().build_anthropic_body(&request, true);
        assert!(
            native.to_string().contains("ENC_REDACTED"),
            "first-party route must replay redacted blocks: {native}"
        );
    }

    #[test]
    fn gateway_withhold_drops_messages_that_only_carried_redacted_blocks() {
        // A redacted-only assistant message (reachable from foreign or
        // imported sessions; the live engine never persists a reasoning-only
        // turn) must not go out as `content: []` after the withhold — that
        // is exactly the every-retry 400 this withhold exists to prevent.
        // Dropping the message is safe: no `tool_use` means no dangling
        // `tool_result`.
        let client = anthropic_test_client(Some("https://gateway.example.com/anthropic"));
        let mut request = request_with("claude-sonnet-4-6", None, None, None);
        request.messages = vec![
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "do the thing".to_string(),
                    cache_control: None,
                }],
            },
            Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Thinking {
                    thinking: String::new(),
                    signature: None,
                    state: None,
                    redacted_data: Some("ENC_REDACTED_ONLY".to_string()),
                }],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "continue".to_string(),
                    cache_control: None,
                }],
            },
        ];

        let body = client.build_anthropic_body(&request, true);
        let messages = body["messages"].as_array().expect("messages array");
        assert_eq!(
            messages.len(),
            2,
            "the redacted-only assistant message must be dropped, not emptied: {body}"
        );
        assert_eq!(messages[0]["role"].as_str(), Some("user"));
        assert_eq!(messages[1]["role"].as_str(), Some("user"));
        assert!(
            !body.to_string().contains("ENC_REDACTED_ONLY"),
            "the payload must stay withheld: {body}"
        );

        // First-party Anthropic keeps the message and its block.
        let native = test_client().build_anthropic_body(&request, true);
        assert_eq!(
            native["messages"].as_array().map(Vec::len),
            Some(3),
            "native routes keep the redacted-only turn: {native}"
        );
    }

    #[test]
    fn sse_redacted_content_block_start_decodes_through_the_stream_decoder() {
        // The exact event whose decode used to drop the turn's block: a
        // `redacted_thinking` content_block_start through the SSE decoder.
        // The contract-violating no-data form must still decode (empty
        // payload + capture warn) instead of failing the event and silently
        // losing the block.
        use codewhale_models::ContentBlockStart;

        let decode = |line: &str| {
            convert_anthropic_sse_data(line)
                .expect("content_block_start must be recognized")
                .expect("decode must succeed")
        };

        let event = decode(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"ENC"}}"#,
        );
        match event {
            StreamEvent::ContentBlockStart {
                index,
                content_block: ContentBlockStart::RedactedThinking { data },
            } => {
                assert_eq!(index, 1);
                assert_eq!(data, "ENC");
            }
            other => panic!("unexpected event for redacted start: {other:?}"),
        }

        let malformed = decode(
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"redacted_thinking"}}"#,
        );
        match malformed {
            StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::RedactedThinking { data },
                ..
            } => assert_eq!(data, "", "missing data must default to an empty payload"),
            other => panic!("unexpected event for malformed redacted start: {other:?}"),
        }
    }

    #[test]
    fn non_stream_redacted_thinking_blocks_decode_into_redacted_data() {
        let payload = json!({
            "id": "msg_1",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "content": [
                { "type": "redacted_thinking", "data": "ENC_FROM_API" },
                { "type": "text", "text": "answer" }
            ],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "usage": {
                "input_tokens": 1,
                "output_tokens": 1
            }
        });

        // An unknown block tag used to fail the whole response decode; the
        // shim must rewrite it into the provider-neutral shape instead.
        let mut value = payload;
        if let Some(usage) = value.get_mut("usage") {
            *usage = json!(parse_anthropic_usage(usage));
        }
        normalize_redacted_thinking_blocks(&mut value);
        let response: MessageResponse = serde_json::from_value(value)
            .expect("redacted_thinking must not fail the response decode");
        assert_eq!(response.content.len(), 2);
        match &response.content[0] {
            ContentBlock::Thinking {
                thinking,
                redacted_data,
                ..
            } => {
                assert_eq!(redacted_data.as_deref(), Some("ENC_FROM_API"));
                assert!(thinking.is_empty());
            }
            other => panic!("expected thinking block, got {other:?}"),
        }
    }

    #[test]
    fn breakpoints_are_capped_at_four_dropping_earliest() {
        let client = test_client();
        let mut request = request_with("claude-sonnet-4-6", None, None, None);
        // Five caller-marked user turns + the two placed breakpoints.
        request.messages = (0..5)
            .map(|i| Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: format!("turn {i}"),
                    cache_control: Some(CacheControl {
                        cache_type: "ephemeral".to_string(),
                    }),
                }],
            })
            .collect();

        let body = client.build_anthropic_body(&request, true);
        let mut count = 0;
        if body.pointer("/system/0/cache_control").is_some() {
            count += 1;
        }
        for message in body["messages"].as_array().unwrap() {
            for block in message["content"].as_array().unwrap() {
                if block.get("cache_control").is_some() {
                    count += 1;
                }
            }
        }
        assert!(
            count <= MAX_CACHE_BREAKPOINTS,
            "breakpoints must be capped at {MAX_CACHE_BREAKPOINTS}, got {count}: {body}"
        );
        // The latest user turn keeps its marker (longest prefix coverage).
        assert!(
            body.pointer("/messages/4/content/0/cache_control")
                .is_some(),
            "{body}"
        );
    }

    #[test]
    fn sse_fixture_decodes_text_thinking_signature_and_tool_use() {
        use codewhale_models::{ContentBlockStart, Delta};

        let events = [
            r#"{"type":"message_start","message":{"id":"msg_01","type":"message","role":"assistant","content":[],"model":"claude-sonnet-4-6","stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"cache_creation_input_tokens":2045,"cache_read_input_tokens":18000,"output_tokens":1}}}"#,
            r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me check"}}"#,
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig-xyz"}}"#,
            r#"{"type":"content_block_stop","index":0}"#,
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Reading the file."}}"#,
            r#"{"type":"content_block_stop","index":1}"#,
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_9","name":"read_file","input":{}}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"path\":"}}"#,
            r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"a.txt\"}"}}"#,
            r#"{"type":"content_block_stop","index":2}"#,
            r#"{"type":"ping"}"#,
            r#"{"type":"message_delta","delta":{"stop_reason":"tool_use","stop_sequence":null},"usage":{"output_tokens":42}}"#,
            r#"{"type":"message_stop"}"#,
        ];

        let decoded: Vec<StreamEvent> = events
            .iter()
            .map(|data| {
                convert_anthropic_sse_data(data)
                    .expect("known event")
                    .expect("decodes")
            })
            .collect();

        // message_start usage normalized to the #2961 convention.
        let StreamEvent::MessageStart { message } = &decoded[0] else {
            panic!("expected MessageStart, got {:?}", decoded[0]);
        };
        assert_eq!(message.usage.input_tokens, 3 + 2045 + 18000);
        assert_eq!(message.usage.prompt_cache_hit_tokens, Some(18000));
        assert_eq!(message.usage.prompt_cache_miss_tokens, Some(3));
        assert_eq!(message.usage.prompt_cache_write_tokens, Some(2045));

        assert!(matches!(
            &decoded[1],
            StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::Thinking { .. },
                ..
            }
        ));
        assert!(matches!(
            &decoded[3],
            StreamEvent::ContentBlockDelta {
                delta: Delta::SignatureDelta { signature },
                ..
            } if signature == "sig-xyz"
        ));
        assert!(matches!(
            &decoded[6],
            StreamEvent::ContentBlockDelta {
                delta: Delta::TextDelta { text },
                ..
            } if text == "Reading the file."
        ));
        let mut tool_json = String::new();
        for event in &decoded {
            if let StreamEvent::ContentBlockDelta {
                delta: Delta::InputJsonDelta { partial_json },
                ..
            } = event
            {
                tool_json.push_str(partial_json);
            }
        }
        assert_eq!(
            serde_json::from_str::<Value>(&tool_json).expect("accumulated tool args parse"),
            json!({"path": "a.txt"})
        );
        assert!(matches!(&decoded[12], StreamEvent::Ping));
        let StreamEvent::MessageDelta { delta, usage } = &decoded[13] else {
            panic!("expected MessageDelta");
        };
        assert_eq!(delta.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(usage.as_ref().map(|u| u.output_tokens), Some(42));
        assert!(matches!(&decoded[14], StreamEvent::MessageStop));
    }

    #[test]
    fn sse_error_event_and_unknown_events_are_handled() {
        let error = convert_anthropic_sse_data(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .expect("error event decodes")
        .expect("error event is a StreamEvent");
        let StreamEvent::Error { error } = error else {
            panic!("expected StreamEvent::Error");
        };
        let (error_type, message) = anthropic_error_fields(&error);
        assert_eq!(error_type, "overloaded_error");
        assert_eq!(message, "Overloaded");

        assert!(
            convert_anthropic_sse_data(r#"{"type":"content_block_started_v2","index":0}"#)
                .is_none(),
            "unknown event types are tolerated"
        );
        assert!(convert_anthropic_sse_data("   ").is_none());
    }

    #[test]
    fn sse_decode_failures_keep_legacy_outcomes_on_the_direct_path() {
        // Malformed JSON: the invalid-input error, not the unrecognized one.
        let error = convert_anthropic_sse_data("{oops")
            .expect("malformed is Some")
            .expect_err("malformed is Err");
        assert!(error.to_string().contains("invalid SSE JSON"), "{error:?}");
        // Structurally invalid known event: unrecognized, not tolerated.
        let error = convert_anthropic_sse_data(r#"{"type":"content_block_stop"}"#)
            .expect("known type is Some")
            .expect_err("missing index is Err");
        assert!(
            error.to_string().contains("unrecognized SSE event"),
            "{error:?}"
        );
        // Local-only receipt: never provider SSE, stays ignored.
        assert!(
            convert_anthropic_sse_data(
                r#"{"type":"tool_projection_warning","provider":"x","omitted_tool_names":[],"omitted_tool_count":0}"#
            )
            .is_none()
        );
    }

    #[test]
    fn usage_mapping_handles_missing_cache_fields() {
        let usage = parse_anthropic_usage(&json!({"input_tokens": 10, "output_tokens": 5}));
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 5);
        assert_eq!(usage.prompt_cache_hit_tokens, Some(0));
        assert_eq!(usage.prompt_cache_miss_tokens, Some(10));
        assert_eq!(usage.prompt_cache_write_tokens, Some(0));
    }

    #[test]
    fn usage_mapping_keeps_cache_write_separate_from_miss() {
        let usage = parse_anthropic_usage(&json!({
            "input_tokens": 3,
            "cache_creation_input_tokens": 2045,
            "cache_read_input_tokens": 18000,
            "output_tokens": 1,
        }));
        assert_eq!(usage.input_tokens, 3 + 2045 + 18000);
        assert_eq!(usage.prompt_cache_hit_tokens, Some(18000));
        assert_eq!(usage.prompt_cache_miss_tokens, Some(3));
        assert_eq!(usage.prompt_cache_write_tokens, Some(2045));
    }

    #[test]
    fn data_url_image_becomes_a_base64_source_not_a_url_source() {
        // Anthropic rejects a `data:` URL under `{"type":"url"}`. This is the
        // whole reason the projection exists; if it regresses, every locally
        // attached screenshot 400s on the native route.
        let block = content_block_to_anthropic(&ContentBlock::ImageUrl {
            image_url: codewhale_models::ImageUrlContent {
                url: "data:image/png;base64,QUJD".to_string(),
            },
        })
        .expect("image block");

        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["type"], "base64");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "QUJD");
        assert!(
            block["source"].get("url").is_none(),
            "base64 sources must not carry a url field: {block}"
        );
    }

    #[test]
    fn tool_result_image_stays_inside_the_native_tool_result_block() {
        let content = anthropic_tool_result_content(
            "screenshot captured",
            Some(&[json!({
                "type": "image",
                "mime_type": "image/png",
                "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==",
            })]),
        );
        let blocks = content.as_array().expect("rich tool_result content");

        assert_eq!(
            blocks[0],
            json!({"type": "text", "text": "screenshot captured"})
        );
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["type"], "base64");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert_eq!(
            blocks[1]["source"]["data"],
            "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg=="
        );
    }

    #[test]
    fn remote_image_url_stays_a_url_source() {
        let block = content_block_to_anthropic(&ContentBlock::ImageUrl {
            image_url: codewhale_models::ImageUrlContent {
                url: "https://example.com/shot.png".to_string(),
            },
        })
        .expect("image block");

        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["type"], "url");
        assert_eq!(block["source"]["url"], "https://example.com/shot.png");
    }

    #[test]
    fn unrepresentable_image_reference_degrades_to_visible_text() {
        for url in [
            "file:///tmp/shot.png",
            "/tmp/shot.png",
            "data:image/png,QUJD",
        ] {
            let block = content_block_to_anthropic(&ContentBlock::ImageUrl {
                image_url: codewhale_models::ImageUrlContent {
                    url: url.to_string(),
                },
            })
            .expect("block");

            assert_eq!(block["type"], "text", "{url} should degrade: {block}");
            assert!(
                block["text"].as_str().expect("text").contains(url),
                "the degraded text should name the reference: {block}"
            );
        }
    }

    #[test]
    fn messages_url_tolerates_v1_suffix() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com/"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://gateway.example/v1"),
            "https://gateway.example/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.deepseek.com/anthropic"),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.minimax.io/anthropic"),
            "https://api.minimax.io/anthropic/v1/messages"
        );
        assert_eq!(
            anthropic_messages_url("https://api.minimaxi.com/anthropic"),
            "https://api.minimaxi.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn anthropic_body_serializes_the_child_catalog_without_duplication() {
        // The real child catalog fixture (not a hand-built tool list) must
        // survive Messages serialization with exactly one canonical `read`
        // entry — no dedup, filter, or sanitizer may drop or duplicate it.
        // `load_skill` is eager in DEFAULT_ACTIVE_NATIVE_TOOLS, and children
        // resolve the same catalog authority the parent does, so it appears
        // here exactly once like any other default tool.
        let tools = crate::tools::subagent::kimi_general_child_request_tools_fixture();
        assert_eq!(
            tools.iter().filter(|tool| tool.name == "read").count(),
            1,
            "catalog fixture carries one canonical read"
        );
        assert_eq!(
            tools
                .iter()
                .filter(|tool| tool.name == "load_skill")
                .count(),
            1,
            "child wire catalog carries one canonical load_skill"
        );
        let client = test_client();
        let mut request = request_with("claude-sonnet-4-6", None, None, None);
        request.tools = Some(tools);
        let body = client.build_anthropic_body(&request, true);
        let serialized = body["tools"]
            .as_array()
            .expect("tools serialize as an array");
        let reads: Vec<_> = serialized
            .iter()
            .filter(|tool| tool["name"] == "read")
            .collect();
        assert_eq!(
            reads.len(),
            1,
            "exactly one canonical read definition reaches the Messages wire"
        );
        assert!(
            reads[0]["input_schema"]["properties"].is_object(),
            "read keeps a valid object schema: {}",
            reads[0]
        );
        assert_eq!(
            serialized
                .iter()
                .filter(|tool| tool["name"] == "load_skill")
                .count(),
            1,
            "exactly one canonical load_skill definition reaches the Messages wire"
        );
    }

    #[tokio::test]
    async fn anthropic_stream_opens_through_shared_seam_preserving_headers() {
        use futures_util::StreamExt;
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // The wire-specific Accept header must survive the shared stream-entry
        // open path; the mock only answers when it is present.
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(header("Accept", "text/event-stream"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_string("data: {\"type\":\"message_stop\"}\n\n"),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_test_client(&server.uri());
        let mut stream = client
            .handle_anthropic_stream(
                &client
                    .prepare_outbound_request(request_with("deepseek-v4", None, None, None), true)
                    .expect("anthropic request prepares"),
            )
            .await
            .expect("stream opens through the shared seam");

        let mut saw_stop = false;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while let Some(event) = stream.next().await {
                if matches!(event.expect("stream event"), StreamEvent::MessageStop) {
                    saw_stop = true;
                }
            }
        })
        .await
        .expect("stream finishes after message_stop");
        assert!(saw_stop, "message_stop should arrive through the seam");
    }

    async fn collect_anthropic_stream(body: &'static str) -> Vec<Result<StreamEvent>> {
        use futures_util::StreamExt;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;
        let client = deepseek_test_client(&server.uri());
        let stream = client
            .handle_anthropic_stream(
                &client
                    .prepare_outbound_request(request_with("deepseek-v4", None, None, None), true)
                    .expect("anthropic request prepares"),
            )
            .await
            .expect("stream opens");
        tokio::time::timeout(std::time::Duration::from_secs(5), stream.collect())
            .await
            .expect("stream ends")
    }

    #[tokio::test]
    async fn anthropic_stream_joins_multiline_data_fields_into_one_event() {
        use codewhale_models::Delta;
        // One JSON payload split across two `data:` fields of a single event.
        let events = collect_anthropic_stream(concat!(
            "event: content_block_delta\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\n",
            "data: \"delta\":{\"type\":\"text_delta\",\"text\":\"joined\"}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        ))
        .await;
        assert!(
            events.iter().any(|event| matches!(
                event,
                Ok(StreamEvent::ContentBlockDelta {
                    delta: Delta::TextDelta { text },
                    ..
                }) if text == "joined"
            )),
            "the split event was lost: {events:?}"
        );
        assert!(matches!(events.last(), Some(Ok(StreamEvent::MessageStop))));
    }

    #[tokio::test]
    async fn anthropic_stream_eof_without_message_stop_is_an_error() {
        let events = collect_anthropic_stream(
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
        )
        .await;
        let last = events.last().expect("the delta and a terminal item");
        assert!(
            last.as_ref()
                .is_err_and(|error| error.to_string().contains("closed before message_stop")),
            "a truncated stream must not end quietly: {events:?}"
        );
    }

    /// Fault injection (#6184): a provider that answers the headers and then
    /// sends nothing fails the stream at the first-byte bound with a
    /// distinct error and a `crashes/` stall record, instead of holding the
    /// turn for the full idle budget.
    #[tokio::test]
    async fn stall_first_byte_timeout_fails_stream_and_records_stall() {
        use futures_util::StreamExt;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().expect("tempdir");
        codewhale_runtime::stall_report::set_test_stall_record_dir(Some(dir.path().to_path_buf()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0u8; 64 * 1024];
            let _ = socket.read(&mut buf).await;
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n",
                )
                .await
                .expect("headers");
            // Hold the connection open with no body bytes.
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            drop(socket);
        });

        let mut client = deepseek_test_client(&base_url);
        client.stream_idle_timeout = std::time::Duration::from_secs(1);
        let started = std::time::Instant::now();
        let mut stream = client
            .handle_anthropic_stream(
                &client
                    .prepare_outbound_request(request_with("deepseek-v4", None, None, None), true)
                    .expect("anthropic request prepares"),
            )
            .await
            .expect("headers arrive");
        let error = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match stream.next().await {
                    Some(Err(error)) => break error,
                    Some(Ok(_)) => continue,
                    None => panic!("stream ended without the first-byte error"),
                }
            }
        })
        .await
        .expect("first-byte bound fires");
        assert!(error.to_string().contains("first-byte timeout"), "{error}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        let records: Vec<String> = std::fs::read_dir(dir.path())
            .expect("record dir")
            .flatten()
            .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
            .collect();
        assert_eq!(records.len(), 1, "{records:?}");
        assert!(records[0].contains("first byte"), "{}", records[0]);
        server.abort();
        codewhale_runtime::stall_report::set_test_stall_record_dir(None);
    }

    #[tokio::test]
    async fn anthropic_stream_open_error_is_not_retried() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // A definitive provider error before any stream body must fail fast:
        // exactly one request, no H1 fallback, envelope preserved.
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(401).set_body_string(
                "{\"error\":{\"type\":\"authentication_error\",\"message\":\"bad key\"}}",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let client = deepseek_test_client(&server.uri());
        let err = match client
            .handle_anthropic_stream(
                &client
                    .prepare_outbound_request(request_with("deepseek-v4", None, None, None), true)
                    .expect("anthropic request prepares"),
            )
            .await
        {
            Ok(_) => panic!("auth errors must fail fast"),
            Err(err) => err,
        };
        // Typed through the shared classifier, provider message kept.
        assert!(
            matches!(
                err.chain()
                    .find_map(|cause| cause.downcast_ref::<crate::llm_client::LlmError>()),
                Some(crate::llm_client::LlmError::AuthenticationError(_))
            ),
            "a 401 must stay a typed authentication error: {err:#}"
        );
        assert!(format!("{err:#}").contains("bad key"), "{err:#}");
    }

    #[tokio::test]
    async fn anthropic_rate_limit_is_retried_honoring_retry_after() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        for force_http1 in [false, true] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "1")
                    .set_body_string(
                        "{\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\",\"message\":\"slow down\"}}",
                    ),
            )
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
            Mock::given(method("POST"))
                .and(path("/v1/messages"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .insert_header("Content-Type", "text/event-stream")
                        .set_body_string("data: {\"type\":\"message_stop\"}\n\n"),
                )
                .expect(1)
                .mount(&server)
                .await;

            let mut client = deepseek_test_client(&server.uri());
            client.force_http1 = force_http1;
            client.stream_open_timeout = std::time::Duration::from_millis(250);
            client.retry.enabled = true;
            client.retry.max_retries = 2;
            client.retry.initial_delay = 0.0;
            client.retry.max_delay = 0.0;
            let started = std::time::Instant::now();
            let stream = client
                .handle_anthropic_stream(
                    &client
                        .prepare_outbound_request(
                            request_with("deepseek-v4", None, None, None),
                            true,
                        )
                        .expect("anthropic request prepares"),
                )
                .await;
            assert!(
                stream.is_ok(),
                "a 429 before the stream body must be retried, not surfaced"
            );
            // The backoff is configured to zero, so only the provider's
            // `Retry-After: 1` can account for the wait before the retry.
            assert!(
                started.elapsed() >= std::time::Duration::from_millis(900),
                "the retry must wait out Retry-After, waited {:?}",
                started.elapsed()
            );
        }
    }

    /// A `system`-role history message — what a compaction summary, a branch
    /// summary, or an imported journal `system` entry becomes once it reaches
    /// `MessageRequest::messages` — must not be emitted verbatim: the Messages
    /// API accepts only `user` and `assistant` in `messages[].role` and 400s
    /// the whole conversation otherwise, on every retry.
    #[test]
    fn system_role_history_message_is_not_emitted_verbatim_on_the_messages_wire() {
        let mut request = request_with("claude-opus-4-6", None, None, None);
        request.messages.insert(
            0,
            Message {
                role: Role::System,
                content: vec![ContentBlock::Text {
                    text: "[compaction summary] the user is porting the parser".to_string(),
                    cache_control: None,
                }],
            },
        );

        let body = test_client().build_anthropic_body(&request, false);
        let messages = body["messages"].as_array().expect("messages array");

        for message in messages {
            let role = message["role"].as_str().expect("role is a string");
            assert!(
                role == "user" || role == "assistant",
                "Messages API rejects role {role:?}"
            );
        }
        let carried = messages.iter().any(|message| {
            message["content"].as_array().is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block["text"].as_str()
                        == Some("[compaction summary] the user is porting the parser")
                })
            })
        });
        assert!(carried, "the summary text must survive: {messages:?}");
    }
}

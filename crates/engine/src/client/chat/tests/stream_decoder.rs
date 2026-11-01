//! Drive `parse_sse_chunk` (the in-place SSE event extractor) over canned
//! chunk sequences. The full `handle_chat_completion_stream` path needs a
//! live `reqwest::Response` so it isn't unit-testable without a mock HTTP
//! harness (issue #69 tracks that). For #103 we exercise the chunk decoder
//! directly to verify each "class of stream failure" the engine relies on.
use super::*;
use crate::client::wire::{SseLineDecoder, SseLineError};
use codewhale_models::{ContentBlockStart, Delta, StreamEvent};

/// Decode a raw SSE-data JSON chunk into our internal events, mirroring
/// the per-event call shape used by `handle_chat_completion_stream`.
fn decode_chunk(json_text: &str) -> Vec<StreamEvent> {
    decode_chunk_with_reasoning(json_text, true)
}

fn decode_chunk_with_reasoning(json_text: &str, is_reasoning_model: bool) -> Vec<StreamEvent> {
    let chunk: Value = serde_json::from_str(json_text).expect("valid SSE JSON");
    let mut content_index = 0u32;
    let mut text_started = false;
    let mut thinking_started = false;
    let mut tool_indices = std::collections::HashMap::new();
    let mut reasoning_detail_buffers = std::collections::HashMap::new();
    parse_sse_chunk(
        &chunk,
        &mut content_index,
        &mut text_started,
        &mut thinking_started,
        &mut tool_indices,
        &mut reasoning_detail_buffers,
        is_reasoning_model,
    )
}

fn decode_chunks_with_style(
    chunks: &[&str],
    reasoning_stream_style: ReasoningStreamStyle,
) -> Vec<StreamEvent> {
    let mut content_index = 0u32;
    let mut text_started = false;
    let mut thinking_started = false;
    let mut tool_indices = std::collections::HashMap::new();
    let mut reasoning_detail_buffers = std::collections::HashMap::new();
    let mut inline_reasoning_tags = InlineReasoningTagState::default();
    let mut events = Vec::new();

    for chunk in chunks {
        let value: Value = serde_json::from_str(chunk).expect("valid SSE JSON");
        events.extend(parse_sse_chunk_with_reasoning_style(
            &value,
            &mut content_index,
            &mut text_started,
            &mut thinking_started,
            &mut tool_indices,
            &mut reasoning_detail_buffers,
            &mut inline_reasoning_tags,
            reasoning_stream_style,
        ));
    }
    events
}

/// Drive the Chat Completions SSE path with raw byte chunks so tests can
/// split a multi-byte UTF-8 character across HTTP/2-style DATA boundaries.
fn decode_sse_byte_chunks(chunks: &[&[u8]]) -> Result<Vec<StreamEvent>, SseLineError> {
    struct FrameState {
        line_buf: String,
        content_index: u32,
        text_started: bool,
        thinking_started: bool,
        tool_indices: std::collections::HashMap<u32, u32>,
        reasoning_detail_buffers: std::collections::HashMap<u32, String>,
        inline_reasoning_tags: InlineReasoningTagState,
        events: Vec<StreamEvent>,
    }

    impl FrameState {
        fn new() -> Self {
            Self {
                line_buf: String::new(),
                content_index: 0,
                text_started: false,
                thinking_started: false,
                tool_indices: std::collections::HashMap::new(),
                reasoning_detail_buffers: std::collections::HashMap::new(),
                inline_reasoning_tags: InlineReasoningTagState::default(),
                events: Vec::new(),
            }
        }

        fn handle_line(&mut self, line: &str) -> bool {
            if line.is_empty() {
                return matches!(self.flush_frame(), SseDataFrame::Done);
            }
            if let Some(data) = extract_sse_data_value(line) {
                if !self.line_buf.is_empty() {
                    self.line_buf.push('\n');
                }
                self.line_buf.push_str(data);
            }
            false
        }

        fn flush_frame(&mut self) -> SseDataFrame {
            if self.line_buf.is_empty() {
                return SseDataFrame::Events(Vec::new());
            }
            let data = std::mem::take(&mut self.line_buf);
            match parse_sse_data_frame(
                &data,
                &mut self.content_index,
                &mut self.text_started,
                &mut self.thinking_started,
                &mut self.tool_indices,
                &mut self.reasoning_detail_buffers,
                &mut self.inline_reasoning_tags,
                ReasoningStreamStyle::SeparateField,
            ) {
                SseDataFrame::Done => SseDataFrame::Done,
                SseDataFrame::Events(frame_events) => {
                    self.events.extend(frame_events);
                    SseDataFrame::Events(Vec::new())
                }
            }
        }
    }

    let mut decoder = SseLineDecoder::new();
    let mut state = FrameState::new();
    for chunk in chunks {
        for line in decoder.push(chunk)? {
            if state.handle_line(&line) {
                return Ok(state.events);
            }
        }
    }
    if let Some(line) = decoder.finish()?
        && state.handle_line(&line)
    {
        return Ok(state.events);
    }
    state.flush_frame();
    Ok(state.events)
}

fn cjk_content_sse(text: &str) -> Vec<u8> {
    let payload = serde_json::json!({
        "choices": [{ "delta": { "content": text } }]
    });
    format!("data: {payload}\n\n").into_bytes()
}

fn mid_char_split(bytes: &[u8], ch: char) -> usize {
    let needle = ch.to_string();
    let start = bytes
        .windows(needle.len())
        .position(|window| window == needle.as_bytes())
        .unwrap_or_else(|| panic!("{ch:?} present in SSE frame"));
    start + 1
}

fn text_delta_text(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ContentBlockDelta {
                delta: Delta::TextDelta { text },
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn thinking_delta_text(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ContentBlockDelta {
                delta: Delta::ThinkingDelta { thinking },
                ..
            } => Some(thinking.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn decoder_reassembles_cjk_split_across_byte_chunks() {
    let frame = cjk_content_sse("你好世界");
    let split = mid_char_split(&frame, '好');
    let events = decode_sse_byte_chunks(&[&frame[..split], &frame[split..]]).expect("valid utf-8");
    let text = text_delta_text(&events);
    assert_eq!(text, "你好世界");
    assert!(
        !text.contains('\u{FFFD}'),
        "HTTP/2 mid-character split must not substitute U+FFFD; got {text:?}"
    );
}

#[test]
fn decoder_reassembles_emoji_and_cjk_fed_one_byte_at_a_time() {
    let frame = cjk_content_sse("你好🌊世界");
    let chunks: Vec<&[u8]> = frame.chunks(1).collect();
    let events = decode_sse_byte_chunks(&chunks).expect("valid utf-8");
    let text = text_delta_text(&events);
    assert_eq!(text, "你好🌊世界");
    assert!(
        !text.contains('\u{FFFD}'),
        "byte-at-a-time feed garbled: {text:?}"
    );
}

#[test]
fn decoder_rejects_invalid_sse_bytes_without_replacement() {
    let mut frame = cjk_content_sse("ok");
    // Bare 0xFF is never valid UTF-8. Insert it inside the first SSE line.
    let newline = frame.iter().position(|&b| b == b'\n').expect("SSE line");
    frame.insert(newline, 0xFF);
    let result = decode_sse_byte_chunks(&[&frame]);
    let err = result.expect_err("invalid SSE bytes must fail closed");
    assert!(
        !err.to_string().contains('\u{FFFD}'),
        "error path must not substitute U+FFFD: {err}"
    );

    // Unterminated tail of continuation bytes: fail on flush, no replacement.
    let result = decode_sse_byte_chunks(&[&[0x80, 0xBF]]);
    let err = result.expect_err("invalid unterminated flush must fail closed");
    assert!(!err.to_string().contains('\u{FFFD}'));
}

#[test]
fn decoder_emits_text_delta_for_content_chunk() {
    // The "happy" first chunk: a normal content delta. The engine treats
    // this as `any_content_received = true` and would NOT transparently
    // retry on a subsequent error.
    let events = decode_chunk(r#"{"choices":[{"delta":{"content":"hello"}}]}"#);
    assert!(
        matches!(
            events.first(),
            Some(StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::Text { .. },
                ..
            })
        ),
        "first event should open a text block; got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ContentBlockDelta {
                    delta: Delta::TextDelta { text },
                    ..
                } if text == "hello")),
        "should yield a TextDelta carrying 'hello'; got {events:?}"
    );
}

#[test]
fn decoder_emits_thinking_delta_for_reasoning_chunk() {
    // V4 thinking models surface reasoning_content first — the engine
    // also counts these as content received (so a subsequent stream error
    // surfaces rather than retrying transparently).
    let events = decode_chunk(r#"{"choices":[{"delta":{"reasoning_content":"plan..."}}]}"#);
    assert!(
        matches!(
            events.first(),
            Some(StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::Thinking { .. },
                ..
            })
        ),
        "first event should open a thinking block; got {events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, StreamEvent::ContentBlockDelta {
                    delta: Delta::ThinkingDelta { thinking },
                    ..
                } if thinking == "plan...")),
        "should yield a ThinkingDelta carrying 'plan...'; got {events:?}"
    );
}

#[test]
fn decoder_streams_moonshot_multi_chunk_reasoning_as_thinking() {
    // #3016: recorded shape from Moonshot's native endpoint — kimi-k2.6
    // streams `reasoning_content` deltas before the answer text. The
    // thinking deltas must accumulate into ONE thinking block and the
    // answer must arrive as text, not be glued into the trace.
    let chunks = [
        r#"{"id":"cmpl-kimi","model":"kimi-k2.6","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"Let me check"}}]}"#,
        r#"{"id":"cmpl-kimi","model":"kimi-k2.6","choices":[{"index":0,"delta":{"reasoning_content":" the config."}}]}"#,
        r#"{"id":"cmpl-kimi","model":"kimi-k2.6","choices":[{"index":0,"delta":{"content":"The answer is 42."}}]}"#,
    ];

    let is_reasoning =
        is_reasoning_model_for_stream(codewhale_config::ProviderKind::Moonshot, "kimi-k2.6");
    let mut content_index = 0u32;
    let mut text_started = false;
    let mut thinking_started = false;
    let mut tool_indices = std::collections::HashMap::new();
    let mut reasoning_detail_buffers = std::collections::HashMap::new();
    let mut events = Vec::new();
    for chunk in chunks {
        let value: Value = serde_json::from_str(chunk).expect("valid SSE JSON");
        events.extend(parse_sse_chunk(
            &value,
            &mut content_index,
            &mut text_started,
            &mut thinking_started,
            &mut tool_indices,
            &mut reasoning_detail_buffers,
            is_reasoning,
        ));
    }

    let thinking: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ContentBlockDelta {
                delta: Delta::ThinkingDelta { thinking },
                ..
            } => Some(thinking.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, "Let me check the config.");

    let thinking_starts = events
        .iter()
        .filter(|event| {
            matches!(
                event,
                StreamEvent::ContentBlockStart {
                    content_block: ContentBlockStart::Thinking { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(thinking_starts, 1, "one thinking block: {events:?}");

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ContentBlockDelta {
                delta: Delta::TextDelta { text },
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "The answer is 42.");
}

#[test]
fn decoder_accepts_openrouter_reasoning_delta_with_extra_fields() {
    let events = decode_chunk(
        r#"{"id":"or-1","choices":[{"delta":{"reasoning":"openrouter thought","reasoning_details":[{"type":"summary","text":"extra"}],"native_finish_reason":null}}],"usage":{"completion_tokens_details":{"reasoning_tokens":3}}}"#,
    );

    assert!(
        events.iter().any(|e| matches!(
            e,
            StreamEvent::ContentBlockDelta {
                delta: Delta::ThinkingDelta { thinking },
                ..
            } if thinking == "openrouter thought"
        )),
        "OpenRouter-style reasoning deltas with extra fields should not crash decoding; got {events:?}"
    );
}

#[test]
fn decoder_streams_minimax_reasoning_details_as_incremental_thinking() {
    // MiniMax's reasoning_split stream reports reasoning_details text as
    // a cumulative buffer. Emit only the suffix so the Thinking cell does
    // not duplicate earlier reasoning chunks.
    let chunks = [
        r#"{"id":"minimax-1","choices":[{"index":0,"delta":{"reasoning_details":[{"type":"text","text":"Inspect"}]}}]}"#,
        r#"{"id":"minimax-1","choices":[{"index":0,"delta":{"reasoning_details":[{"type":"text","text":"Inspect config"}]}}]}"#,
        r#"{"id":"minimax-1","choices":[{"index":0,"delta":{"content":"Done."}}]}"#,
    ];

    let is_reasoning = is_reasoning_model_for_stream(ProviderKind::Minimax, "MiniMax-M3");
    let mut content_index = 0u32;
    let mut text_started = false;
    let mut thinking_started = false;
    let mut tool_indices = std::collections::HashMap::new();
    let mut reasoning_detail_buffers = std::collections::HashMap::new();
    let mut events = Vec::new();
    for chunk in chunks {
        let value: Value = serde_json::from_str(chunk).expect("valid SSE JSON");
        events.extend(parse_sse_chunk(
            &value,
            &mut content_index,
            &mut text_started,
            &mut thinking_started,
            &mut tool_indices,
            &mut reasoning_detail_buffers,
            is_reasoning,
        ));
    }

    let thinking: String = events
        .iter()
        .filter_map(|event| match event {
            StreamEvent::ContentBlockDelta {
                delta: Delta::ThinkingDelta { thinking },
                ..
            } => Some(thinking.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thinking, "Inspect config");

    assert!(!events.iter().any(|event| matches!(
        event,
        StreamEvent::ContentBlockDelta {
            delta: Delta::TextDelta { text },
            ..
        } if text == "Inspect" || text == "Inspect config"
    )));
}

#[test]
fn modelstudio_streams_reasoning_content_as_thinking() {
    // Recorded-style DashScope OpenAI-compatible frames (shape lifted from
    // Model Studio's deep-thinking docs): reasoning streams in
    // `delta.reasoning_content`, the answer in `delta.content`, and a
    // trailing usage-only chunk closes the stream.
    let chunks = [
        r#"{"choices":[{"delta":{"content":null,"role":"assistant","reasoning_content":""},"index":0,"logprobs":null,"finish_reason":null}],"object":"chat.completion.chunk","usage":null,"model":"qwen3.8-max","id":"chatcmpl-ms-1"}"#,
        r#"{"choices":[{"delta":{"reasoning_content":"Let me think"},"index":0}],"object":"chat.completion.chunk","model":"qwen3.8-max","id":"chatcmpl-ms-1"}"#,
        r#"{"choices":[{"delta":{"reasoning_content":" about this."},"index":0}],"object":"chat.completion.chunk","model":"qwen3.8-max","id":"chatcmpl-ms-1"}"#,
        r#"{"choices":[{"delta":{"content":"The answer."},"index":0}],"object":"chat.completion.chunk","model":"qwen3.8-max","id":"chatcmpl-ms-1"}"#,
        r#"{"choices":[{"finish_reason":"stop","delta":{"content":"","reasoning_content":null},"index":0}],"object":"chat.completion.chunk","model":"qwen3.8-max","id":"chatcmpl-ms-1"}"#,
        r#"{"choices":[],"object":"chat.completion.chunk","usage":{"prompt_tokens":10,"completion_tokens":30,"total_tokens":40,"completion_tokens_details":{"reasoning_tokens":20}},"model":"qwen3.8-max","id":"chatcmpl-ms-1"}"#,
    ];

    // Both OpenAI-dialect plans classify their reasoning catalog.
    for (provider, base_url, model) in [
        (
            ProviderKind::ModelstudioTokenPlan,
            codewhale_config::descriptors::defaults::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL,
            "qwen3.8-max",
        ),
        (
            ProviderKind::ModelstudioCodingPlan,
            codewhale_config::descriptors::defaults::DEFAULT_MODELSTUDIO_CODING_PLAN_BASE_URL,
            "qwen3.7-plus",
        ),
    ] {
        let style = reasoning_stream_style_for_route(provider, base_url, model, None);
        assert_eq!(style, ReasoningStreamStyle::SeparateField, "{provider:?}");

        let mut content_index = 0u32;
        let mut text_started = false;
        let mut thinking_started = false;
        let mut tool_indices = std::collections::HashMap::new();
        let mut reasoning_detail_buffers = std::collections::HashMap::new();
        let mut inline_reasoning_tags = InlineReasoningTagState::default();
        let mut events = Vec::new();
        for chunk in chunks {
            let value: Value = serde_json::from_str(chunk).expect("valid SSE JSON");
            events.extend(parse_sse_chunk_with_reasoning_style(
                &value,
                &mut content_index,
                &mut text_started,
                &mut thinking_started,
                &mut tool_indices,
                &mut reasoning_detail_buffers,
                &mut inline_reasoning_tags,
                style,
            ));
        }

        let thinking: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ContentBlockDelta {
                    delta: Delta::ThinkingDelta { thinking },
                    ..
                } => Some(thinking.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking, "Let me think about this.", "{provider:?}");

        let text: String = events
            .iter()
            .filter_map(|event| match event {
                StreamEvent::ContentBlockDelta {
                    delta: Delta::TextDelta { text },
                    ..
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text, "The answer.", "{provider:?}");

        // The trailing usage chunk still surfaces token accounting.
        assert!(
            events.iter().any(|event| matches!(
                event,
                StreamEvent::MessageDelta { usage: Some(usage), .. }
                    if usage.output_tokens == 30
            )),
            "{provider:?}: {events:?}"
        );
    }

    // A model id the catalog does not know still routes the reasoning field
    // to Thinking (#6501); a model that sends no reasoning field gets none.
    let style = reasoning_stream_style_for_route(
        ProviderKind::ModelstudioTokenPlan,
        codewhale_config::descriptors::defaults::DEFAULT_MODELSTUDIO_TOKEN_PLAN_BASE_URL,
        "qwen3.8-max-lite-unknown",
        None,
    );
    assert_eq!(style, ReasoningStreamStyle::SeparateField);
}

#[test]
fn decoder_does_not_render_reasoning_as_text_for_known_provider_models() {
    let mut content_index = 0u32;
    let mut text_started = false;
    let mut thinking_started = false;
    let mut tool_indices = std::collections::HashMap::new();
    let mut reasoning_detail_buffers = std::collections::HashMap::new();
    let is_reasoning_model =
        is_reasoning_model_for_stream(ProviderKind::XiaomiMimo, "mimo-v2.5-pro");
    let events = parse_sse_chunk(
        &serde_json::json!({
            "choices": [{
                "delta": {
                    "reasoning_content": "private plan"
                }
            }]
        }),
        &mut content_index,
        &mut text_started,
        &mut thinking_started,
        &mut tool_indices,
        &mut reasoning_detail_buffers,
        is_reasoning_model,
    );

    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::ContentBlockDelta {
            delta: Delta::ThinkingDelta { thinking },
            ..
        } if thinking == "private plan"
    )));
    assert!(!events.iter().any(|event| matches!(
        event,
        StreamEvent::ContentBlockDelta {
            delta: Delta::TextDelta { text },
            ..
        } if text == "private plan"
    )));
}

#[test]
fn decoder_treats_reasoning_content_as_text_when_provider_does_not_support_reasoning() {
    let events = decode_chunk_with_reasoning(
        r#"{"choices":[{"delta":{"reasoning_content":"hello"}}]}"#,
        false,
    );

    assert!(
        matches!(
            events.first(),
            Some(StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::Text { .. },
                ..
            })
        ),
        "first event should open a text block; got {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            StreamEvent::ContentBlockDelta {
                delta: Delta::TextDelta { text },
                ..
            } if text == "hello"
        )),
        "should yield a TextDelta carrying 'hello'; got {events:?}"
    );
    assert!(
        !events.iter().any(|e| matches!(
            e,
            StreamEvent::ContentBlockDelta {
                delta: Delta::ThinkingDelta { .. },
                ..
            }
        )),
        "should not emit thinking deltas for generic providers; got {events:?}"
    );
}

#[test]
fn reasoning_style_separate_field_routes_reasoning_to_thinking() {
    let events = decode_chunks_with_style(
        &[
            r#"{"choices":[{"delta":{"reasoning_content":"private plan"}}]}"#,
            r#"{"choices":[{"delta":{"content":"Public answer."}}]}"#,
        ],
        ReasoningStreamStyle::SeparateField,
    );

    assert_eq!(thinking_delta_text(&events), "private plan");
    assert_eq!(text_delta_text(&events), "Public answer.");
}

#[test]
fn exact_kimi_code_k3_streams_reasoning_content_as_thinking() {
    let style = reasoning_stream_style_for_route(
        ProviderKind::Moonshot,
        codewhale_config::descriptors::defaults::DEFAULT_KIMI_CODE_BASE_URL,
        codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL,
        None,
    );
    assert_eq!(style, ReasoningStreamStyle::SeparateField);

    let events = decode_chunks_with_style(
        &[r#"{"choices":[{"delta":{"reasoning_content":"private K3 plan"}}]}"#],
        style,
    );
    assert_eq!(thinking_delta_text(&events), "private K3 plan");
    assert_eq!(text_delta_text(&events), "");

    let generic_style = reasoning_stream_style_for_route(
        ProviderKind::Moonshot,
        codewhale_config::descriptors::defaults::DEFAULT_MOONSHOT_BASE_URL,
        codewhale_config::catalog::reviewed::constants::KIMI_CODE_K3_MODEL,
        None,
    );
    assert_eq!(generic_style, ReasoningStreamStyle::SeparateField);
}

/// #6501 regression: the founder's grok-4.7 (xAI) and mimo-v2.6-pro
/// (Xiaomi MiMo) sessions persisted the reasoning summary glued to the answer
/// in one Text block ("...I should help them find large f...Sure, I'd be
/// happy to help"). Decode that stream shape through the real route style.
#[test]
fn issue_6501_reasoning_field_never_leaks_into_answer_text_on_unlisted_routes() {
    for (provider, base_url, model) in [
        (
            ProviderKind::Xai,
            codewhale_config::descriptors::defaults::DEFAULT_XAI_BASE_URL,
            "grok-4.7",
        ),
        (
            ProviderKind::XiaomiMimo,
            "https://token-plan-sgp.xiaomimimo.com/v1",
            "mimo-v2.7-pro-unreleased",
        ),
        (
            ProviderKind::Openai,
            "https://gateway.example.test/v1",
            "some-new-reasoner",
        ),
    ] {
        let style = reasoning_stream_style_for_route(provider, base_url, model, None);
        assert_eq!(style, ReasoningStreamStyle::SeparateField, "{provider:?}");
        let events = decode_chunks_with_style(
            &[
                r#"{"choices":[{"delta":{"role":"assistant","reasoning_content":"The user wants help finding large files"}}]}"#,
                r#"{"choices":[{"delta":{"reasoning_content":"..."}}]}"#,
                r#"{"choices":[{"delta":{"content":"Sure, I'd be happy to help."}}]}"#,
                r#"{"choices":[{"delta":{"reasoning":"then a tool"}}]}"#,
                r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"exec_shell","arguments":"{}"}}]}}]}"#,
                r#"{"choices":[{"finish_reason":"tool_calls"}]}"#,
            ],
            style,
        );
        assert_eq!(
            thinking_delta_text(&events),
            "The user wants help finding large files...then a tool",
            "{provider:?}"
        );
        assert_eq!(
            text_delta_text(&events),
            "Sure, I'd be happy to help.",
            "{provider:?}: reasoning must not reach answer text"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                StreamEvent::ContentBlockStart {
                    content_block: ContentBlockStart::ToolUse { .. },
                    ..
                }
            )),
            "{provider:?}: reasoning -> tool call transition must keep the tool call"
        );
    }

    // The explicit opt-out keeps the legacy pass-through for gateways that
    // really stream their answer in `reasoning_content`.
    let passthrough = decode_chunks_with_style(
        &[r#"{"choices":[{"delta":{"reasoning_content":"answer via reasoning field"}}]}"#],
        reasoning_stream_style_for_route(
            ProviderKind::Openai,
            "https://gateway.example.test/v1",
            "some-new-reasoner",
            Some("none"),
        ),
    );
    assert_eq!(thinking_delta_text(&passthrough), "");
    assert_eq!(text_delta_text(&passthrough), "answer via reasoning field");
}

#[test]
fn reasoning_style_inline_tags_routes_think_blocks_to_thinking() {
    let events = decode_chunks_with_style(
        &[
            r#"{"choices":[{"delta":{"content":"Before <thi"}}]}"#,
            r#"{"choices":[{"delta":{"content":"nk>private plan</thi"}}]}"#,
            r#"{"choices":[{"delta":{"content":"nk> after."}}]}"#,
        ],
        ReasoningStreamStyle::InlineTags,
    );

    assert_eq!(thinking_delta_text(&events), "private plan");
    assert_eq!(text_delta_text(&events), "Before  after.");
    assert!(
        !text_delta_text(&events).contains("<think>"),
        "inline reasoning tags must not leak into visible text: {events:?}"
    );
}

#[test]
fn reasoning_style_inline_tags_flushes_unclosed_think_at_stream_end() {
    let events = decode_chunks_with_style(
        &[
            r#"{"choices":[{"delta":{"content":"Before <think>partial reasoning"}}]}"#,
            r#"{"choices":[{"finish_reason":"stop"}]}"#,
        ],
        ReasoningStreamStyle::InlineTags,
    );

    assert_eq!(thinking_delta_text(&events), "partial reasoning");
    assert_eq!(text_delta_text(&events), "Before ");
}

#[test]
fn reasoning_style_inline_tags_ignores_separate_reasoning_field() {
    let events = decode_chunks_with_style(
        &[
            r#"{"choices":[{"delta":{"reasoning_content":"metadata","content":"<think>tagged</think> answer"}}]}"#,
        ],
        ReasoningStreamStyle::InlineTags,
    );

    assert_eq!(thinking_delta_text(&events), "tagged");
    assert_eq!(text_delta_text(&events), " answer");
}

#[test]
fn reasoning_style_none_keeps_inline_tags_visible_text() {
    let events = decode_chunks_with_style(
        &[r#"{"choices":[{"delta":{"content":"<think>visible</think> answer"}}]}"#],
        ReasoningStreamStyle::None,
    );

    assert_eq!(thinking_delta_text(&events), "");
    assert_eq!(text_delta_text(&events), "<think>visible</think> answer");
}

#[test]
fn configured_reasoning_style_overrides_route_default() {
    assert_eq!(
        reasoning_stream_style_for_stream(ProviderKind::Openai, "custom-minimax", None),
        ReasoningStreamStyle::SeparateField
    );
    assert_eq!(
        reasoning_stream_style_for_stream(
            ProviderKind::Openai,
            "custom-minimax",
            Some("inline-tags")
        ),
        ReasoningStreamStyle::InlineTags
    );
    assert_eq!(
        reasoning_stream_style_for_stream(ProviderKind::XiaomiMimo, "mimo-v2.5-pro", None),
        ReasoningStreamStyle::SeparateField
    );
    assert_eq!(
        reasoning_stream_style_for_stream(ProviderKind::XiaomiMimo, "mimo-v2.5-pro", Some("none")),
        ReasoningStreamStyle::None
    );
}

#[test]
fn decoder_yields_no_events_for_keepalive_chunk() {
    // DeepSeek often sends `{"choices":[]}` keepalive chunks before
    // emitting real content. The engine MUST treat a stream error after
    // these as "no content received" and be eligible for transparent
    // retry — assert here that the decoder yields no payload events.
    let events = decode_chunk(r#"{"choices":[]}"#);
    assert!(
        events.is_empty(),
        "empty-choices chunk must produce no events; got {events:?}"
    );
}

#[test]
fn decoder_treats_done_frame_as_terminal() {
    let mut content_index = 0u32;
    let mut text_started = false;
    let mut thinking_started = false;
    let mut tool_indices = std::collections::HashMap::new();
    let mut reasoning_detail_buffers = std::collections::HashMap::new();
    let mut inline_reasoning_tags = InlineReasoningTagState::default();

    let outcome = parse_sse_data_frame(
        "  [DONE]  ",
        &mut content_index,
        &mut text_started,
        &mut thinking_started,
        &mut tool_indices,
        &mut reasoning_detail_buffers,
        &mut inline_reasoning_tags,
        ReasoningStreamStyle::SeparateField,
    );

    assert!(
        matches!(outcome, SseDataFrame::Done),
        "`data: [DONE]` must terminate the stream instead of waiting for the HTTP connection to close"
    );
    assert_eq!(content_index, 0);
    assert!(!text_started);
    assert!(!thinking_started);
    assert!(tool_indices.is_empty());
}

#[test]
fn decoder_emits_tool_use_block_for_tool_call_delta() {
    // Tool-call deltas are content too — once one arrives, transparent
    // retry must be off (the model has committed to a tool invocation
    // path that DeepSeek has billed for).
    let events = decode_chunk(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","function":{"name":"grep_files","arguments":"{\"pattern\":\"foo\"}"}}]}}]}"#,
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::ToolUse { name, ..},
                ..
            } if name == "grep_files"
        )),
        "should open a ToolUse block for grep_files; got {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            StreamEvent::ContentBlockDelta {
                delta: Delta::InputJsonDelta { partial_json },
                ..
            } if partial_json.contains("\"pattern\"")
        )),
        "should yield InputJsonDelta carrying the tool args; got {events:?}"
    );
}

#[test]
fn decoder_uses_fallback_name_for_empty_streaming_tool_name() {
    let events = decode_chunk(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_empty","function":{"name":"","arguments":"{}"}}]}}]}"#,
    );

    assert!(
        events.iter().any(|event| matches!(
            event,
            StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::ToolUse { name, ..},
                ..
            } if name == "unknown_tool"
        )),
        "empty upstream tool names should render as unknown_tool; got {events:?}"
    );
}

#[test]
fn non_streaming_response_uses_fallback_name_for_missing_tool_name() {
    let payload: Value = serde_json::from_str(
        r#"{
                "id": "chatcmpl_1",
                "model": "deepseek-v4-pro",
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call_missing",
                            "function": { "arguments": "{}" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            }"#,
    )
    .expect("valid response");

    let parsed = parse_chat_message(&payload).expect("message parses");
    let tool_name = parsed.content.iter().find_map(|block| match block {
        ContentBlock::ToolUse { name, .. } => Some(name.as_str()),
        _ => None,
    });

    assert_eq!(tool_name, Some("unknown_tool"));
}

/// Regression for the parallel-tool-calls-without-id collision (audit
/// Finding 8): when the upstream chunk omits the `id` field, the
/// fallback used to be the literal string `"tool_call"` for every
/// parallel call, so two tool calls in one delta ended up sharing an
/// id. Downstream routing then matched the first call's tool_result
/// twice and the second call hung. The fallback is now indexed by the
/// content-block position, keeping each call unique within the
/// response.
#[test]
fn decoder_assigns_unique_fallback_ids_to_parallel_tool_calls_missing_id() {
    let events = decode_chunk(
        r#"{"choices":[{"delta":{"tool_calls":[
                {"index":0,"function":{"name":"grep_files","arguments":"{\"pattern\":\"a\"}"}},
                {"index":1,"function":{"name":"read_file","arguments":"{\"path\":\"x\"}"}}
            ]}}]}"#,
    );

    let ids: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::ToolUse { id, .. },
                ..
            } => Some(id.as_str()),
            _ => None,
        })
        .collect();

    assert_eq!(
        ids.len(),
        2,
        "expected two tool-use blocks for parallel tool calls; got {events:?}"
    );
    assert_ne!(
        ids[0], ids[1],
        "parallel tool calls without upstream `id` must get distinct fallback ids; got {ids:?}"
    );
}

#[test]
fn decoder_preserves_upstream_tool_call_id_when_present() {
    // Counter-test to the fallback regression: when the upstream chunk
    // does include `id`, we forward it verbatim — we shouldn't quietly
    // rewrite ids the API gave us just because we have a fallback path.
    let events = decode_chunk(
        r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_xyz","function":{"name":"grep_files","arguments":"{}"}}]}}]}"#,
    );
    let id = events
        .iter()
        .find_map(|e| match e {
            StreamEvent::ContentBlockStart {
                content_block: ContentBlockStart::ToolUse { id, .. },
                ..
            } => Some(id.as_str()),
            _ => None,
        })
        .expect("tool-use block present");
    assert_eq!(id, "call_xyz");
}

#[test]
fn request_builder_preserves_internal_system_messages() {
    let messages = vec![Message {
        role: Role::System,
        content: vec![ContentBlock::Text {
            text: "internal runtime event".to_string(),
            cache_control: None,
        }],
    }];

    let built = build_chat_messages(None, &messages, "deepseek-v4-flash");

    assert_eq!(built.len(), 1);
    assert_eq!(built[0]["role"], "system");
    assert_eq!(built[0]["content"], "internal runtime event");
}

fn tool_use_message(id: &str, name: &str, input: Value) -> Message {
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

fn tool_result_message(id: &str, content: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![ContentBlock::ToolResult {
            execution_id: None,
            tool_use_id: id.to_string(),
            content: content.to_string(),
            is_error: None,
            content_blocks: None,
        }],
    }
}

fn user_message_with_turn_meta(turn_meta: &str, task: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: turn_meta.to_string(),
                cache_control: None,
            },
            ContentBlock::Text {
                text: task.to_string(),
                cache_control: None,
            },
        ],
    }
}

fn user_message_with_tail_turn_meta(task: &str, turn_meta: &str) -> Message {
    Message {
        role: Role::User,
        content: vec![
            ContentBlock::Text {
                text: task.to_string(),
                cache_control: None,
            },
            ContentBlock::Text {
                text: turn_meta.to_string(),
                cache_control: None,
            },
        ],
    }
}

fn tool_message_content(messages: &[Value], index: usize) -> &str {
    messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("tool"))
        .nth(index)
        .and_then(|message| message.get("content").and_then(Value::as_str))
        .expect("tool message content")
}

fn user_message_content(messages: &[Value], index: usize) -> &str {
    messages
        .iter()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .nth(index)
        .and_then(|message| message.get("content").and_then(Value::as_str))
        .expect("user message content")
}

fn with_tool_result_sha_spillover_root<T>(f: impl FnOnce() -> T) -> T {
    let _guard = crate::tools::truncate::TEST_SPILLOVER_GUARD
        .lock()
        .unwrap_or_else(|err| err.into_inner());
    let tmp = tempfile::tempdir().expect("tempdir");
    let prior = crate::tools::truncate::set_test_spillover_root(Some(
        tmp.path().join(".deepseek").join("tool_outputs"),
    ));
    struct Restore(Option<std::path::PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::tools::truncate::set_test_spillover_root(self.0.take());
        }
    }
    let _restore = Restore(prior);
    f()
}

#[test]
fn request_builder_deduplicates_consecutive_identical_turn_meta_for_wire() {
    let turn_meta = "<turn_meta>\nCurrent local date: 2026-05-09\n</turn_meta>";
    let messages = vec![
        user_message_with_turn_meta(turn_meta, "first task"),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "first answer".to_string(),
                cache_control: None,
            }],
        },
        user_message_with_turn_meta(turn_meta, "second task"),
    ];

    let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
    let first = user_message_content(&built, 0);
    let second = user_message_content(&built, 1);
    let expected_ref = "<turn_meta_unchanged />";

    assert!(first.starts_with(turn_meta), "got: {first}");
    assert!(second.starts_with(expected_ref), "got: {second}");
    assert!(second.ends_with("second task"), "got: {second}");
    assert_eq!(
        second,
        format!("{expected_ref}\nsecond task"),
        "ref text must stay stable"
    );
}

#[test]
fn request_builder_keeps_tail_turn_meta_after_user_text_for_wire() {
    let turn_meta = "<turn_meta>\nCurrent local date: 2026-05-09\n</turn_meta>";
    let messages = vec![
        user_message_with_tail_turn_meta("first task", turn_meta),
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "first answer".to_string(),
                cache_control: None,
            }],
        },
        user_message_with_tail_turn_meta("second task", turn_meta),
    ];

    let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
    let first = user_message_content(&built, 0);
    let second = user_message_content(&built, 1);
    let expected_ref = "<turn_meta_unchanged />";

    assert_eq!(first, format!("first task\n{turn_meta}"));
    assert_eq!(second, format!("second task\n{expected_ref}"));
}

#[test]
fn request_builder_keeps_changed_turn_meta_full_and_updates_recent_hash() {
    let first_meta = "<turn_meta>\nCurrent local date: 2026-05-09\n</turn_meta>";
    let second_meta =
        "<turn_meta>\nCurrent local date: 2026-05-09\nWorking set: src/lib.rs\n</turn_meta>";
    let messages = vec![
        user_message_with_turn_meta(first_meta, "first task"),
        user_message_with_turn_meta(second_meta, "second task"),
    ];

    let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
    let first = user_message_content(&built, 0);
    let second = user_message_content(&built, 1);

    assert!(first.starts_with(first_meta), "got: {first}");
    assert!(second.starts_with(second_meta), "got: {second}");
    assert!(!second.contains("<TURN_META_REF"), "got: {second}");
}

#[test]
fn turn_meta_dedup_is_wire_only_and_does_not_mutate_session_message() {
    let turn_meta = "<turn_meta>\nCurrent local date: 2026-05-09\n</turn_meta>";
    let messages = vec![
        user_message_with_turn_meta(turn_meta, "first task"),
        user_message_with_turn_meta(turn_meta, "second task"),
    ];

    let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
    assert!(
        user_message_content(&built, 1).starts_with("<turn_meta_unchanged />"),
        "got: {}",
        user_message_content(&built, 1)
    );

    match &messages[1].content[0] {
        ContentBlock::Text { text, .. } => assert_eq!(text, turn_meta),
        other => panic!("expected text block, got {other:?}"),
    }
}

#[test]
fn cache_inspect_reports_turn_meta_dedup_metadata() {
    let turn_meta = format!(
        "<turn_meta>\nCurrent local date: 2026-05-09\n{}\n</turn_meta>",
        "Working set: src/lib.rs\n".repeat(20)
    );
    let request = MessageRequest {
        model: "deepseek-v4-flash".to_string(),
        messages: vec![
            user_message_with_turn_meta(&turn_meta, "first task"),
            user_message_with_turn_meta(&turn_meta, "second task"),
        ],
        max_tokens: 0,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: None,
        temperature: None,
        top_p: None,
    };

    let inspection = inspect_prompt_for_request(&request);
    let turn_meta_layers: Vec<_> = inspection
        .layers
        .iter()
        .filter_map(|layer| layer.turn_meta.as_ref())
        .collect();

    assert_eq!(turn_meta_layers.len(), 2);
    assert_eq!(
        turn_meta_layers[0].original_chars,
        turn_meta.chars().count()
    );
    assert_eq!(turn_meta_layers[0].sent_chars, turn_meta.chars().count());
    assert!(!turn_meta_layers[0].deduplicated);
    assert_eq!(turn_meta_layers[0].sha256, sha256_hex(turn_meta.as_bytes()));
    assert_eq!(
        turn_meta_layers[1].original_chars,
        turn_meta.chars().count()
    );
    assert!(turn_meta_layers[1].sent_chars < turn_meta_layers[1].original_chars);
    assert!(turn_meta_layers[1].deduplicated);
    assert_eq!(turn_meta_layers[1].sha256, turn_meta_layers[0].sha256);
}

#[test]
fn request_builder_truncates_large_tool_result_for_wire() {
    let long_output = format!("{}{}", "A".repeat(70_000), "Z".repeat(70_000));
    let messages = vec![
        tool_use_message(
            "tool-long",
            "shell_command",
            json!({"command": "cargo test"}),
        ),
        tool_result_message("tool-long", &long_output),
    ];

    let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
    let sent = tool_message_content(&built, 0);

    assert!(sent.contains("[TOOL_RESULT_TRUNCATED]"), "got: {sent}");
    assert!(sent.contains("tool_name: shell_command"), "got: {sent}");
    assert!(sent.contains("command_or_query: cargo test"), "got: {sent}");
    assert!(sent.contains("original_chars: 140000"), "got: {sent}");
    assert!(sent.contains("sha256:"), "got: {sent}");
    assert!(
        sent.contains("exact_detail: unavailable; no session-owned artifact was recorded"),
        "got: {sent}"
    );
    assert!(!sent.contains("retrieve_tool_result"), "got: {sent}");
    // The model catalog supplies the window when no explicit route is available.
    let budget = tool_result_sent_char_budget("deepseek-v4-flash");
    assert!(budget >= 100_000);
    let excerpt = budget - TOOL_RESULT_EXCERPT_FRAME_CHARS;
    let head = excerpt * 2 / 3;
    assert!(sent.contains(&"A".repeat(head)), "head kept");
    assert!(sent.contains(&"Z".repeat(excerpt - head)), "tail kept");
    assert!(
        sent.contains(&format!(
            "truncated {} chars from middle",
            140_000 - excerpt
        )),
        "omitted count"
    );
    assert_ne!(sent, long_output);
}

#[test]
fn restored_tool_results_obey_the_active_route_budget() {
    let output = "x".repeat(20_000);
    let recovery = format!(
        "{output}\n{}",
        codewhale_tools::names::SPILLOVER_RECOVERY_HINT
    );
    let request = MessageRequest {
        model: "trinity-mini".into(),
        messages: vec![
            tool_use_message("raw", "read_file", json!({"path": "a.rs"})),
            tool_result_message("raw", &output),
            tool_use_message("repeat", "read_file", json!({"path": "a.rs"})),
            tool_result_message("repeat", &output),
            tool_use_message("saved", "read_file", json!({"path": "b.rs"})),
            tool_result_message("saved", &recovery),
        ],
        max_tokens: 1_024,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: None,
        temperature: None,
        top_p: None,
    };
    // The bundled 128K route and an explicit smaller offering both beat
    // the old unknown-route 100K ceiling. Streaming and blocking share it.
    for (limits, expected_budget) in [
        (None, 15_360),
        (
            Some(RouteLimits {
                context_tokens: Some(32_000),
                ..RouteLimits::default()
            }),
            3_840,
        ),
    ] {
        for stream in [false, true] {
            let wire = build_chat_wire_body(
                &request,
                ProviderKind::Arcee,
                "https://api.arcee.ai/api/v1",
                stream,
                limits,
            )
            .unwrap();
            let messages = wire.body["messages"].as_array().unwrap();
            for index in [0, 1] {
                let sent = tool_message_content(messages, index);
                assert!(sent.contains("[TOOL_RESULT_TRUNCATED]"));
                assert!(sent.chars().count() <= expected_budget);
                assert!(!sent.contains("<TOOL_RESULT_REF"));
                assert!(sent.contains("exact_detail: unavailable"));
            }
            assert_eq!(tool_message_content(messages, 2), recovery);
        }
    }
    assert!(matches!(&request.messages[1].content[0],
        ContentBlock::ToolResult { content, .. } if content == &output));
}

#[test]
fn request_builder_keeps_unowned_extreme_tool_output_bounded_without_false_hint() {
    with_tool_result_sha_spillover_root(|| {
        let huge_output = format!(
            "{}{}{}",
            "DIFF_HEAD\n".repeat(10_000),
            "MIDDLE_POISON\n".repeat(10_000),
            "DIFF_TAIL\n".repeat(10_000)
        );
        let sha = sha256_hex(huge_output.as_bytes());
        let messages = vec![
            tool_use_message("tool-huge", "exec_shell", json!({"command": "git diff"})),
            tool_result_message("tool-huge", &huge_output),
        ];

        let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
        let sent = tool_message_content(&built, 0);

        assert!(sent.contains("[TOOL_RESULT_TRUNCATED]"), "got: {sent}");
        assert!(sent.contains("tool_name: exec_shell"), "got: {sent}");
        assert!(sent.contains("command_or_query: git diff"), "got: {sent}");
        assert!(sent.contains(&format!("sha256: {sha}")), "got: {sent}");
        assert!(sent.contains("exact_detail: unavailable"), "got: {sent}");
        assert!(!sent.contains("retrieve_tool_result"), "got: {sent}");
        assert!(
            sent.chars().count() <= tool_result_sent_char_budget("deepseek-v4-flash"),
            "truncated result should stay bounded, sent {} chars",
            sent.chars().count()
        );
        assert!(
            !sent.contains("MIDDLE_POISON"),
            "omitted middle should not be sent to the next model turn"
        );
        assert_ne!(sent, huge_output);
    });
}

#[test]
fn request_builder_does_not_dedup_short_tool_results_for_wire() {
    let output = "same tool output";
    let messages = vec![
        tool_use_message("tool-1", "read_file", json!({"path": "README.md"})),
        tool_result_message("tool-1", output),
        tool_use_message("tool-2", "read_file", json!({"path": "README.md"})),
        tool_result_message("tool-2", output),
    ];

    let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
    let first = tool_message_content(&built, 0);
    let second = tool_message_content(&built, 1);

    assert_eq!(first, output);
    assert_eq!(second, output);
    assert!(!second.contains("<TOOL_RESULT_REF"), "got: {second}");
}

#[test]
fn request_builder_deduplicates_medium_identical_tool_results_to_earlier_message() {
    with_tool_result_sha_spillover_root(|| {
        // 2,000 chars is intentionally above TOOL_RESULT_DEDUP_MIN_CHARS
        // (1,024) but below the wire backstop budget. This
        // verifies the cache-saving path for repeated medium outputs that
        // do not otherwise need truncation.
        let output = "A".repeat(2_000);
        let messages = vec![
            tool_use_message("tool-1", "read_file", json!({"path": "README.md"})),
            tool_result_message("tool-1", &output),
            tool_use_message("tool-2", "read_file", json!({"path": "README.md"})),
            tool_result_message("tool-2", &output),
        ];

        let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
        let first = tool_message_content(&built, 0);
        let second = tool_message_content(&built, 1);

        assert_eq!(first, output);
        assert!(!first.contains("[TOOL_RESULT_TRUNCATED]"), "got: {first}");
        assert!(
            second.starts_with("<TOOL_RESULT_REF sha=\""),
            "got: {second}"
        );
        assert!(
            second.contains("original_message=\"Message #1\""),
            "got: {second}"
        );
        assert!(second.contains("chars=\"2000\""), "got: {second}");
        assert!(
            second.contains("source: full content appears in Message #1 earlier in this request"),
            "got: {second}"
        );
        assert!(!second.contains("retrieve_tool_result"), "got: {second}");
    });
}

#[test]
fn request_builder_never_dedups_large_identical_write_file_confirmations() {
    with_tool_result_sha_spillover_root(|| {
        // A `write_file` result embeds the unified diff + summary; it is a
        // confirmation, not retrievable data. Two identical >1024-char
        // write_file results must BOTH stay inline — collapsing the second
        // to a SHA ref makes the model lose write-success context and
        // report the file as missing (#1695).
        let output = "A".repeat(2_000);
        let messages = vec![
            tool_use_message("tool-1", "write_file", json!({"path": "big.txt"})),
            tool_result_message("tool-1", &output),
            tool_use_message("tool-2", "write_file", json!({"path": "big.txt"})),
            tool_result_message("tool-2", &output),
        ];

        let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
        let first = tool_message_content(&built, 0);
        let second = tool_message_content(&built, 1);

        assert_eq!(first, output);
        assert_eq!(second, output);
        assert!(!second.contains("<TOOL_RESULT_REF"), "got: {second}");

        // Non-mutation tools still dedup: an identical medium read_file
        // result points back to the first full message in this request.
        let read_messages = vec![
            tool_use_message("read-1", "read_file", json!({"path": "README.md"})),
            tool_result_message("read-1", &output),
            tool_use_message("read-2", "read_file", json!({"path": "README.md"})),
            tool_result_message("read-2", &output),
        ];
        let read_built = build_chat_messages(None, &read_messages, "deepseek-v4-flash");
        let read_first = tool_message_content(&read_built, 0);
        let read_second = tool_message_content(&read_built, 1);
        assert_eq!(read_first, output);
        assert!(
            read_second.starts_with("<TOOL_RESULT_REF sha=\""),
            "got: {read_second}"
        );
        assert!(read_second.contains("source: full content appears in Message #1"));
        assert!(!read_second.contains("retrieve_tool_result"));
    });
}

#[test]
fn large_unowned_results_stay_bounded_without_false_retrieval_handles() {
    // The adaptive router normally replaces a large result with a
    // session-owned artifact receipt before this provider-wire fallback.
    // If legacy/raw history reaches here, it may be excerpted but must not
    // advertise the process-wide SHA store as retrievable.
    let big_diff = "D".repeat(120_000);
    let sha = sha256_hex(big_diff.as_bytes());

    let messages = vec![
        tool_use_message("w-1", "write_file", json!({"path": "huge.rs"})),
        tool_result_message("w-1", &big_diff),
        tool_use_message("w-2", "write_file", json!({"path": "huge.rs"})),
        tool_result_message("w-2", &big_diff),
    ];
    let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
    let first = tool_message_content(&built, 0);
    let second = tool_message_content(&built, 1);

    // Mutation confirmations are independently excerpted, never deduped.
    assert!(
        first.contains("[TOOL_RESULT_TRUNCATED]"),
        "first should be truncated, got: {first}"
    );
    assert!(
        !first.contains("<TOOL_RESULT_REF"),
        "first must not be a dedup ref, got: {first}"
    );
    assert!(
        !second.contains("<TOOL_RESULT_REF"),
        "second identical write_file must stay inline (#1695), got: {second}"
    );
    assert!(
        second.contains("[TOOL_RESULT_TRUNCATED]"),
        "second should also be inline-truncated, got: {second}"
    );
    assert!(
        first.contains(&format!("sha256: {sha}")),
        "truncation block should retain an integrity digest, got: {first}"
    );
    assert!(first.contains("exact_detail: unavailable"));
    assert!(!first.contains("retrieve_tool_result"));

    // A huge non-mutation result cannot refer to an earlier *full* message,
    // because both wire messages are excerpts. It therefore stays a
    // truthful bounded excerpt too.
    let read_messages = vec![
        tool_use_message("r-1", "read_file", json!({"path": "huge.rs"})),
        tool_result_message("r-1", &big_diff),
        tool_use_message("r-2", "read_file", json!({"path": "huge.rs"})),
        tool_result_message("r-2", &big_diff),
    ];
    let read_built = build_chat_messages(None, &read_messages, "deepseek-v4-flash");
    let read_second = tool_message_content(&read_built, 1);
    assert!(read_second.contains("[TOOL_RESULT_TRUNCATED]"));
    assert!(!read_second.contains("<TOOL_RESULT_REF"));
    assert!(!read_second.contains("retrieve_tool_result"));
}

#[test]
fn tool_result_budget_is_wire_only_and_does_not_mutate_session_message() {
    let long_output = format!("{}{}", "A".repeat(70_000), "Z".repeat(70_000));
    let messages = vec![
        tool_use_message(
            "tool-long",
            "shell_command",
            json!({"command": "cargo test"}),
        ),
        tool_result_message("tool-long", &long_output),
    ];

    let built = build_chat_messages(None, &messages, "deepseek-v4-flash");
    let sent = tool_message_content(&built, 0);
    assert_ne!(sent, long_output);

    match &messages[1].content[0] {
        ContentBlock::ToolResult { content, .. } => assert_eq!(content, &long_output),
        other => panic!("expected tool result, got {other:?}"),
    }
}

#[test]
fn cache_inspect_reports_bounded_unowned_tool_result_metadata() {
    let long_output = format!("{}{}", "A".repeat(70_000), "Z".repeat(70_000));
    let request = MessageRequest {
        model: "deepseek-v4-flash".to_string(),
        messages: vec![
            tool_use_message("tool-1", "shell_command", json!({"command": "cargo test"})),
            tool_result_message("tool-1", &long_output),
            tool_use_message("tool-2", "shell_command", json!({"command": "cargo test"})),
            tool_result_message("tool-2", &long_output),
        ],
        max_tokens: 0,
        system: None,
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        reasoning_effort: None,
        stream: None,
        temperature: None,
        top_p: None,
    };

    let inspection = inspect_prompt_for_request(&request);
    let tool_layers: Vec<_> = inspection
        .layers
        .iter()
        .filter_map(|layer| layer.tool_result.as_ref())
        .collect();

    assert_eq!(tool_layers.len(), 2);
    for layer in tool_layers {
        assert_eq!(layer.original_chars, 140_000);
        assert!(layer.sent_chars < layer.original_chars);
        assert!(layer.truncated);
        assert!(!layer.deduplicated);
    }
}

#[test]
fn mistral_stream_blocks_are_decoded_only_by_the_mistral_style() {
    let chunk = r#"{
            "choices": [{
                "index": 0,
                "delta": {"content": [
                    {"type": "thinking", "thinking": [
                        {"type": "text", "text": "private trace"}
                    ], "closed": true},
                    {"type": "text", "text": "public answer"}
                ]},
                "finish_reason": null
            }]
        }"#;

    let mistral = decode_chunks_with_style(&[chunk], ReasoningStreamStyle::MistralBlocks);
    assert!(mistral.iter().any(|event| matches!(
        event,
        StreamEvent::ContentBlockDelta {
            delta: Delta::ThinkingDelta { thinking },
            ..
        } if thinking == "private trace"
    )));
    assert!(mistral.iter().any(|event| matches!(
        event,
        StreamEvent::ContentBlockDelta {
            delta: Delta::TextDelta { text },
            ..
        } if text == "public answer"
    )));

    let generic = decode_chunks_with_style(&[chunk], ReasoningStreamStyle::None);
    assert!(!generic.iter().any(|event| matches!(
        event,
        StreamEvent::ContentBlockDelta {
            delta: Delta::ThinkingDelta { .. },
            ..
        }
    )));
    assert!(!generic.iter().any(|event| matches!(
        event,
        StreamEvent::ContentBlockDelta {
            delta: Delta::TextDelta { .. },
            ..
        }
    )));
}

#[test]
fn deepseek_flash_v41_classifies_reasoning_through_the_catalog() {
    // #6044: V4.1's official id dropped the version number, so the literal
    // `deepseek-v4` arms cannot see it. The catalog owns the capability and
    // every classifier — stream style, wire replay, prompt inspection — must
    // read it there instead of relying on another classifier's fallback.
    let base_url = "https://api.deepseek.com";
    assert!(
        requires_reasoning_content("deepseek-flash"),
        "the name gate must recognize the official V4.1 id through the catalog"
    );
    assert!(
        should_replay_reasoning_content("deepseek-flash", None),
        "prompt inspection must agree with the wire request"
    );
    assert!(should_replay_reasoning_content_for_provider_on_route(
        ProviderKind::Deepseek,
        base_url,
        "deepseek-flash",
        None,
    ));

    let style =
        reasoning_stream_style_for_route(ProviderKind::Deepseek, base_url, "deepseek-flash", None);
    assert_eq!(style, ReasoningStreamStyle::SeparateField);
    let events = decode_chunks_with_style(
        &[r#"{"choices":[{"delta":{"reasoning_content":"private flash plan"}}]}"#],
        style,
    );
    assert_eq!(thinking_delta_text(&events), "private flash plan");
    assert_eq!(
        text_delta_text(&events),
        "",
        "reasoning must never leak into visible prose"
    );

    // A non-reasoning DeepSeek name stays literal: the prefix alone is not
    // evidence, the catalog entry is.
    assert!(!requires_reasoning_content("deepseek-coder"));
}

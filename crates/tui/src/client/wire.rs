//! Shared token-usage and SSE byte-line decoding extracted from `client.rs`.
//!
//! Protocol adapters retain event interpretation; this module owns the existing
//! accounting and fail-closed UTF-8 line helpers used across those adapters.

use anyhow::Result;
use serde_json::Value;

use codewhale_models::{ServerToolUsage, Usage};

pub(crate) fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

pub(crate) fn parse_usage(usage: Option<&Value>) -> Usage {
    let input_tokens = usage
        .and_then(|u| u.get("input_tokens").or_else(|| u.get("prompt_tokens")))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut output_tokens = usage
        .and_then(|u| {
            u.get("output_tokens")
                .or_else(|| u.get("completion_tokens"))
        })
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let total_tokens = usage
        .and_then(|u| u.get("total_tokens"))
        .and_then(Value::as_u64);
    let reasoning_tokens_raw = usage
        .and_then(|u| u.get("completion_tokens_details"))
        .and_then(|details| details.get("reasoning_tokens"))
        .and_then(Value::as_u64);
    if output_tokens == 0
        && let Some(reasoning_tokens) = reasoning_tokens_raw
    {
        output_tokens = reasoning_tokens;
    } else if output_tokens == 0
        && let Some(total_tokens) = total_tokens
    {
        output_tokens = total_tokens.saturating_sub(input_tokens);
    }
    let cached_tokens = usage
        .and_then(|u| u.get("prompt_tokens_details"))
        .and_then(|details| details.get("cached_tokens"))
        .and_then(Value::as_u64);
    let prompt_cache_hit_tokens = usage
        .and_then(|u| u.get("prompt_cache_hit_tokens"))
        .and_then(Value::as_u64)
        .or(cached_tokens)
        .map(saturating_u32);
    // Keep reported cache creation separate so pricing can apply its write
    // rate. Fall back per field: a details object without this field must not
    // hide the alternate dialect's value.
    let nested_cache_write_tokens = usage.and_then(|u| {
        u.pointer("/prompt_tokens_details/cache_write_tokens")
            .and_then(Value::as_u64)
            .or_else(|| {
                u.pointer("/input_tokens_details/cache_write_tokens")
                    .and_then(Value::as_u64)
            })
    });
    let prompt_cache_write_tokens = usage
        .and_then(|u| u.get("prompt_cache_write_tokens"))
        .and_then(Value::as_u64)
        .or(nested_cache_write_tokens)
        .map(saturating_u32);
    let prompt_cache_miss_tokens = usage
        .and_then(|u| u.get("prompt_cache_miss_tokens"))
        .and_then(Value::as_u64)
        .or_else(|| {
            // No cache report means unknown, including when usage is absent.
            // A reported zero is still a report and permits derivation.
            (prompt_cache_hit_tokens.is_some() || prompt_cache_write_tokens.is_some()).then(|| {
                let accounted = u64::from(prompt_cache_hit_tokens.unwrap_or(0))
                    .saturating_add(u64::from(prompt_cache_write_tokens.unwrap_or(0)));
                input_tokens.saturating_sub(accounted)
            })
        })
        .map(saturating_u32);
    // Reasoning tokens are a *subset* of the completion count every provider
    // bills, so they are never added to output. A payload claiming more
    // reasoning than output contradicts that invariant, which makes the figure
    // invalid telemetry rather than extra billable output: drop it instead of
    // letting a bad number reach the cost surfaces (#4318).
    let reasoning_tokens = reasoning_tokens_raw
        .filter(|reasoning| *reasoning <= output_tokens)
        .map(saturating_u32);

    let server_tool_use = usage.and_then(|u| u.get("server_tool_use")).map(|server| {
        let code_execution_requests = server
            .get("code_execution_requests")
            .and_then(Value::as_u64)
            .map(saturating_u32);
        let tool_search_requests = server
            .get("tool_search_requests")
            .and_then(Value::as_u64)
            .map(saturating_u32);
        ServerToolUsage {
            code_execution_requests,
            tool_search_requests,
        }
    });

    Usage {
        input_tokens: saturating_u32(input_tokens),
        output_tokens: saturating_u32(output_tokens),
        prompt_cache_hit_tokens,
        prompt_cache_miss_tokens,
        prompt_cache_write_tokens,
        reasoning_tokens,
        reasoning_replay_tokens: None,
        server_tool_use,

        input_audio_tokens: usage
            .and_then(|usage| {
                usage
                    .pointer("/prompt_tokens_details/audio_tokens")
                    .or_else(|| usage.pointer("/input_tokens_details/audio_tokens"))
            })
            .and_then(Value::as_u64)
            .map(saturating_u32),
        output_audio_tokens: usage
            .and_then(|usage| {
                usage
                    .pointer("/completion_tokens_details/audio_tokens")
                    .or_else(|| usage.pointer("/output_tokens_details/audio_tokens"))
            })
            .and_then(Value::as_u64)
            .map(saturating_u32),

        request_usages: Vec::new(),
    }
}

pub(super) fn extract_sse_data_value(line: &str) -> Option<&str> {
    line.strip_prefix("data:")
        .map(|value| value.strip_prefix(' ').unwrap_or(value))
}

/// Hard ceiling for one pending SSE line, matching the MCP frame limit.
/// This does not bound the size of a multi-line event or the whole stream.
const MAX_SSE_LINE_BYTES: usize = 8 * 1024 * 1024;
/// Hard ceiling for one assembled SSE event (its `data:` fields joined), so a
/// peer that never sends the blank line ending an event cannot grow memory
/// without bound. Same size as the single-line ceiling.
const MAX_SSE_EVENT_BYTES: usize = MAX_SSE_LINE_BYTES;

/// Invalid or oversized SSE line (or unterminated flush).
///
/// HTTP/2 DATA and other transports may split a multi-byte character across
/// chunks. That is not this error: callers must buffer raw bytes until a
/// complete line (or stream end) before decoding. We never substitute
/// U+FFFD — fail closed so garbled CJK cannot enter the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SseLineError {
    InvalidUtf8 { valid_up_to: usize },
    TooLong,
    EventTooLong,
}

impl std::fmt::Display for SseLineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUtf8 { valid_up_to } => {
                write!(f, "invalid UTF-8 in SSE stream at byte {valid_up_to}")
            }
            Self::TooLong => write!(
                f,
                "SSE line exceeded {MAX_SSE_LINE_BYTES} bytes (8 MiB) — aborting stream"
            ),
            Self::EventTooLong => write!(
                f,
                "SSE event exceeded {MAX_SSE_EVENT_BYTES} bytes (8 MiB) — aborting stream"
            ),
        }
    }
}

impl std::error::Error for SseLineError {}

/// Decode one assembled SSE line (or stream-end tail) with `str::from_utf8`.
/// Does not substitute U+FFFD.
fn decode_sse_line_bytes(bytes: &[u8]) -> Result<&str, SseLineError> {
    std::str::from_utf8(bytes).map_err(|err| SseLineError::InvalidUtf8 {
        valid_up_to: err.valid_up_to(),
    })
}

/// Take the next COMPLETE line (up to the first `\n`) off a raw byte buffer,
/// draining it, and return it trimmed. Returns `Ok(None)` when no full line is
/// buffered yet. Decoding only complete lines (never an arbitrary network-read
/// boundary) means a multi-byte UTF-8 char — CJK, emoji, accented letter —
/// split across two reads is never corrupted to U+FFFD, since the `\n`
/// delimiter is ASCII and can never fall inside a multi-byte sequence.
///
/// Genuine invalid bytes fail closed (`Err(SseLineError)`); we do not
/// substitute U+FFFD.
pub(super) fn take_sse_line(buffer: &mut Vec<u8>) -> Result<Option<String>, SseLineError> {
    let mut remaining = buffer.as_slice();
    let decoded = take_sse_line_from_slice(&mut remaining);
    let consumed = buffer.len() - remaining.len();
    buffer.drain(..consumed);
    decoded
}

/// Advance over one complete line without moving the remaining bytes. Chunk
/// consumers can drain the consumed prefix once after processing all its lines.
pub(super) fn take_sse_line_from_slice(buffer: &mut &[u8]) -> Result<Option<String>, SseLineError> {
    let line_end = buffer.iter().position(|&b| b == b'\n');
    if line_end.unwrap_or(buffer.len()) > MAX_SSE_LINE_BYTES {
        *buffer = &[];
        return Err(SseLineError::TooLong);
    }
    let Some(line_end) = line_end else {
        return Ok(None);
    };
    // Strip a preceding `\r` so CRLF-delimited SSE frames do not leave CR.
    let mut end = line_end;
    if end > 0 && buffer[end - 1] == b'\r' {
        end -= 1;
    }
    let decoded = decode_sse_line_bytes(&buffer[..end]).map(|text| text.trim().to_string());
    *buffer = &buffer[line_end + 1..];
    decoded.map(Some)
}

/// Decode the unterminated tail left in `buffer` at stream end.
///
/// Same fail-closed UTF-8 contract as [`take_sse_line`]. Empty / whitespace-only
/// tails yield `Ok(None)`.
pub(super) fn flush_sse_line(buffer: &mut Vec<u8>) -> Result<Option<String>, SseLineError> {
    if buffer.len() > MAX_SSE_LINE_BYTES {
        buffer.clear();
        return Err(SseLineError::TooLong);
    }
    if buffer.is_empty() {
        return Ok(None);
    }
    let mut end = buffer.len();
    if buffer[end - 1] == b'\r' {
        end -= 1;
    }
    let decoded = decode_sse_line_bytes(&buffer[..end]).map(|text| text.trim().to_string());
    buffer.clear();
    decoded.map(|line| (!line.is_empty()).then_some(line))
}

/// Append one `data:` field to the event being assembled. The SSE spec joins
/// the `data:` fields of one event with '\n' and dispatches the event at the
/// blank line that ends it; a provider may split one JSON payload across
/// several fields, so parsing each field alone would drop the event. Fails
/// closed (clearing the event) past [`MAX_SSE_EVENT_BYTES`].
pub(super) fn push_sse_event_data(event: &mut String, data: &str) -> Result<(), SseLineError> {
    let needed = event
        .len()
        .saturating_add(usize::from(!event.is_empty()))
        .saturating_add(data.len());
    if needed > MAX_SSE_EVENT_BYTES {
        event.clear();
        return Err(SseLineError::EventTooLong);
    }
    if !event.is_empty() {
        event.push('\n');
    }
    event.push_str(data);
    Ok(())
}

/// Next decoded SSE line. When `at_end` is false, wait for `\n`. When `at_end`
/// is true, also flush an unterminated tail (stream closed).
pub(super) fn next_sse_line(
    buffer: &mut Vec<u8>,
    at_end: bool,
) -> Result<Option<String>, SseLineError> {
    match take_sse_line(buffer)? {
        Some(line) => Ok(Some(line)),
        None if at_end => flush_sse_line(buffer),
        None => Ok(None),
    }
}

/// Incremental raw-byte SSE line assembler for tests and the Chat Completions
/// decoder. HTTP/2 DATA may split a multi-byte UTF-8 character across chunks;
/// we never decode until a complete line or [`SseLineDecoder::finish`].
#[cfg(test)]
pub(super) struct SseLineDecoder {
    buffer: Vec<u8>,
}

#[cfg(test)]
impl SseLineDecoder {
    pub(super) fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, SseLineError> {
        self.buffer.extend_from_slice(chunk);
        let mut lines = Vec::new();
        while let Some(line) = take_sse_line(&mut self.buffer)? {
            lines.push(line);
        }
        Ok(lines)
    }

    pub(super) fn finish(mut self) -> Result<Option<String>, SseLineError> {
        flush_sse_line(&mut self.buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sse_line_limit_rejects_oversized_pending_complete_and_final_lines() {
        let mut decoder = SseLineDecoder::new();
        let at_limit = vec![b'x'; MAX_SSE_LINE_BYTES];
        assert!(decoder.push(&at_limit).unwrap().is_empty());
        let error = decoder
            .push(b"x")
            .expect_err("pending line exceeds the limit");
        assert_eq!(error, SseLineError::TooLong);
        assert!(error.to_string().contains("8 MiB"));
        assert!(decoder.buffer.is_empty());

        let mut complete = at_limit.clone();
        complete.push(b'\n');
        assert_eq!(
            take_sse_line(&mut complete).unwrap().unwrap().len(),
            MAX_SSE_LINE_BYTES
        );
        assert!(complete.is_empty());

        let mut oversized = vec![b'x'; MAX_SSE_LINE_BYTES + 1];
        assert_eq!(flush_sse_line(&mut oversized), Err(SseLineError::TooLong));
        assert!(oversized.is_empty());
        oversized.resize(MAX_SSE_LINE_BYTES + 1, b'x');
        oversized.push(b'\n');
        assert_eq!(
            next_sse_line(&mut oversized, false),
            Err(SseLineError::TooLong)
        );
        assert!(oversized.is_empty());

        // The cap applies per line, not to a chunk containing several lines.
        let mut multiple = at_limit;
        multiple.extend_from_slice(b"\nnext\n");
        assert!(next_sse_line(&mut multiple, false).unwrap().is_some());
        assert_eq!(
            next_sse_line(&mut multiple, false).unwrap().as_deref(),
            Some("next")
        );
    }

    #[test]
    fn parse_usage_preserves_absent_cache_telemetry_and_explicit_zero() {
        for payload in [
            None,
            Some(json!({})),
            Some(json!({"prompt_tokens": 100, "completion_tokens": 20})),
        ] {
            let usage = parse_usage(payload.as_ref());
            assert_eq!(usage.prompt_cache_hit_tokens, None);
            assert_eq!(usage.prompt_cache_miss_tokens, None);
            assert_eq!(usage.prompt_cache_write_tokens, None);
        }
        let zero = parse_usage(Some(&json!({
            "prompt_tokens": 100,
            "prompt_cache_hit_tokens": 0,
            "prompt_cache_write_tokens": 0,
            "prompt_tokens_details": {"cache_write_tokens": 75}
        })));
        assert_eq!(zero.prompt_cache_hit_tokens, Some(0));
        assert_eq!(zero.prompt_cache_write_tokens, Some(0));
        assert_eq!(zero.prompt_cache_miss_tokens, Some(100));
        let explicit_miss = parse_usage(Some(&json!({
            "prompt_tokens": 100,
            "prompt_cache_miss_tokens": 0
        })));
        assert_eq!(explicit_miss.prompt_cache_miss_tokens, Some(0));
    }

    #[test]
    fn parse_usage_cache_write_dialects_partition_the_input_total() {
        for payload in [
            json!({
                "input_tokens": 1000,
                "prompt_cache_hit_tokens": 600,
                "prompt_cache_write_tokens": 200
            }),
            json!({
                "prompt_tokens": 1000,
                "prompt_tokens_details": {"cached_tokens": 600, "cache_write_tokens": 200}
            }),
            json!({
                "input_tokens": 1000,
                "prompt_cache_hit_tokens": 600,
                "input_tokens_details": {"cache_write_tokens": 200}
            }),
            // Both objects can exist; the first lacks the write field.
            json!({
                "prompt_tokens": 1000,
                "prompt_tokens_details": {"cached_tokens": 600},
                "input_tokens_details": {"cache_write_tokens": 200}
            }),
        ] {
            let usage = parse_usage(Some(&payload));
            assert_eq!(usage.input_tokens, 1000);
            assert_eq!(usage.prompt_cache_hit_tokens, Some(600));
            assert_eq!(usage.prompt_cache_write_tokens, Some(200));
            assert_eq!(usage.prompt_cache_miss_tokens, Some(200));
            let priced = crate::pricing::token_usage_for_pricing(&usage);
            assert_eq!(priced.cache_read, 600);
            assert_eq!(priced.cache_write, 200);
            assert_eq!(priced.input, 200);
            assert_eq!(priced.input + priced.cache_read + priced.cache_write, 1000);
        }
        let write_only = parse_usage(Some(&json!({
            "prompt_tokens": 1000,
            "input_tokens_details": {"cache_write_tokens": 200}
        })));
        assert_eq!(write_only.prompt_cache_hit_tokens, None);
        assert_eq!(write_only.prompt_cache_write_tokens, Some(200));
        assert_eq!(write_only.prompt_cache_miss_tokens, Some(800));
    }

    #[test]
    fn parse_usage_cache_write_saturates_and_keeps_explicit_miss() {
        let excessive = parse_usage(Some(&json!({
            "prompt_tokens": 100,
            "prompt_cache_hit_tokens": 60,
            "prompt_cache_write_tokens": u64::MAX
        })));
        assert_eq!(excessive.input_tokens, 100);
        assert_eq!(excessive.prompt_cache_write_tokens, Some(u32::MAX));
        assert_eq!(excessive.prompt_cache_miss_tokens, Some(0));
        let explicit = parse_usage(Some(&json!({
            "prompt_tokens": 100,
            "prompt_cache_hit_tokens": 60,
            "prompt_cache_write_tokens": 20,
            "prompt_cache_miss_tokens": 10
        })));
        assert_eq!(explicit.prompt_cache_write_tokens, Some(20));
        assert_eq!(explicit.prompt_cache_miss_tokens, Some(10));
    }

    #[test]
    fn parse_usage_scenario() {
        // Scenario consolidation of: parse_usage_reads_deepseek_cache_and_reasoning_tokens, parse_usage_saturates_every_u64_token_field, parse_usage_counts_reasoning_tokens_when_completion_tokens_are_zero, parse_usage_derives_completion_tokens_from_total_tokens_when_needed, parse_usage_reads_v4_prompt_tokens_details_cached_tokens, parse_usage_infers_cache_miss_from_selected_hit_source
        // from parse_usage_reads_deepseek_cache_and_reasoning_tokens
        {
            let usage = parse_usage(Some(&json!({
                "prompt_tokens": 100,
                "completion_tokens": 20,
                "prompt_cache_hit_tokens": 70,
                "prompt_cache_miss_tokens": 30,
                "completion_tokens_details": {
                    "reasoning_tokens": 12
                }
            })));

            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 20);
            assert_eq!(usage.prompt_cache_hit_tokens, Some(70));
            assert_eq!(usage.prompt_cache_miss_tokens, Some(30));
            assert_eq!(usage.reasoning_tokens, Some(12));
        }
        // from parse_usage_saturates_every_u64_token_field
        {
            let usage = parse_usage(Some(&json!({
                "input_tokens": u64::MAX,
                "output_tokens": u64::MAX,
                "prompt_cache_hit_tokens": u64::MAX,
                "prompt_cache_miss_tokens": u64::MAX,
                "completion_tokens_details": { "reasoning_tokens": u64::MAX },
                "server_tool_use": {
                    "code_execution_requests": u64::MAX,
                    "tool_search_requests": u64::MAX
                }
            })));
            assert_eq!(usage.input_tokens, u32::MAX);
            assert_eq!(usage.output_tokens, u32::MAX);
            assert_eq!(usage.prompt_cache_hit_tokens, Some(u32::MAX));
            assert_eq!(usage.prompt_cache_miss_tokens, Some(u32::MAX));
            assert_eq!(usage.reasoning_tokens, Some(u32::MAX));
            let server = usage.server_tool_use.expect("server usage");
            assert_eq!(server.code_execution_requests, Some(u32::MAX));
            assert_eq!(server.tool_search_requests, Some(u32::MAX));
        }
        // from parse_usage_counts_reasoning_tokens_when_completion_tokens_are_zero
        {
            let usage = parse_usage(Some(&json!({
                "prompt_tokens": 100,
                "completion_tokens": 0,
                "completion_tokens_details": {
                    "reasoning_tokens": 12
                }
            })));

            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 12);
            assert_eq!(usage.reasoning_tokens, Some(12));
            assert!(
                crate::pricing::calculate_turn_cost_from_usage("deepseek-v4-pro", &usage)
                    .expect("DeepSeek V4 Pro pricing should apply")
                    > 0.0
            );
        }
        // from parse_usage_derives_completion_tokens_from_total_tokens_when_needed
        {
            let usage = parse_usage(Some(&json!({
                "prompt_tokens": 100,
                "total_tokens": 125,
                "prompt_cache_hit_tokens": 70,
                "prompt_cache_miss_tokens": 30
            })));

            assert_eq!(usage.input_tokens, 100);
            assert_eq!(usage.output_tokens, 25);
            assert_eq!(usage.prompt_cache_hit_tokens, Some(70));
            assert_eq!(usage.prompt_cache_miss_tokens, Some(30));
        }
        // from parse_usage_reads_v4_prompt_tokens_details_cached_tokens
        {
            let usage = parse_usage(Some(&json!({
                "prompt_tokens": 4000,
                "completion_tokens": 20,
                "prompt_tokens_details": {
                    "cached_tokens": 3000
                }
            })));

            assert_eq!(usage.input_tokens, 4000);
            assert_eq!(usage.output_tokens, 20);
            assert_eq!(usage.prompt_cache_hit_tokens, Some(3000));
            assert_eq!(usage.prompt_cache_miss_tokens, Some(1000));
        }
        // from parse_usage_infers_cache_miss_from_selected_hit_source
        {
            let usage = parse_usage(Some(&json!({
                "prompt_tokens": 4000,
                "completion_tokens": 20,
                "prompt_cache_hit_tokens": 3000,
                "prompt_tokens_details": {
                    "cached_tokens": 1000
                }
            })));

            assert_eq!(usage.input_tokens, 4000);
            assert_eq!(usage.prompt_cache_hit_tokens, Some(3000));
            assert_eq!(usage.prompt_cache_miss_tokens, Some(1000));
        }
    }

    /// Real-shaped Chat-Completions usage payloads from the three providers most
    /// likely to report reasoning tokens, carried end-to-end into pricing.
    ///
    /// Two invariants hold for every fixture: `reasoning_tokens <= output_tokens`,
    /// and pricing never adds reasoning on top of output — dropping the reasoning
    /// field entirely must not change the cost by a single cent.
    #[test]
    fn reasoning_parser_fixtures_never_exceed_or_add_to_billable_output() {
        use crate::config::ProviderKind;
        use crate::pricing::{calculate_turn_cost_estimate_for_provider, token_usage_for_pricing};

        // (label, provider, model, payload)
        let fixtures: [(&str, ProviderKind, &str, serde_json::Value); 3] = [
            (
                "moonshot",
                ProviderKind::Moonshot,
                "kimi-k2.7-code",
                json!({
                    "prompt_tokens": 30_000,
                    "completion_tokens": 2_400,
                    "total_tokens": 32_400,
                    "prompt_tokens_details": { "cached_tokens": 24_000 },
                    "completion_tokens_details": { "reasoning_tokens": 1_900 }
                }),
            ),
            (
                "minimax",
                ProviderKind::Minimax,
                "minimax-m3",
                json!({
                    "prompt_tokens": 12_000,
                    "completion_tokens": 3_000,
                    "total_tokens": 15_000,
                    "prompt_tokens_details": { "cached_tokens": 4_000 },
                    "completion_tokens_details": { "reasoning_tokens": 2_950 }
                }),
            ),
            (
                "openrouter",
                ProviderKind::Openrouter,
                "qwen/qwen3.7-plus",
                json!({
                    "prompt_tokens": 8_000,
                    "completion_tokens": 1_500,
                    "total_tokens": 9_500,
                    "prompt_tokens_details": { "cached_tokens": 2_000 },
                    "completion_tokens_details": { "reasoning_tokens": 1_500 }
                }),
            ),
        ];

        for (label, provider, model, payload) in fixtures {
            let usage = parse_usage(Some(&payload));
            let reasoning = usage.reasoning_tokens.expect("fixture reports reasoning");

            // Invariant 1: reasoning is a subset of the billed completion count.
            assert!(
                reasoning <= usage.output_tokens,
                "{label}: reasoning {reasoning} exceeds output {}",
                usage.output_tokens
            );
            // Billable output is exactly the reported completion count.
            let classes = token_usage_for_pricing(&usage);
            assert_eq!(
                classes.output,
                u64::from(usage.output_tokens),
                "{label}: reasoning leaked into billable output"
            );

            // Invariant 2: pricing does not add reasoning a second time. The same
            // usage with the reasoning field removed must cost the same.
            let without = codewhale_models::Usage {
                reasoning_tokens: None,
                ..usage.clone()
            };
            assert_eq!(
                calculate_turn_cost_estimate_for_provider(provider, model, &usage),
                calculate_turn_cost_estimate_for_provider(provider, model, &without),
                "{label}: reasoning changed the price"
            );
        }
    }

    /// A payload claiming more reasoning than output contradicts the subset
    /// invariant. That is broken telemetry, so the field is discarded — and it
    /// must never become extra billable output.
    #[test]
    fn pathological_reasoning_above_output_is_rejected_not_billed() {
        let usage = parse_usage(Some(&json!({
            "prompt_tokens": 1_000,
            "completion_tokens": 100,
            "completion_tokens_details": { "reasoning_tokens": 5_000 }
        })));

        assert_eq!(usage.output_tokens, 100, "output stays as reported");
        assert_eq!(
            usage.reasoning_tokens, None,
            "impossible reasoning telemetry is dropped rather than trusted"
        );
        let classes = crate::pricing::token_usage_for_pricing(&usage);
        assert_eq!(classes.output, 100);

        // `completion_tokens: 0` with reasoning present is the *legitimate*
        // shape this filter must not break: providers that report only reasoning
        // set output from it, keeping reasoning == output.
        let zero_output = parse_usage(Some(&json!({
            "prompt_tokens": 1_000,
            "completion_tokens": 0,
            "completion_tokens_details": { "reasoning_tokens": 12 }
        })));
        assert_eq!(zero_output.output_tokens, 12);
        assert_eq!(zero_output.reasoning_tokens, Some(12));
    }

    fn mid_char_split(text: &str, ch: char) -> usize {
        let needle = ch.to_string();
        let start = text
            .as_bytes()
            .windows(needle.len())
            .position(|window| window == needle.as_bytes())
            .unwrap_or_else(|| panic!("{ch:?} present in {text:?}"));
        start + 1
    }

    #[test]
    fn take_sse_scenario() {
        // Scenario consolidation of: take_sse_line_preserves_multibyte_split_across_reads, take_sse_line_returns_none_without_newline, take_sse_line_reassembles_cjk_and_rejects_invalid_bytes, take_sse_line_rejects_invalid_bytes_without_replacement
        // from take_sse_line_preserves_multibyte_split_across_reads
        {
            // "你好" streamed so the 3-byte '好' straddles a read boundary.
            let full = "data: 你好\n";
            let bytes = full.as_bytes();
            let split = mid_char_split(full, '好');
            let mut buffer: Vec<u8> = Vec::new();
            // First read: no complete line yet.
            buffer.extend_from_slice(&bytes[..split]);
            assert_eq!(take_sse_line(&mut buffer).expect("valid prefix"), None);
            // Second read completes the line; '好' must be intact, not U+FFFD.
            buffer.extend_from_slice(&bytes[split..]);
            let line = take_sse_line(&mut buffer)
                .expect("valid utf-8")
                .expect("a complete line");
            assert_eq!(line, "data: 你好");
            assert!(!line.contains('\u{FFFD}'), "multibyte char was corrupted");
            assert_eq!(extract_sse_data_value(&line), Some("你好"));
            // Buffer fully drained.
            assert!(buffer.is_empty());
        }
        // from take_sse_line_returns_none_without_newline
        {
            let mut buffer = b"data: partial".to_vec();
            assert_eq!(take_sse_line(&mut buffer).expect("valid utf-8"), None);
            assert_eq!(buffer, b"data: partial");
        }
        // from take_sse_line_reassembles_cjk_and_rejects_invalid_bytes
        {
            let full = "data: 测试中文\n";
            let split = mid_char_split(full, '试');
            let mut buffer = full.as_bytes()[..split].to_vec();
            assert_eq!(take_sse_line(&mut buffer).expect("valid prefix"), None);
            buffer.extend_from_slice(&full.as_bytes()[split..]);
            let line = take_sse_line(&mut buffer)
                .expect("valid utf-8")
                .expect("complete line");
            assert_eq!(line, "data: 测试中文");
            assert!(!line.contains('\u{FFFD}'));

            let mut invalid = b"data: ok".to_vec();
            invalid.push(0xFF);
            invalid.push(b'\n');
            let err = take_sse_line(&mut invalid).expect_err("invalid bytes must fail closed");
            assert!(!err.to_string().contains('\u{FFFD}'));
            assert_eq!(err, SseLineError::InvalidUtf8 { valid_up_to: 8 });
            assert!(
                invalid.is_empty(),
                "invalid line is consumed so retries cannot loop"
            );
        }
        // from take_sse_line_rejects_invalid_bytes_without_replacement
        {
            let mut buffer = b"data: ok".to_vec();
            buffer.push(0xFF);
            buffer.extend_from_slice(b"\n");
            let err = take_sse_line(&mut buffer).expect_err("0xFF is not UTF-8");
            assert_eq!(err, SseLineError::InvalidUtf8 { valid_up_to: 8 });
            assert!(!err.to_string().contains('\u{FFFD}'));
            assert!(buffer.is_empty(), "invalid line must be drained");
        }
    }

    #[test]
    fn flush_sse_scenario() {
        // Scenario consolidation of: flush_sse_line_reassembles_cjk_and_rejects_invalid_bytes, flush_sse_line_preserves_unterminated_cjk, flush_sse_line_rejects_truncated_multibyte_sequence
        // from flush_sse_line_reassembles_cjk_and_rejects_invalid_bytes
        {
            let text = "data: 你好世界";
            let split = mid_char_split(text, '好');
            let mut buffer = text.as_bytes()[..split].to_vec();
            assert_eq!(take_sse_line(&mut buffer).expect("no newline yet"), None);
            buffer.extend_from_slice(&text.as_bytes()[split..]);
            let line = flush_sse_line(&mut buffer)
                .expect("valid utf-8")
                .expect("unterminated tail");
            assert_eq!(line, "data: 你好世界");
            assert!(!line.contains('\u{FFFD}'));
            assert!(buffer.is_empty());
            assert_eq!(flush_sse_line(&mut buffer).expect("empty"), None);

            let mut invalid = vec![0x80, 0xBF];
            let err = flush_sse_line(&mut invalid).expect_err("invalid flush must fail closed");
            assert!(!err.to_string().contains('\u{FFFD}'));
            assert_eq!(err, SseLineError::InvalidUtf8 { valid_up_to: 0 });
            assert!(invalid.is_empty());
        }
        // from flush_sse_line_preserves_unterminated_cjk
        {
            let mut buffer = "data: 你好".as_bytes().to_vec();
            let line = flush_sse_line(&mut buffer)
                .expect("valid utf-8")
                .expect("residual line");
            assert_eq!(line, "data: 你好");
            assert!(!line.contains('\u{FFFD}'));
            assert!(buffer.is_empty());
        }
        // from flush_sse_line_rejects_truncated_multibyte_sequence
        {
            let mut buffer = "data: ".as_bytes().to_vec();
            buffer.extend_from_slice(&"好".as_bytes()[..2]);
            let err = flush_sse_line(&mut buffer).expect_err("truncated UTF-8");
            assert_eq!(err, SseLineError::InvalidUtf8 { valid_up_to: 6 });
            assert!(!err.to_string().contains('\u{FFFD}'));
            assert!(buffer.is_empty());
        }
    }

    #[test]
    fn decode_sse_line_bytes_rejects_invalid_without_replacement() {
        let ok = decode_sse_line_bytes("data: 你好".as_bytes()).expect("valid");
        assert_eq!(ok, "data: 你好");
        assert!(!ok.contains('\u{FFFD}'));

        let err = decode_sse_line_bytes(&[0xFF]).expect_err("bare 0xFF is invalid");
        assert!(!err.to_string().contains('\u{FFFD}'));
        assert_eq!(err, SseLineError::InvalidUtf8 { valid_up_to: 0 });
    }

    #[test]
    fn extract_sse_scenario() {
        // Scenario consolidation of: extract_sse_data_value_accepts_optional_space, extract_sse_data_value_handles_done_marker, extract_sse_data_value_rejects_non_data_lines
        // from extract_sse_data_value_accepts_optional_space
        {
            assert_eq!(
                extract_sse_data_value("data: {\"ok\":true}"),
                Some("{\"ok\":true}")
            );
            assert_eq!(
                extract_sse_data_value("data:{\"ok\":true}"),
                Some("{\"ok\":true}")
            );
        }
        // from extract_sse_data_value_handles_done_marker
        {
            assert_eq!(extract_sse_data_value("data: [DONE]"), Some("[DONE]"));
            assert_eq!(extract_sse_data_value("data:[DONE]"), Some("[DONE]"));
        }
        // from extract_sse_data_value_rejects_non_data_lines
        {
            assert_eq!(extract_sse_data_value("event: message"), None);
            assert_eq!(extract_sse_data_value(": heartbeat"), None);
        }
    }
}

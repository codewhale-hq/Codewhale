//! API request/response models for `DeepSeek` and OpenAI-compatible endpoints.

use serde::{Deserialize, Serialize};

pub mod auto_route_evidence;
pub mod hand_pricing;
pub mod image_window;
pub mod reasoning_effort_tier;
pub mod schema_canonicalize;
pub mod schema_sanitize;
pub mod tool_projection;
pub mod turn_classify;

/// Context window used only for legacy DeepSeek model IDs that do not name a
/// newer V4 alias and do not carry an explicit `*k` suffix.
pub const LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS: u32 = 128_000;
pub use codewhale_config::catalog::reviewed::constants::DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS;
/// Conservative Kimi Code K3 context baseline. The membership route's real
/// context is plan-tier dependent (verified 2026-07-20 from
/// <https://www.kimi.com/code/docs/en/kimi-code/models>): Moderato gets 256K,
/// while Allegretto and above get up to 1M. Bare `k3` therefore keeps this
/// safe floor everywhere; higher plan entitlements must come from an explicit
/// provider `context_window` configuration or fresh provider facts while
/// preserving the `k3` wire id.
pub const KIMI_CODE_K3_CONTEXT_WINDOW_TOKENS: u32 = 262_144;
/// Kimi K3 context window on the open platform (`kimi-k3` pay-as-you-go).
/// Verified 2026-07-20 from <https://platform.kimi.ai/docs/guide/kimi-k3-quickstart>
/// (1,048,576 tokens). Max output is a separate fact below and must never be
/// conflated with this window.
pub use codewhale_config::catalog::reviewed::constants::KIMI_K3_CONTEXT_WINDOW_TOKENS;
/// Conservative K3 default generation ceiling. The direct Kimi API defaults
/// `max_completion_tokens` to 131,072, while its documented route maximum is
/// a separate exact-route fact below. Membership and neighboring routes do
/// not inherit that direct-platform maximum.
pub use codewhale_config::catalog::reviewed::constants::KIMI_K3_DEFAULT_MAX_COMPLETION_TOKENS;
/// Documented maximum output for the exact direct Kimi K3 API route.
///
/// Source: <https://platform.kimi.ai/docs/guide/kimi-k3-quickstart> (verified 2026-07-20).
pub const DIRECT_KIMI_K3_MAX_OUTPUT_TOKENS: u32 = 1_048_576;
/// Last-resort compaction trigger when [`context_window_for_model`] returns
/// `None` (an unrecognised model id). v0.8.11 raised this from `50_000` to
/// `102_400` (80% of [`LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS`]) so unknown
/// models inherit the same late-trigger discipline as V4 instead of paying
/// the prefix-cache hit at 5% of the V4 window. Known DeepSeek / Claude
/// models resolve to their own scaled value via
/// `compaction_threshold_for_model` (#664).
pub const DEFAULT_COMPACTION_TOKEN_THRESHOLD: usize = 102_400;
pub fn canonical_official_deepseek_model_id(model: &str) -> Option<&'static str> {
    codewhale_config::catalog::reviewed::official_deepseek_model_id(model)
}

#[cfg(any(test, feature = "test-support"))]
const COMPACTION_THRESHOLD_PERCENT: u32 = 80;

// === Core Message Types ===

// Keep the historical TUI path stable while the production request DTOs are
// owned by `codewhale-core`. Existing transports and response decoders do not
// need a flag day, and headless callers can depend on core directly.
// Some process-test crates include this module privately and exercise only a
// subset of the compatibility surface, so their crate-local dead-import view
// is not evidence that a re-export can be removed.
#[allow(unused_imports)]
pub use codewhale_core::request::{
    CacheControl, ContentBlock, INTERRUPTED_ASSISTANT_CONTEXT_PREFIX, INTERRUPTED_ASSISTANT_ROLE,
    ImageUrlContent, Message, MessageRequest, OpaqueReasoningState, SystemBlock, SystemPrompt,
    Tool, ToolCallKey, ToolCaller,
};
#[allow(unused_imports)]
pub use codewhale_core::role::Role;

/// Container metadata for code-execution style server tools.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ContainerInfo {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// Server-side tool usage counters.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct ServerToolUsage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code_execution_requests: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_search_requests: Option<u32>,
}

/// Response payload for a message request.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageResponse {
    pub id: String,
    pub r#type: String,
    pub role: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<ContainerInfo>,
    pub usage: Usage,
}

/// True when the provider ended generation because its output allowance was
/// exhausted. Providers use several wire spellings for the same condition.
#[must_use]
pub fn is_output_limit_stop_reason(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| {
        let reason = reason
            .trim()
            .strip_prefix("incomplete:")
            .unwrap_or_else(|| reason.trim());
        matches!(
            reason.to_ascii_lowercase().as_str(),
            "length"
                | "max_tokens"
                | "max_output_tokens"
                | "model_length"
                | "output_limit"
                | "max_completion_tokens"
        )
    })
}

/// Stop reason recorded for a response that reported `stop` while its own usage
/// shows it ran to the requested output ceiling (#6889). The Chat Completions
/// spelling, so every existing output-limit path treats it as a `length` stop.
pub const OUTPUT_CEILING_STOP_REASON: &str = "length";

/// True when a Chat Completions `stop` is contradicted by the provider's own
/// usage: the completion tokens it reported equal the output ceiling the
/// request asked for (#6889). A finished answer leaves the allowance unfilled,
/// so this response was cut at the ceiling, typically by reasoning that used
/// the allowance before or during the answer.
///
/// `reported_output_tokens` is `None` when the provider reported no usage; no
/// inference is made then. The match is exact: a provider that does not count
/// reasoning against the cap can report more than the ceiling on a finished
/// answer. Only `stop` is second-guessed. The Anthropic and Responses wires
/// report a limit stop through typed fields of their own, and one Responses
/// route sends no ceiling at all.
#[must_use]
pub fn stop_contradicts_output_ceiling(
    reason: Option<&str>,
    reported_output_tokens: Option<u32>,
    requested_max_tokens: u32,
) -> bool {
    requested_max_tokens > 0
        && reported_output_tokens == Some(requested_max_tokens)
        && reason.is_some_and(|reason| reason.trim().eq_ignore_ascii_case("stop"))
}

/// True when the provider explicitly reported that it did not complete the
/// response. Responses API reasons carry an `incomplete:` prefix so unknown
/// future reasons cannot accidentally be accepted as a finished answer.
#[must_use]
pub fn is_incomplete_stop_reason(reason: Option<&str>) -> bool {
    is_output_limit_stop_reason(reason)
        || reason.is_some_and(|reason| {
            let reason = reason.trim().to_ascii_lowercase();
            reason.starts_with("incomplete:")
                || matches!(
                    reason.as_str(),
                    "content_filter"
                        | "model_context_window_exceeded"
                        | "insufficient_system_resource"
                        | "aborted"
                )
        })
}

#[must_use]
pub fn stop_reason_detail(reason: Option<&str>) -> &str {
    reason
        .map(str::trim)
        .and_then(|reason| reason.strip_prefix("incomplete:").or(Some(reason)))
        .filter(|reason| !reason.is_empty())
        .unwrap_or("unknown")
}

/// Token usage metadata for a response.
#[derive(Debug, Serialize, Deserialize, Clone, Default, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_hit_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_miss_tokens: Option<u32>,
    /// Cache-creation / cache-write tokens (Anthropic `cache_creation_input_tokens`).
    /// Billed at the cache-write rate when the pricing row publishes one (#4318).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_cache_write_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_audio_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_audio_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_usages: Vec<Usage>,
    /// Approximate input tokens spent re-sending prior `reasoning_content`
    /// across user-message boundaries in DeepSeek V4 thinking-mode tool-calling
    /// turns (V4 §5.1.1 "Interleaved Thinking"). Estimated client-side at
    /// ~4 chars/token from the outgoing request body, before the model sees it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_replay_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_tool_use: Option<ServerToolUsage>,
}

/// Map known models to their approximate context window sizes.
///
/// Exact catalog and recognized model facts take precedence. Otherwise an
/// explicit `_Nk` suffix supplies an unverified hint for self-hosted models.
/// Unrecognized DeepSeek family names remain unknown.
#[must_use]
pub fn context_window_for_model(model: &str) -> Option<u32> {
    codewhale_config::catalog::reviewed::intrinsic_model(model)
        .and_then(|row| row.context_window)
        .or_else(|| {
            reviewed_snapshot_model(model).and_then(|id| {
                codewhale_config::catalog::reviewed::intrinsic_model(id)
                    .and_then(|row| row.context_window)
            })
        })
        .or_else(|| explicit_context_window_hint(&model.to_ascii_lowercase()))
}

#[must_use]
pub fn max_output_tokens_for_model(model: &str) -> Option<u32> {
    codewhale_config::catalog::reviewed::intrinsic_model(model)
        .and_then(|row| row.generation_default.or(row.max_output))
        .or_else(|| {
            reviewed_snapshot_model(model).and_then(|id| {
                codewhale_config::catalog::reviewed::intrinsic_model(id)
                    .and_then(|row| row.generation_default.or(row.max_output))
            })
        })
}

/// Catalog-first reasoning capability. `None` means no catalog row and no
/// remaining cited fallback — unknown, not "not a reasoning model".
///
/// Prefer this over [`model_supports_reasoning`] when the caller can surface
/// unknown the way unknown cost already is. The bool wrapper still defaults
/// unknown to `false` for existing stream/UI gates.
#[must_use]
pub fn model_reasoning_capability(model: &str) -> Option<bool> {
    codewhale_config::catalog::reviewed::intrinsic_model(model)
        .and_then(|row| row.reasoning)
        .or_else(|| {
            reviewed_snapshot_model(model).and_then(|id| {
                codewhale_config::catalog::reviewed::intrinsic_model(id)
                    .and_then(|row| row.reasoning)
            })
        })
}

#[must_use]
pub fn model_supports_reasoning(model: &str) -> bool {
    model_reasoning_capability(model).unwrap_or(false)
}

/// Contributor tier of Muse Spark 1.2 is a distinct selectable id with
/// its own wire model (`muse-spark-1.2-contributor`) and cheaper billing in
/// exchange for training-data opt-in. Do not collapse it to the standard tier.
#[must_use]
pub fn effective_muse_wire_id(model: &str) -> &str {
    model
}

#[must_use]
pub fn model_is_openai_reasoning_family(model: &str) -> bool {
    let lower = model.to_ascii_lowercase();
    let reviewed = codewhale_config::catalog::reviewed::bundled_reviewed();
    reviewed.openai_reasoning_ids.contains_key(&lower)
        // Snapshot resolution spans every reviewed family; the resolved
        // target must itself be an OpenAI reasoning row, or a DeepSeek
        // snapshot would be relabelled as one.
        || reviewed_snapshot_model(&lower)
            .is_some_and(|target| reviewed.openai_reasoning_ids.contains_key(target))
}

pub fn is_openai_gpt_56_api_model(model_lower: &str) -> bool {
    codewhale_config::catalog::reviewed::bundled_reviewed()
        .openai_reasoning_ids
        .get(model_lower)
        .is_some_and(|kind| kind == "gpt_56")
}

pub fn has_date_snapshot_suffix(model_lower: &str, prefix: &str) -> bool {
    let Some(rest) = model_lower.strip_prefix(prefix) else {
        return false;
    };
    let bytes = rest.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(idx, byte)| idx == 4 || idx == 7 || byte.is_ascii_digit())
}

/// Resolve a snapshot or variant model id to the reviewed intrinsic row it
/// denotes, without inventing facts for names the catalog does not own.
///
/// Two documented contracts, applied in order:
/// - the authored `snapshot_prefixes` rows (`gpt-5.5-<YYYY-MM-DD>`);
/// - a trailing date stamp (`-YYYY-MM-DD` or `-MMDD`) or an explicit variant
///   marker (`-vision-exp`, `-vision`, `-exp`), accepted only when stripping
///   it leaves the exact id of a reviewed intrinsic row
///   (`deepseek-v4-pro-0813` denotes `deepseek-v4-pro`;
///   `deepseek-v4-flash-vision` denotes `deepseek-v4-flash`).
///   The compact `-YYYYMMDD` shape is deliberately not inferred: the
///   sibling-metadata contract rejects it.
///
/// Every layer is resolved against the bundled reviewed catalog only. An
/// unrecognized name still returns `None`: the unknown stays observable.
fn reviewed_snapshot_model(model: &str) -> Option<&'static str> {
    let reviewed = codewhale_config::catalog::reviewed::bundled_reviewed();
    let via_authored_contract = |candidate: &str| {
        reviewed
            .snapshot_prefixes
            .iter()
            .find_map(|(prefix, target)| {
                has_date_snapshot_suffix(candidate, prefix).then_some(target.as_str())
            })
    };
    let mut candidate = model.to_ascii_lowercase();
    for _ in 0..3 {
        if let Some(target) = via_authored_contract(&candidate) {
            return Some(target);
        }
        let stripped = strip_snapshot_or_variant_suffix(&candidate)?;
        if let Some((key, _)) = reviewed.intrinsic.get_key_value(&stripped) {
            return Some(key.as_str());
        }
        candidate = stripped;
    }
    None
}

/// One snapshot/variant normalization layer, longest marker first. The
/// caller re-resolves the shortened id against the reviewed catalog and
/// never accepts a shortening on its own.
fn strip_snapshot_or_variant_suffix(id: &str) -> Option<String> {
    for marker in ["-vision-exp", "-vision", "-exp"] {
        if let Some(base) = id.strip_suffix(marker)
            && !base.is_empty()
        {
            return Some(base.to_string());
        }
    }
    strip_date_stamp(id)
}

/// Strip one trailing date stamp: `-YYYY-MM-DD` or `-MMDD` (the reviewed
/// snapshot conventions). The compact `-YYYYMMDD` form is deliberately not
/// inferred: the sibling-metadata contract rejects that shape (see
/// `unrecognized_deepseek_models_do_not_inherit_sibling_metadata`).
fn strip_date_stamp(id: &str) -> Option<String> {
    // `-YYYY-MM-DD`: a ten-character tail preceded by its own dash. The split
    // index is a byte offset, so it must land on a char boundary: a multi-byte
    // model id would panic in `split_at` otherwise.
    if id.len() > 11 && id.is_char_boundary(id.len() - 10) {
        let (head, tail) = id.split_at(id.len() - 10);
        if let Some(head) = head.strip_suffix('-')
            && !head.is_empty()
            && has_date_snapshot_suffix(tail, "")
        {
            return Some(head.to_string());
        }
    }
    let (head, tail) = id.rsplit_once('-')?;
    if head.is_empty() || tail.is_empty() {
        return None;
    }
    let digits = tail.as_bytes();
    if digits.len() == 4
        && digits.iter().all(u8::is_ascii_digit)
        && valid_month_day(&digits[0..2], &digits[2..4])
    {
        return Some(head.to_string());
    }
    None
}

/// Validate a two-digit month/day pair (ranges only, no calendar math).
fn valid_month_day(month_digits: &[u8], day_digits: &[u8]) -> bool {
    let two = |bytes: &[u8]| -> Option<u32> {
        (bytes.len() == 2 && bytes.iter().all(u8::is_ascii_digit))
            .then(|| u32::from(bytes[0] - b'0') * 10 + u32::from(bytes[1] - b'0'))
    };
    match (two(month_digits), two(day_digits)) {
        (Some(month), Some(day)) => (1..=12).contains(&month) && (1..=31).contains(&day),
        _ => false,
    }
}

/// The context window a model name's `_Nk` suffix advertises, when the
/// catalog does not already describe the model (#5441).
///
/// Exposed separately from `explicit_context_window_hint` because the
/// honesty surfaces need to know *whether the number they are holding came
/// from the name* — a naming convention the serving engine may ignore is not
/// a fact about the route, and every surface that shows such a window must
/// mark it unverified.
#[must_use]
pub fn name_suffix_context_window_hint(model: &str) -> Option<u32> {
    if codewhale_config::catalog::reviewed::intrinsic_model(model)
        .and_then(|row| row.context_window)
        .is_some()
        || reviewed_snapshot_model(model).is_some()
    {
        return None;
    }
    explicit_context_window_hint(&model.to_ascii_lowercase())
}

/// Parse an explicit `_Nk` context-window hint from a model name (vendor
/// agnostic). Returns the window in tokens for `N` in `8..=1024`.
fn explicit_context_window_hint(model_lower: &str) -> Option<u32> {
    let bytes = model_lower.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'k' {
                continue;
            }

            let before_ok = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
            let after_ok = i + 1 >= bytes.len() || !bytes[i + 1].is_ascii_alphanumeric();
            if !before_ok || !after_ok {
                continue;
            }

            if let Ok(kilo_tokens) = model_lower[start..i].parse::<u32>()
                && (8..=1024).contains(&kilo_tokens)
            {
                return Some(kilo_tokens.saturating_mul(1000));
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Derive a compaction token threshold from model context and a caller-supplied
/// percentage.
#[must_use]
#[cfg(any(test, feature = "test-support"))]
pub fn compaction_threshold_for_model_at_percent(model: &str, percent: f64) -> usize {
    let Some(window) = context_window_for_model(model) else {
        return DEFAULT_COMPACTION_TOKEN_THRESHOLD;
    };

    let percent = percent.clamp(10.0, 100.0);
    let threshold = (f64::from(window) * percent / 100.0).round();
    let threshold = if threshold.is_finite() && threshold > 0.0 {
        threshold as u64
    } else {
        u64::from(window) * u64::from(COMPACTION_THRESHOLD_PERCENT) / 100
    };
    usize::try_from(threshold).unwrap_or(DEFAULT_COMPACTION_TOKEN_THRESHOLD)
}

/// Whether auto-compaction should be enabled when the user did not explicitly
/// configure it. Known model windows default automatic continuity on; an
/// explicit `auto_compact = false` remains authoritative at the call sites.
#[must_use]
#[cfg(test)]
pub fn auto_compact_default_for_model(model: &str) -> bool {
    context_window_for_model(model).is_some()
}

// === Streaming Structures ===

/// Decode the `redacted_thinking` start-event payload, tolerating the
/// missing-key shape (`#[serde(default)]`) and an explicit `"data": null`
/// from nonconforming gateways. Both decode to the empty payload that the
/// capture site warns about, instead of failing the whole SSE event.
fn deserialize_redacted_data<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
/// Streaming event types for SSE responses.
pub enum StreamEvent {
    /// Local pre-stream receipt: the provider request was sent with a reduced
    /// tool surface. This is not provider SSE and must not count as content.
    #[serde(rename = "tool_projection_warning")]
    ToolProjectionWarning {
        provider: String,
        omitted_tool_names: Vec<String>,
        omitted_tool_count: usize,
    },
    #[serde(rename = "message_start")]
    MessageStart { message: MessageResponse },
    #[serde(rename = "content_block_start")]
    ContentBlockStart {
        index: u32,
        content_block: ContentBlockStart,
    },
    #[serde(rename = "content_block_delta")]
    ContentBlockDelta { index: u32, delta: Delta },
    #[serde(rename = "content_block_stop")]
    ContentBlockStop { index: u32 },
    #[serde(rename = "message_delta")]
    MessageDelta {
        delta: MessageDelta,
        usage: Option<Usage>,
    },
    #[serde(rename = "message_stop")]
    MessageStop,
    #[serde(rename = "ping")]
    Ping,
    /// Anthropic SSE error event (#3014).
    #[serde(rename = "error")]
    Error { error: serde_json::Value },
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
/// Content block types used in streaming starts.
pub enum ContentBlockStart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "thinking")]
    Thinking { thinking: String },
    /// Anthropic `redacted_thinking`: the provider withheld the whole block
    /// for safety and replaced it with an encrypted payload that arrives
    /// complete on the start event (no deltas). It must be captured and
    /// replayed byte-exact — the API decrypts it to restore the turn's
    /// reasoning, and dropping it makes the next tool-loop request fail with
    /// "Expected `thinking` or `redacted_thinking`, but found `tool_use`".
    #[serde(rename = "redacted_thinking")]
    RedactedThinking {
        /// Tolerant of both a missing and an explicit-null `data`
        /// (`deserialize_redacted_data` + default): a contract-violating
        /// start event still decodes (empty payload + capture warn) instead
        /// of failing the whole event and silently losing the turn. Note
        /// that `#[serde(default)]` alone only covers the missing key — an
        /// explicit `"data": null` would still fail `String` decoding.
        #[serde(default, deserialize_with = "deserialize_redacted_data")]
        data: String,
    },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value, // usually empty or partial
        #[serde(skip_serializing_if = "Option::is_none")]
        caller: Option<ToolCaller>,
        /// Google thought signature, when the first streaming chunk of this
        /// tool call carried `extra_content.google.thought_signature`.
        #[serde(skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    #[serde(rename = "server_tool_use")]
    ServerToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

// Variant names match legacy streaming spec, suppressing style warning
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type")]
/// Delta events emitted during streaming responses.
pub enum Delta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
    /// Anthropic signed-thinking signature delta (#3014); arrives at the end
    /// of a thinking block on the native Messages stream.
    #[serde(rename = "signature_delta")]
    SignatureDelta { signature: String },
    /// Opaque Responses reasoning continuity, attached only when the provider
    /// returns an encrypted item on the exact originating route.
    #[serde(rename = "reasoning_state_delta")]
    ReasoningStateDelta { state: OpaqueReasoningState },
}

#[derive(Debug, Deserialize, Clone)]
/// Delta payload for message-level updates.
pub struct MessageDelta {
    pub stop_reason: Option<String>,
    pub stop_sequence: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::TypeId;

    #[test]
    fn redacted_thinking_stream_start_decodes_with_its_payload() {
        // A safeguard-triggered redacted block arrives complete on the start
        // event. Before the variant existed this line failed the whole SSE
        // event decode and lost the turn.
        let block: ContentBlockStart = serde_json::from_value(serde_json::json!({
            "type": "redacted_thinking",
            "data": "ENC_REDACTED_PAYLOAD",
        }))
        .expect("redacted_thinking must decode");
        match block {
            ContentBlockStart::RedactedThinking { data } => {
                assert_eq!(data, "ENC_REDACTED_PAYLOAD");
            }
            other => panic!("expected RedactedThinking, got {other:?}"),
        }
    }

    #[test]
    fn redacted_thinking_start_tolerates_an_explicit_null_payload() {
        // `#[serde(default)]` covers a missing key only; a nonconforming
        // gateway sending `"data": null` must still decode to the empty
        // payload (capture warn downstream) instead of failing the whole
        // SSE event and silently losing the turn's redacted block.
        let block: ContentBlockStart = serde_json::from_value(serde_json::json!({
            "type": "redacted_thinking",
            "data": null,
        }))
        .expect("an explicit null payload must not fail the event decode");
        match block {
            ContentBlockStart::RedactedThinking { data } => {
                assert_eq!(data, "");
            }
            other => panic!("expected RedactedThinking, got {other:?}"),
        }
    }

    /// #6032: `model_supports_reasoning` consults the catalog before its
    /// hand-maintained pile, so a literal arm that duplicates a catalog row is
    /// unreachable. These 27 arms were exactly that and were deleted. They must
    /// keep answering `true` from the catalog *alone* — if a row is ever
    /// dropped, this fails loudly here rather than silently reverting them to
    /// "reasoning not expected", which leaks their `reasoning_content` into
    /// ordinary prose (#6044).
    #[test]
    fn unknown_reasoning_capability_is_observable() {
        assert_eq!(
            model_reasoning_capability("not-a-real-model-xyz"),
            None,
            "unknown must not collapse to false at this layer"
        );
        assert!(
            !model_supports_reasoning("not-a-real-model-xyz"),
            "legacy bool wrapper still defaults unknown to false"
        );
        assert_eq!(model_reasoning_capability("kimi-for-coding"), Some(true));
    }

    #[test]
    fn catalog_alone_covers_the_models_removed_from_the_heuristic_pile() {
        let removed = [
            "claude-opus-4-8",
            "claude-opus-5",
            "claude-sonnet-4-6",
            "claude-sonnet-5",
            "claude-fable-5",
            "gpt-5-codex",
            "gpt-5.3-codex",
            "trinity-mini",
            "trinity-large-thinking",
            "moonshotai/kimi-k2.7-code",
            "kimi-k2.7-code",
            "minimax/minimax-m3",
            "minimax/minimax-m2.7",
            "minimax-m2.7",
            "qwen/qwen3.6-flash",
            "mimo-v2.5",
            "mimo-v2.5-pro",
            "mimo-v2.5-pro-ultraspeed",
            "z-ai/glm-5.2",
            "z-ai/glm-5.3",
            "z-ai/glm-5.3-flash",
            "glm-5.2",
            "glm-5.3",
            "glm-5.3-flash",
            "muse-spark-1.1",
            "muse-spark-1.2",
            "muse-spark-1.2-contributor",
            // Part 2: cited qwen3.x / Kimi coding-route arms moved into the
            // bundled catalog (Alibaba Cloud Model Studio deep-thinking docs;
            // #3016 plus the 2026 K2.7 update).
            "kimi-for-coding",
            "kimi-for-coding-highspeed",
            "kimi-k2.5",
            "kimi-k2.6",
            "qwen3.5-flash",
            "qwen3.5-plus",
            "qwen3.6-flash",
            "qwen3.6-plus",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3.8-flash",
            "qwen3.8-max",
            "qwen3.8-max-preview",
        ];
        for model in removed {
            assert_eq!(
                codewhale_config::catalog::reviewed::intrinsic_model(model)
                    .and_then(|row| row.reasoning),
                Some(true),
                "{model} no longer has a catalog row, but its heuristic arm was \
                 deleted in #6032 — restore the row, or put the arm back"
            );
            assert!(
                model_supports_reasoning(model),
                "{model} must still classify as reasoning-capable"
            );
        }
    }

    /// #6032 part 3: the eight literal arms deleted in favor of the bundled
    /// Models.dev snapshot. Each id's `reasoning` bool is sourced there (all
    /// `true`), so they must keep answering without a heuristic arm — if a
    /// row is dropped from the asset, this fails loudly instead of silently
    /// reverting them to "reasoning not expected" (#6044).
    #[test]
    fn models_dev_bundled_asset_covers_the_arms_deleted_from_the_heuristic_pile() {
        let catalog = codewhale_config::catalog::bundled_models_dev_catalog();
        let deleted = [
            "qwen/qwen3.8-flash",
            "qwen/qwen3.6-35b-a3b",
            "qwen/qwen3.6-plus",
            "qwen/qwen3.7-plus",
            "glm-5.1",
            "grok-4.6",
            "grok-4.5",
            "grok-4.3",
        ];
        for model in deleted {
            assert_eq!(
                catalog.reasoning_support(model),
                Some(true),
                "{model} lost its bundled Models.dev row — restore its reviewed \
                 source row (#6032)"
            );
            assert!(
                model_supports_reasoning(model),
                "{model} must still classify as reasoning-capable"
            );
        }
    }

    /// #6032 coverage guard: exact reviewed facts and documented snapshot
    /// contracts classify known models. Mistral's documented latest IDs now
    /// have explicit reviewed reasoning facts; their family names and private
    /// model names do not inherit those facts through a prefix heuristic.
    #[test]
    fn remaining_reasoning_heuristic_arms_are_unsourced_in_models_dev_bundled_asset() {
        // Known facts are explicit data. Family resemblance is not a sourced
        // capability, while the documented date-snapshot contract remains exact.
        for model in [
            "kimi-k2.9",
            "magistral-medium",
            "claude-private-server",
            "gpt-5.6-private",
        ] {
            assert_eq!(model_reasoning_capability(model), None, "{model}");
        }
        for model in [
            "mistral-medium-latest",
            "mistral-small-latest",
            "gpt-5.5-2026-06-01",
            "gpt-5.1-codex-max",
            "codex-gpt-5.5-preview",
            "chatgpt-gpt-5.5",
        ] {
            assert_eq!(model_reasoning_capability(model), Some(true), "{model}");
        }
    }

    #[test]
    fn output_limit_stop_reason_accepts_provider_aliases_only() {
        for reason in [
            "length",
            "max_tokens",
            "max_output_tokens",
            " MAX_TOKENS ",
            "incomplete:max_output_tokens",
        ] {
            assert!(is_output_limit_stop_reason(Some(reason)), "{reason}");
        }
        for reason in [None, Some("end_turn"), Some("tool_use"), Some("")] {
            assert!(!is_output_limit_stop_reason(reason), "{reason:?}");
        }
    }

    #[test]
    fn stop_at_the_requested_ceiling_is_an_output_limit_stop() {
        // #6889: 16000 requested, 16000 reported, `finish_reason: stop`.
        assert!(stop_contradicts_output_ceiling(
            Some("stop"),
            Some(16_000),
            16_000
        ));
        assert!(stop_contradicts_output_ceiling(
            Some(" STOP "),
            Some(50),
            50
        ));
        assert!(is_output_limit_stop_reason(Some(
            OUTPUT_CEILING_STOP_REASON
        )));
        assert!(is_incomplete_stop_reason(Some(OUTPUT_CEILING_STOP_REASON)));

        // A finished answer leaves the allowance unfilled.
        assert!(!stop_contradicts_output_ceiling(
            Some("stop"),
            Some(15_999),
            16_000
        ));
        // Reasoning the provider does not count against the cap.
        assert!(!stop_contradicts_output_ceiling(
            Some("stop"),
            Some(20_000),
            16_000
        ));
        // No usage reported: nothing to compare.
        assert!(!stop_contradicts_output_ceiling(Some("stop"), None, 16_000));
        // No ceiling requested.
        assert!(!stop_contradicts_output_ceiling(Some("stop"), Some(0), 0));
        // Other wires and other stops keep their own reporting.
        for reason in [None, Some("end_turn"), Some("tool_calls"), Some("length")] {
            assert!(
                !stop_contradicts_output_ceiling(reason, Some(16_000), 16_000),
                "{reason:?}"
            );
        }
    }

    #[test]
    fn incomplete_stop_reason_never_accepts_unknown_responses_failures() {
        assert!(is_incomplete_stop_reason(Some("incomplete:content_filter")));
        assert!(is_incomplete_stop_reason(Some("content_filter")));
        assert!(is_incomplete_stop_reason(Some(
            "model_context_window_exceeded"
        )));
        assert!(is_incomplete_stop_reason(Some("max_tokens")));
        assert!(!is_incomplete_stop_reason(Some("end_turn")));
        assert_eq!(
            stop_reason_detail(Some("incomplete:content_filter")),
            "content_filter"
        );
    }

    #[test]
    fn historical_tui_request_path_is_the_core_request_type() {
        assert_eq!(
            TypeId::of::<MessageRequest>(),
            TypeId::of::<codewhale_core::request::MessageRequest>()
        );

        let via_tui_path = MessageRequest {
            model: "model".to_string(),
            messages: vec![],
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
        };
        let via_core_path: codewhale_core::request::MessageRequest = via_tui_path.clone();
        assert_eq!(
            serde_json::to_vec(&via_tui_path).expect("serialize TUI path"),
            serde_json::to_vec(&via_core_path).expect("serialize core path")
        );
    }

    #[test]
    fn interrupted_assistant_role_round_trips_as_distinct_session_item() {
        let message = Message {
            role: Role::InterruptedAssistant,
            content: vec![ContentBlock::Text {
                text: "partial output".to_string(),
                cache_control: None,
            }],
        };
        let encoded = serde_json::to_string(&message).expect("message should serialize");
        let decoded: Message = serde_json::from_str(&encoded).expect("message should deserialize");
        assert_eq!(decoded, message);
        assert_ne!(decoded.role, "assistant");
    }

    #[test]
    fn unrecognized_deepseek_models_do_not_inherit_sibling_metadata() {
        for model in [
            "deepseek-v4-flash-20260423",
            "deepseek-v4-pro-20260423",
            "deepseek-coder",
            "deepseek-v3.2-0324",
            "deepseek-v4.1-flash-expires-on-0910",
        ] {
            assert_eq!(context_window_for_model(model), None, "{model}");
        }
        assert!(!model_supports_reasoning(
            "deepseek-v4.1-flash-expires-on-0910"
        ));
    }

    #[test]
    fn deepseek_v4_models_map_to_1m_context_window() {
        assert_eq!(
            context_window_for_model("deepseek-v4-pro"),
            Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS)
        );
        assert_eq!(
            context_window_for_model("deepseek-v4-flash"),
            Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS)
        );
        assert_eq!(
            context_window_for_model("deepseek-ai/deepseek-v4-pro"),
            Some(DEEPSEEK_V4_CONTEXT_WINDOW_TOKENS)
        );
    }

    #[test]
    fn deepseek_v4_output_caps_require_exact_catalog_metadata() {
        for model in [
            "deepseek-v4.1-flash-expires-on-0910",
            "deepseek-v4.1-flash",
            "deepseek-v4.1-pro",
            "vendor/deepseek-v4.1-flash",
            "deepseek-v4-flash-vendor",
        ] {
            assert!(
                codewhale_config::catalog::reviewed::intrinsic_model(model).is_none(),
                "{model}"
            );
            assert_eq!(max_output_tokens_for_model(model), None, "{model}");
        }

        for model in [
            "deepseek-v4-flash",
            "deepseek-v4-pro",
            "deepseek-v4-flash-vision-exp",
            "DEEPSEEK-V4-FLASH",
        ] {
            assert_eq!(max_output_tokens_for_model(model), Some(384_000), "{model}");
        }
    }

    #[test]
    fn recent_openrouter_large_models_have_static_windows() {
        for (model, expected_window) in [
            ("arcee-ai/trinity-large-thinking", 262_144),
            ("trinity-large-thinking", 262_144),
            (concat!("qwen/", "qwen3.8-flash"), 1_000_000),
            (concat!("qwen/", "qwen3.6-flash"), 1_000_000),
            (concat!("qwen/", "qwen3.6-35b-a3b"), 262_144),
            (concat!("qwen/", "qwen3.6-max-preview"), 262_144),
            (concat!("qwen/", "qwen3.6-plus"), 1_000_000),
            (concat!("xiaomi/", "mimo-v2.5-pro"), 1_000_000),
            ("mimo-v2.5-pro", 1_000_000),
            ("mimo-v2.5-pro-ultraspeed", 1_000_000),
            ("mimo-v2.5", 1_000_000),
            ("minimax/minimax-m3", 1_000_000),
            ("minimax/minimax-m2.7", 204_800),
            ("moonshotai/kimi-k2.7-code", 262_144),
            ("moonshotai/kimi-k2.6", 262_144),
            ("google/gemma-4-31b-it", 262_144),
            ("z-ai/glm-5.1", 202_752),
            ("z-ai/glm-5.2", 1_000_000),
            ("z-ai/glm-5.3", 1_000_000),
            ("z-ai/glm-5.3-flash", 1_000_000),
        ] {
            assert_eq!(context_window_for_model(model), Some(expected_window));
            assert!(model_supports_reasoning(model));
        }
    }

    #[test]
    fn openai_api_and_codex_models_have_verified_context_metadata() {
        for model in ["gpt-5.6", "gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna"] {
            assert_eq!(context_window_for_model(model), Some(1_050_000));
            assert_eq!(max_output_tokens_for_model(model), Some(128_000));
            assert!(model_supports_reasoning(model));
            assert_eq!(
                compaction_threshold_for_model_at_percent(model, 80.0),
                840_000
            );
        }

        for model in [
            "gpt-5.5",
            "gpt-5.5-pro",
            "gpt-5.5-2026-04-23",
            "gpt-5.5-pro-2026-04-23",
        ] {
            assert_eq!(context_window_for_model(model), Some(1_050_000));
            assert_eq!(max_output_tokens_for_model(model), Some(128_000));
            assert!(model_supports_reasoning(model));
            assert_eq!(
                compaction_threshold_for_model_at_percent(model, 80.0),
                840_000
            );
        }

        for model in [
            "gpt-5-codex",
            "gpt-5.1-codex",
            "gpt-5.1-codex-mini",
            "gpt-5.1-codex-max",
            "gpt-5.2-codex",
            "gpt-5.3-codex",
            "codex-gpt-5.5",
            "chatgpt-gpt-5.5",
            "gpt-5.5-codex",
            "gpt-5.5-codex-preview",
        ] {
            assert_eq!(context_window_for_model(model), Some(400_000));
            assert_eq!(max_output_tokens_for_model(model), Some(128_000));
            assert!(model_supports_reasoning(model));
            assert_eq!(
                compaction_threshold_for_model_at_percent(model, 80.0),
                320_000
            );
        }

        assert_eq!(context_window_for_model("gpt-5.5-nano"), None);
        assert_eq!(max_output_tokens_for_model("gpt-5.5-nano"), None);
        assert!(!model_supports_reasoning("gpt-5.5-nano"));
    }

    #[test]
    fn anthropic_stepfun_and_sakana_limits_match_2026_07_09_audit() {
        // Sonnet 4.6 output cap raised 64K -> 128K per
        // https://platform.claude.com/docs/en/about-claude/models/overview;
        // Haiku stays at 64K.
        assert_eq!(
            max_output_tokens_for_model("claude-sonnet-4-6"),
            Some(128_000)
        );
        assert_eq!(
            max_output_tokens_for_model("claude-haiku-4-5"),
            Some(64_000)
        );
        // step-3.7-flash max output is third-party sourced (models.dev +
        // Artificial Analysis; the official StepFun page is silent):
        // https://models.dev/models/stepfun/step-3.7-flash/
        assert_eq!(max_output_tokens_for_model("step-3.7-flash"), Some(256_000));
        assert_eq!(context_window_for_model("step-3.7-flash"), Some(256_000));
        // fugu-ultra limits are third-party sourced (Requesty; Sakana's own
        // >272K price tier at https://console.sakana.ai/pricing confirms the
        // context window exceeds 272K).
        for model in ["fugu-ultra", "fugu-ultra-20260615"] {
            assert_eq!(context_window_for_model(model), Some(1_000_000), "{model}");
            assert_eq!(max_output_tokens_for_model(model), Some(131_000), "{model}");
        }
    }

    #[test]
    fn stepfun_current_coding_models_have_verified_metadata() {
        assert_eq!(context_window_for_model("step-5-preview"), Some(1_000_000));
        assert_eq!(
            max_output_tokens_for_model("step-5-preview"),
            Some(1_000_000)
        );
        for model in [
            "step-5-preview",
            "step-3.7-flash",
            "step-3.5-flash",
            "step-3.5-flash-2603",
        ] {
            assert!(model_supports_reasoning(model), "{model}");
        }
        for model in ["step-3.5-flash", "step-3.5-flash-2603"] {
            assert_eq!(context_window_for_model(model), Some(256_000));
            assert_eq!(max_output_tokens_for_model(model), None);
        }
    }

    #[test]
    fn claude_fable_5_and_sonnet_5_have_verified_metadata() {
        // 1M context / 128K output per
        // https://platform.claude.com/docs/en/about-claude/pricing (2026-07-09).
        for model in ["claude-fable-5", "claude-sonnet-5"] {
            assert_eq!(context_window_for_model(model), Some(1_000_000), "{model}");
            assert_eq!(max_output_tokens_for_model(model), Some(128_000), "{model}");
            assert!(model_supports_reasoning(model), "{model}");
        }
    }

    #[test]
    fn claude_opus_5_has_verified_metadata() {
        // 1M context / 128K output, adaptive thinking, per
        // https://platform.claude.com/docs/en/about-claude/models/overview
        // (2026-08-17).
        assert_eq!(context_window_for_model("claude-opus-5"), Some(1_000_000));
        assert_eq!(max_output_tokens_for_model("claude-opus-5"), Some(128_000));
        assert!(model_supports_reasoning("claude-opus-5"));
    }

    #[test]
    fn kimi_k2_7_code_highspeed_shares_the_k2_7_code_limits() {
        // https://platform.kimi.ai/docs/pricing/chat-k27-code (2026-08-17):
        // same model as kimi-k2.7-code, 262,144 context.
        for model in [
            "kimi-k2.7-code-highspeed",
            "moonshotai/kimi-k2.7-code-highspeed",
        ] {
            assert_eq!(context_window_for_model(model), Some(262_144), "{model}");
            assert_eq!(max_output_tokens_for_model(model), Some(32_768), "{model}");
            assert!(model_supports_reasoning(model), "{model}");
        }
    }

    #[test]
    fn gemini_api_models_have_documented_token_limits() {
        // Every current Gemini API text model page lists 1,048,576 input /
        // 65,536 output (verified 2026-08-17, see
        // `known_context_window_for_model`).
        for model in [
            "gemini-3.7-flash",
            "gemini-3.6-flash",
            "gemini-3.5-flash",
            "gemini-3.5-flash-lite",
            "gemini-3.1-pro-preview",
            "gemini-3-pro-preview",
            "gemini-2.5-pro",
            "gemini-2.5-flash",
        ] {
            assert_eq!(context_window_for_model(model), Some(1_048_576), "{model}");
            assert_eq!(max_output_tokens_for_model(model), Some(65_536), "{model}");
        }
    }

    #[test]
    fn muse_spark_has_verified_context_and_reasoning_metadata() {
        assert_eq!(context_window_for_model("muse-spark-1.1"), Some(1_000_000));
        assert_eq!(max_output_tokens_for_model("muse-spark-1.1"), Some(32_000));
        assert!(model_supports_reasoning("muse-spark-1.1"));
        // Muse Spark 1.2 standard: 1M context, $1.25/$4.25 + $0.15 cache (Artificial Analysis).
        assert_eq!(context_window_for_model("muse-spark-1.2"), Some(1_000_000));
        assert_eq!(max_output_tokens_for_model("muse-spark-1.2"), Some(32_000));
        assert!(model_supports_reasoning("muse-spark-1.2"));
        // Contributor tier: same model/limits, ~12×/21× cheaper in exchange for training-data opt-in.
        assert_eq!(
            context_window_for_model("muse-spark-1.2-contributor"),
            Some(1_000_000)
        );
        assert_eq!(
            max_output_tokens_for_model("muse-spark-1.2-contributor"),
            Some(32_000)
        );
        assert!(model_supports_reasoning("muse-spark-1.2-contributor"));
    }

    #[test]
    fn modelstudio_qwen38_max_is_1m_context_not_128k() {
        // Owner Token Plan console + curated catalog (2026-08-03). The 128K
        // figure is max output, not the window — never collapse them.
        for model in ["qwen3.8-max", "qwen3.8-max-preview"] {
            assert_eq!(context_window_for_model(model), Some(1_000_000), "{model}");
            assert_eq!(max_output_tokens_for_model(model), Some(131_072), "{model}");
        }
    }

    #[test]
    fn openrouter_qwen38_flash_is_1m_context_with_128k_output() {
        // models.dev OpenRouter listing 2026-08-26: 1,000,000 / 131,072.
        // Both the namespaced wire id and the bare short id must resolve.
        for model in ["qwen/qwen3.8-flash", "qwen3.8-flash"] {
            assert_eq!(context_window_for_model(model), Some(1_000_000), "{model}");
            assert_eq!(max_output_tokens_for_model(model), Some(131_072), "{model}");
            assert!(model_supports_reasoning(model), "{model}");
        }
    }

    #[test]
    fn modelstudio_bare_qwen_models_support_reasoning() {
        // Model Studio's deep-thinking docs: every qwen3.x family the Token /
        // Coding Plan catalogs carry is hybrid-thinking (reasoning_content on
        // the OpenAI dialect, thinking blocks on the Anthropic dialect).
        for model in [
            "qwen3.8-max",
            "qwen3.8-max-preview",
            "qwen3.7-max",
            "qwen3.7-plus",
            "qwen3.6-plus",
            "qwen3.6-flash",
            "qwen3.5-plus",
            "qwen3.5-flash",
        ] {
            assert!(model_supports_reasoning(model), "{model}");
        }
    }

    #[test]
    fn model_metadata_catalog_override_flows_through_models_chokepoint() {
        let private=codewhale_config::models_dev::ModelsDevCatalog::parse_json(r#"{"providers":{"private":{"models":{"catalog-only-model":{"limit":{"context":777000,"output":55000},"reasoning":true}}}}}"#).unwrap();
        assert!(
            codewhale_config::catalog::reviewed::intrinsic_model_in(&private, "catalog-only-model")
                .is_none()
        );
        assert_eq!(context_window_for_model("catalog-only-model"), None);
        assert_eq!(max_output_tokens_for_model("catalog-only-model"), None);
        assert_eq!(model_reasoning_capability("catalog-only-model"), None);
    }

    #[test]
    fn moonshot_native_kimi_ids_support_reasoning_including_coding_route() {
        // #3016: bare Moonshot ids (no moonshotai/ prefix) emit
        // reasoning_content; kimi-for-coding currently rides the K2.7 Code path.
        assert!(model_supports_reasoning("kimi-k2.7-code"));
        assert!(model_supports_reasoning("kimi-k2.6"));
        assert!(model_supports_reasoning("kimi-for-coding"));
        assert!(model_supports_reasoning("kimi-for-coding-highspeed"));
        assert!(model_supports_reasoning("kimi-k2.5"));
    }

    #[test]
    fn xai_grok_models_have_static_context_metadata() {
        for (model, expected_window, supports_reasoning) in [
            ("grok-4.6", 500_000, true),
            ("grok-4.5", 500_000, true),
            ("grok-4.3", 1_000_000, true),
            ("grok-build", 512_000, true),
            ("grok-composer-2.5-fast", 200_000, false),
            ("grok-4.20-0309-reasoning", 2_000_000, true),
            ("grok-4.20-0309-non-reasoning", 2_000_000, false),
        ] {
            assert_eq!(context_window_for_model(model), Some(expected_window));
            assert_eq!(max_output_tokens_for_model(model), None);
            assert_eq!(model_supports_reasoning(model), supports_reasoning);
        }
    }

    #[test]
    fn arcee_direct_models_preserve_verified_capabilities_only() {
        assert_eq!(
            context_window_for_model("trinity-large-preview"),
            Some(262_144)
        );
        assert!(!model_supports_reasoning("trinity-large-preview"));
        assert_eq!(context_window_for_model("trinity-mini"), Some(128_000));
        assert_eq!(max_output_tokens_for_model("trinity-mini"), None);
        assert!(model_supports_reasoning("trinity-mini"));
    }

    #[test]
    fn qwen37_plus_and_inkling_reasoning_do_not_invent_limits() {
        for model in ["qwen/qwen3.7-plus", "thinkingmachines/inkling"] {
            assert_eq!(context_window_for_model(model), None, "{model}");
            assert_eq!(max_output_tokens_for_model(model), None, "{model}");
            assert!(model_supports_reasoning(model), "{model}");
        }
    }

    #[test]
    fn recent_openrouter_large_models_have_known_output_caps() {
        assert_eq!(
            max_output_tokens_for_model("arcee-ai/trinity-large-thinking"),
            Some(262_144)
        );
        assert_eq!(
            max_output_tokens_for_model("trinity-large-thinking"),
            Some(262_144)
        );
        assert_eq!(
            max_output_tokens_for_model(concat!("qwen/", "qwen3.8-flash")),
            Some(131_072)
        );
        assert_eq!(
            max_output_tokens_for_model(concat!("qwen/", "qwen3.6-flash")),
            Some(65_536)
        );
        assert_eq!(
            max_output_tokens_for_model(concat!("qwen/", "qwen3.6-max-preview")),
            Some(65_536)
        );
        assert_eq!(
            max_output_tokens_for_model(concat!("qwen/", "qwen3.6-plus")),
            Some(65_536)
        );
        assert_eq!(
            max_output_tokens_for_model(concat!("xiaomi/", "mimo-v2.5-pro")),
            Some(131_072)
        );
        assert_eq!(max_output_tokens_for_model("mimo-v2.5-pro"), Some(131_072));
        assert_eq!(
            max_output_tokens_for_model("mimo-v2.5-pro-ultraspeed"),
            Some(131_072)
        );
        assert_eq!(max_output_tokens_for_model("mimo-v2.5"), Some(131_072));
        assert_eq!(
            max_output_tokens_for_model("minimax/minimax-m3"),
            Some(524_288)
        );
        assert_eq!(max_output_tokens_for_model("z-ai/glm-5.1"), Some(131_072));
        assert_eq!(max_output_tokens_for_model("z-ai/glm-5.2"), Some(131_072));
        assert_eq!(max_output_tokens_for_model("z-ai/glm-5.3"), Some(131_072));
        assert_eq!(
            max_output_tokens_for_model("z-ai/glm-5-turbo"),
            Some(131_072)
        );
        assert_eq!(max_output_tokens_for_model("glm-5-turbo"), Some(131_072));
    }

    #[test]
    fn k3_route_ids_use_verified_contracts_not_legacy_128k() {
        // Open-platform K3 carries the verified 1M contract.
        assert_eq!(context_window_for_model("kimi-k3"), Some(1_048_576));
        assert_eq!(
            context_window_for_model("opencode-go/kimi-k3"),
            Some(1_048_576)
        );
        // Bare `k3` (Kimi Code membership) is plan-tier dependent, so it
        // keeps the documented safe floor — and must never fall through to
        // the 128K legacy default.
        assert_eq!(context_window_for_model("k3"), Some(262_144));
        assert_eq!(context_window_for_model("k3-256k"), Some(262_144));
        assert_eq!(max_output_tokens_for_model("k3"), Some(131_072));
        assert_eq!(max_output_tokens_for_model("k3-256k"), Some(131_072));
        assert_eq!(max_output_tokens_for_model("kimi-k3"), Some(131_072));
        // Never project max output as the context window.
        assert_ne!(
            context_window_for_model("k3"),
            max_output_tokens_for_model("k3")
        );
        assert_ne!(
            context_window_for_model("kimi-k3"),
            max_output_tokens_for_model("kimi-k3")
        );
    }

    #[test]
    fn kimi_code_membership_ids_mirror_their_family_facts() {
        // The high-speed membership id rides the kimi-for-coding family
        // context fact (256K) and reasoning support via the same `kimi-`
        // native-id rule as `kimi-for-coding`. No client-side output ceiling
        // is claimed for the membership ids — the membership catalog is the
        // source of truth, so the generic lookup returns None.
        assert_eq!(
            context_window_for_model("kimi-for-coding-highspeed"),
            Some(262_144)
        );
        assert_eq!(
            max_output_tokens_for_model("kimi-for-coding-highspeed"),
            None
        );
        assert_eq!(max_output_tokens_for_model("kimi-for-coding"), None);
        assert!(model_supports_reasoning("kimi-for-coding-highspeed"));
    }

    #[test]
    fn bare_provider_model_ids_mirror_vendor_prefixed_rows() {
        // Direct-provider routes (Moonshot, MiniMax, Z.ai) serve bare model
        // ids without the OpenRouter vendor prefix; both spellings must
        // resolve identical metadata (#1310 ride-along on #3023).
        for (model, expected_window) in [
            ("kimi-k3", 1_048_576),
            ("kimi-k2.7-code", 262_144),
            ("kimi-k2.6", 262_144),
            ("minimax-m3", 1_000_000),
            ("minimax-m2.7", 204_800),
            ("minimax-m2.5-highspeed", 204_800),
            ("minimax-m2", 204_800),
            ("glm-5.1", 202_752),
            ("glm-5.2", 1_000_000),
            // Inherited from glm-5.2 pending official Z.ai release metadata.
            ("glm-5.3", 1_000_000),
            ("glm-5.3-flash", 1_000_000),
            ("glm-5-turbo", 202_752),
        ] {
            assert_eq!(context_window_for_model(model), Some(expected_window));
            assert!(model_supports_reasoning(model));
        }
        assert_eq!(context_window_for_model("kimi-for-coding"), Some(262_144));
        assert!(model_supports_reasoning("kimi-for-coding"));
        assert_eq!(context_window_for_model("glm-5v-turbo"), Some(202_752));
        assert!(!model_supports_reasoning("glm-5v-turbo"));
        // GLM-5-Turbo is a fast text sibling (distinct from the glm-5v-turbo
        // vision model): same compact window as 5.1 but reasoning-capable.
        assert_eq!(context_window_for_model("z-ai/glm-5-turbo"), Some(202_752));
        assert!(model_supports_reasoning("z-ai/glm-5-turbo"));
        assert_eq!(
            codewhale_config::catalog::reviewed::intrinsic_model("kimi-k2.7-code")
                .and_then(|row| row.generation_default.or(row.max_output)),
            Some(32_768)
        );
        assert_eq!(max_output_tokens_for_model("kimi-k2.7-code"), Some(32_768));
        assert_eq!(max_output_tokens_for_model("kimi-k2.6"), Some(32_768));
        assert_eq!(max_output_tokens_for_model("kimi-for-coding"), None);
        assert_eq!(max_output_tokens_for_model("kimi-k3"), Some(131_072));
        assert_eq!(max_output_tokens_for_model("minimax-m3"), Some(524_288));
        assert_eq!(max_output_tokens_for_model("glm-5.1"), Some(131_072));
        assert_eq!(max_output_tokens_for_model("glm-5.2"), Some(131_072));
        assert_eq!(max_output_tokens_for_model("glm-5.3"), Some(131_072));
        assert_eq!(max_output_tokens_for_model("glm-5.3-flash"), Some(131_072));
    }

    #[test]
    fn deepseek_models_with_k_suffix_use_hint() {
        assert_eq!(context_window_for_model("deepseek-v3.2-32k"), Some(32_000));
        assert_eq!(
            context_window_for_model("deepseek-v3.2-256k-preview"),
            Some(256_000)
        );
        assert_eq!(context_window_for_model("deepseek-v3.2-2k-preview"), None);
    }

    /// 2026-10-05: custom gateways serve V4 under snapshot and variant ids
    /// the reviewed catalog does not enumerate (`deepseek-v4-pro-0813`,
    /// `deepseek-v4-flash-vision`). They denote the same rows as their base
    /// ids and must inherit the base facts instead of falling back to the
    /// 128K unknown shape.
    #[test]
    fn deepseek_v4_snapshot_and_variant_ids_inherit_reviewed_facts() {
        for model in [
            "deepseek-v4-pro-0813",
            "DeepSeek-V4-Pro-0813",
            "deepseek-v4-pro-2025-08-13",
            "deepseek-v4-flash-vision",
            "deepseek-v4-flash-vision-0813",
        ] {
            assert_eq!(context_window_for_model(model), Some(1_000_000), "{model}");
            assert_eq!(max_output_tokens_for_model(model), Some(384_000), "{model}");
            assert!(model_supports_reasoning(model), "{model}");
        }
        // A trailing number that is not a recognized date stamp, an
        // eight-digit compact stamp, or an unknown base, stays unknown: the
        // resolver never invents a row.
        for model in [
            "not-a-model-0813",
            "deepseek-v4-pro-9913",
            "deepseek-v4-pro-08130",
            "deepseek-v4-pro-x0813",
            "deepseek-v4-pro-20250813",
        ] {
            assert_eq!(context_window_for_model(model), None, "{model}");
            assert_eq!(model_reasoning_capability(model), None, "{model}");
        }
        // Snapshot resolution must not relabel a DeepSeek row as an OpenAI
        // reasoning model, while the OpenAI snapshot contract keeps working.
        assert!(!model_is_openai_reasoning_family("deepseek-v4-pro-0813"));
        assert!(model_is_openai_reasoning_family("gpt-5.5-2026-06-01"));
    }

    /// Multi-byte model ids must not panic the byte-indexed date scan: the
    /// `-YYYY-MM-DD` layer splits at `len - 10`, which need not be a UTF-8
    /// char boundary (review finding from the PR #6 review).
    #[test]
    fn non_ascii_ids_do_not_panic_the_snapshot_scan() {
        for model in ["模型模型模型模型", "ローカルモデル-2026", "モデル-0813"] {
            assert_eq!(context_window_for_model(model), None, "{model}");
            assert_eq!(model_reasoning_capability(model), None, "{model}");
        }
    }

    #[test]
    fn compaction_threshold_scales_with_context_window() {
        assert_eq!(
            compaction_threshold_for_model_at_percent("deepseek-v3.2-128k", 80.0),
            102_400
        );
        // v0.8.11 (#664): unknown-model fallback also resolves to 80% of
        // `LEGACY_DEEPSEEK_CONTEXT_WINDOW_TOKENS` (128K legacy DeepSeek
        // fallback) — same late-trigger discipline as the V4 path. Was
        // `50_000` pre-v0.8.11; that hardcoded value compacted at ~5% of a
        // 1M window when model detection silently fell through, which is
        // exactly the prefix-cache-burning behaviour we're getting away from.
        assert_eq!(
            compaction_threshold_for_model_at_percent("unknown-model", 80.0),
            102_400
        );
    }

    #[test]
    fn compaction_scales_for_deepseek_v4_1m_context() {
        assert_eq!(
            compaction_threshold_for_model_at_percent("deepseek-v4-pro", 80.0),
            800_000
        );
    }

    #[test]
    fn compaction_threshold_honors_configured_percent() {
        assert_eq!(
            compaction_threshold_for_model_at_percent("deepseek-v4-pro", 75.0),
            750_000
        );
        assert_eq!(
            compaction_threshold_for_model_at_percent("trinity-large-thinking", 80.0),
            209_715
        );
    }

    #[test]
    fn auto_compaction_defaults_on_for_known_supported_model_windows() {
        assert!(auto_compact_default_for_model("trinity-large-thinking"));
        assert!(auto_compact_default_for_model("deepseek-v3.2-128k"));
        assert!(auto_compact_default_for_model("deepseek-v4-pro"));
        assert!(auto_compact_default_for_model("mimo-v2.5-pro"));
        assert!(!auto_compact_default_for_model("unknown-model"));
    }
}

//! The single prepared-outbound-request seam shared by production dispatch
//! and `/preview-request` (#1004, #3928).
//!
//! Every **primary agent turn** — `LlmClient::create_message` and
//! `create_message_stream`, in Chat Completions, Anthropic Messages, and
//! OpenAI Responses alike — reaches the wire through
//! [`crate::client::CodewhaleClient::prepare_outbound_request`], which returns a
//! [`PreparedOutboundRequest`]. The transports send it; the preview command
//! describes it. Because there is exactly one builder, a preview cannot
//! report a request different from the one a turn would send.
//!
//! Scope, stated plainly: this is *not* every outbound request Codewhale
//! makes. Chat-dialect translation builds its own small fixed body, and FIM,
//! speech, provider-native search, model listing, and the auto-router
//! classifier are separate calls with separate shapes. They are auxiliary and
//! are not described by the request manifest. See `docs/PREVIEW_REQUEST.md`.
//!
//! Nothing in this module performs I/O, mutates client state, or reads the
//! filesystem. It is safe to call on any thread at any time.
//!
//! The seam concept — prepare the exact outbound body once, then let both the
//! sender and the inspector consume it — is harvested from PR #1099
//! (`build_sanitized_chat_completion_body`) by TaoMu (GTC2080). The
//! implementation here is written against the current multi-dialect client.

use serde::Serialize;
use serde_json::Value;

use codewhale_config::provider::WireFormat;

use crate::config::ProviderKind;

/// The wire protocol a prepared request speaks.
///
/// This is the production dialect set. It is deliberately *not* collapsed to
/// Chat Completions: projecting an Anthropic Messages or Responses turn
/// through the Chat builder would describe a body that is never sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum WireDialect {
    /// OpenAI-style `POST /chat/completions`.
    ChatCompletions,
    /// Anthropic-style `POST /v1/messages`.
    AnthropicMessages,
    /// OpenAI-style `POST /responses`.
    OpenAiResponses,
}

impl WireDialect {
    pub(crate) fn from_wire_format(format: WireFormat) -> Self {
        match format {
            WireFormat::ChatCompletions => Self::ChatCompletions,
            WireFormat::AnthropicMessages => Self::AnthropicMessages,
            WireFormat::Responses => Self::OpenAiResponses,
        }
    }

    /// Stable machine label. Used in manifests and tests.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat-completions",
            Self::AnthropicMessages => "anthropic-messages",
            Self::OpenAiResponses => "openai-responses",
        }
    }
}

/// The provider-specific *shape* selected inside a dialect.
///
/// Two routes can share a dialect and still produce structurally different
/// bodies and different endpoint paths (DeepSeek's strict-tools `/beta` path,
/// Kimi Code's nested `thinking.effort`, the ChatGPT Codex Responses path).
/// Naming the shape keeps the manifest honest about which builder branch ran
/// without exposing the route URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum RouteShape {
    /// Plain dialect defaults for this provider.
    Standard,
    /// DeepSeek's `/beta/chat/completions` strict-tools path.
    DeepseekBetaStrictTools,
    /// The exact Kimi Code membership route (nested `thinking.effort`).
    KimiCodeK3,
    /// The exact pay-as-you-go Moonshot K3 route (fixed sampling).
    DirectMoonshotK3,
    /// The ChatGPT plan Responses path; identifier retained for saved receipts.
    CodexResponses,
    /// OpenCode Zen, whose model route re-resolves the wire model per request.
    OpencodeZen,
    /// A user-configured custom/compatible endpoint on a standard dialect.
    CustomCompatible,
}

impl RouteShape {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::DeepseekBetaStrictTools => "deepseek-beta-strict-tools",
            Self::KimiCodeK3 => "kimi-code-k3",
            Self::DirectMoonshotK3 => "direct-moonshot-k3",
            Self::CodexResponses => "codex-responses",
            Self::OpencodeZen => "opencode-zen",
            Self::CustomCompatible => "custom-compatible",
        }
    }
}

/// Which endpoint this request would be POSTed to, as typed facts.
///
/// `url` is the real, unredacted target: production needs it to send. Every
/// display surface must go through [`super::redact_url_for_display`] rather
/// than printing it, and the manifest only ever publishes the redacted
/// scheme/host and a fingerprint — never the path, which can itself carry a
/// deployment secret.
#[derive(Debug, Clone)]
pub(crate) struct EndpointIdentity {
    /// Stable provider id (`ProviderKind::as_str`).
    pub(crate) provider_id: String,
    /// Human-facing provider name.
    pub(crate) provider_display: String,
    /// The configured route identity when the user named a custom provider,
    /// e.g. a `[providers.<name>]` key. `None` for built-ins.
    pub(crate) route_id: Option<String>,
    /// Full POST target. Never rendered directly.
    pub(crate) url: String,
    /// Which builder branch produced the body.
    pub(crate) shape: RouteShape,
}

/// What reasoning controls actually landed on the wire, and what was asked
/// for.
///
/// The receipt is derived from the finished body, not from the intent that
/// went in: if a route-specific shaper stripped `reasoning_effort` and wrote
/// a nested `thinking.effort` instead, that is what this reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReasoningReceipt {
    /// The effort string handed to the builder, if any.
    pub(crate) requested_effort: Option<String>,
    /// Reasoning-shaping fields present on the finished body, in a stable
    /// order. Keys only come from the dialect allowlist below, so no message
    /// or prompt content can leak through this field.
    pub(crate) wire_controls: Vec<(String, Value)>,
}

/// A key that only *discloses* reasoning output; it does not ask the route to
/// think. Reporting `include` as a reasoning control would make every Responses
/// turn look like a deliberate thinking request.
const REASONING_DISCLOSURE_ONLY_KEYS: &[&str] = &["include"];

impl ReasoningReceipt {
    /// Reasoning-control keys, per dialect. Anything not on this list is not
    /// a reasoning control and never enters the receipt.
    fn control_keys(dialect: WireDialect) -> &'static [&'static str] {
        match dialect {
            WireDialect::ChatCompletions => &[
                "reasoning_effort",
                "thinking",
                "think",
                "reasoning",
                "reasoning_split",
                "chat_template_kwargs",
            ],
            WireDialect::AnthropicMessages => &["thinking", "output_config"],
            WireDialect::OpenAiResponses => &["reasoning", "include"],
        }
    }

    fn from_body(dialect: WireDialect, body: &Value, requested_effort: Option<String>) -> Self {
        let mut wire_controls = Vec::new();
        for key in Self::control_keys(dialect) {
            if let Some(value) = body.get(*key) {
                wire_controls.push(((*key).to_string(), value.clone()));
            }
        }
        Self {
            requested_effort,
            wire_controls,
        }
    }

    /// The plain `reasoning_effort` string when the route uses that dialect.
    pub(crate) fn wire_effort_string(&self) -> Option<&str> {
        self.wire_controls
            .iter()
            .find(|(key, _)| key == "reasoning_effort")
            .and_then(|(_, value)| value.as_str())
    }

    /// The effort **actually on the wire**, with the key path it was read from.
    ///
    /// Flat `reasoning_effort` is only one of the shapes production emits. The
    /// Kimi Code route writes `thinking.effort`, the Responses dialect writes
    /// `reasoning.effort`, and the Anthropic dialect writes
    /// `output_config.effort`. Reporting only the flat key made every nested
    /// route read as "no effort sent", which is exactly backwards: those are
    /// the routes that were asked to think hardest.
    ///
    /// The returned key path is a compile-time constant taken from
    /// [`Self::control_keys`], never a key read out of the body, so no
    /// provider-shaped field name can reach a manifest surface through it.
    pub(crate) fn wire_effort(&self) -> Option<(&'static str, &str)> {
        if let Some(effort) = self.wire_effort_string() {
            return Some(("reasoning_effort", effort));
        }
        for (key, value) in &self.wire_controls {
            let Some(effort) = value.get("effort").and_then(Value::as_str) else {
                continue;
            };
            let path = match key.as_str() {
                "thinking" => "thinking.effort",
                "reasoning" => "reasoning.effort",
                "output_config" => "output_config.effort",
                "think" => "think.effort",
                "reasoning_split" => "reasoning_split.effort",
                "chat_template_kwargs" => "chat_template_kwargs.effort",
                _ => continue,
            };
            return Some((path, effort));
        }
        None
    }

    /// True when the body actually asks the route to think.
    ///
    /// Deliberately *not* "the receipt is non-empty": a Responses body that
    /// carries only `include: ["reasoning.encrypted_content"]` is *disclosing*
    /// reasoning output, not requesting a tier, and must not be reported as an
    /// explicit reasoning selection.
    pub(crate) fn controls_reasoning(&self) -> bool {
        self.wire_controls
            .iter()
            .any(|(key, _)| !REASONING_DISCLOSURE_ONLY_KEYS.contains(&key.as_str()))
    }
}

/// Which transport entry point asked for this request.
///
/// This is *caller* intent, not a wire fact. The OpenAI Responses blocking
/// entry point deliberately opens an SSE stream and folds it into one
/// response, so its body carries `"stream": true` while the caller mode is
/// [`Self::Blocking`]. Reporting the two separately is the only way for a
/// manifest to describe the body exactly and still say which entry point it
/// described. See [`PreparedOutboundRequest::wire_stream_field`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum CallerStreamMode {
    /// `create_message_stream` — the caller consumes stream events.
    Streaming,
    /// `create_message` — the caller wants one finished response.
    Blocking,
}

impl CallerStreamMode {
    pub(crate) fn from_stream_flag(stream: bool) -> Self {
        if stream {
            Self::Streaming
        } else {
            Self::Blocking
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Streaming => "streaming",
            Self::Blocking => "blocking",
        }
    }
}

/// One fully prepared, not-yet-sent outbound request.
///
/// Both `CodewhaleClient::create_message*` and `/preview-request` consume this
/// value. Adding a field here is how a new wire fact becomes visible to the
/// preview; there is no second builder to keep in sync.
#[derive(Debug, Clone)]
pub(crate) struct PreparedOutboundRequest {
    pub(crate) dialect: WireDialect,
    pub(crate) endpoint: EndpointIdentity,
    /// The model id literally placed on the wire, after route remapping.
    pub(crate) wire_model: String,
    /// The final, provider-shaped body. This is the exact JSON that would be
    /// serialized and POSTed.
    pub(crate) body: Value,
    pub(crate) reasoning: ReasoningReceipt,
    /// Tokens re-sent because thinking-mode replay substituted
    /// `reasoning_content` (Chat streaming only).
    pub(crate) replay_input_tokens: Option<u32>,
    /// Which transport entry point prepared this request. Never a substitute
    /// for [`Self::wire_stream_field`], which is the wire truth.
    pub(crate) entrypoint: CallerStreamMode,
    /// Wire-normalized tool names omitted by a route-specific compatibility
    /// check. This stopgap receipt is intentionally not a projection layer;
    /// it only transports a bounded user-visible warning.
    pub(crate) omitted_tool_names: Vec<String>,
}

impl PreparedOutboundRequest {
    pub(crate) fn new(
        dialect: WireDialect,
        endpoint: EndpointIdentity,
        wire_model: String,
        body: Value,
        requested_effort: Option<String>,
        replay_input_tokens: Option<u32>,
        entrypoint: CallerStreamMode,
    ) -> Self {
        super::chat::dump_wire_prefix_if_requested(&body);
        let reasoning = ReasoningReceipt::from_body(dialect, &body, requested_effort);
        Self {
            dialect,
            endpoint,
            wire_model,
            body,
            reasoning,
            replay_input_tokens,
            entrypoint,
            omitted_tool_names: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn with_omitted_tool_names(mut self, names: Vec<String>) -> Self {
        self.omitted_tool_names = names;
        self
    }

    /// The `stream` field **as it appears on the finished body**, or `None`
    /// when the body carries no such field at all.
    ///
    /// This is the wire truth and the only value a manifest may present as
    /// "what the request says". It is deliberately not derived from
    /// [`Self::entrypoint`]: the Responses blocking path sends
    /// `"stream": true` and the Chat blocking path omits the field entirely,
    /// so both would be misreported by the caller mode.
    pub(crate) fn wire_stream_field(&self) -> Option<bool> {
        self.body.get("stream").and_then(Value::as_bool)
    }

    /// Canonical serialization of the **complete** final body.
    ///
    /// `serde_json` is built with `preserve_order` in this crate, so insertion
    /// order — not key order — drives `to_string`. Canonicalizing here means
    /// the hash is stable across builder orderings while still changing when
    /// any value anywhere in the body changes: max-token fields, tool choice,
    /// nested reasoning controls, transformed tool schemas, attachment parts,
    /// stream options, and every message.
    pub(crate) fn canonical_body(&self) -> String {
        canonical_json(&self.body)
    }

    /// SHA-256 over [`Self::canonical_body`]. Whole-body, not a prefix.
    pub(crate) fn body_sha256(&self) -> String {
        crate::hashing::sha256_hex(self.canonical_body().as_bytes())
    }

    /// Dialect-aware view of the finished body, for counting and estimation.
    pub(crate) fn wire_view(&self) -> WireBodyView<'_> {
        WireBodyView::extract(self.dialect, &self.body)
    }

    /// Attach the caller's named route identity (a `[providers.<name>]` key,
    /// or any other route id the resolved turn plan owns).
    #[must_use]
    pub(crate) fn with_route_id(mut self, route_id: Option<String>) -> Self {
        self.endpoint.route_id = route_id;
        self
    }

    /// SHA-256 of the full endpoint URL. Lets two previews be compared for
    /// "same endpoint?" without either of them printing the path.
    pub(crate) fn endpoint_fingerprint(&self) -> String {
        crate::hashing::sha256_hex(self.endpoint.url.as_bytes())
    }

    /// A bounded endpoint class that never publishes a remote authority.
    /// Custom-provider tenant subdomains can contain credentials, so every
    /// non-loopback authority is represented by a short digest.
    pub(crate) fn safe_endpoint_host_class(&self) -> String {
        let Ok(url) = reqwest::Url::parse(&self.endpoint.url) else {
            let digest = crate::hashing::sha256_hex(self.endpoint.url.as_bytes());
            return format!("unparseable sha256:{}", &digest[..12]);
        };
        let scheme = match url.scheme() {
            "http" => "http",
            "https" => "https",
            _ => "other",
        };
        let host = url.host_str().unwrap_or_default();
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|address| address.is_loopback());
        if loopback {
            return format!("{scheme} loopback");
        }
        let authority = url.port().map_or_else(
            || host.to_ascii_lowercase(),
            |port| format!("{}:{port}", host.to_ascii_lowercase()),
        );
        let digest = crate::hashing::sha256_hex(authority.as_bytes());
        format!("{scheme} remote sha256:{}", &digest[..12])
    }

    /// Output cap literally serialized into the primary request body.
    pub(crate) fn wire_output_cap_tokens(&self) -> Option<u64> {
        ["max_tokens", "max_completion_tokens", "max_output_tokens"]
            .into_iter()
            .find_map(|key| self.body.get(key).and_then(Value::as_u64))
    }
}

/// Canonical JSON: object keys sorted, no insignificant whitespace.
///
/// Deterministic for a given `Value` regardless of how it was built.
pub(crate) fn canonical_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            out.push('{');
            for (index, key) in keys.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                push_json_string(key, out);
                out.push(':');
                write_canonical(&map[*key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn push_json_string(value: &str, out: &mut String) {
    out.push_str(&Value::String(value.to_string()).to_string());
}

/// Where a given dialect keeps its system text, turn items, and tool schemas.
///
/// Extraction is by shape, never by guessing: a Responses body has
/// `instructions`/`input`, an Anthropic body has `system`/`messages`, a Chat
/// body carries the system prompt as the first `system`-role message.
///
/// # The byte accounting sums exactly
///
/// `system_bytes + tool_schema_bytes + item_bytes + framing_bytes ==
/// body_bytes`, where `body_bytes` is the length of
/// [`PreparedOutboundRequest::canonical_body`] — a stable, key-sorted semantic
/// serialization of the body. Production sends the same JSON value, but the
/// transport serializer may preserve a different object-key order. These are
/// therefore canonical JSON sizes, not literal HTTP payload byte counts. This
/// is an accounting decomposition, not a set of four borrowed
/// byte ranges in the JSON buffer:
///
/// - `system_bytes`, `tool_schema_bytes`, and `item_bytes` are the canonical
///   serializations of their *values* (for Chat, `system_bytes` is the
///   serialized system-role messages, carved out of the `messages` array);
/// - `framing_bytes` is the algebraic remainder after those three canonical
///   value-region sizes. It includes every other top-level field and whatever
///   JSON structure was not already counted inside a selected array value.
///
/// The earlier shape counted selected values and then serialized a *separate*
/// object for "framing", which double-omitted key names, brackets, and
/// separators and made the parts sum to less than the whole. Framing is now
/// defined as the remainder precisely so that cannot happen again;
/// [`WireBodyView::partition_is_exact`] asserts only the sum identity; the
/// regional names remain attribution estimates over canonical values.
///
/// `tool_result_bytes` and `attachment_bytes` are deliberately *not* part of
/// the partition: they are subsets of `item_bytes`, reported for attribution.
#[derive(Debug, Default)]
pub(crate) struct WireBodyView<'a> {
    /// Canonical byte length of the complete wire body.
    pub(crate) body_bytes: usize,
    /// Serialized bytes of the system/instructions region.
    pub(crate) system_bytes: usize,
    /// SHA-256 of the canonicalized system/instructions region — the hash of
    /// the prompt this prepared request would actually send. Empty when the
    /// request carries no system region.
    pub(crate) system_sha256: String,
    /// Serialized bytes of the tool-schema region.
    pub(crate) tool_schema_bytes: usize,
    /// SHA-256 of the canonicalized **wire** tool region: the schemas exactly
    /// as the provider receives them, after every dialect transform and
    /// strict-mode sanitizer. Empty when the body carries no `tools` field.
    pub(crate) tool_schema_sha256: String,
    /// Number of tool schemas on the wire.
    pub(crate) tool_count: usize,
    /// Turn items (messages / input items), excluding the system region.
    pub(crate) items: Vec<&'a Value>,
    /// Serialized bytes of the turn-item region, including the array's own
    /// brackets and separators and excluding any carved-out system messages.
    pub(crate) item_bytes: usize,
    /// Serialized bytes of tool-result items specifically. Subset of
    /// [`Self::item_bytes`].
    pub(crate) tool_result_bytes: usize,
    /// Number of attachment (image) parts referenced anywhere in the items.
    pub(crate) attachment_count: usize,
    /// Serialized bytes of those attachment parts. Subset of
    /// [`Self::item_bytes`].
    pub(crate) attachment_bytes: usize,
    /// Algebraic remainder after the three canonical value-region sizes. This
    /// includes other top-level fields and JSON structure not already counted
    /// inside a selected array value.
    pub(crate) framing_bytes: usize,
}

impl<'a> WireBodyView<'a> {
    fn extract(dialect: WireDialect, body: &'a Value) -> Self {
        let body_bytes = canonical_json(body).len();
        let mut view = Self {
            body_bytes,
            ..Self::default()
        };
        let Some(object) = body.as_object() else {
            view.framing_bytes = view.body_bytes;
            return view;
        };

        let (system_key, items_key) = match dialect {
            WireDialect::ChatCompletions => (None, "messages"),
            WireDialect::AnthropicMessages => (Some("system"), "messages"),
            WireDialect::OpenAiResponses => (Some("instructions"), "input"),
        };

        // The system region is accumulated as canonical text so it can be
        // hashed once, then dropped. The text itself never leaves this scope.
        let mut system_region = String::new();
        if let Some(key) = system_key
            && let Some(system) = object.get(key)
        {
            system_region.push_str(&canonical_json(system));
        }

        if let Some(tools) = object.get("tools") {
            let canonical_tools = canonical_json(tools);
            view.tool_schema_bytes = canonical_tools.len();
            view.tool_schema_sha256 = crate::hashing::sha256_hex(canonical_tools.as_bytes());
            view.tool_count = tools.as_array().map_or(0, |tools| {
                tools
                    .iter()
                    .map(|tool| {
                        if tool.get("type").and_then(Value::as_str) == Some("namespace") {
                            tool.get("tools")
                                .and_then(Value::as_array)
                                .map_or(0, Vec::len)
                        } else {
                            1
                        }
                    })
                    .sum()
            });
        }

        if let Some(items_value) = object.get(items_key) {
            // The whole array, brackets and separators included, so the
            // accounting can include the canonical array value itself.
            let mut item_region_bytes = canonical_json(items_value).len();
            if let Some(items) = items_value.as_array() {
                for item in items {
                    let bytes = canonical_json(item).len();
                    // Chat Completions carries the system prompt inline as the
                    // first system-role message. Account for it as system, not
                    // as conversation, so cross-dialect numbers stay
                    // comparable — and subtract it from the item region so the
                    // two never double-count the same bytes.
                    if dialect == WireDialect::ChatCompletions
                        && item.get("role").and_then(Value::as_str) == Some("system")
                    {
                        system_region.push_str(&canonical_json(item));
                        item_region_bytes = item_region_bytes.saturating_sub(bytes);
                        continue;
                    }
                    if is_tool_result_item(dialect, item) {
                        view.tool_result_bytes = view.tool_result_bytes.saturating_add(bytes);
                    }
                    let (count, attachment_bytes) = count_attachments(dialect, item);
                    view.attachment_count = view.attachment_count.saturating_add(count);
                    view.attachment_bytes = view.attachment_bytes.saturating_add(attachment_bytes);
                    view.items.push(item);
                }
            }
            view.item_bytes = item_region_bytes;
        }

        view.system_bytes = system_region.len();
        if !system_region.is_empty() {
            view.system_sha256 = crate::hashing::sha256_hex(system_region.as_bytes());
        }

        // Framing is the algebraic remainder, never a separately serialized
        // object. The sum is exact; these are not four disjoint byte slices.
        view.framing_bytes = view
            .body_bytes
            .saturating_sub(view.system_bytes)
            .saturating_sub(view.tool_schema_bytes)
            .saturating_sub(view.item_bytes);
        view
    }

    /// Whether the four partition classes sum to the whole wire body.
    ///
    /// The manifest publishes these as exact byte facts, so the invariant is
    /// asserted in tests across every dialect and both entry points rather
    /// than merely documented.
    pub(crate) fn partition_is_exact(&self) -> bool {
        self.system_bytes
            .saturating_add(self.tool_schema_bytes)
            .saturating_add(self.item_bytes)
            .saturating_add(self.framing_bytes)
            == self.body_bytes
    }
}

fn is_tool_result_item(dialect: WireDialect, item: &Value) -> bool {
    match dialect {
        WireDialect::ChatCompletions => item.get("role").and_then(Value::as_str) == Some("tool"),
        WireDialect::AnthropicMessages => item
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            }),
        WireDialect::OpenAiResponses => {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
        }
    }
}

/// Count attachment parts and their serialized size. Only sizes leave this
/// function — never a URL, path, or payload.
fn count_attachments(dialect: WireDialect, item: &Value) -> (usize, usize) {
    let Some(parts) = item.get("content").and_then(Value::as_array) else {
        return (0, 0);
    };
    let mut count = 0usize;
    let mut bytes = 0usize;
    for part in parts {
        let part_type = part.get("type").and_then(Value::as_str);
        let is_attachment = match dialect {
            WireDialect::ChatCompletions => {
                part_type == Some("image_url") || part.get("image_url").is_some()
            }
            WireDialect::AnthropicMessages => {
                matches!(part_type, Some("image" | "document"))
            }
            WireDialect::OpenAiResponses => {
                matches!(part_type, Some("input_image" | "input_file"))
            }
        };
        if !is_attachment {
            continue;
        }
        count += 1;
        bytes = bytes.saturating_add(canonical_json(part).len());
    }
    (count, bytes)
}

/// Classify the provider-specific shape of a prepared Chat Completions body.
pub(crate) fn chat_route_shape(
    provider: ProviderKind,
    base_url: &str,
    wire_model: &str,
    url: &str,
) -> RouteShape {
    if provider == ProviderKind::OpencodeZen {
        return RouteShape::OpencodeZen;
    }
    if url.contains("/beta/chat/completions") {
        return RouteShape::DeepseekBetaStrictTools;
    }
    if crate::config::is_exact_kimi_code_k3_route(provider, base_url, wire_model) {
        return RouteShape::KimiCodeK3;
    }
    if crate::config::is_exact_direct_moonshot_k3_route(provider, base_url, wire_model) {
        return RouteShape::DirectMoonshotK3;
    }
    if provider == ProviderKind::Custom {
        return RouteShape::CustomCompatible;
    }
    RouteShape::Standard
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, json};

    fn endpoint() -> EndpointIdentity {
        EndpointIdentity {
            provider_id: "deepseek".to_string(),
            provider_display: "DeepSeek".to_string(),
            route_id: None,
            url: "https://api.deepseek.com/chat/completions".to_string(),
            shape: RouteShape::Standard,
        }
    }

    fn prepared(body: Value) -> PreparedOutboundRequest {
        PreparedOutboundRequest::new(
            WireDialect::ChatCompletions,
            endpoint(),
            "deepseek-chat".to_string(),
            body,
            Some("high".to_string()),
            None,
            CallerStreamMode::Streaming,
        )
    }

    #[test]
    fn canonical_json_is_key_order_independent() {
        let a = json!({"b": 1, "a": {"z": 2, "y": [3, {"q": 4, "p": 5}]}});
        let mut b = Map::new();
        b.insert("a".to_string(), json!({"y": [3, {"p": 5, "q": 4}], "z": 2}));
        b.insert("b".to_string(), json!(1));
        assert_eq!(canonical_json(&a), canonical_json(&Value::Object(b)));
        assert_eq!(
            canonical_json(&a),
            r#"{"a":{"y":[3,{"p":5,"q":4}],"z":2},"b":1}"#
        );
    }

    #[test]
    fn body_hash_covers_every_wire_field() {
        let base = prepared(json!({
            "model": "deepseek-chat",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 4096,
            "tools": [{"type": "function", "function": {"name": "read_file"}}],
            "tool_choice": {"type": "auto"},
            "reasoning_effort": "high",
            "stream": true,
        }));
        let baseline = base.body_sha256();

        // Every one of these is a field a preview would have to notice.
        let mutations: Vec<(&str, Value)> = vec![
            ("max_tokens", json!(2048)),
            ("tool_choice", json!("required")),
            ("reasoning_effort", json!("low")),
            ("stream", json!(false)),
            ("temperature", json!(0.2)),
        ];
        for (key, value) in mutations {
            let mut body = base.body.clone();
            body[key] = value;
            assert_ne!(
                baseline,
                prepared(body).body_sha256(),
                "mutating `{key}` must change the whole-body hash"
            );
        }

        // Nested changes: a transformed tool schema and a nested reasoning
        // control both have to move the hash.
        let mut nested = base.body.clone();
        nested["tools"][0]["function"]["parameters"] = json!({"type": "object"});
        assert_ne!(baseline, prepared(nested).body_sha256());

        let mut thinking = base.body.clone();
        thinking["thinking"] = json!({"type": "enabled", "effort": "max"});
        assert_ne!(baseline, prepared(thinking).body_sha256());
    }

    #[test]
    fn endpoint_host_class_never_prints_remote_authority_or_path() {
        let hostile = |url: &str| {
            let mut endpoint = endpoint();
            endpoint.url = url.to_string();
            PreparedOutboundRequest::new(
                WireDialect::ChatCompletions,
                endpoint,
                "model".to_string(),
                json!({"model": "model", "messages": []}),
                None,
                None,
                CallerStreamMode::Streaming,
            )
        };

        let token_host =
            hostile("https://sk-live-abcdef0123456789.tenant.example/v1/deployments/secret/chat");
        let same_host_other_path =
            hostile("https://sk-live-abcdef0123456789.tenant.example/other/private/path");
        let idn = hostile("https://秘密.example/private/path?api_key=secret");

        let class = token_host.safe_endpoint_host_class();
        assert_eq!(class, same_host_other_path.safe_endpoint_host_class());
        assert_ne!(
            token_host.endpoint_fingerprint(),
            same_host_other_path.endpoint_fingerprint(),
            "the separate full-endpoint fingerprint must still detect path drift"
        );
        for forbidden in ["sk-live", "tenant", "example", "deployment", "secret"] {
            assert!(!class.contains(forbidden), "{forbidden} leaked in {class}");
        }
        let idn_class = idn.safe_endpoint_host_class();
        for forbidden in ["秘密", "xn--", "example", "private", "api_key", "secret"] {
            assert!(
                !idn_class.contains(forbidden),
                "{forbidden} leaked in {idn_class}"
            );
        }
        assert!(class.starts_with("https remote sha256:"), "{class}");
        assert!(class.len() <= 40, "{class}");

        let loopback = hostile("http://127.0.0.1:8080/private/token-shaped-path");
        assert_eq!(loopback.safe_endpoint_host_class(), "http loopback");
    }

    #[test]
    fn wire_output_cap_is_read_only_from_the_finished_body() {
        assert_eq!(
            prepared(json!({"max_tokens": 1024})).wire_output_cap_tokens(),
            Some(1024)
        );
        assert_eq!(
            prepared(json!({"max_completion_tokens": 2048})).wire_output_cap_tokens(),
            Some(2048)
        );
        assert_eq!(
            prepared(json!({"model": "m"})).wire_output_cap_tokens(),
            None
        );
    }

    #[test]
    fn reasoning_receipt_reads_the_finished_body_not_the_intent() {
        // Kimi Code K3 strips `reasoning_effort` and writes nested thinking.
        let kimi = prepared(json!({
            "model": "kimi-k3",
            "messages": [],
            "thinking": {"type": "enabled", "effort": "max"},
        }));
        assert_eq!(kimi.reasoning.requested_effort.as_deref(), Some("high"));
        assert_eq!(kimi.reasoning.wire_effort_string(), None);
        assert_eq!(
            kimi.reasoning.wire_controls,
            vec![(
                "thinking".to_string(),
                json!({"type": "enabled", "effort": "max"})
            )]
        );
    }

    #[test]
    fn receipt_never_captures_message_or_prompt_fields() {
        let leaky = prepared(json!({
            "model": "m",
            "messages": [{"role": "user", "content": "SECRET PROMPT"}],
            "instructions": "SECRET INSTRUCTIONS",
            "reasoning_effort": "high",
        }));
        let rendered = format!("{:?}", leaky.reasoning);
        assert!(!rendered.contains("SECRET PROMPT"), "{rendered}");
        assert!(!rendered.contains("SECRET INSTRUCTIONS"), "{rendered}");
    }

    #[test]
    fn chat_view_folds_the_system_message_into_the_system_region() {
        let request = prepared(json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "SYS"},
                {"role": "user", "content": "hi"},
                {"role": "tool", "tool_call_id": "c1", "content": "OUT"},
            ],
            "tools": [{"type": "function", "function": {"name": "a"}}],
            "max_tokens": 100,
        }));
        let view = request.wire_view();
        assert!(view.system_bytes > 0);
        assert_eq!(view.items.len(), 2, "system message is not a turn item");
        assert!(view.tool_result_bytes > 0);
        assert_eq!(view.tool_count, 1);
        assert!(view.framing_bytes > 0);
    }

    #[test]
    fn anthropic_and_responses_views_use_their_own_shapes() {
        let anthropic_request = PreparedOutboundRequest::new(
            WireDialect::AnthropicMessages,
            endpoint(),
            "claude".to_string(),
            json!({
                "model": "claude",
                "system": "SYS",
                "messages": [
                    {"role": "user", "content": [{"type": "tool_result", "content": "OUT"}]},
                    {"role": "user", "content": [{"type": "image", "source": {"data": "AAA"}}]},
                ],
                "tools": [{"name": "a"}, {"name": "b"}],
            }),
            None,
            None,
            CallerStreamMode::Streaming,
        );
        let anthropic = anthropic_request.wire_view();
        assert!(anthropic.system_bytes > 0);
        assert_eq!(anthropic.items.len(), 2);
        assert!(anthropic.tool_result_bytes > 0);
        assert_eq!(anthropic.attachment_count, 1);
        assert_eq!(anthropic.tool_count, 2);

        let responses_request = PreparedOutboundRequest::new(
            WireDialect::OpenAiResponses,
            endpoint(),
            "gpt".to_string(),
            json!({
                "model": "gpt",
                "instructions": "SYS",
                "input": [
                    {"type": "message", "role": "user", "content": [{"type": "input_text"}]},
                    {"type": "function_call_output", "output": "OUT"},
                ],
                "tools": [{"name": "a"}],
            }),
            None,
            None,
            CallerStreamMode::Streaming,
        );
        let responses = responses_request.wire_view();
        assert!(responses.system_bytes > 0);
        assert_eq!(responses.items.len(), 2);
        assert!(responses.tool_result_bytes > 0);
        assert_eq!(responses.tool_count, 1);
    }

    /// The reviewed defect: nested reasoning shapes were invisible, so every
    /// route that actually thinks hardest read as "no effort sent".
    #[test]
    fn nested_reasoning_efforts_are_read_from_every_dialect() {
        let kimi = prepared(json!({
            "model": "kimi-k3",
            "messages": [],
            "thinking": {"type": "enabled", "effort": "max"},
        }));
        assert_eq!(
            kimi.reasoning.wire_effort(),
            Some(("thinking.effort", "max"))
        );
        assert!(kimi.reasoning.controls_reasoning());

        let responses = PreparedOutboundRequest::new(
            WireDialect::OpenAiResponses,
            endpoint(),
            "gpt".to_string(),
            json!({
                "model": "gpt",
                "input": [],
                "reasoning": {"effort": "high", "summary": "auto"},
                "include": ["reasoning.encrypted_content"],
            }),
            None,
            None,
            CallerStreamMode::Streaming,
        );
        assert_eq!(
            responses.reasoning.wire_effort(),
            Some(("reasoning.effort", "high"))
        );
        assert!(responses.reasoning.controls_reasoning());

        let anthropic = PreparedOutboundRequest::new(
            WireDialect::AnthropicMessages,
            endpoint(),
            "claude".to_string(),
            json!({
                "model": "claude",
                "messages": [],
                "output_config": {"effort": "low"},
            }),
            None,
            None,
            CallerStreamMode::Streaming,
        );
        assert_eq!(
            anthropic.reasoning.wire_effort(),
            Some(("output_config.effort", "low"))
        );

        // Flat still wins when the dialect uses it.
        let chat = prepared(json!({
            "model": "m",
            "messages": [],
            "reasoning_effort": "medium",
        }));
        assert_eq!(
            chat.reasoning.wire_effort(),
            Some(("reasoning_effort", "medium"))
        );
    }

    /// `include` discloses reasoning output; it does not request a tier. A
    /// body carrying only `include` must not read as a reasoning control.
    #[test]
    fn responses_include_alone_is_not_a_reasoning_control() {
        let disclosure_only = PreparedOutboundRequest::new(
            WireDialect::OpenAiResponses,
            endpoint(),
            "gpt".to_string(),
            json!({
                "model": "gpt",
                "input": [],
                "include": ["reasoning.encrypted_content"],
            }),
            None,
            None,
            CallerStreamMode::Streaming,
        );
        assert!(
            !disclosure_only.reasoning.wire_controls.is_empty(),
            "`include` is still disclosed on the receipt"
        );
        assert!(
            !disclosure_only.reasoning.controls_reasoning(),
            "`include` alone must not read as a reasoning request"
        );
        assert_eq!(disclosure_only.reasoning.wire_effort(), None);
    }

    fn assert_partition_exact(request: &PreparedOutboundRequest, what: &str) {
        let view = request.wire_view();
        assert_eq!(
            view.body_bytes,
            request.canonical_body().len(),
            "{what}: the view must measure the bytes that would be POSTed"
        );
        assert!(
            view.partition_is_exact(),
            "{what}: {} + {} + {} + {} != {}",
            view.system_bytes,
            view.tool_schema_bytes,
            view.item_bytes,
            view.framing_bytes,
            view.body_bytes
        );
        assert!(view.tool_result_bytes <= view.item_bytes, "{what}");
        assert!(view.attachment_bytes <= view.item_bytes, "{what}");
    }

    /// The byte classes are published as exact facts, so they must account for
    /// every byte of the wire body — key names, brackets, and separators
    /// included — in every dialect and on both entry points.
    #[test]
    fn byte_classes_sum_to_the_whole_wire_body_in_every_dialect() {
        assert_partition_exact(
            &prepared(json!({
                "model": "m",
                "messages": [
                    {"role": "system", "content": "SYS"},
                    {"role": "user", "content": "hi"},
                    {"role": "tool", "tool_call_id": "c1", "content": "OUT"},
                ],
                "tools": [{"type": "function", "function": {"name": "a"}}],
                "tool_choice": {"type": "auto"},
                "max_tokens": 100,
                "stream": true,
            })),
            "chat streaming",
        );
        assert_partition_exact(
            &prepared(json!({
                "model": "m",
                "messages": [{"role": "user", "content": "hi"}],
                "max_tokens": 100,
            })),
            "chat blocking (no tools, no system, no stream field)",
        );
        assert_partition_exact(
            &prepared(json!({"model": "m", "messages": []})),
            "chat minimal",
        );
        assert_partition_exact(
            &PreparedOutboundRequest::new(
                WireDialect::AnthropicMessages,
                endpoint(),
                "claude".to_string(),
                json!({
                    "model": "claude",
                    "system": [{"type": "text", "text": "SYS"}],
                    "messages": [
                        {"role": "user", "content": [{"type": "tool_result", "content": "OUT"}]},
                        {"role": "user", "content": [{"type": "image", "source": {"data": "AAA"}}]},
                    ],
                    "tools": [{"name": "a"}],
                    "stream": true,
                }),
                None,
                None,
                CallerStreamMode::Streaming,
            ),
            "anthropic streaming",
        );
        assert_partition_exact(
            &PreparedOutboundRequest::new(
                WireDialect::OpenAiResponses,
                endpoint(),
                "gpt".to_string(),
                json!({
                    "model": "gpt",
                    "instructions": "SYS",
                    "input": [
                        {"type": "message", "role": "user", "content": [{"type": "input_text"}]},
                        {"type": "function_call_output", "output": "OUT"},
                    ],
                    "tools": [{"name": "a"}],
                    "reasoning": {"effort": "high"},
                    "stream": true,
                }),
                None,
                None,
                CallerStreamMode::Blocking,
            ),
            "responses blocking entry point (wire still streams)",
        );
    }

    /// Mutating any region must keep the partition exact *and* move the class
    /// the mutation belongs to. A partition that stayed exact by dumping the
    /// difference into framing would be arithmetically true and useless.
    #[test]
    fn byte_classes_track_the_region_that_changed() {
        let base = json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "SYS"},
                {"role": "user", "content": "hi"},
            ],
            "tools": [{"type": "function", "function": {"name": "a"}}],
            "max_tokens": 100,
        });
        let baseline = prepared(base.clone());
        let baseline_view = baseline.wire_view();

        let mut bigger_system = base.clone();
        bigger_system["messages"][0]["content"] = json!("SYSTEM PROMPT, MUCH LONGER");
        let request = prepared(bigger_system);
        let view = request.wire_view();
        assert_partition_exact(&request, "grown system");
        assert!(view.system_bytes > baseline_view.system_bytes);
        assert_eq!(view.item_bytes, baseline_view.item_bytes);

        let mut bigger_tools = base.clone();
        bigger_tools["tools"][0]["function"]["parameters"] = json!({"type": "object"});
        let request = prepared(bigger_tools);
        let view = request.wire_view();
        assert_partition_exact(&request, "grown tool schema");
        assert!(view.tool_schema_bytes > baseline_view.tool_schema_bytes);
        assert_ne!(view.tool_schema_sha256, baseline_view.tool_schema_sha256);

        let mut extra_message = base.clone();
        extra_message["messages"]
            .as_array_mut()
            .expect("messages array")
            .push(json!({"role": "user", "content": "the hypothetical next prompt"}));
        let request = prepared(extra_message);
        let view = request.wire_view();
        assert_partition_exact(&request, "appended message");
        assert!(view.item_bytes > baseline_view.item_bytes);
        assert_eq!(view.system_bytes, baseline_view.system_bytes);

        let mut extra_framing = base;
        extra_framing["stream_options"] = json!({"include_usage": true});
        let request = prepared(extra_framing);
        let view = request.wire_view();
        assert_partition_exact(&request, "added framing field");
        assert!(view.framing_bytes > baseline_view.framing_bytes);
        assert_eq!(view.item_bytes, baseline_view.item_bytes);
    }

    /// The prefix digest is derived from this hash, so a provider-side schema
    /// transform that leaves the logical catalog untouched must still move it.
    #[test]
    fn wire_tool_hash_tracks_dialect_schema_shaping() {
        let logical = prepared(json!({
            "model": "m",
            "messages": [],
            "tools": [{"type": "function", "function": {"name": "a", "parameters": {"type": "object"}}}],
        }));
        let shaped = prepared(json!({
            "model": "m",
            "messages": [],
            "tools": [{"type": "function", "function": {
                "name": "a",
                "parameters": {"type": "object", "additionalProperties": false},
                "strict": true,
            }}],
        }));
        assert_ne!(
            logical.wire_view().tool_schema_sha256,
            shaped.wire_view().tool_schema_sha256,
            "strict-mode schema sanitizing must move the wire tool hash"
        );

        let toolless = prepared(json!({"model": "m", "messages": []}));
        assert!(toolless.wire_view().tool_schema_sha256.is_empty());
    }

    #[test]
    fn dialect_labels_are_stable() {
        assert_eq!(
            WireDialect::from_wire_format(WireFormat::ChatCompletions).as_str(),
            "chat-completions"
        );
        assert_eq!(
            WireDialect::from_wire_format(WireFormat::AnthropicMessages).as_str(),
            "anthropic-messages"
        );
        assert_eq!(
            WireDialect::from_wire_format(WireFormat::Responses).as_str(),
            "openai-responses"
        );
    }
}

/// Per-dialect proof that `/preview-request` and production dispatch consume
/// the same bytes.
///
/// Each case builds a real client for a production route, prepares a request
/// through [`CodewhaleClient::prepare_outbound_request`] — the value the
/// transports send and the preview describes — and compares its whole-body
/// hash against the dialect's own builder run over the identically
/// pre-processed request. A divergence here means a second body builder has
/// reappeared.
#[cfg(test)]
mod dialect_seam_tests {
    use super::*;
    use crate::config::{Config, ProviderConfig, ProvidersConfig};
    use codewhale_models::Role;
    use codewhale_models::{ContentBlock, Message, MessageRequest, SystemPrompt, Tool};
    use serde_json::json;

    use super::super::CodewhaleClient;

    fn tool(name: &str) -> Tool {
        Tool {
            tool_type: None,
            name: name.to_string(),
            description: format!("{name} description"),
            input_schema: json!({"type": "object", "properties": {}}),
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    fn request(model: &str) -> MessageRequest {
        MessageRequest {
            model: model.to_string(),
            messages: vec![Message {
                role: Role::User,
                content: vec![ContentBlock::Text {
                    text: "hello".to_string(),
                    cache_control: None,
                }],
            }],
            max_tokens: 4096,
            system: Some(SystemPrompt::Text("BASE PROMPT".to_string())),
            tools: Some(vec![tool("read_file"), tool("Bash")]),
            tool_choice: Some(json!({"type": "auto"})),
            metadata: None,
            thinking: None,
            reasoning_effort: Some("high".to_string()),
            stream: Some(true),
            temperature: None,
            top_p: None,
        }
    }

    fn client(provider: &str, configure: impl FnOnce(&mut ProvidersConfig)) -> CodewhaleClient {
        let mut providers = ProvidersConfig::default();
        configure(&mut providers);
        CodewhaleClient::new(&Config {
            provider: Some(provider.to_string()),
            providers: Some(providers),
            ..Config::default()
        })
        .expect("client resolves for this route")
    }

    fn configured(api_key: &str, base_url: Option<&str>, model: &str) -> ProviderConfig {
        ProviderConfig {
            api_key: Some(api_key.to_string()),
            base_url: base_url.map(str::to_string),
            model: Some(model.to_string()),
            ..ProviderConfig::default()
        }
    }

    fn sha256(value: &str) -> String {
        crate::hashing::sha256_hex(value.as_bytes())
    }

    /// The exact pre-processing `prepare_outbound_request` applies before the
    /// dialect builder runs. Reproduced here so the reference body is built
    /// from the same input, not from a differently-sanitized one.
    fn preprocessed(client: &CodewhaleClient, request: MessageRequest) -> MessageRequest {
        client
            .bind_request_to_protocol(client.prepare_model_bound_request(request))
            .expect("protocol binding succeeds")
            .0
    }

    #[test]
    fn output_cap_reaches_all_three_wire_dialects_with_reasoning_inside_allowance() {
        let _env = crate::test_support::lock_test_env();
        for wire in ["chat-completions", "anthropic-messages", "responses"] {
            let config = Config {
                provider: Some("output-cap-fixture".into()),
                providers: Some(ProvidersConfig {
                    custom: std::collections::HashMap::from([(
                        "output-cap-fixture".into(),
                        ProviderConfig {
                            kind: Some("openai-compatible".into()),
                            base_url: Some("http://127.0.0.1:18181/v1".into()),
                            api_key: Some("fixture-output-cap".into()),
                            model: Some("fixture-model".into()),
                            wire: Some(wire.into()),
                            ..Default::default()
                        },
                    )]),
                    ..Default::default()
                }),
                ..Config::default()
            };
            let client = CodewhaleClient::new(&config).unwrap();
            let mut request = request("fixture-model");
            request.max_tokens = 1500;
            let prepared = client.prepare_outbound_request(request, true).unwrap();
            assert_eq!(prepared.wire_output_cap_tokens(), Some(1500), "{wire}");
            if let Some(thinking) = prepared
                .body
                .pointer("/thinking/budget_tokens")
                .and_then(Value::as_u64)
            {
                assert!(
                    thinking < 1500,
                    "reasoning must fit inside the shared allowance"
                );
            }
        }
    }

    #[test]
    fn chat_completions_preview_matches_the_production_chat_builder() {
        let client = client("deepseek", |providers| {
            providers.deepseek = configured("sk-test-deepseek", None, "deepseek-chat");
        });
        let prepared = client
            .prepare_outbound_request(request("deepseek-chat"), true)
            .expect("chat request prepares");
        assert_eq!(prepared.dialect, WireDialect::ChatCompletions);

        let reference = super::super::chat::build_chat_wire_body(
            &preprocessed(&client, request("deepseek-chat")),
            client.api_provider(),
            client.base_url(),
            true,
            None,
        )
        .expect("reference body builds");

        assert_eq!(
            prepared.body_sha256(),
            sha256(&canonical_json(&reference.body))
        );
        assert_eq!(prepared.wire_model, reference.model);
        assert!(
            prepared.body.get("tool_choice").is_none(),
            "DeepSeek thinking requests omit tool_choice on the final wire body"
        );
    }

    #[test]
    fn kimi_code_keeps_its_own_shape_and_is_not_projected_through_plain_chat() {
        let client = client("moonshot", |providers| {
            providers.moonshot = configured(
                "sk-test-kimi",
                Some(crate::config::DEFAULT_KIMI_CODE_BASE_URL),
                crate::config::KIMI_CODE_K3_MODEL,
            );
        });
        let prepared = client
            .prepare_outbound_request(request(crate::config::KIMI_CODE_K3_MODEL), true)
            .expect("kimi code request prepares");

        assert_eq!(prepared.dialect, WireDialect::ChatCompletions);
        assert_eq!(prepared.endpoint.shape, RouteShape::KimiCodeK3);
        // The route-specific shaper replaces flat `reasoning_effort` with the
        // nested `thinking.effort` dialect; the receipt must show that.
        assert_eq!(prepared.reasoning.wire_effort_string(), None);
        assert!(
            prepared
                .reasoning
                .wire_controls
                .iter()
                .any(|(key, _)| key == "thinking"),
            "{:?}",
            prepared.reasoning
        );

        let reference = super::super::chat::build_chat_wire_body(
            &preprocessed(&client, request(crate::config::KIMI_CODE_K3_MODEL)),
            client.api_provider(),
            client.base_url(),
            true,
            None,
        )
        .expect("reference body builds");
        assert_eq!(
            prepared.body_sha256(),
            sha256(&canonical_json(&reference.body))
        );
    }

    /// Every production Anthropic Messages route: native Anthropic, the
    /// DeepSeek Messages route, and the MiniMax Messages route. Each shapes
    fn message(role: Role, text: &str) -> Message {
        Message {
            role,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    /// Anthropic Messages has no in-transcript `system` role, but a compaction
    /// summary cannot be dropped or hoisted without changing transcript
    /// meaning. The seam keeps it in place and the adapter projects it onto a
    /// user message, one of the two roles the wire accepts.
    #[test]
    fn seam_preserves_an_in_transcript_system_message_on_anthropic() {
        let client = client("anthropic", |providers| {
            providers.anthropic = configured("sk-ant-test", None, "claude-sonnet-4-5");
        });
        let mut request = request("claude-sonnet-4-5");
        request
            .messages
            .push(message(Role::System, "compaction summary"));

        let prepared = client
            .prepare_outbound_request(request, true)
            .expect("Anthropic projects positioned system history onto user");
        let carried = prepared.body["messages"]
            .as_array()
            .expect("messages")
            .iter()
            .find(|message| {
                message["content"].as_array().is_some_and(|blocks| {
                    blocks
                        .iter()
                        .any(|block| block["text"] == "compaction summary")
                })
            })
            .expect("compaction summary survives");
        assert_eq!(carried["role"], "user");
    }

    /// The dialects that have always dropped an unrepresentable role keep
    /// dropping it. Turning that into a hard failure would break live
    /// sessions; the point of the seam is to make the choice explicit, not to
    /// make every dialect strict.
    #[test]
    fn seam_lets_the_openai_shaped_dialects_keep_dropping_unknown_roles() {
        let client = client("deepseek", |providers| {
            providers.deepseek = configured("sk-test-deepseek", None, "deepseek-chat");
        });
        let mut request = request("deepseek-chat");
        request.messages.push(message(
            Role::Unrecognized("future_role".to_string()),
            "from a newer build",
        ));

        let prepared = client
            .prepare_outbound_request(request, true)
            .expect("an unknown role must not fail a Chat Completions turn");
        let body = serde_json::to_string(&prepared.body).expect("serialize body");
        assert!(
            !body.contains("from a newer build"),
            "an unknown role must not reach the wire: {body}"
        );
        assert!(!body.contains("\"future_role\""), "{body}");
    }

    /// thinking differently, so each is checked against its own builder run.
    #[test]
    fn anthropic_messages_preview_matches_the_production_messages_builder() {
        type ProviderCase = (
            &'static str,
            &'static str,
            Box<dyn Fn(&mut ProvidersConfig)>,
        );

        let cases: Vec<ProviderCase> = vec![
            (
                "anthropic",
                "claude-sonnet-4-5",
                Box::new(|providers: &mut ProvidersConfig| {
                    providers.anthropic = configured("sk-ant-test", None, "claude-sonnet-4-5");
                }),
            ),
            (
                "deepseek-anthropic",
                "deepseek-v4",
                Box::new(|providers: &mut ProvidersConfig| {
                    providers.deepseek_anthropic =
                        configured("sk-test-deepseek-anthropic", None, "deepseek-v4");
                }),
            ),
            (
                "minimax-anthropic",
                "MiniMax-M3",
                Box::new(|providers: &mut ProvidersConfig| {
                    providers.minimax_anthropic =
                        configured("sk-test-minimax-anthropic", None, "MiniMax-M3");
                }),
            ),
        ];

        for (provider, model, configure) in cases {
            let client = client(provider, |providers| configure(providers));
            let prepared = client
                .prepare_outbound_request(request(model), true)
                .unwrap_or_else(|error| panic!("{provider} request prepares: {error}"));

            assert_eq!(
                prepared.dialect,
                WireDialect::AnthropicMessages,
                "{provider} must keep the Messages dialect, not be projected through Chat"
            );

            let reference =
                client.build_anthropic_body(&preprocessed(&client, request(model)), true);
            assert_eq!(
                prepared.body_sha256(),
                sha256(&canonical_json(&reference)),
                "{provider} preview body must hash identically to the production builder"
            );
            assert_eq!(
                prepared
                    .body
                    .get("tool_choice")
                    .and_then(|value| value.get("type"))
                    .and_then(serde_json::Value::as_str),
                Some("auto"),
                "{provider} tool_choice must come from the final Messages body"
            );

            // The Messages dialect never carries a flat `reasoning_effort`;
            // the receipt must reflect the dialect's own controls.
            assert_eq!(prepared.reasoning.wire_effort_string(), None, "{provider}");
            assert_eq!(
                prepared.reasoning.requested_effort.as_deref(),
                Some("high"),
                "{provider}"
            );
        }
    }

    /// The official plan route requires Codewhale's own protected grant.
    fn codex_client() -> CodewhaleClient {
        let _env_lock = crate::test_support::lock_test_env();
        let home = tempfile::tempdir().expect("isolated ChatGPT credential home");
        let root = home
            .path()
            .canonicalize()
            .expect("canonical credential home");
        let _home = crate::test_support::EnvVarGuard::set("CODEWHALE_HOME", &root);
        let mut config = Config {
            provider: Some("openai-codex".to_string()),
            ..Config::default()
        };
        config
            .provider_config_for_mut(&config.test_identity_for_kind(ProviderKind::OpenaiCodex))
            .unwrap()
            .model = Some("gpt-5-codex".into());
        crate::oauth::install_test_chatgpt_registration(&mut config).expect("official test grant");
        CodewhaleClient::new(&config).expect("official ChatGPT client")
    }

    #[test]
    fn responses_preview_matches_the_production_responses_builder() {
        let client = codex_client();
        let prepared = client
            .prepare_outbound_request(request("gpt-5-codex"), true)
            .expect("responses request prepares");

        assert_eq!(prepared.dialect, WireDialect::OpenAiResponses);
        assert_eq!(prepared.endpoint.shape, RouteShape::CodexResponses);
        assert_eq!(prepared.endpoint.url, "https://api.openai.com/v1/responses");
        assert_eq!(prepared.body["tools"][0]["type"], "namespace");
        assert_eq!(prepared.body["tools"][0]["name"], "codewhale");
        assert_eq!(
            prepared.body["tools"][0]["tools"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            prepared.wire_view().tool_count,
            2,
            "namespace wrappers are not executable tools"
        );

        let reference = super::super::responses::build_responses_body_for_provider(
            &preprocessed(&client, request("gpt-5-codex")),
            ProviderKind::OpenaiCodex,
            client.chatgpt_reasoning_api.as_deref(),
        );
        assert_eq!(prepared.body_sha256(), sha256(&canonical_json(&reference)));
        assert_eq!(prepared.body.get("tool_choice"), Some(&json!("auto")));
    }

    #[test]
    fn responses_prepared_request_preserves_full_history_for_its_grant() {
        let client = codex_client();
        let scope = client.chatgpt_reasoning_api.as_ref().unwrap();
        let mut request = request("gpt-5-codex");
        for id in ["first", "second"] {
            request.messages.push(codewhale_models::Message {
                role: codewhale_models::Role::Assistant,
                content: vec![codewhale_models::ContentBlock::Thinking {
                    thinking: "readable-reasoning-must-stay-local".into(),
                    signature: None,
                    state: Some(codewhale_models::OpaqueReasoningState {
                        provider: "openai-codex".into(),
                        api: scope.clone(),
                        model: "gpt-5-codex".into(),
                        id: Some(id.into()),
                        encrypted_content: format!("opaque-{id}"),
                    }),
                    redacted_data: None,
                }],
            });
        }
        let prepared = client
            .prepare_outbound_request(request.clone(), true)
            .unwrap();
        let reasoning: Vec<_> = prepared.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|item| item["type"] == "reasoning")
            .collect();
        assert_eq!(reasoning.len(), 2);
        assert_eq!(reasoning[0]["encrypted_content"], "opaque-first");
        assert_eq!(reasoning[1]["encrypted_content"], "opaque-second");
        assert!(
            !prepared
                .body
                .to_string()
                .contains("readable-reasoning-must-stay-local")
        );
        let mut different_grant = client.clone();
        different_grant.chatgpt_reasoning_api = Some("openai-responses-siwc-v1:other".into());
        let rejected = different_grant
            .prepare_outbound_request(request, true)
            .unwrap();
        assert!(!rejected.body.to_string().contains("opaque-first"));
        assert!(!rejected.body.to_string().contains("opaque-second"));
    }

    #[test]
    fn every_dialect_reports_a_distinct_body_hash_for_the_same_logical_request() {
        // Guards against the reviewed failure mode: projecting every route
        // through the Chat builder would make these collide.
        let chat = client("deepseek", |providers| {
            providers.deepseek = configured("sk-test-deepseek", None, "deepseek-chat");
        })
        .prepare_outbound_request(request("deepseek-chat"), true)
        .expect("chat prepares");
        let codex = codex_client();
        let responses = codex
            .prepare_outbound_request(request("gpt-5-codex"), true)
            .expect("responses prepares");

        assert_ne!(chat.dialect, responses.dialect);
        assert_ne!(chat.body_sha256(), responses.body_sha256());
    }

    #[test]
    fn streaming_and_blocking_bodies_are_distinguished_not_conflated() {
        let client = client("deepseek", |providers| {
            providers.deepseek = configured("sk-test-deepseek", None, "deepseek-chat");
        });
        let streaming = client
            .prepare_outbound_request(request("deepseek-chat"), true)
            .expect("streaming prepares");
        let blocking = client
            .prepare_outbound_request(request("deepseek-chat"), false)
            .expect("blocking prepares");

        assert_eq!(streaming.entrypoint, CallerStreamMode::Streaming);
        assert_eq!(blocking.entrypoint, CallerStreamMode::Blocking);
        // Chat is the dialect where caller mode and wire fact agree: the
        // streaming body sets `stream: true`, the blocking body omits it.
        assert_eq!(streaming.wire_stream_field(), Some(true));
        assert_eq!(blocking.wire_stream_field(), None);
        assert_ne!(streaming.body_sha256(), blocking.body_sha256());
    }

    /// #1004 review finding: the Responses blocking entry point opens an SSE
    /// stream and folds it into one response, so its body says
    /// `"stream": true` while the caller mode is blocking. The manifest must
    /// read the body, never the caller mode.
    #[test]
    fn responses_wire_streaming_is_read_from_the_body_not_the_caller_mode() {
        let client = codex_client();
        let blocking = client
            .prepare_outbound_request(request("gpt-5-codex"), false)
            .expect("blocking responses prepares");

        assert_eq!(blocking.entrypoint, CallerStreamMode::Blocking);
        assert_eq!(
            blocking.wire_stream_field(),
            Some(true),
            "the Responses blocking path genuinely sends an SSE body"
        );

        let streaming = client
            .prepare_outbound_request(request("gpt-5-codex"), true)
            .expect("streaming responses prepares");
        assert_eq!(streaming.wire_stream_field(), Some(true));
        assert_eq!(
            streaming.body_sha256(),
            blocking.body_sha256(),
            "the two Responses entry points send the same bytes; only the \
             caller mode differs"
        );
    }

    #[test]
    fn preparation_is_deterministic_across_repeated_calls() {
        let client = client("deepseek", |providers| {
            providers.deepseek = configured("sk-test-deepseek", None, "deepseek-chat");
        });
        let first = client
            .prepare_outbound_request(request("deepseek-chat"), true)
            .expect("first prepares");
        let second = client
            .prepare_outbound_request(request("deepseek-chat"), true)
            .expect("second prepares");
        assert_eq!(first.body_sha256(), second.body_sha256());
        assert_eq!(first.endpoint_fingerprint(), second.endpoint_fingerprint());
    }
}

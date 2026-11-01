//! Background advisor watcher (#3982).
//!
//! When enabled, the advisor wakes on turn boundaries, reads a bounded slice
//! of recent tool calls from the session transcript, makes a concise LLM
//! advisory call on an exactly resolved provider/model client, and
//! emits an [`Event::AdvisoryNote`] fire-and-forget.
//!
//! Key design properties:
//! - **Off by default** — enabled via `[advisor] enabled = true` or `/advisor on`.
//! - **Bounded input** — at most `max_tool_calls` tool-call/result pairs are
//!   included; the rest are dropped oldest-first.
//! - **Rate-limited** — at most one emission per `rate_limit_secs` seconds.
//! - **Deduplicated** — notes whose content hash matches the previous note
//!   within `dedup_window_secs` are silently dropped.
//! - **Child-failure isolated** — advisor errors are logged but never surface
//!   as parent turn failures.
//! - **Policy-bounded** — the advisor uses a read-only reviewer prompt and
//!   no tool access; it cannot exceed the parent session policy.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use codewhale_config::AdvisorConfigToml;
use tokio::sync::mpsc;
use tracing::debug;

use crate::client::CodewhaleClient;
use crate::core::events::Event;
use crate::llm_client::LlmClient;
use codewhale_models::Role;
use codewhale_models::{ContentBlock, Message, MessageRequest, SystemPrompt};
use codewhale_runtime::config::Config;
use codewhale_util::utils::truncate_with_ellipsis;

/// Maximum tokens the advisor may generate. Kept short so the note stays
/// concise and does not compete with the parent turn's billing budget.
const ADVISOR_MAX_TOKENS: u32 = 256;

/// Maximum characters of tool input + result to include per tool-call pair.
const MAX_CHARS_PER_PAIR: usize = 800;

/// System prompt for the advisor LLM call. Read-only review posture — no
/// tool access, no code generation.
const ADVISOR_SYSTEM_PROMPT: &str = "You are a concise background advisor reviewing recent tool activity. \
Your role: identify one or two concrete concerns (correctness, risk, or \
missed alternatives) in the tool calls provided. \
If nothing notable stands out, respond with exactly the word \"ok\". \
Otherwise write one to three short sentences — no preamble, no markdown, \
no praise. Focus on signal; omit noise.";

/// A single tool-call/result pair extracted from the session transcript.
#[derive(Debug, Clone)]
pub struct ToolCallPair {
    /// Tool name (e.g. `exec_shell`, `file_write`).
    pub name: String,
    /// Bounded serialization of the tool input.
    pub input_preview: String,
    /// Bounded serialization of the tool result.
    pub result_preview: String,
}

/// Resolved advisor configuration derived from [`AdvisorConfigToml`].
#[derive(Debug, Clone)]
pub struct AdvisorConfig {
    /// Whether the advisor is currently enabled (session-level toggle).
    pub enabled: bool,
    /// Max tool-call pairs to review per turn.
    pub max_tool_calls: u32,
    /// Min seconds between consecutive emissions.
    pub rate_limit: Duration,
    /// Window during which duplicate notes are suppressed.
    pub dedup_window: Duration,
    /// Optional model override (falls back to session model when `None`).
    pub model: Option<String>,
}

impl AdvisorConfig {
    /// Build a resolved config from the TOML schema.
    #[must_use]
    pub fn from_toml(toml: &AdvisorConfigToml) -> Self {
        Self {
            enabled: toml.enabled,
            max_tool_calls: toml.max_tool_calls.clamp(1, 50),
            rate_limit: Duration::from_secs(toml.rate_limit_secs.clamp(5, 3600)),
            dedup_window: Duration::from_secs(toml.dedup_window_secs),
            model: toml.model.clone(),
        }
    }

    /// Default disabled config (matches `[advisor]` absent from config.toml).
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            max_tool_calls: 10,
            rate_limit: Duration::from_secs(60),
            dedup_window: Duration::from_secs(300),
            model: None,
        }
    }
}

/// Runtime emission guard: tracks the last emission time and the hash of the
/// last advisory note to enforce rate limiting and deduplication.
#[derive(Debug)]
pub struct EmissionGuard {
    last_emission: Option<Instant>,
    last_note_hash: Option<u64>,
    last_note_hash_at: Option<Instant>,
}

/// Accounting ownership captured while the originating turn is still live.
///
/// Runtime turns retain their synchronous durable sink through the lease;
/// ordinary interactive turns fall back to the exact session cost generation
/// captured here. Neither path can spill into a later session.
#[derive(Debug)]
pub struct AdvisorUsageContext {
    cost_scope: crate::cost_status::CostScopeToken,
    runtime_usage_lease: Option<crate::cost_status::RuntimeUsageLease>,
}

impl AdvisorUsageContext {
    #[must_use]
    pub fn capture(runtime_owner: Option<&str>) -> Self {
        Self {
            cost_scope: crate::cost_status::scope_token(),
            runtime_usage_lease: runtime_owner
                .and_then(crate::cost_status::acquire_runtime_usage_lease),
        }
    }

    fn report(
        &self,
        source_id: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
        usage: &codewhale_models::Usage,
    ) {
        crate::cost_status::report_effective_route_for_runtime(
            self.cost_scope,
            self.runtime_usage_lease
                .as_ref()
                .map(crate::cost_status::RuntimeUsageLease::owner),
            source_id,
            route,
            usage,
        );
    }

    fn report_unreceipted(
        &self,
        source_id: &str,
        route: &crate::cost_status::EffectiveRouteEnvelope,
    ) {
        crate::cost_status::report_unreceipted_provider_success(
            self.cost_scope,
            self.runtime_usage_lease
                .as_ref()
                .map(crate::cost_status::RuntimeUsageLease::owner),
            source_id,
            route,
        );
    }
}

impl EmissionGuard {
    /// Create a fresh guard with no emission history.
    #[must_use]
    pub fn new() -> Self {
        Self {
            last_emission: None,
            last_note_hash: None,
            last_note_hash_at: None,
        }
    }

    /// Check whether emitting `note` is allowed under `config`'s rate-limit
    /// and dedup policy. Returns `true` when the note may be emitted.
    #[must_use]
    pub fn may_emit(&self, note: &str, config: &AdvisorConfig) -> bool {
        // Suppress trivial "ok" responses from the model.
        if note.trim().eq_ignore_ascii_case("ok") {
            return false;
        }

        let now = Instant::now();

        // Rate limit: require at least `rate_limit` since last emission.
        if let Some(last) = self.last_emission
            && now.duration_since(last) < config.rate_limit
        {
            return false;
        }

        // Dedup: suppress if the note content hash matches the previous note
        // within the dedup window.
        let note_hash = hash_str(note);
        if let (Some(prev_hash), Some(prev_at)) = (self.last_note_hash, self.last_note_hash_at)
            && prev_hash == note_hash
            && now.duration_since(prev_at) < config.dedup_window
        {
            return false;
        }

        true
    }

    /// Record that `note` was emitted now. Must be called immediately after
    /// sending the `AdvisoryNote` event.
    pub fn record_emission(&mut self, note: &str) {
        let now = Instant::now();
        self.last_emission = Some(now);
        self.last_note_hash = Some(hash_str(note));
        self.last_note_hash_at = Some(now);
    }
}

impl Default for EmissionGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract bounded tool-call/result pairs from a session message slice.
///
/// Takes the newest `max_pairs` tool calls, then returns their unambiguous
/// pairs oldest-first. Missing results remain pending; corrupt or ambiguous
/// identities are omitted rather than borrowing another execution's output.
#[must_use]
pub fn extract_tool_call_pairs(messages: &[Message], max_pairs: usize) -> Vec<ToolCallPair> {
    let mut uses = Vec::new();
    let mut use_counts = std::collections::HashMap::new();
    let mut results = std::collections::HashMap::new();

    for msg in messages {
        for block in &msg.content {
            let Some(key) = block.tool_call_key() else {
                continue;
            };
            match block {
                ContentBlock::ToolUse {
                    id, name, input, ..
                } => {
                    let input_str = truncate_with_ellipsis(
                        &serde_json::to_string(input).unwrap_or_default(),
                        MAX_CHARS_PER_PAIR / 2,
                        "…",
                    );
                    *use_counts.entry(key).or_insert(0usize) += 1;
                    uses.push((key, id.as_str(), name.as_str(), input_str));
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => {
                    results
                        .entry(key)
                        .and_modify(|result| *result = None)
                        .or_insert_with(|| {
                            Some((
                                tool_use_id.as_str(),
                                truncate_with_ellipsis(content, MAX_CHARS_PER_PAIR / 2, "…"),
                            ))
                        });
                }
                _ => {}
            }
        }
    }

    let start = uses.len().saturating_sub(max_pairs);
    uses[start..]
        .iter()
        .filter_map(|(key, provider_id, name, input)| {
            if key.as_str().trim().is_empty()
                || provider_id.trim().is_empty()
                || use_counts.get(key) != Some(&1)
            {
                return None;
            }
            let result = match results.get(key) {
                Some(Some((result_provider, content))) if result_provider == provider_id => {
                    content.clone()
                }
                Some(_) => return None,
                None => "(pending)".to_string(),
            };
            Some(ToolCallPair {
                name: (*name).to_string(),
                input_preview: input.clone(),
                result_preview: result,
            })
        })
        .collect()
}

/// Build the user prompt for the advisor from a slice of tool-call pairs.
#[must_use]
pub fn build_advisor_prompt(pairs: &[ToolCallPair]) -> String {
    let mut out = String::from("Recent tool activity to review (oldest → newest):\n\n");
    for (i, pair) in pairs.iter().enumerate() {
        out.push_str(&format!(
            "{}. tool={}\n   input: {}\n   result: {}\n\n",
            i + 1,
            pair.name,
            pair.input_preview,
            pair.result_preview
        ));
    }
    out.push_str(
        "Provide your advisory in one to three sentences, or respond with \"ok\" if nothing notable.",
    );
    out
}

/// Run one advisor review cycle for a completed turn.
///
/// This is the async work dispatched by `spawn_supervised` in the engine. It:
/// 1. Checks whether emission is allowed by `guard`.
/// 2. Extracts bounded tool-call pairs from `messages`.
/// 3. Makes a non-streaming LLM call with a short read-only prompt.
/// 4. Checks emission again (the LLM call may have taken time).
/// 5. Sends `Event::AdvisoryNote` if the note passes the guard.
///
/// All errors are logged and swallowed — the advisor must never fail the
/// parent turn.
pub async fn run_advisor_for_turn(
    turn_id: String,
    messages: Vec<Message>,
    config: AdvisorConfig,
    client: CodewhaleClient,
    route_config: Config,
    session_model: String,
    usage_context: AdvisorUsageContext,
    guard: std::sync::Arc<tokio::sync::Mutex<EmissionGuard>>,
    tx_event: mpsc::Sender<Event>,
) {
    // Pre-flight: skip if the guard already blocks (avoids the LLM call when
    // rate-limited, which is the common case for rapid turn sequences).
    {
        let g = guard.lock().await;
        // We don't have the note content yet, so we only check the rate limit
        // here by testing with a placeholder. The dedup check runs after the
        // LLM call, when we have the actual content.
        if let Some(last) = g.last_emission
            && std::time::Instant::now().duration_since(last) < config.rate_limit
        {
            debug!(target: "advisor", "rate-limited, skipping advisor run for turn {turn_id}");
            return;
        }
    }

    // Extract a bounded slice of tool-call pairs.
    let pairs = extract_tool_call_pairs(&messages, config.max_tool_calls as usize);
    if pairs.is_empty() {
        debug!(target: "advisor", "no tool calls found; skipping advisor for turn {turn_id}");
        return;
    }

    let tool_call_count = pairs.len() as u32;
    let prompt = build_advisor_prompt(&pairs);
    let model = config
        .model
        .clone()
        .unwrap_or_else(|| session_model.clone());

    let (client, model) = match exact_advisor_client(&route_config, client, &session_model, &model)
    {
        Ok(route) => route,
        Err(error) => {
            tracing::warn!(target: "advisor", "advisor route resolution failed for turn {turn_id}: {error}");
            return;
        }
    };
    let route = client.effective_route_envelope(&model, chrono::Utc::now());
    let request = MessageRequest {
        model: model.clone(),
        messages: vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: prompt,
                cache_control: None,
            }],
        }],
        max_tokens: ADVISOR_MAX_TOKENS,
        system: Some(SystemPrompt::Text(ADVISOR_SYSTEM_PROMPT.to_string())),
        tools: None,
        tool_choice: None,
        metadata: None,
        thinking: None,
        // The advisor has a deliberately tiny answer contract. Hidden
        // reasoning would spend that allowance before the note is emitted.
        reasoning_effort: Some("off".to_string()),
        stream: Some(false),
        temperature: None,
        top_p: None,
    };

    let response = match client.create_message(request).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(target: "advisor", "advisor LLM call failed for turn {turn_id}: {e}");
            return;
        }
    };

    // A decoded provider response is billable even when its partial/empty
    // content is rejected below or the emission guard suppresses a duplicate.
    let usage_source_id = format!("advisor:{turn_id}:provider-response:0");
    if response.usage == codewhale_models::Usage::default() {
        usage_context.report_unreceipted(&usage_source_id, &route);
        tracing::warn!(
            target: "advisor",
            "advisor provider response omitted usage for turn {turn_id}; cost coverage is unknown"
        );
    } else {
        usage_context.report(&usage_source_id, &route, &response.usage);
    }

    if codewhale_models::is_incomplete_stop_reason(response.stop_reason.as_deref()) {
        tracing::warn!(
            target: "advisor",
            "advisor response incomplete for turn {turn_id} (stop reason `{}`); dropping partial note",
            codewhale_models::stop_reason_detail(response.stop_reason.as_deref())
        );
        return;
    }

    // Extract the text from the response.
    let note: String = response
        .content
        .iter()
        .filter_map(|block| {
            if let ContentBlock::Text { text, .. } = block {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();

    if note.is_empty() {
        debug!(target: "advisor", "empty advisor response for turn {turn_id}; skipping");
        return;
    }

    // Post-flight emission check (rate limit + dedup).
    let mut guard_lock = guard.lock().await;
    if !guard_lock.may_emit(&note, &config) {
        debug!(target: "advisor", "emission suppressed by guard for turn {turn_id}");
        return;
    }

    guard_lock.record_emission(&note);
    drop(guard_lock);

    let _ = tx_event
        .send(Event::AdvisoryNote {
            turn_id: turn_id.clone(),
            note: note.clone(),
            tool_call_count,
        })
        .await;

    debug!(target: "advisor", "advisory note emitted for turn {turn_id} ({tool_call_count} tool calls reviewed)");
}

fn exact_advisor_client(
    config: &Config,
    parent_client: CodewhaleClient,
    session_model: &str,
    requested_model: &str,
) -> anyhow::Result<(CodewhaleClient, String)> {
    if requested_model
        .trim()
        .eq_ignore_ascii_case(session_model.trim())
    {
        return Ok((parent_client, session_model.trim().to_string()));
    }

    if config.providers.as_ref().is_some_and(|providers| {
        providers.custom.values().any(|provider| {
            provider
                .model
                .as_deref()
                .is_some_and(|model| model.trim().eq_ignore_ascii_case(requested_model.trim()))
        })
    }) {
        anyhow::bail!(
            "advisor model `{}` belongs to a custom provider but no exact provider identity is carried",
            requested_model.trim()
        );
    }

    let active = config
        .active_provider_identity()
        .map_err(anyhow::Error::msg)?;
    let selection =
        crate::model_routing::resolve_explicit_route_with_inventory(config, requested_model);
    let (identity, model) = if let Some(selection) = selection {
        (selection.provider, selection.model)
    } else {
        let candidates =
            crate::model_routing::explicit_route_candidate_providers(config, requested_model);
        if !candidates.is_empty() && !candidates.contains(&active) {
            anyhow::bail!(
                "advisor model `{}` is not owned by the originating provider and has no unique exact route",
                requested_model.trim()
            );
        }
        (active, requested_model.trim().to_string())
    };
    let client =
        crate::route_runtime::resolve_runtime_route_for_identity(config, &identity, Some(&model))
            .map_err(anyhow::Error::msg)?
            .validate()
            .map(|route| route.client)
            .map_err(anyhow::Error::msg)?;
    Ok((client, model))
}

fn hash_str(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_runtime::config::{ProviderConfig, ProvidersConfig};
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config() -> AdvisorConfig {
        AdvisorConfig {
            enabled: true,
            max_tool_calls: 5,
            rate_limit: Duration::from_secs(1),
            dedup_window: Duration::from_secs(10),
            model: None,
        }
    }

    // ── enable/disable ────────────────────────────────────────────────────

    #[test]
    fn disabled_config_has_enabled_false() {
        let cfg = AdvisorConfig::disabled();
        assert!(!cfg.enabled);
    }

    #[test]
    fn from_toml_clamps_max_tool_calls() {
        let toml = AdvisorConfigToml {
            enabled: true,
            max_tool_calls: 999,
            rate_limit_secs: 60,
            dedup_window_secs: 300,
            model: None,
        };
        let cfg = AdvisorConfig::from_toml(&toml);
        assert_eq!(
            cfg.max_tool_calls, 50,
            "max_tool_calls must be clamped to 50"
        );
    }

    #[test]
    fn from_toml_clamps_rate_limit() {
        let toml = AdvisorConfigToml {
            enabled: true,
            max_tool_calls: 10,
            rate_limit_secs: 0, // below minimum of 5
            dedup_window_secs: 300,
            model: None,
        };
        let cfg = AdvisorConfig::from_toml(&toml);
        assert!(
            cfg.rate_limit >= Duration::from_secs(5),
            "rate_limit must be at least 5s"
        );
    }

    // ── bounded input ─────────────────────────────────────────────────────

    fn make_messages_with_n_tool_calls(n: usize) -> Vec<Message> {
        let mut messages = Vec::new();
        for i in 0..n {
            let id = format!("tool_{i}");
            // assistant message with ToolUse
            messages.push(Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    execution_id: None,
                    id: id.clone(),
                    name: "exec_shell".to_string(),
                    input: serde_json::json!({"command": format!("echo {i}")}),
                    caller: None,
                    thought_signature: None,
                }],
            });
            // user message with ToolResult
            messages.push(Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    execution_id: None,
                    tool_use_id: id,
                    content: format!("{i}"),
                    is_error: None,
                    content_blocks: None,
                }],
            });
        }
        messages
    }

    #[test]
    fn extract_tool_call_pairs_bounded_by_max() {
        let messages = make_messages_with_n_tool_calls(20);
        let pairs = extract_tool_call_pairs(&messages, 5);
        assert_eq!(pairs.len(), 5, "must return at most max_pairs");
        // Should be the last 5 (newest).
        assert_eq!(pairs[0].name, "exec_shell");
    }

    #[test]
    fn extract_tool_call_pairs_empty_when_no_tool_calls() {
        let messages = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text {
                text: "hello".to_string(),
                cache_control: None,
            }],
        }];
        let pairs = extract_tool_call_pairs(&messages, 5);
        assert!(pairs.is_empty());
    }

    #[test]
    fn extract_tool_call_pairs_fewer_than_max_returns_all() {
        let messages = make_messages_with_n_tool_calls(3);
        let pairs = extract_tool_call_pairs(&messages, 10);
        assert_eq!(pairs.len(), 3);
    }

    #[test]
    fn extract_tool_call_pairs_keep_execution_and_legacy_domains_distinct() {
        let messages: Vec<Message> = serde_json::from_value(serde_json::json!([
            {"role":"assistant", "content":[{"type":"tool_use", "id":"wire",
                "execution_id":"first", "name":"read_file", "input":{"path":"first.txt"}}]},
            {"role":"user", "content":[{"type":"tool_result", "tool_use_id":"wire",
                "execution_id":"first", "content":"first output"}]},
            {"role":"assistant", "content":[{"type":"tool_use", "id":"wire",
                "execution_id":"second", "name":"read_file", "input":{"path":"second.txt"}}]},
            {"role":"user", "content":[{"type":"tool_result", "tool_use_id":"wire",
                "execution_id":"second", "content":"second output"}]},
            {"role":"assistant", "content":[{"type":"tool_use", "id":"first",
                "name":"read_file", "input":{"path":"legacy.txt"}}]},
            {"role":"user", "content":[{"type":"tool_result", "tool_use_id":"first",
                "content":"legacy output"}]}
        ]))
        .expect("transcript");
        let pairs = extract_tool_call_pairs(&messages, 3);
        assert_eq!(pairs.len(), 3);
        for (pair, path, output) in [
            (&pairs[0], "first.txt", "first output"),
            (&pairs[1], "second.txt", "second output"),
            (&pairs[2], "legacy.txt", "legacy output"),
        ] {
            assert!(pair.input_preview.contains(path));
            assert_eq!(pair.result_preview, output);
        }
        let bounded = extract_tool_call_pairs(&messages, 2);
        assert_eq!(bounded.len(), 2);
        assert_eq!(bounded[0].result_preview, "second output");
        assert_eq!(bounded[1].result_preview, "legacy output");
        assert!(extract_tool_call_pairs(&messages, 0).is_empty());
    }

    #[test]
    fn extract_tool_call_pairs_refuse_corrupt_identity_without_fallback() {
        let call = |execution_id: Option<&str>| {
            serde_json::json!({
                "type":"tool_use", "id":"wire", "execution_id":execution_id,
                "name":"read_file", "input":{"path":"selected.txt"}
            })
        };
        let result = |execution_id: Option<&str>, provider: &str| {
            serde_json::json!({
                "type":"tool_result", "tool_use_id":provider, "execution_id":execution_id,
                "content":"must not borrow this output"
            })
        };
        for (label, calls, results, pending) in [
            (
                "duplicate local calls",
                vec![call(Some("exec")), call(Some("exec"))],
                vec![result(Some("exec"), "wire")],
                false,
            ),
            (
                "duplicate legacy calls",
                vec![call(None), call(None)],
                vec![result(None, "wire")],
                false,
            ),
            (
                "duplicate local results",
                vec![call(Some("exec"))],
                vec![result(Some("exec"), "wire"), result(Some("exec"), "wire")],
                false,
            ),
            (
                "duplicate legacy results",
                vec![call(None)],
                vec![result(None, "wire"), result(None, "wire")],
                false,
            ),
            (
                "wrong provider",
                vec![call(Some("exec"))],
                vec![result(Some("exec"), "other")],
                false,
            ),
            (
                "empty local identity",
                vec![call(Some(""))],
                vec![result(Some(""), "wire")],
                false,
            ),
            (
                "no local fallback",
                vec![call(Some("exec"))],
                vec![result(Some("wrong"), "wire"), result(None, "wire")],
                true,
            ),
            (
                "no legacy fallback",
                vec![call(None)],
                vec![result(Some("wire"), "wire")],
                true,
            ),
            ("missing result", vec![call(Some("exec"))], vec![], true),
        ] {
            let messages: Vec<Message> = serde_json::from_value(serde_json::json!([
                {"role":"assistant", "content":calls}, {"role":"user", "content":results}
            ]))
            .expect("transcript");
            let pairs = extract_tool_call_pairs(&messages, 5);
            assert_eq!(pairs.len(), usize::from(pending), "{label}");
            if pending {
                assert_eq!(pairs[0].result_preview, "(pending)", "{label}");
            }
            assert!(
                !build_advisor_prompt(&pairs).contains("must not borrow"),
                "{label}"
            );
        }
    }

    // ── rate limiting ─────────────────────────────────────────────────────

    #[test]
    fn emission_guard_allows_first_emission() {
        let guard = EmissionGuard::new();
        let config = test_config();
        assert!(
            guard.may_emit("something concerning here", &config),
            "first emission must be allowed"
        );
    }

    #[test]
    fn emission_guard_blocks_immediately_after_emission() {
        let mut guard = EmissionGuard::new();
        let config = test_config();
        let note = "something concerning";
        guard.record_emission(note);
        assert!(
            !guard.may_emit("a completely different note", &config),
            "emission must be blocked immediately after a prior emission (rate limit)"
        );
    }

    #[test]
    fn emission_guard_allows_after_rate_limit_expires() {
        let mut guard = EmissionGuard::new();
        // Rate limit of 0ms — always expired.
        let config = AdvisorConfig {
            rate_limit: Duration::ZERO,
            dedup_window: Duration::from_secs(300),
            ..AdvisorConfig::disabled()
        };
        let note = "first note";
        guard.record_emission(note);
        assert!(
            guard.may_emit("second different note", &config),
            "emission must be allowed when rate limit duration is zero"
        );
    }

    // ── deduplication ─────────────────────────────────────────────────────

    #[test]
    fn emission_guard_suppresses_ok_response() {
        let guard = EmissionGuard::new();
        let config = test_config();
        assert!(!guard.may_emit("ok", &config), "\"ok\" must be suppressed");
        assert!(!guard.may_emit("OK", &config), "\"OK\" must be suppressed");
        assert!(
            !guard.may_emit("  ok  ", &config),
            "\" ok \" must be suppressed"
        );
    }

    #[test]
    fn emission_guard_dedup_blocks_identical_note_within_window() {
        let mut guard = EmissionGuard::new();
        // Use a zero rate limit so only dedup is tested.
        let config = AdvisorConfig {
            rate_limit: Duration::ZERO,
            dedup_window: Duration::from_secs(300),
            ..AdvisorConfig::disabled()
        };
        let note = "risky shell command with no error checking";
        guard.record_emission(note);
        assert!(
            !guard.may_emit(note, &config),
            "identical note must be suppressed within the dedup window"
        );
    }

    #[test]
    fn emission_guard_allows_different_note_within_dedup_window() {
        let mut guard = EmissionGuard::new();
        let config = AdvisorConfig {
            rate_limit: Duration::ZERO,
            dedup_window: Duration::from_secs(300),
            ..AdvisorConfig::disabled()
        };
        guard.record_emission("first note");
        assert!(
            guard.may_emit("entirely different note", &config),
            "a different note must be allowed even within the dedup window"
        );
    }

    // ── child failure isolation ───────────────────────────────────────────

    #[test]
    fn advisor_prompt_is_non_empty_for_non_empty_pairs() {
        let pairs = vec![ToolCallPair {
            name: "exec_shell".to_string(),
            input_preview: r#"{"command":"ls -la"}"#.to_string(),
            result_preview: "total 4\ndrwxr-xr-x 2 user user 4096".to_string(),
        }];
        let prompt = build_advisor_prompt(&pairs);
        assert!(
            prompt.contains("exec_shell"),
            "prompt must include the tool name"
        );
        assert!(
            prompt.contains("ls -la"),
            "prompt must include the tool input"
        );
    }

    #[test]
    fn advisor_model_override_builds_the_owning_provider_client() {
        let config = {
            let __cw_config_init_0_0 = Some("deepseek".to_string());
            let __cw_config_init_0_1 = Some(ProvidersConfig {
                deepseek: ProviderConfig {
                    api_key: Some("sk-deepseek-advisor-test".to_string()),
                    model: Some("deepseek-chat".to_string()),
                    ..ProviderConfig::default()
                },
                zai: ProviderConfig {
                    api_key: Some("zai-advisor-test-key".to_string()),
                    model: Some(
                        codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL.to_string(),
                    ),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_0_value: Config = Config::default();
            __cw_config_init_0_value.provider = __cw_config_init_0_0;
            __cw_config_init_0_value.providers = __cw_config_init_0_1;
            __cw_config_init_0_value
        };
        let parent = CodewhaleClient::new(&config).expect("parent client");
        let (advisor, resolved_model) = exact_advisor_client(
            &config,
            parent,
            "deepseek-chat",
            codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL,
        )
        .expect("cross-provider advisor route");
        let route = advisor.effective_route_envelope(&resolved_model, chrono::Utc::now());

        assert_eq!(route.provider, codewhale_config::ProviderKind::Zai);
        assert_eq!(route.provider_identity, "zai");
        assert_eq!(
            route.model,
            codewhale_config::descriptors::defaults::DEFAULT_ZAI_MODEL
        );
    }

    #[test]
    fn advisor_foreign_custom_override_fails_closed_without_exact_identity() {
        let config = {
            let __cw_config_init_1_0 = Some("deepseek".to_string());
            let __cw_config_init_1_1 = Some(ProvidersConfig {
                deepseek: ProviderConfig {
                    api_key: Some("sk-deepseek-advisor-test".to_string()),
                    model: Some("deepseek-chat".to_string()),
                    ..ProviderConfig::default()
                },
                custom: [(
                    "private-route".to_string(),
                    ProviderConfig {
                        api_key: Some("custom-advisor-test-key".to_string()),
                        base_url: Some("https://custom.invalid/v1".to_string()),
                        model: Some("private-advisor-model".to_string()),
                        ..ProviderConfig::default()
                    },
                )]
                .into_iter()
                .collect(),
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_1_value: Config = Config::default();
            __cw_config_init_1_value.provider = __cw_config_init_1_0;
            __cw_config_init_1_value.providers = __cw_config_init_1_1;
            __cw_config_init_1_value
        };
        let parent = CodewhaleClient::new(&config).expect("parent client");
        let error =
            match exact_advisor_client(&config, parent, "deepseek-chat", "private-advisor-model") {
                Ok(_) => panic!("generic custom kind cannot identify the exact foreign route"),
                Err(error) => error,
            };
        assert!(
            error.to_string().contains("exact provider identity"),
            "{error}"
        );
    }

    #[test]
    fn advisor_named_custom_a_cannot_route_model_owned_by_custom_b() {
        let config = {
            let __cw_config_init_2_0 = Some("custom-a".to_string());
            let __cw_config_init_2_1 = Some(ProvidersConfig {
                custom: [
                    (
                        "custom-a".to_string(),
                        ProviderConfig {
                            api_key: Some("custom-a-advisor-test-key".to_string()),
                            base_url: Some("https://custom-a.invalid/v1".to_string()),
                            model: Some("custom-a-model".to_string()),
                            kind: Some("openai-compatible".to_string()),
                            ..ProviderConfig::default()
                        },
                    ),
                    (
                        "custom-b".to_string(),
                        ProviderConfig {
                            api_key: Some("custom-b-advisor-test-key".to_string()),
                            base_url: Some("https://custom-b.invalid/v1".to_string()),
                            model: Some("custom-b-model".to_string()),
                            kind: Some("openai-compatible".to_string()),
                            ..ProviderConfig::default()
                        },
                    ),
                ]
                .into_iter()
                .collect(),
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_2_value: Config = Config::default();
            __cw_config_init_2_value.provider = __cw_config_init_2_0;
            __cw_config_init_2_value.providers = __cw_config_init_2_1;
            __cw_config_init_2_value
        };
        let parent = CodewhaleClient::new(&config).expect("active custom-a client");
        let error = match exact_advisor_client(&config, parent, "custom-a-model", "custom-b-model")
        {
            Ok(_) => panic!("custom-b must not reuse custom-a's endpoint or credential"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("exact provider identity"),
            "{error}"
        );
    }

    async fn run_billed_advisor_fixture(
        note: &str,
        stop_reason: &str,
        suppress_as_duplicate: bool,
        include_usage: bool,
    ) -> (crate::cost_status::PendingBackgroundCost, Option<Event>) {
        let _scope = crate::cost_status::test_scope();
        let server = MockServer::start().await;
        let mut provider_response = serde_json::json!({
            "id": "advisor-provider-response",
            "model": "deepseek-chat",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": note},
                "finish_reason": stop_reason
            }]
        });
        if include_usage {
            provider_response["usage"] = serde_json::json!({
                "prompt_tokens": 9,
                "completion_tokens": 3,
                "total_tokens": 12
            });
        }
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(provider_response))
            .expect(1)
            .mount(&server)
            .await;
        let route_config = {
            let __cw_config_init_3_0 = Some("deepseek".to_string());
            let __cw_config_init_3_1 = Some(ProvidersConfig {
                deepseek: ProviderConfig {
                    api_key: Some("sk-deepseek-advisor-test".to_string()),
                    model: Some("deepseek-chat".to_string()),
                    ..ProviderConfig::default()
                },
                ..ProvidersConfig::default()
            });
            let mut __cw_config_init_3_value: Config = Config::default();
            __cw_config_init_3_value.provider = __cw_config_init_3_0;
            __cw_config_init_3_value.providers = __cw_config_init_3_1;
            __cw_config_init_3_value
        };
        let mut client = CodewhaleClient::new(&route_config).expect("advisor client");
        client.set_test_chat_transport_base_url(server.uri());
        let mut emission_guard = EmissionGuard::new();
        if suppress_as_duplicate {
            emission_guard.record_emission(note);
        }
        let guard = std::sync::Arc::new(tokio::sync::Mutex::new(emission_guard));
        let (tx, mut rx) = mpsc::channel(1);
        run_advisor_for_turn(
            "advisor-turn".to_string(),
            make_messages_with_n_tool_calls(1),
            AdvisorConfig {
                enabled: true,
                max_tool_calls: 5,
                rate_limit: Duration::ZERO,
                dedup_window: Duration::from_secs(60),
                model: None,
            },
            client,
            route_config,
            "deepseek-chat".to_string(),
            AdvisorUsageContext::capture(None),
            guard,
            tx,
        )
        .await;
        (crate::cost_status::drain(), rx.try_recv().ok())
    }

    #[tokio::test]
    async fn advisor_incomplete_and_dedup_suppressed_responses_are_each_billed_once() {
        let (incomplete, incomplete_event) =
            run_billed_advisor_fixture("partial note", "max_tokens", false, true).await;
        assert!(incomplete_event.is_none());
        assert_eq!(
            incomplete
                .priced_turns
                .saturating_add(incomplete.unpriced_turns),
            1
        );

        let (dedup, dedup_event) =
            run_billed_advisor_fixture("same advisory", "stop", true, true).await;
        assert!(dedup_event.is_none());
        assert_eq!(dedup.priced_turns.saturating_add(dedup.unpriced_turns), 1);
    }

    #[tokio::test]
    async fn advisor_provider_success_without_usage_marks_unknown_once() {
        let (pending, event) =
            run_billed_advisor_fixture("use a smaller focused slice", "stop", false, false).await;

        assert!(
            event.is_some(),
            "the semantic advisor response remains usable"
        );
        assert_eq!(pending.priced_turns, 0);
        assert_eq!(pending.unpriced_turns, 1);
        assert_eq!(pending.cny_unpriced_turns, 1);
        assert!(
            pending
                .unpriced_reasons
                .contains("provider_success_missing_usage")
        );
    }
}

//! Three-zone prompt contract types for prefix-cache stability (#2264).
//!
//! Divides every request into three rigid zones:
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │ PinnedPrefix (frozen after construction) │ ← system prompt + tool catalog
//! │   combined_sha256 computed at freeze()   │   cache hit candidate
//! ├─────────────────────────────────────────┤
//! │ AppendLog (append-only)                  │ ← conversation history
//! │   push() only, no insert / remove / edit │   preserves prefix of prior turns
//! ├─────────────────────────────────────────┤
//! │ TurnScratch (ephemeral)                  │ ← per-turn metadata
//! │   cleared at every turn boundary         │   the only new content per request
//! └─────────────────────────────────────────┘
//! ```
//!
//! ## Status (Phase 1 foundation)
//!
//! `PinnedPrefix` / `FrozenPrefix` / `PrefixDrift` are ready for use.
//! `AppendLog` / `TurnScratch` / `ThreeZoneRequest` are type scaffolding
//! for future phases — not yet wired into the request path.

use codewhale_models::Role;
use codewhale_models::{Message, SystemPrompt, Tool};
use std::sync::Arc;
// ── helpers ────────────────────────────────────────────────────────────

fn sha256_hex(bytes: &[u8]) -> String {
    crate::hashing::sha256_hex(bytes)
}

fn system_text(system: Option<&SystemPrompt>) -> String {
    match system {
        Some(SystemPrompt::Text(text)) => text.clone(),
        Some(SystemPrompt::Blocks(blocks)) => {
            let mut text = String::new();
            for block in blocks {
                text.push_str(&block.text);
                text.push('\n');
            }
            text
        }
        None => String::new(),
    }
}

/// Serialize tools to a deterministic, sorted JSON string for hashing.
fn tool_catalog_digest(tools: &[Tool]) -> String {
    let mut serialized: Vec<String> = tools
        .iter()
        .filter_map(|t| serde_json::to_string(t).ok())
        .collect();
    serialized.sort();
    serialized.join("\n")
}

fn combined_hash(system_text: &str, tools: &[Tool]) -> String {
    let system_sha = sha256_hex(system_text.as_bytes());
    let tools_digest = tool_catalog_digest(tools);
    let tools_sha = sha256_hex(tools_digest.as_bytes());
    let combined = format!("{system_sha}:{tools_sha}");
    sha256_hex(combined.as_bytes())
}

// ── FrozenPrefix ───────────────────────────────────────────────────────

/// An immutable frozen prefix — system prompt text + tool catalog,
/// hashed at freeze time. The hash is stable as long as the system prompt
/// text and full tool definitions (name, description, schema) are unchanged.
///
/// Use [`PinnedPrefix::freeze`] to produce one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrozenPrefix {
    pub(crate) system_text: String,
    pub(crate) tool_catalog: String,
    pub(crate) combined_sha256: String,
}

impl FrozenPrefix {
    /// Verify that `current_system_text` and `current_tools` match the frozen
    /// prefix. Returns `Ok(())` when stable, `Err(PrefixDrift)` on mismatch.
    ///
    /// Fast path: compares raw text before falling back to SHA-256.
    pub fn verify(
        &self,
        current_system_text: &str,
        current_tools: &[Tool],
    ) -> Result<(), PrefixDrift> {
        let system_changed = current_system_text != self.system_text;
        let current_tool_catalog = tool_catalog_digest(current_tools);
        let tools_changed = current_tool_catalog != self.tool_catalog;

        if !system_changed && !tools_changed {
            return Ok(());
        }

        let current_hash = combined_hash(current_system_text, current_tools);
        Err(PrefixDrift {
            system_changed,
            tools_changed,
            frozen_hash: self.combined_sha256.clone(),
            current_hash,
        })
    }

    /// Returns a short (12-char) human-readable id for display.
    #[must_use]
    pub fn short_id(&self) -> &str {
        if self.combined_sha256.len() >= 12 {
            &self.combined_sha256[..12]
        } else {
            &self.combined_sha256
        }
    }

    /// Returns the full combined SHA-256.
    #[must_use]
    pub fn hash(&self) -> &str {
        &self.combined_sha256
    }
}

// ── PinnedPrefix ───────────────────────────────────────────────────────

/// A mutable prefix builder. Construct from the system prompt and tool
/// catalog, then call [`freeze`](Self::freeze) to produce a [`FrozenPrefix`].
#[derive(Debug, Clone)]
pub struct PinnedPrefix {
    system_text: String,
    tools: Vec<Tool>,
}

impl PinnedPrefix {
    #[must_use]
    pub fn new(system: Option<&SystemPrompt>, tools: Vec<Tool>) -> Self {
        Self {
            system_text: system_text(system),
            tools,
        }
    }

    /// Freeze this prefix into an immutable [`FrozenPrefix`].
    #[must_use]
    pub fn freeze(&self) -> FrozenPrefix {
        let tool_catalog = tool_catalog_digest(&self.tools);
        let combined_sha256 = combined_hash(&self.system_text, &self.tools);

        FrozenPrefix {
            system_text: self.system_text.clone(),
            tool_catalog,
            combined_sha256,
        }
    }
}

// ── PrefixDrift ────────────────────────────────────────────────────────

/// Describes how the current prefix differs from the frozen baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixDrift {
    pub system_changed: bool,
    pub tools_changed: bool,
    pub frozen_hash: String,
    pub current_hash: String,
}

impl std::fmt::Display for PrefixDrift {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cause = match (self.system_changed, self.tools_changed) {
            (true, true) => "system prompt and tool set",
            (true, false) => "system prompt",
            (false, true) => "tool set",
            (false, false) => "unknown component",
        };
        write!(
            f,
            "prefix drift: {cause} changed (frozen={}, current={})",
            &self.frozen_hash[..12.min(self.frozen_hash.len())],
            &self.current_hash[..12.min(self.current_hash.len())]
        )
    }
}

/// Exact header of the parent turn's latest model request, shared with
/// fork children so a same-route child can extend the parent's live
/// cached prefix instead of starting cold.
///
/// Written once per model request by the turn loop (after the request
/// is final), read once per fork spawn. `None` until the first request
/// of the session goes out. The history half of the prefix rides
/// `SubAgentForkContext.messages` (turn-start snapshot); the system and
/// tools here must byte-match a cached request or inheritance silently
/// costs more than a cold start — every gate below fails closed.
#[derive(Debug, Clone)]
pub struct LiveHeaderSnapshot<R = ()> {
    /// Exact system prompt, shape-preserving (`Text` stays `Text`).
    pub system: Option<SystemPrompt>,
    /// Wire-order canonical tools JSON (see [`wire_tool_catalog_json`]).
    pub tools_json: String,
    /// Canonical ordered tool names used to match the child's already-active
    /// policy-filtered tools without activating additional schemas.
    pub active_names: Vec<String>,
    /// Route the request was built for (caches are per route).
    pub model: String,
    pub provider: codewhale_config::ProviderKind,
    pub provider_identity: String,
    pub endpoint_fingerprint: Option<String>,
    pub route_receipt: Option<R>,
    /// Request messages the hot proof covers. Each new request must
    /// start with these byte-for-byte (element-wise) to keep the
    /// proof; compaction or any rewrite resets to cold.
    pub history: Vec<Message>,
    /// Provider-reported cached-input tokens for this logical request, carried
    /// across equal headers and append-only history with the same opaque route.
    /// Positive usage permits matching prefix reuse; it does not establish
    /// which components or the entire history were cache hits.
    pub last_hit_tokens: Option<u32>,
}

/// Cloneable handle to the turn's live-header cell. Created once per
/// Engine (turns run strictly one at a time); fork contexts clone the
/// `Arc` and read it at spawn.
pub type SharedLiveHeader<R = ()> = Arc<std::sync::Mutex<Option<LiveHeaderSnapshot<R>>>>;

/// Empty live-header cell: nothing cached yet, every gate reads cold.
#[must_use]
pub fn new_live_header_cell<R>() -> SharedLiveHeader<R> {
    Arc::new(std::sync::Mutex::new(None))
}

/// Wire-canonical tools serialization: per-tool chat-API JSON in wire
/// order, joined by `\n`. Each segment goes through
/// [`codewhale_core::prefix_cache::tool_to_api_json`], so internal-only
/// fields (`defer_loading`, `allowed_callers`, …) that never reach the
/// provider cannot false-negative a byte comparison. Unlike
/// [`tool_catalog_digest`] (sorted for hashing), order is part of the
/// wire bytes. `None` if any tool fails to serialize; the inherit gate
/// treats that as a mismatch.
#[must_use]
pub fn wire_tool_catalog_json(tools: &[Tool]) -> Option<String> {
    let mut out = String::new();
    for (index, tool) in tools.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        out.push_str(&codewhale_core::prefix_cache::tool_to_api_json(tool)?);
    }
    Some(out)
}

/// Trial gate: `CODEWHALE_FORK_INHERIT=off` disables prefix inheritance
/// (same-binary A/B against the default-on arm). Process-pinned.
#[must_use]
pub fn fork_inherit_enabled() -> bool {
    static GATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GATE.get_or_init(|| {
        std::env::var("CODEWHALE_FORK_INHERIT")
            .map(|raw| raw.trim() != "off")
            .unwrap_or(true)
    })
}

/// Trial diagnostics: `CODEWHALE_FORK_TRIAL=1` mirrors fork-inherit trial
/// events (decision + child first-response cache split) to stderr as
/// single-line JSON. Headless `exec` installs no tracing subscriber, so
/// the `tracing` trial logs are invisible there; this is the metered
/// readout for same-binary A/B trials. Off unless explicitly enabled.
#[must_use]
pub fn fork_trial_diag_enabled() -> bool {
    static GATE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GATE.get_or_init(|| {
        std::env::var("CODEWHALE_FORK_TRIAL")
            .map(|raw| raw.trim() == "1")
            .unwrap_or(false)
    })
}

/// Hot-proof carry-over for the per-request snapshot write.
///
/// Tool calls (including fork spawns) execute before the current
/// request's usage lands, so a snapshot that reset its proof on every
/// write would read cold at every spawn. The new request keeps the old
/// proof only when it extends the old head byte-for-byte: same system,
/// same wire tools, messages grown append-only. Drift, compaction, or
/// any rewrite resets to `None`. Fails closed throughout.
#[must_use]
pub fn carry_hot_forward<R: PartialEq>(
    old: Option<&LiveHeaderSnapshot<R>>,
    new_system: &Option<SystemPrompt>,
    new_tools_json: &str,
    new_history: &[Message],
    new_route_receipt: Option<&R>,
) -> Option<u32> {
    let old = old?;
    if new_route_receipt.is_none() || old.route_receipt.as_ref() != new_route_receipt {
        return None;
    }
    let hits = old.last_hit_tokens.filter(|hits| *hits > 0)?;
    if &old.system != new_system {
        return None;
    }
    if old.tools_json != new_tools_json {
        return None;
    }
    if !new_history.starts_with(&old.history) {
        return None;
    }
    Some(hits)
}

/// Fork-inherit decision with a trial-log reason and, when inheriting,
/// the head of the parent's wire block the child reproduces byte-for-byte.
#[derive(Debug, Clone, PartialEq)]
pub struct ForkInheritDecision {
    pub inherit: bool,
    pub reason: &'static str,
    /// Longest matching canonical tool head from the child's policy-filtered
    /// active catalog. It determines ordering, never grants or removes tools.
    /// Empty unless `inherit` is true.
    pub prefix_tools: Vec<Tool>,
}

/// Pure inherit gate with graceful prefix tiers.
///
/// Full-block equality almost never holds in production: the parent's
/// wire block grows by deferred discovery while the child's grant is
/// fixed and usually narrower. Instead the gate resolves the longest
/// head of the parent's wire block the child reproduces byte-for-byte
/// (same actual route receipt and positive parent cache usage required):
/// `inherited_full` matches all parent tool definitions; `inherited_prefix`
/// matches a shorter head. Neither outcome proves full history cache coverage
/// or provider savings. No common head and every guard failure stay cold.
///
/// The grant boundary enforces itself through catalog lookup: names
/// outside the child's grant end the run, so narrowed roles inherit a
/// shorter head instead of cold-starting — no role table. `gate_on` is
/// [`fork_inherit_enabled`] read by the caller, so every arm stays
/// unit-testable.
#[must_use]
#[allow(clippy::too_many_arguments)]
pub fn resolve_fork_inherit<R: PartialEq>(
    gate_on: bool,
    snapshot: Option<&LiveHeaderSnapshot<R>>,
    child_model: &str,
    child_provider: codewhale_config::ProviderKind,
    child_provider_identity: &str,
    child_wire_format: codewhale_config::provider::WireFormat,
    child_endpoint_fingerprint: Option<&str>,
    child_route_receipt: Option<&R>,
    child_catalog: &[Tool],
    history_len: usize,
) -> ForkInheritDecision {
    let cold = |reason: &'static str| ForkInheritDecision {
        inherit: false,
        reason,
        prefix_tools: Vec::new(),
    };
    if !gate_on {
        return cold("gate_off");
    }
    if child_wire_format != codewhale_config::provider::WireFormat::ChatCompletions {
        return cold("wire_format_unsupported");
    }
    let Some(snapshot) = snapshot else {
        return cold("no_snapshot");
    };
    if snapshot.system.is_none() {
        return cold("no_parent_system");
    }
    if snapshot.model != child_model {
        return cold("model_mismatch");
    }
    if snapshot.provider != child_provider {
        return cold("provider_mismatch");
    }
    if snapshot.provider_identity != child_provider_identity {
        return cold("route_mismatch");
    }
    if snapshot.endpoint_fingerprint.as_deref() != child_endpoint_fingerprint
        || child_endpoint_fingerprint.is_none()
    {
        return cold("endpoint_mismatch");
    }
    if child_route_receipt.is_none() || snapshot.route_receipt.as_ref() != child_route_receipt {
        return cold("route_receipt_mismatch");
    }
    if snapshot.last_hit_tokens.unwrap_or(0) == 0 {
        return cold("prefix_cold");
    }
    if history_len == 0 {
        return cold("empty_history");
    }
    if snapshot.active_names.is_empty() {
        return cold("no_parent_tools");
    }
    let parent_wire: Vec<&str> = snapshot.tools_json.split('\n').collect();
    let mut prefix = Vec::new();
    for (index, name) in snapshot.active_names.iter().enumerate() {
        let Some(parent_json) = parent_wire.get(index) else {
            break;
        };
        let Some(tool) = child_catalog.iter().find(|tool| &tool.name == name) else {
            break;
        };
        let tool = tool.clone();
        let wire = codewhale_core::prefix_cache::tool_to_api_json(&tool);
        if wire.as_deref() != Some(*parent_json) {
            break;
        }
        prefix.push(tool);
    }
    if prefix.is_empty() {
        return cold("tools_mismatch");
    }
    let full = prefix.len() == snapshot.active_names.len();
    ForkInheritDecision {
        inherit: true,
        reason: if full {
            "inherited_full"
        } else {
            "inherited_prefix"
        },
        prefix_tools: prefix,
    }
}

// ── AppendLog ──────────────────────────────────────────────────────────

/// Append-only conversation history. Derefs to `&[Message]` via
/// [`Deref`](std::ops::Deref) for transparent read access; mutations go
/// through explicit methods (`push`, `truncate_to`, `trim_front`, `clear`)
/// whose names make cache impact obvious.
///
/// Phase 4: backing store for `Session.messages` (#2264).
///
/// The history is reference-counted (#6214 T2): snapshots hand out `Arc`
/// clones instead of deep-copying the transcript per event, and mutations
/// copy-on-write only while a snapshot is outstanding.
#[derive(Debug, Clone)]
pub struct AppendLog {
    messages: Arc<Vec<Message>>,
}

impl AppendLog {
    pub fn new() -> Self {
        Self {
            messages: Arc::new(Vec::new()),
        }
    }

    pub fn from_messages(messages: Vec<Message>) -> Self {
        Self {
            messages: Arc::new(messages),
        }
    }

    /// Share the current history without copying. The engine hands this to
    /// `Event::SessionUpdated`; the `Arc` is immutable, so an outstanding
    /// snapshot can never observe a later mutation.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Vec<Message>> {
        Arc::clone(&self.messages)
    }

    /// Append a message to the log. A single-message push is the cheapest
    /// mutation for prefix-cache stability — it extends the byte sequence
    /// without disturbing earlier turns.
    pub fn push(&mut self, message: Message) {
        Arc::make_mut(&mut self.messages).push(message);
    }

    /// Append multiple messages in one operation (fewer cache-line
    /// invalidations than repeated `push`).
    pub fn push_batch(&mut self, batch: Vec<Message>) {
        Arc::make_mut(&mut self.messages).extend(batch);
    }

    /// Truncate to keep only the first `new_len` messages.
    /// Discards newer messages (and their prefix-cache contribution)
    /// from the tail.
    pub fn truncate_to(&mut self, new_len: usize) {
        Arc::make_mut(&mut self.messages).truncate(new_len);
    }

    /// Remove `count` messages from the front (oldest first).
    /// Cache-destroying: drops the prefix that earlier turns share.
    pub fn trim_front(&mut self, count: usize) {
        let messages = Arc::make_mut(&mut self.messages);
        if count >= messages.len() {
            messages.clear();
        } else {
            messages.drain(0..count);
        }
    }

    /// Remove all messages. Resets cache state completely.
    pub fn clear(&mut self) {
        Arc::make_mut(&mut self.messages).clear();
    }

    /// Return a mutable reference to the last message, if any.
    /// Prefer this over `last_mut()` on the inner vec — the name signals
    /// that only the most recent turn's content is being modified.
    #[must_use]
    pub fn last_mut(&mut self) -> Option<&mut Message> {
        Arc::make_mut(&mut self.messages).last_mut()
    }

    /// Consume and return the inner `Vec<Message>`, copying only if a
    /// snapshot still shares it.
    #[must_use]
    pub fn into_inner(self) -> Vec<Message> {
        Arc::try_unwrap(self.messages).unwrap_or_else(|shared| (*shared).clone())
    }
}

impl Default for AppendLog {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Vec<Message>> for AppendLog {
    fn from(messages: Vec<Message>) -> Self {
        Self {
            messages: Arc::new(messages),
        }
    }
}

impl From<AppendLog> for Vec<Message> {
    fn from(log: AppendLog) -> Self {
        log.into_inner()
    }
}

impl std::ops::Deref for AppendLog {
    type Target = Vec<Message>;

    fn deref(&self) -> &Self::Target {
        &self.messages
    }
}

// ── TurnScratch ────────────────────────────────────────────────────────

/// Per-turn ephemeral data. Cleared at every turn boundary.
///
/// **Phase 1 scaffolding** — not yet wired into the engine request path.
/// `allow`, not `expect`: its only non-test use is a field of the dead
/// `ThreeZoneRequest`, which rustc 1.89 counts as a use and newer rustc does
/// not, so an expectation is unfulfilled on the MSRV (#6543).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Default)]
pub struct TurnScratch {
    pub working_set: Vec<String>,
    pub user_message: Option<Message>,
}

impl TurnScratch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn clear(&mut self) {
        self.working_set.clear();
        self.user_message = None;
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.working_set.is_empty() && self.user_message.is_none()
    }
}

// ── ThreeZoneRequest ───────────────────────────────────────────────────

/// A composed three-zone request ready for DeepSeek API serialization.
///
/// **Phase 1 scaffolding** — not yet wired into the engine request path.
/// Currently the engine continues to use `MessageRequest` directly.
#[derive(Debug, Clone)]
pub struct ThreeZoneRequest<'a> {
    pub prefix: &'a FrozenPrefix,
    pub log: &'a AppendLog,
    pub scratch: TurnScratch,
    pub model: String,
    pub max_tokens: u32,
    pub system: Option<SystemPrompt>,
    pub tools: Option<Vec<Tool>>,
    pub tool_choice: Option<serde_json::Value>,
    pub reasoning_effort: Option<String>,
    pub thinking: Option<serde_json::Value>,
    pub stream: Option<bool>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub metadata: Option<serde_json::Value>,
}

impl<'a> ThreeZoneRequest<'a> {
    /// Build the full message list from system prompt, append-log messages,
    /// and scratch user message. The returned vector is serialized as the
    /// `messages` field in the DeepSeek chat-completion request.
    #[must_use]
    pub fn build_messages(&self) -> Vec<Message> {
        let mut messages = Vec::with_capacity(self.message_count());

        match self.system.as_ref() {
            Some(SystemPrompt::Text(text)) => {
                messages.push(Message {
                    role: Role::System,
                    content: vec![codewhale_models::ContentBlock::Text {
                        text: text.clone(),
                        cache_control: None,
                    }],
                });
            }
            Some(SystemPrompt::Blocks(blocks)) => {
                let content: Vec<codewhale_models::ContentBlock> = blocks
                    .iter()
                    .map(|block| codewhale_models::ContentBlock::Text {
                        text: block.text.clone(),
                        cache_control: block.cache_control.clone(),
                    })
                    .collect();
                messages.push(Message {
                    role: Role::System,
                    content,
                });
            }
            None => {}
        }

        for msg in self.log.iter() {
            messages.push(msg.clone());
        }

        if let Some(ref user_msg) = self.scratch.user_message {
            messages.push(user_msg.clone());
        }

        messages
    }

    #[must_use]
    pub fn message_count(&self) -> usize {
        let system_count = if self.system.is_some() { 1 } else { 0 };
        let scratch_count = if self.scratch.user_message.is_some() {
            1
        } else {
            0
        };
        system_count + self.log.len() + scratch_count
    }
}

// ── tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use codewhale_models::ContentBlock;

    fn make_tool(name: &str) -> Tool {
        Tool {
            name: name.to_string(),
            description: String::new(),
            input_schema: serde_json::Value::Null,
            tool_type: None,
            allowed_callers: None,
            defer_loading: None,
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    fn make_message(role: &str, text: &str) -> Message {
        Message {
            role: Role::from(role),
            content: vec![ContentBlock::Text {
                text: text.to_string(),
                cache_control: None,
            }],
        }
    }

    // ── AppendLog ────────────────────────────────────────────────

    #[test]
    fn append_log_snapshot_shares_and_mutation_detaches() {
        let mut log = AppendLog::new();
        log.push(make_message("user", "hello"));
        let shared = log.snapshot();
        // No copy: the snapshot aliases the live log.
        assert!(Arc::ptr_eq(&shared, &log.snapshot()));
        log.push(make_message("assistant", "hi"));
        // Copy-on-write: the outstanding snapshot still sees one message.
        assert_eq!(shared.len(), 1);
        assert_eq!(log.len(), 2);
    }

    // ── FrozenPrefix / PinnedPrefix ────────────────────────────────

    #[test]
    fn freeze_produces_stable_hash() {
        let tools = vec![make_tool("read"), make_tool("write")];
        let sys = SystemPrompt::Text("hello world".to_string());

        let a = PinnedPrefix::new(Some(&sys), tools.clone()).freeze();
        let b = PinnedPrefix::new(Some(&sys), tools).freeze();

        assert_eq!(a.combined_sha256, b.combined_sha256);
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a.short_id(), b.short_id());
    }

    #[test]
    fn freeze_tool_order_is_stable() {
        let sys = SystemPrompt::Text("system".to_string());
        let tools_a = vec![make_tool("b"), make_tool("a")];
        let tools_b = vec![make_tool("a"), make_tool("b")];

        let a = PinnedPrefix::new(Some(&sys), tools_a).freeze();
        let b = PinnedPrefix::new(Some(&sys), tools_b).freeze();

        assert_eq!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn freeze_empty_tools() {
        let sys = SystemPrompt::Text("system".to_string());
        let frozen = PinnedPrefix::new(Some(&sys), vec![]).freeze();
        assert!(frozen.tool_catalog.is_empty());
        assert!(!frozen.combined_sha256.is_empty());
        assert_eq!(frozen.short_id().len(), 12);
    }

    #[test]
    fn freeze_no_system() {
        let tools = vec![make_tool("t1")];
        let frozen = PinnedPrefix::new(None, tools).freeze();
        assert!(frozen.system_text.is_empty());
        assert!(frozen.tool_catalog.contains("t1"));
    }

    #[test]
    fn verify_passes_when_stable() {
        let sys = SystemPrompt::Text("system".to_string());
        let tools = vec![make_tool("a")];
        let frozen = PinnedPrefix::new(Some(&sys), tools.clone()).freeze();

        assert!(frozen.verify("system", &tools).is_ok());
    }

    #[test]
    fn verify_detects_system_change() {
        let sys = SystemPrompt::Text("old".to_string());
        let tools = vec![make_tool("a")];
        let frozen = PinnedPrefix::new(Some(&sys), tools.clone()).freeze();

        let drift = frozen.verify("new", &tools).unwrap_err();
        assert!(drift.system_changed);
        assert!(!drift.tools_changed);
    }

    #[test]
    fn verify_detects_tool_change() {
        let sys = SystemPrompt::Text("system".to_string());
        let tools_a = vec![make_tool("a")];
        let frozen = PinnedPrefix::new(Some(&sys), tools_a).freeze();

        let tools_b = vec![make_tool("b")];
        let drift = frozen.verify("system", &tools_b).unwrap_err();
        assert!(!drift.system_changed);
        assert!(drift.tools_changed);
    }

    #[test]
    fn verify_detects_both_changes() {
        let sys = SystemPrompt::Text("old".to_string());
        let tools = vec![make_tool("a")];
        let frozen = PinnedPrefix::new(Some(&sys), tools).freeze();

        let drift = frozen.verify("new", &[make_tool("b")]).unwrap_err();
        assert!(drift.system_changed);
        assert!(drift.tools_changed);
    }

    #[test]
    fn verify_detects_schema_change() {
        let sys = SystemPrompt::Text("system".to_string());
        let tool_a = make_tool("a");
        let mut tool_a_v2 = make_tool("a");
        tool_a_v2.description = "updated desc".to_string();

        let frozen = PinnedPrefix::new(Some(&sys), vec![tool_a]).freeze();
        let drift = frozen.verify("system", &[tool_a_v2]).unwrap_err();
        // Same name, different schema — should detect the change.
        assert!(drift.tools_changed);
    }

    #[test]
    fn prefix_drift_display_is_readable() {
        let drift = PrefixDrift {
            system_changed: true,
            tools_changed: false,
            frozen_hash: "a".repeat(64),
            current_hash: "b".repeat(64),
        };
        let display = drift.to_string();
        assert!(display.contains("system prompt"));
        assert!(display.contains("aaaaaaaaaaaa"));
        assert!(display.contains("bbbbbbbbbbbb"));
    }

    // ── AppendLog ─────────────────────────────────────────────────

    #[test]
    fn append_log_push_and_iter() {
        let mut log = AppendLog::new();
        assert!(log.is_empty());

        log.push(make_message("user", "hello"));
        log.push(make_message("assistant", "hi"));

        assert_eq!(log.len(), 2);
        assert!(!log.is_empty());

        let messages: Vec<_> = log.iter().collect();
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn append_log_from_messages() {
        let msgs = vec![make_message("user", "a"), make_message("assistant", "b")];
        let log = AppendLog::from_messages(msgs);
        assert_eq!(log.len(), 2);
        assert_eq!(log.as_slice().len(), 2);
    }

    // ── TurnScratch ───────────────────────────────────────────────

    #[test]
    fn scratch_clear_empties_all_fields() {
        let mut scratch = TurnScratch::new();
        scratch.working_set.push("file.rs".to_string());
        scratch.user_message = Some(make_message("user", "task"));

        assert!(!scratch.is_empty());
        scratch.clear();
        assert!(scratch.is_empty());
        assert!(scratch.working_set.is_empty());
        assert!(scratch.user_message.is_none());
    }

    // ── ThreeZoneRequest ──────────────────────────────────────────

    #[test]
    fn build_messages_concatenates_zones() {
        let sys = SystemPrompt::Text("you are helpful".to_string());
        let tools = vec![make_tool("read")];
        let prefix = PinnedPrefix::new(Some(&sys), tools).freeze();

        let mut log = AppendLog::new();
        log.push(make_message("user", "prev question"));
        log.push(make_message("assistant", "prev answer"));

        let scratch = TurnScratch {
            working_set: vec!["main.rs".to_string()],
            user_message: Some(make_message("user", "current task")),
        };

        let request = ThreeZoneRequest {
            prefix: &prefix,
            log: &log,
            scratch,
            model: "deepseek-v4-pro".to_string(),
            max_tokens: 4096,
            system: Some(sys),
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            thinking: None,
            stream: None,
            temperature: None,
            top_p: None,
            metadata: None,
        };

        let messages = request.build_messages();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "system");
        assert_eq!(messages[1].role, "user");
        assert_eq!(messages[2].role, "assistant");
        assert_eq!(messages[3].role, "user");
        assert_eq!(request.message_count(), 4);
    }

    #[test]
    fn build_messages_no_system_no_scratch() {
        let prefix = PinnedPrefix::new(None, vec![]).freeze();

        let mut log = AppendLog::new();
        log.push(make_message("user", "hi"));

        let request = ThreeZoneRequest {
            prefix: &prefix,
            log: &log,
            scratch: TurnScratch::new(),
            model: "x".to_string(),
            max_tokens: 1,
            system: None,
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            thinking: None,
            stream: None,
            temperature: None,
            top_p: None,
            metadata: None,
        };

        let messages = request.build_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(request.message_count(), 1);
    }

    #[test]
    fn blocks_system_prompt_preserves_cache_control() {
        use codewhale_models::{CacheControl, SystemBlock};
        let cc = Some(CacheControl {
            cache_type: "ephemeral".to_string(),
        });
        let blocks = SystemPrompt::Blocks(vec![SystemBlock {
            block_type: "text".to_string(),
            text: "hello".to_string(),
            cache_control: cc.clone(),
        }]);

        let prefix = PinnedPrefix::new(Some(&blocks), vec![]).freeze();
        let log = AppendLog::new();
        let scratch = TurnScratch::new();
        let request = ThreeZoneRequest {
            prefix: &prefix,
            log: &log,
            scratch,
            model: "x".to_string(),
            max_tokens: 1,
            system: Some(blocks),
            tools: None,
            tool_choice: None,
            reasoning_effort: None,
            thinking: None,
            stream: None,
            temperature: None,
            top_p: None,
            metadata: None,
        };

        let messages = request.build_messages();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, "system");
        // cache_control should be preserved on the block.
        if let ContentBlock::Text {
            cache_control: actual_cc,
            ..
        } = &messages[0].content[0]
        {
            assert_eq!(
                actual_cc.as_ref().map(|c| c.cache_type.as_str()),
                Some("ephemeral")
            );
        } else {
            panic!("expected Text content block");
        }
    }

    fn inherit_snapshot() -> LiveHeaderSnapshot<&'static str> {
        let tools = vec![make_tool("read"), make_tool("agent")];
        LiveHeaderSnapshot {
            system: Some(SystemPrompt::Text("parent system".to_string())),
            tools_json: wire_tool_catalog_json(&tools).expect("serializes"),
            active_names: tools.iter().map(|tool| tool.name.clone()).collect(),
            model: "deepseek-chat".to_string(),
            provider: codewhale_config::ProviderKind::Deepseek,
            provider_identity: "deepseek".to_string(),
            endpoint_fingerprint: Some("fixture-endpoint".to_string()),
            route_receipt: Some("fixture-route"),
            history: vec![make_message("user", "hi")],
            last_hit_tokens: Some(1200),
        }
    }

    #[test]
    fn wire_tool_json_is_order_sensitive_and_stable() {
        let pair = [make_tool("a"), make_tool("b")];
        let swapped = [make_tool("b"), make_tool("a")];
        let forward = wire_tool_catalog_json(&pair).expect("serializes");
        let backward = wire_tool_catalog_json(&swapped).expect("serializes");
        assert_ne!(forward, backward, "wire order is part of the bytes");
        assert_eq!(
            forward,
            wire_tool_catalog_json(&pair).expect("stable"),
            "same order serializes identically"
        );
        assert_eq!(wire_tool_catalog_json(&[]).expect("empty"), "");
        // Internal-only flags never reach the wire bytes.
        let mut flagged = make_tool("a");
        flagged.defer_loading = Some(true);
        flagged.allowed_callers = Some(vec!["x".to_string()]);
        assert_eq!(
            wire_tool_catalog_json(&[flagged]).expect("serializes"),
            wire_tool_catalog_json(&[make_tool("a")]).expect("serializes"),
        );
    }

    #[test]
    fn fork_inherit_requires_every_gate() {
        let snapshot = inherit_snapshot();
        let full_catalog = vec![make_tool("read"), make_tool("agent")];
        let decide =
            |snapshot: Option<&LiveHeaderSnapshot<&'static str>>, model: &str, catalog: &[Tool]| {
                resolve_fork_inherit(
                    true,
                    snapshot,
                    model,
                    codewhale_config::ProviderKind::Deepseek,
                    "deepseek",
                    codewhale_config::provider::WireFormat::ChatCompletions,
                    Some("fixture-endpoint"),
                    Some(&"fixture-route"),
                    catalog,
                    4,
                )
            };
        let ok = decide(Some(&snapshot), "deepseek-chat", &full_catalog);
        assert!(ok.inherit, "{ok:?}");
        assert_eq!(ok.reason, "inherited_full");
        assert_eq!(ok.prefix_tools.len(), 2);
        let mut missing_receipt = snapshot.clone();
        missing_receipt.route_receipt = None;
        assert_eq!(
            decide(Some(&missing_receipt), "deepseek-chat", &full_catalog).reason,
            "route_receipt_mismatch"
        );
        let mut changed_receipt = snapshot.clone();
        changed_receipt.route_receipt = Some("different-fixture-route");
        assert_eq!(
            decide(Some(&changed_receipt), "deepseek-chat", &full_catalog).reason,
            "route_receipt_mismatch"
        );
        let unsupported = resolve_fork_inherit(
            true,
            Some(&snapshot),
            "deepseek-chat",
            codewhale_config::ProviderKind::Deepseek,
            "deepseek",
            codewhale_config::provider::WireFormat::AnthropicMessages,
            Some("fixture-endpoint"),
            Some(&"fixture-route"),
            &full_catalog,
            4,
        );
        assert!(!unsupported.inherit);
        assert_eq!(unsupported.reason, "wire_format_unsupported");

        // Narrowed grant: head matches, tail grant-gap ends the run.
        let narrowed = vec![make_tool("read")];
        let prefix = decide(Some(&snapshot), "deepseek-chat", &narrowed);
        assert!(prefix.inherit, "{prefix:?}");
        assert_eq!(prefix.reason, "inherited_prefix");
        assert_eq!(prefix.prefix_tools.len(), 1);

        // Grant gap at the head: no common head, stays cold.
        let gap_at_head = vec![make_tool("agent")];
        assert_eq!(
            decide(Some(&snapshot), "deepseek-chat", &gap_at_head).reason,
            "tools_mismatch"
        );
        // Schema skew on the head tool also ends the run at zero.
        let mut skewed = make_tool("read");
        skewed.description = "different".to_string();
        assert_eq!(
            decide(Some(&snapshot), "deepseek-chat", &[skewed]).reason,
            "tools_mismatch"
        );
        // Internal-only flag skew must NOT end the run: those bytes
        // never reach the provider.
        let mut flagged = make_tool("read");
        flagged.defer_loading = Some(true);
        flagged.allowed_callers = Some(vec!["parent-only".to_string()]);
        let flagged_catalog = vec![flagged, make_tool("agent")];
        let flagged_ok = decide(Some(&snapshot), "deepseek-chat", &flagged_catalog);
        assert!(flagged_ok.inherit, "{flagged_ok:?}");
        assert_eq!(flagged_ok.reason, "inherited_full");

        assert_eq!(
            decide(None, "deepseek-chat", &full_catalog).reason,
            "no_snapshot"
        );
        let mut no_system = snapshot.clone();
        no_system.system = None;
        assert_eq!(
            decide(Some(&no_system), "deepseek-chat", &full_catalog).reason,
            "no_parent_system"
        );
        assert_eq!(
            decide(Some(&snapshot), "other-model", &full_catalog).reason,
            "model_mismatch"
        );
        let provider_mismatch = resolve_fork_inherit(
            true,
            Some(&snapshot),
            "deepseek-chat",
            codewhale_config::ProviderKind::Openai,
            "deepseek",
            codewhale_config::provider::WireFormat::ChatCompletions,
            Some("fixture-endpoint"),
            Some(&"fixture-route"),
            &full_catalog,
            4,
        );
        assert_eq!(provider_mismatch.reason, "provider_mismatch");
        assert!(!provider_mismatch.inherit);
        let route_mismatch = resolve_fork_inherit(
            true,
            Some(&snapshot),
            "deepseek-chat",
            codewhale_config::ProviderKind::Deepseek,
            "custom-mirror",
            codewhale_config::provider::WireFormat::ChatCompletions,
            Some("fixture-endpoint"),
            Some(&"fixture-route"),
            &full_catalog,
            4,
        );
        assert_eq!(route_mismatch.reason, "route_mismatch");
        for cold in [None, Some(0)] {
            let mut snapshot = snapshot.clone();
            snapshot.last_hit_tokens = cold;
            assert_eq!(
                decide(Some(&snapshot), "deepseek-chat", &full_catalog).reason,
                "prefix_cold",
                "cold={cold:?}"
            );
        }
        let empty_history = resolve_fork_inherit(
            true,
            Some(&snapshot),
            "deepseek-chat",
            codewhale_config::ProviderKind::Deepseek,
            "deepseek",
            codewhale_config::provider::WireFormat::ChatCompletions,
            Some("fixture-endpoint"),
            Some(&"fixture-route"),
            &full_catalog,
            0,
        );
        assert_eq!(empty_history.reason, "empty_history");
        let mut no_tools = snapshot.clone();
        no_tools.active_names.clear();
        assert_eq!(
            decide(Some(&no_tools), "deepseek-chat", &full_catalog).reason,
            "no_parent_tools"
        );
        let gated_off = resolve_fork_inherit(
            false,
            Some(&snapshot),
            "deepseek-chat",
            codewhale_config::ProviderKind::Deepseek,
            "deepseek",
            codewhale_config::provider::WireFormat::ChatCompletions,
            Some("fixture-endpoint"),
            Some(&"fixture-route"),
            &full_catalog,
            4,
        );
        assert_eq!(gated_off.reason, "gate_off");
        assert!(!gated_off.inherit);
    }

    #[test]
    fn carry_hot_forward_keeps_append_only_and_resets_rewrites() {
        let snapshot = inherit_snapshot();
        let grown = vec![
            make_message("user", "hi"),
            make_message("assistant", "hello"),
        ];
        assert_eq!(
            carry_hot_forward(
                Some(&snapshot),
                &snapshot.system,
                &snapshot.tools_json,
                &grown,
                Some(&"fixture-route")
            ),
            Some(1200),
            "append-only growth keeps the proof"
        );
        // Identical re-send (transparent retry) also extends.
        assert_eq!(
            carry_hot_forward(
                Some(&snapshot),
                &snapshot.system,
                &snapshot.tools_json,
                &snapshot.history,
                Some(&"fixture-route")
            ),
            Some(1200)
        );
        let edited = vec![make_message("user", "edited")];
        assert_eq!(
            carry_hot_forward(
                Some(&snapshot),
                &snapshot.system,
                &snapshot.tools_json,
                &edited,
                Some(&"fixture-route")
            ),
            None,
            "in-place rewrite resets"
        );
        assert_eq!(
            carry_hot_forward(
                Some(&snapshot),
                &snapshot.system,
                &snapshot.tools_json,
                &[],
                Some(&"fixture-route")
            ),
            None,
            "compaction shrink resets"
        );
        let other_system = Some(SystemPrompt::Text("other".to_string()));
        assert_eq!(
            carry_hot_forward(
                Some(&snapshot),
                &other_system,
                &snapshot.tools_json,
                &grown,
                Some(&"fixture-route")
            ),
            None,
            "system drift resets"
        );
        assert_eq!(
            carry_hot_forward(
                Some(&snapshot),
                &snapshot.system,
                "{\"name\":\"other\"}",
                &grown,
                Some(&"fixture-route")
            ),
            None,
            "tool drift resets"
        );
        assert_eq!(
            carry_hot_forward(
                None,
                &snapshot.system,
                &snapshot.tools_json,
                &grown,
                Some(&"fixture-route")
            ),
            None
        );
        assert_eq!(
            carry_hot_forward(
                Some(&snapshot),
                &snapshot.system,
                &snapshot.tools_json,
                &grown,
                None
            ),
            None
        );
        assert_eq!(
            carry_hot_forward(
                Some(&snapshot),
                &snapshot.system,
                &snapshot.tools_json,
                &grown,
                Some(&"different-fixture-route")
            ),
            None
        );
        let mut cold = snapshot.clone();
        cold.last_hit_tokens = Some(0);
        assert_eq!(
            carry_hot_forward(
                Some(&cold),
                &cold.system,
                &cold.tools_json,
                &grown,
                Some(&"fixture-route")
            ),
            None,
            "zero hits never carry"
        );
    }
}

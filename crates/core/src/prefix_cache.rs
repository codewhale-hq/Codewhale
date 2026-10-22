//! Prefix-cache stability manager (inspired by Reasonix's Pillar 1).
//!
//! DeepSeek's automatic prefix caching activates only when the *exact*
//! byte prefix of a request matches the prior request. Any system-prompt
//! drift, tool-list reordering, or message-rewriting busts the cache
//! for every token after the changed byte.
//!
//! This module provides a `PrefixStabilityManager` that:
//!
//! 1. **Fingerprints** the immutable prefix (system prompt + tool specs)
//!    at session start, using SHA-256 for strong collision resistance.
//! 2. **Verifies** the current prefix against the pinned fingerprint before
//!    every request.
//! 3. **Attributes** every change: a header change the engine declared
//!    (`/model`, `/mode`, goal edits, MCP or deferred-tool activation,
//!    session sync) re-pins under a logged reason; an undeclared change is
//!    *drift* — it is recorded and reported, and the original pin stays so
//!    later checks keep counting the miss instead of quietly adopting it.
//! 4. **Emits events** so the TUI can surface stability to the user.
//!
//! The invariant this guards: after session start, system and tools are
//! frozen bytes; history only grows; a miss is allowed only when we log why.
//!
//! ## Three-region model (from Reasonix)
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │ IMMUTABLE PREFIX                        │ ← fixed for session
//! │   system + tool_specs                    │   cache hit candidate
//! ├─────────────────────────────────────────┤
//! │ APPEND-ONLY HISTORY                     │ ← grows monotonically
//! │   [assistant₁][tool₁][assistant₂]...    │   preserves prefix of prior turns
//! ├─────────────────────────────────────────┤
//! │ LATEST USER TURN                        │ ← the only new content per request
//! └─────────────────────────────────────────┘
//! ```

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

use crate::request::{SystemPrompt, Tool};

/// A snapshot of the immutable prefix's fingerprint.
///
/// Matching hashes show stable system text and the normalized OpenAI-style
/// tool catalog. They do not measure provider cache hits or fingerprint every
/// provider-specific wire transformation; request replay tests cover those.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefixFingerprint {
    /// SHA-256 of the system prompt text.
    pub system_sha256: String,
    /// SHA-256 of the full tool catalog JSON (names, descriptions, schemas).
    pub tools_sha256: String,
    /// SHA-256 of system_sha256 ++ tools_sha256 (combined).
    pub combined_sha256: String,
}

impl PrefixFingerprint {
    /// Compute a fingerprint from system prompt text and tool list.
    ///
    /// Tools are serialized to the same JSON shape the chat API receives
    /// (`type`, `name`, `description`, `parameters`, `strict`) **in provider
    /// wire order**, then SHA-256 hashed. Order is load-bearing: the provider
    /// KV cache is order-sensitive, so sorting before hashing could read a
    /// false-green while the real prefix cache misses every turn (ops C1,
    /// DSH invariant). This also catches schema/description drift that
    /// actually affects the API prefix, while ignoring internal-only fields
    /// like `allowed_callers` (#2264).
    ///
    /// This entry point shares a process-local [`ToolCatalogCache`] with
    /// every other call, so a stable tool set (the common case after the
    /// first turn of a session) avoids the per-tool JSON serialization
    /// and join entirely. Callers that hold their own cache — e.g.
    /// [`PrefixStabilityManager`] — should use
    /// [`Self::compute_with_tool_cache`] to share *that* cache instead
    /// and avoid the thread-local lookup.
    #[cfg(test)]
    pub fn compute(system_text: &str, tools: Option<&[Tool]>) -> Self {
        let mut cache = ToolCatalogCache::new();
        Self::compute_with_tool_cache(system_text, tools, &mut cache)
    }

    /// Compute a fingerprint while reusing a [`ToolCatalogCache`] for the
    /// tool-side work. The cache holds the joined+SHA-256'd catalog
    /// under a content-derived identity so the per-tool JSON serialization
    /// and the join only run on the first call for a given tool set.
    ///
    /// On a cache hit this function avoids the entire tool serialization
    /// path, which can be 100+ microseconds for a 60-tool catalog.
    pub fn compute_with_tool_cache(
        system_text: &str,
        tools: Option<&[Tool]>,
        cache: &mut ToolCatalogCache,
    ) -> Self {
        let system_sha256 = sha256_hex(system_text.as_bytes());

        let tools_sha256 = match tools {
            Some(tools) if !tools.is_empty() => {
                // `fingerprint_for` consults the cache first; on a hit
                // it returns the pre-computed hex digest directly.
                cache.fingerprint_for(tools).sha256_hex
            }
            _ => sha256_hex(b""),
        };

        let combined = format!("{system_sha256}:{tools_sha256}");
        let combined_sha256 = sha256_hex(combined.as_bytes());
        Self {
            system_sha256,
            tools_sha256,
            combined_sha256,
        }
    }
}

/// A change record describing what drifted in the prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrefixChange {
    /// The old fingerprint (before the change).
    pub old: PrefixFingerprint,
    /// The new fingerprint (after the change).
    pub new: PrefixFingerprint,
    /// Whether the system prompt component changed.
    pub system_changed: bool,
    /// Whether the tool set component changed.
    pub tools_changed: bool,
}

#[allow(dead_code)]
impl PrefixChange {
    /// Returns a human-readable description of what changed.
    pub fn description(&self) -> String {
        let mut parts = Vec::new();
        if self.system_changed {
            parts.push("system prompt");
        }
        if self.tools_changed {
            parts.push("tool set");
        }
        if parts.is_empty() {
            return "unknown (fingerprint mismatch but no component detected)".to_string();
        }
        format!("prefix cache invalidated: {} changed", parts.join(" and "))
    }

    /// Returns a short label for TUI chip display.
    pub fn label(&self) -> &'static str {
        if self.system_changed && self.tools_changed {
            "sys+tools"
        } else if self.system_changed {
            "sys"
        } else if self.tools_changed {
            "tools"
        } else {
            "prefix"
        }
    }
}

/// Monitors and manages prefix-cache stability across turns.
///
/// This is the core abstraction, mirroring Reasonix's `ImmutablePrefix`
/// concept but adapted to CodeWhale's existing architecture where the
/// system prompt is rebuilt each turn and tools are registered at startup.
///
/// Usage:
/// ```ignore
/// let mgr = PrefixStabilityManager::new(system_text, tools);
/// if mgr.check_and_update(system_text, tools) {
///     println!("Prefix is stable (cache-friendly)");
/// } else {
///     let change = mgr.last_change().unwrap();
///     println!("Prefix drifted: {}", change.description());
/// }
/// ```
#[derive(Debug, Clone)]
pub struct PrefixStabilityManager {
    /// The pinned fingerprint from session start or last stabilization.
    pinned: Option<PrefixFingerprint>,
    /// The most recent fingerprint (computed during last check).
    current: Option<PrefixFingerprint>,
    /// The last detected change, if any.
    last_change: Option<PrefixChange>,
    /// Total number of prefix changes detected this session.
    change_count: u64,
    /// Total number of stability checks performed.
    check_count: u64,
    /// Why the current pin exists: `initial`, `resume`, or `change:<what>`.
    pin_reason: Option<String>,
    /// Bounded log of every attributed change and every undeclared drift.
    history: VecDeque<PrefixHistoryEntry>,
    /// Explanation of the most recent expected cache miss (a declared header
    /// change, a history reset such as compaction, or undeclared drift).
    last_miss_reason: Option<String>,
    /// `<context_update>` snapshots appended this session (workspace drift
    /// delivered as history, with the pinned header untouched).
    context_update_count: u64,
    /// Process-local cache for the tool-catalog JSON serialization. Avoids
    /// re-running `tool_to_api_json` + join on every `check_and_update`
    /// when the tool set is unchanged (the common case once tools are
    /// registered at session start).
    tool_catalog_cache: ToolCatalogCache,
}

/// Maximum retained [`PrefixHistoryEntry`] records per session.
const PREFIX_HISTORY_CAP: usize = 32;

/// One attributed prefix event: a declared header change (re-pinned) or an
/// undeclared drift (pin kept).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PrefixHistoryEntry {
    /// `change:<what>` for declared header changes, `drift:<component>` for
    /// undeclared changes, `reset:<what>` for history resets.
    pub reason: String,
    /// Whether the pin was replaced by this event.
    pub repinned: bool,
    /// Combined SHA-256 before the event.
    pub from_sha256: String,
    /// Combined SHA-256 the request actually carried.
    pub to_sha256: String,
}

/// Outcome of [`PrefixStabilityManager::check`].
#[derive(Debug, Clone)]
pub enum PrefixCheck {
    /// The request prefix matches the pin byte-for-byte.
    Stable,
    /// The prefix changed and the engine declared why; the pin moved.
    Repinned {
        reason: String,
        change: PrefixChange,
    },
    /// The prefix changed with no declared reason. The pin did NOT move.
    Drift { change: PrefixChange },
}

/// Default capacity for the tool-catalog serialization cache. Sized for
/// "session + 1 or 2 forked subagent catalogs" without unbounded growth.
const TOOL_CATALOG_CACHE_CAPACITY: usize = 8;

/// Bounded LRU cache of `(tool_set_identity) -> sha256_hex`.
///
/// The cache key is a content-derived `u64` hash of the tool list (length +
/// per-tool `name` + `description` + serialized `input_schema`). On a hit,
/// `PrefixFingerprint::compute` skips the per-tool JSON serialization and
/// the join — a workload that can be 100+ microseconds for a
/// 60-tool catalog. On a miss, the work runs once and only the digest is
/// retained (#3854); the joined catalog string is ephemeral.
#[derive(Debug, Default, Clone)]
pub struct ToolCatalogCache {
    by_identity: HashMap<u64, CachedCatalog>,
    insertion_order: VecDeque<u64>,
    capacity: usize,
}

/// One entry in [`ToolCatalogCache`]. Production only needs the pre-computed
/// SHA-256 digest of the in-order joined catalog.
#[derive(Debug, Clone)]
pub struct CachedCatalog {
    /// SHA-256 hex digest of the newline-joined, in-order tool-catalog JSON.
    pub sha256_hex: String,
}

impl ToolCatalogCache {
    /// Create a cache with the default capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(TOOL_CATALOG_CACHE_CAPACITY)
    }

    /// Create a cache that holds at most `capacity` tool-set entries.
    /// Smaller values save memory at the cost of more cache misses.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            by_identity: HashMap::with_capacity(cap),
            insertion_order: VecDeque::with_capacity(cap),
            capacity: cap,
        }
    }

    /// Compute (or recall) the joined-and-hashed tool catalog for `tools`.
    /// The cache is keyed on a content-derived `u64` identity so two `&[Tool]`
    /// slices with the same payloads — in the same order — hit the same entry.
    pub fn fingerprint_for(&mut self, tools: &[Tool]) -> CachedCatalog {
        let identity = tool_set_identity(tools);
        if let Some(cached) = self.by_identity.get(&identity) {
            return cached.clone();
        }

        // Miss: serialize, join, hash — in wire order, never sorted. The
        // provider cache is order-sensitive; a sorted fingerprint can say
        // "stable" while the real prefix misses every turn (ops C1). Keep
        // only the digest in the cache — the joined string is not needed on
        // the hot path (#3854).
        let serialized: Vec<String> = tools.iter().filter_map(tool_to_api_json).collect();
        let joined = serialized.join("\n");
        let entry = CachedCatalog {
            sha256_hex: sha256_hex(joined.as_bytes()),
        };

        if self.by_identity.len() >= self.capacity
            && let Some(oldest) = self.insertion_order.pop_front()
        {
            self.by_identity.remove(&oldest);
        }
        self.by_identity.insert(identity, entry.clone());
        self.insertion_order.push_back(identity);
        entry
    }

    /// Drop every cached entry. Used by tool-registry mutation paths
    /// (e.g. plugin hot-reload, MCP attach) when the caller cannot
    /// easily prove the tool set is unchanged.
    #[allow(dead_code)] // observability; called by /cache flush and tests
    pub fn invalidate(&mut self) {
        self.by_identity.clear();
        self.insertion_order.clear();
    }

    /// Returns the number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_identity.len()
    }

    /// Returns `true` if the cache has no entries.
    #[allow(dead_code)] // observability; surfaced via /status
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_identity.is_empty()
    }

    /// Returns `(current_entries, capacity)` for observability. Surfaced via
    /// the `/status` chip in a follow-up; tests exercise the path.
    #[allow(dead_code)] // surfaced via /status in a follow-up; tests exercise it
    #[must_use]
    pub fn stats(&self) -> (usize, usize) {
        (self.len(), self.capacity)
    }
}

/// Content-derived identity for a tool slice. Order-sensitive: two slices
/// with the same tools in different orders produce different identities.
/// (The downstream fingerprint itself is order-insensitive — the sort in
/// `fingerprint_for` takes care of that — but the cache key matches the
/// input order so re-registration of the same set in the same order hits.)
fn tool_set_identity(tools: &[Tool]) -> u64 {
    let mut hasher = DefaultHasher::new();
    tools.len().hash(&mut hasher);
    for tool in tools {
        tool.name.hash(&mut hasher);
        tool.description.hash(&mut hasher);
        // `strict` participates in `tool_to_api_json` output (it is part of
        // the wire-format the chat API receives), so it MUST be part of the
        // identity. Omitting it lets two semantically different catalogs
        // collide and serve a stale fingerprint.
        tool.strict.hash(&mut hasher);
        // Walk the schema JSON directly instead of materializing it as a
        // String. For a 60-tool catalog this saves ~25-40 KB of allocation
        // on every cache miss.
        hash_json_value(&tool.input_schema, &mut hasher);
    }
    hasher.finish()
}

/// Fold a `serde_json::Value` into the hasher without allocating a
/// `String`. Numeric variants are hashed via their bit pattern so `1` and
/// `1.0` produce distinct identities (matching the JSON spec).
fn hash_json_value<H: Hasher>(value: &serde_json::Value, state: &mut H) {
    match value {
        serde_json::Value::Null => 0u8.hash(state),
        serde_json::Value::Bool(b) => {
            1u8.hash(state);
            b.hash(state);
        }
        serde_json::Value::Number(n) => {
            2u8.hash(state);
            if let Some(i) = n.as_i64() {
                i.hash(state);
            } else if let Some(u) = n.as_u64() {
                u.hash(state);
            } else if let Some(f) = n.as_f64() {
                f.to_bits().hash(state);
            }
        }
        serde_json::Value::String(s) => {
            3u8.hash(state);
            s.hash(state);
        }
        serde_json::Value::Array(arr) => {
            4u8.hash(state);
            arr.len().hash(state);
            for v in arr {
                hash_json_value(v, state);
            }
        }
        serde_json::Value::Object(obj) => {
            5u8.hash(state);
            obj.len().hash(state);
            // Iterate by sorted key so `{"a":1,"b":2}` and `{"b":2,"a":1}`
            // collide — the wire format already canonicalizes via the
            // `serde_json` Map ordering, but a defensively-sorted view
            // future-proofs against schema serializers that emit
            // declaration order.
            let mut entries: Vec<(&String, &serde_json::Value)> = obj.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (k, v) in entries {
                k.hash(state);
                hash_json_value(v, state);
            }
        }
    }
}

/// Process-local fallback cache used by `PrefixFingerprint::compute`
/// (when available). Callers that maintain their own cache (e.g.
/// [`PrefixStabilityManager`]) should prefer
/// [`PrefixFingerprint::compute_with_tool_cache`] and pass the cache in
/// directly, both to share state and to avoid the thread-local lookup
/// on the hot path.
#[allow(dead_code)]
impl PrefixStabilityManager {
    /// Create a new manager and immediately pin the first fingerprint.
    pub fn new(system_text: &str, tools: Option<&[Tool]>) -> Self {
        let mut cache = ToolCatalogCache::new();
        let fp = PrefixFingerprint::compute_with_tool_cache(system_text, tools, &mut cache);
        Self {
            pinned: Some(fp.clone()),
            current: Some(fp),
            last_change: None,
            change_count: 0,
            check_count: 0,
            pin_reason: Some("initial".to_string()),
            history: VecDeque::new(),
            last_miss_reason: None,
            context_update_count: 0,
            tool_catalog_cache: cache,
        }
    }

    /// Create a manager in "unpinned" state — no initial fingerprint.
    /// Call `pin()` or `check_and_update()` to establish the baseline.
    pub fn new_unpinned() -> Self {
        Self {
            pinned: None,
            current: None,
            last_change: None,
            change_count: 0,
            check_count: 0,
            pin_reason: None,
            history: VecDeque::new(),
            last_miss_reason: None,
            context_update_count: 0,
            tool_catalog_cache: ToolCatalogCache::new(),
        }
    }

    /// Explicitly pin a fingerprint, replacing any prior pinned state.
    /// Returns `true` if this is the first pin, or `false` if replacing.
    /// Note: does NOT increment `check_count` — that counter is reserved
    /// for `check_and_update` calls so `stability_ratio()` stays accurate.
    pub fn pin(&mut self, system_text: &str, tools: Option<&[Tool]>) -> bool {
        self.pin_with_reason(system_text, tools, "initial")
    }

    /// Pin under an explicit reason (`initial`, `resume`, `change:<what>`).
    pub fn pin_with_reason(
        &mut self,
        system_text: &str,
        tools: Option<&[Tool]>,
        reason: &str,
    ) -> bool {
        let fp = PrefixFingerprint::compute_with_tool_cache(
            system_text,
            tools,
            &mut self.tool_catalog_cache,
        );
        let was_unpinned = self.pinned.is_none();
        self.pinned = Some(fp.clone());
        self.current = Some(fp);
        self.pin_reason = Some(reason.to_string());
        was_unpinned
    }

    /// Record an expected miss that is not a header change (compaction,
    /// `/clear`, an edited turn). The pin is untouched; the reason is kept so
    /// `/cache stats` can explain the next low hit-rate turn.
    pub fn note_history_reset(&mut self, what: &str) {
        let hash = self
            .pinned
            .as_ref()
            .map(|fp| fp.combined_sha256.clone())
            .unwrap_or_default();
        self.push_history(PrefixHistoryEntry {
            reason: format!("reset:{what}"),
            repinned: false,
            from_sha256: hash.clone(),
            to_sha256: hash,
        });
        self.last_miss_reason = Some(format!("reset:{what}"));
    }

    /// Record that workspace drift was delivered as a `<context_update>`
    /// history append. Not a miss: the pin and the prefix are unchanged.
    pub fn note_context_update(&mut self) {
        self.context_update_count = self.context_update_count.saturating_add(1);
        let hash = self
            .pinned
            .as_ref()
            .map(|fp| fp.combined_sha256.clone())
            .unwrap_or_default();
        self.push_history(PrefixHistoryEntry {
            reason: "context_update".to_string(),
            repinned: false,
            from_sha256: hash.clone(),
            to_sha256: hash,
        });
    }

    /// Number of `<context_update>` snapshots appended this session.
    pub fn context_update_count(&self) -> u64 {
        self.context_update_count
    }

    fn push_history(&mut self, entry: PrefixHistoryEntry) {
        if self.history.len() >= PREFIX_HISTORY_CAP {
            self.history.pop_front();
        }
        self.history.push_back(entry);
    }

    /// Verify the request prefix against the pin, attributing any change.
    ///
    /// `declared_change` names a header change the engine performed on
    /// purpose (`model`, `mode`, `goal`, `tools:+web_fetch`, `mcp`, …). When
    /// the prefix changed and a reason is declared, the pin moves and the
    /// change is logged as `change:<reason>`. When it changed with no
    /// declared reason, that is drift: it is logged as `drift:<component>`
    /// and the pin stays put, so the same undeclared prefix keeps counting as
    /// a miss instead of becoming the new baseline.
    pub fn check(
        &mut self,
        system_text: &str,
        tools: Option<&[Tool]>,
        declared_change: Option<&str>,
    ) -> PrefixCheck {
        let fp = PrefixFingerprint::compute_with_tool_cache(
            system_text,
            tools,
            &mut self.tool_catalog_cache,
        );
        let old_fp = self.current.replace(fp.clone());
        self.check_count += 1;

        let pinned = match &self.pinned {
            Some(p) => p.clone(),
            None => {
                self.pinned = Some(fp);
                self.pin_reason = Some(declared_change.unwrap_or("initial").to_string());
                self.last_change = None;
                return PrefixCheck::Stable;
            }
        };

        if fp.combined_sha256 == pinned.combined_sha256 {
            return PrefixCheck::Stable;
        }

        let old = old_fp.unwrap_or_else(|| pinned.clone());
        let system_changed = fp.system_sha256 != pinned.system_sha256;
        let tools_changed = fp.tools_sha256 != pinned.tools_sha256;
        let change = PrefixChange {
            old,
            new: fp.clone(),
            system_changed,
            tools_changed,
        };
        self.last_change = Some(change.clone());
        self.change_count += 1;

        match declared_change {
            Some(reason) => {
                let reason = format!("change:{reason}");
                self.push_history(PrefixHistoryEntry {
                    reason: reason.clone(),
                    repinned: true,
                    from_sha256: pinned.combined_sha256.clone(),
                    to_sha256: fp.combined_sha256.clone(),
                });
                self.last_miss_reason = Some(reason.clone());
                self.pinned = Some(fp);
                self.pin_reason = Some(reason.clone());
                PrefixCheck::Repinned { reason, change }
            }
            None => {
                let reason = format!("drift:{}", change.label());
                self.push_history(PrefixHistoryEntry {
                    reason: reason.clone(),
                    repinned: false,
                    from_sha256: pinned.combined_sha256.clone(),
                    to_sha256: fp.combined_sha256.clone(),
                });
                self.last_miss_reason = Some(reason);
                PrefixCheck::Drift { change }
            }
        }
    }

    /// Why the current pin exists.
    pub fn pin_reason(&self) -> Option<&str> {
        self.pin_reason.as_deref()
    }

    /// Attributed change/drift/reset history, oldest first.
    pub fn history(&self) -> impl Iterator<Item = &PrefixHistoryEntry> {
        self.history.iter()
    }

    /// Explanation of the most recent expected miss, if any.
    pub fn last_miss_reason(&self) -> Option<&str> {
        self.last_miss_reason.as_deref()
    }

    /// Check whether the current prefix matches the pinned fingerprint
    /// without a declared header change.
    ///
    /// - `Ok(true)` when the prefix is stable (or this was the first pin).
    /// - `Err(change)` when the prefix drifted. The pin is **kept**: an
    ///   undeclared change never becomes the new baseline. Use [`Self::check`]
    ///   with a declared reason to move the pin on purpose.
    pub fn check_and_update(
        &mut self,
        system_text: &str,
        tools: Option<&[Tool]>,
    ) -> Result<bool, Box<PrefixChange>> {
        match self.check(system_text, tools, None) {
            PrefixCheck::Stable => Ok(true),
            PrefixCheck::Repinned { change, .. } | PrefixCheck::Drift { change } => {
                Err(Box::new(change))
            }
        }
    }

    /// Returns the most recent prefix change, if any.
    pub fn last_change(&self) -> Option<&PrefixChange> {
        self.last_change.as_ref()
    }

    /// Returns the pinned fingerprint.
    pub fn pinned_fingerprint(&self) -> Option<&PrefixFingerprint> {
        self.pinned.as_ref()
    }

    /// Returns the current (most recently computed) fingerprint.
    pub fn current_fingerprint(&self) -> Option<&PrefixFingerprint> {
        self.current.as_ref()
    }

    /// Returns the total number of prefix changes detected.
    pub fn change_count(&self) -> u64 {
        self.change_count
    }

    /// Returns the total number of stability checks performed.
    pub fn check_count(&self) -> u64 {
        self.check_count
    }

    /// Returns the prefix stability rate as a fraction (0.0 – 1.0).
    /// 1.0 means the prefix has never changed. Returns 1.0 when no
    /// checks have been performed (to avoid division by zero).
    pub fn stability_ratio(&self) -> f64 {
        if self.check_count == 0 {
            1.0
        } else {
            let stable_checks = self.check_count - self.change_count;
            stable_checks as f64 / self.check_count as f64
        }
    }

    /// Returns a human-readable stability summary.
    pub fn summary(&self) -> String {
        let pct = self.stability_ratio() * 100.0;
        let pinned_short = self
            .pinned
            .as_ref()
            .map(|fp| {
                if fp.combined_sha256.len() >= 12 {
                    &fp.combined_sha256[..12]
                } else {
                    &fp.combined_sha256
                }
            })
            .unwrap_or("none");

        format!(
            "Prefix stability: {pct:.1}% ({stable}/{total} checks stable) | fingerprint: {pinned_short} | changes: {changes}",
            pct = pct,
            stable = self.check_count.saturating_sub(self.change_count),
            total = self.check_count,
            pinned_short = pinned_short,
            changes = self.change_count,
        )
    }
}

/// Serialize a tool to the same JSON shape the chat API receives,
/// excluding internal-only fields like `allowed_callers`, `defer_loading`,
/// `input_examples`, and `cache_control` that are never sent to DeepSeek.
///
/// Shared by the fingerprint manager and fork-prefix inheritance: both
/// need byte-equality against provider-cached prefixes, and raw `Tool`
/// serialization would false-negative on internal-only flag skew.
pub fn tool_to_api_json(tool: &Tool) -> Option<String> {
    let mut value = serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.input_schema,
        }
    });
    if let Some(strict) = tool.strict
        && let Some(function) = value.get_mut("function")
    {
        function["strict"] = serde_json::json!(strict);
    }
    serde_json::to_string(&value).ok()
}

/// Compute the SHA-256 hex digest of a byte slice.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    use std::fmt::Write;
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// Bounded line delta between the session context the model last saw and a
/// fresh composition, rendered as a `<context_update>` user-role message.
///
/// The pinned system prompt is never rewritten; this is how workspace,
/// instruction, skills, memory, and goal drift reaches the model as a normal
/// history append. Returns `None` when the two texts are line-identical (only
/// whitespace/ordering noise), so no empty update is ever sent.
pub const CONTEXT_UPDATE_MAX_LINES: usize = 80;
pub const CONTEXT_UPDATE_MAX_BYTES: usize = 6_000;

pub fn context_update_message(known: &str, current: &str) -> Option<String> {
    use std::collections::HashMap;
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for line in known.lines() {
        *counts.entry(line).or_insert(0) -= 1;
    }
    for line in current.lines() {
        *counts.entry(line).or_insert(0) += 1;
    }
    let mut added: Vec<&str> = Vec::new();
    for line in current.lines() {
        if let Some(count) = counts.get_mut(line)
            && *count > 0
        {
            *count -= 1;
            if !line.trim().is_empty() {
                added.push(line);
            }
        }
    }
    let mut removed: Vec<&str> = Vec::new();
    for line in known.lines() {
        if let Some(count) = counts.get_mut(line)
            && *count < 0
        {
            *count += 1;
            if !line.trim().is_empty() {
                removed.push(line);
            }
        }
    }
    if added.is_empty() && removed.is_empty() {
        return None;
    }

    let mut out = String::from(
        "<context_update>\nSession context changed since it was pinned; the pinned system \
         prompt is unchanged. Delta (+ added, - removed):\n",
    );
    let mut lines_written = 0usize;
    let mut truncated = 0usize;
    for (sign, group) in [("+ ", &added), ("- ", &removed)] {
        for line in group {
            if lines_written >= CONTEXT_UPDATE_MAX_LINES
                || out.len() + sign.len() + line.len() + 1 > CONTEXT_UPDATE_MAX_BYTES
            {
                truncated += 1;
                continue;
            }
            out.push_str(sign);
            out.push_str(line);
            out.push('\n');
            lines_written += 1;
        }
    }
    if truncated > 0 {
        out.push_str(&format!(
            "(+{truncated} more changed lines; re-read the files you need)\n"
        ));
    }
    out.push_str("</context_update>");
    Some(out)
}

/// Extract the system prompt text from an optional SystemPrompt,
/// returning an owned String. This is used for prefix fingerprinting
/// and avoids lifetime/leak issues with the rare SystemPrompt::Blocks case.
pub fn system_prompt_text(system: Option<&SystemPrompt>) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn same_prefix_produces_same_fingerprint() {
        let a = PrefixFingerprint::compute("hello world", None);
        let b = PrefixFingerprint::compute("hello world", None);
        assert_eq!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn different_system_produces_different_fingerprint() {
        let a = PrefixFingerprint::compute("hello", None);
        let b = PrefixFingerprint::compute("world", None);
        assert_ne!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn tool_order_affects_fingerprint() {
        // The provider KV cache is order-sensitive; sorting before hashing
        // could read a false-green while the real prefix cache missed every
        // turn (ops C1).
        let tools_a = vec![make_tool("read_file"), make_tool("write_file")];
        let tools_b = vec![make_tool("write_file"), make_tool("read_file")];
        let a = PrefixFingerprint::compute("system", Some(&tools_a));
        let b = PrefixFingerprint::compute("system", Some(&tools_b));
        assert_ne!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn different_tools_produce_different_fingerprint() {
        let tools_a = vec![make_tool("read_file")];
        let tools_b = vec![make_tool("write_file")];
        let a = PrefixFingerprint::compute("system", Some(&tools_a));
        let b = PrefixFingerprint::compute("system", Some(&tools_b));
        assert_ne!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn manager_starts_stable() {
        let mut mgr = PrefixStabilityManager::new("system prompt", None);
        assert!(mgr.check_and_update("system prompt", None).unwrap());
        assert_eq!(mgr.change_count(), 0);
        assert_eq!(mgr.check_count(), 1);
    }

    #[test]
    fn manager_detects_change() {
        let mut mgr = PrefixStabilityManager::new("system prompt", None);
        let result = mgr.check_and_update("different prompt", None);
        assert!(result.is_err());
        assert_eq!(mgr.change_count(), 1);
        let change = mgr.last_change().unwrap();
        assert!(change.system_changed);
        assert!(!change.tools_changed);
    }

    #[test]
    fn manager_detects_tool_change() {
        let tools_a = vec![make_tool("read_file")];
        let tools_b = vec![make_tool("write_file")];
        let mut mgr = PrefixStabilityManager::new("system", Some(&tools_a));
        let result = mgr.check_and_update("system", Some(&tools_b));
        assert!(result.is_err());
        let change = mgr.last_change().unwrap();
        assert!(!change.system_changed);
        assert!(change.tools_changed);
    }

    #[test]
    fn undeclared_drift_never_moves_the_pin() {
        let mut mgr = PrefixStabilityManager::new("old", None);
        assert_eq!(mgr.pin_reason(), Some("initial"));
        assert!(mgr.check_and_update("new", None).is_err());
        // The pin stays on "old": the same undeclared prefix is still a miss.
        assert!(mgr.check_and_update("new", None).is_err());
        assert!(mgr.check_and_update("old", None).unwrap());
        assert_eq!(mgr.change_count(), 2);
        assert_eq!(mgr.last_miss_reason(), Some("drift:sys"));
        let history: Vec<_> = mgr.history().collect();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|entry| !entry.repinned));
        assert_eq!(mgr.pin_reason(), Some("initial"));
    }

    #[test]
    fn declared_header_change_repins_under_a_logged_reason() {
        let mut mgr = PrefixStabilityManager::new("old", None);
        match mgr.check("new", None, Some("model")) {
            PrefixCheck::Repinned { reason, change } => {
                assert_eq!(reason, "change:model");
                assert!(change.system_changed);
            }
            other => panic!("expected repin, got {other:?}"),
        }
        assert_eq!(mgr.pin_reason(), Some("change:model"));
        assert!(matches!(mgr.check("new", None, None), PrefixCheck::Stable));
        assert!(matches!(
            mgr.check("newer", None, None),
            PrefixCheck::Drift { .. }
        ));
        assert_eq!(mgr.pin_reason(), Some("change:model"));
        let reasons: Vec<&str> = mgr.history().map(|e| e.reason.as_str()).collect();
        assert_eq!(reasons, vec!["change:model", "drift:sys"]);
    }

    #[test]
    fn declared_reason_on_a_stable_prefix_is_a_noop() {
        let mut mgr = PrefixStabilityManager::new("same", None);
        assert!(matches!(
            mgr.check("same", None, Some("mode")),
            PrefixCheck::Stable
        ));
        assert_eq!(mgr.change_count(), 0);
        assert_eq!(mgr.pin_reason(), Some("initial"));
    }

    #[test]
    fn context_update_message_reports_added_and_removed_lines_bounded() {
        let known = "## Files\nsrc/a.rs\nsrc/b.rs\n## Instructions\nbe kind\n";
        let current = "## Files\nsrc/a.rs\nsrc/b.rs\nsrc/c.rs\n## Instructions\nbe precise\n";
        let update = context_update_message(known, current).expect("delta");
        assert!(update.starts_with("<context_update>"));
        assert!(update.ends_with("</context_update>"));
        assert!(update.contains("+ src/c.rs"));
        assert!(update.contains("+ be precise"));
        assert!(update.contains("- be kind"));
        assert!(!update.contains("- src/a.rs"));
        assert!(context_update_message(known, known).is_none());

        let mut big = String::new();
        for i in 0..500 {
            big.push_str(&format!("line {i}\n"));
        }
        let bounded = context_update_message("", &big).expect("delta");
        assert!(
            bounded.len() <= CONTEXT_UPDATE_MAX_BYTES + 128,
            "{}",
            bounded.len()
        );
        assert!(bounded.contains("more changed lines"));
    }

    #[test]
    fn context_update_is_logged_but_is_not_a_miss() {
        let mut mgr = PrefixStabilityManager::new("sys", None);
        mgr.note_context_update();
        assert_eq!(mgr.context_update_count(), 1);
        assert_eq!(mgr.last_miss_reason(), None);
        assert!(matches!(mgr.check("sys", None, None), PrefixCheck::Stable));
    }

    #[test]
    fn history_reset_is_logged_without_moving_the_pin() {
        let mut mgr = PrefixStabilityManager::new("sys", None);
        mgr.note_history_reset("compaction");
        assert_eq!(mgr.last_miss_reason(), Some("reset:compaction"));
        assert!(matches!(mgr.check("sys", None, None), PrefixCheck::Stable));
        assert_eq!(mgr.history().count(), 1);
    }

    #[test]
    fn stability_ratio_is_one_for_no_changes() {
        let mut mgr = PrefixStabilityManager::new("hello", None);
        mgr.check_and_update("hello", None).unwrap();
        mgr.check_and_update("hello", None).unwrap();
        assert!((mgr.stability_ratio() - 1.0).abs() < f64::EPSILON);
        assert_eq!(mgr.check_count(), 2);
        assert_eq!(mgr.change_count(), 0);
    }

    #[test]
    fn stability_ratio_reflects_change_rate() {
        let mut mgr = PrefixStabilityManager::new("hello", None);
        mgr.check_and_update("hello", None).unwrap(); // check 1: stable
        let _ = mgr.check("world", None, Some("model")); // check 2: declared change
        mgr.check_and_update("world", None).unwrap(); // check 3: stable
        // 2 stable out of 3 checks = 0.666...
        // (check_count=0 at start, so 3 checks: 3 checks - 1 change = 2 stable)
        assert!((mgr.stability_ratio() - 2.0 / 3.0).abs() < 0.01);
        assert_eq!(mgr.check_count(), 3);
        assert_eq!(mgr.change_count(), 1);
    }

    #[test]
    fn empty_tools_and_none_tools_produce_same_hash() {
        let empty = PrefixFingerprint::compute("system", Some(&[]));
        let none = PrefixFingerprint::compute("system", None);
        // Both should produce sha256(b"") for the tool component
        assert_eq!(empty.tools_sha256, none.tools_sha256);
    }

    #[test]
    fn empty_system_produces_sha256_of_empty_string() {
        let fp = PrefixFingerprint::compute("", None);
        let expected = sha256_hex(b"");
        assert_eq!(fp.system_sha256, expected);
    }

    #[test]
    fn prefix_change_description_is_informative() {
        let old = PrefixFingerprint::compute("old", None);
        let new = PrefixFingerprint::compute("new", None);
        let change = PrefixChange {
            old,
            new,
            system_changed: true,
            tools_changed: false,
        };
        assert_eq!(
            change.description(),
            "prefix cache invalidated: system prompt changed"
        );
        assert_eq!(change.label(), "sys");
    }

    #[test]
    fn new_unpinned_has_no_change_history() {
        let mut mgr = PrefixStabilityManager::new_unpinned();
        assert!(mgr.pinned_fingerprint().is_none());
        assert!(mgr.current_fingerprint().is_none());
        assert!(mgr.last_change().is_none());
        assert_eq!(mgr.change_count(), 0);
        assert_eq!(mgr.check_count(), 0);
        // First check should pin automatically and count as a check.
        assert!(mgr.check_and_update("hello", None).unwrap());
        assert!(mgr.pinned_fingerprint().is_some());
        assert_eq!(mgr.check_count(), 1);
    }

    #[test]
    fn fingerprint_detects_schema_change_not_just_name_change() {
        let tool_a = make_tool("my_tool");
        let mut tool_a_v2 = make_tool("my_tool");
        tool_a_v2.description = "updated description".to_string();

        let a = PrefixFingerprint::compute("system", Some(&[tool_a]));
        let b = PrefixFingerprint::compute("system", Some(&[tool_a_v2]));
        // Same name, different description — must produce different hash.
        assert_ne!(a.tools_sha256, b.tools_sha256);
        assert_ne!(a.combined_sha256, b.combined_sha256);
    }

    #[test]
    fn system_prompt_text_returns_empty_for_none() {
        assert_eq!(system_prompt_text(None), "");
    }

    // ── ToolCatalogCache tests ──────────────────────────────────

    #[test]
    fn tool_catalog_cache_miss_then_hit_returns_same_digest() {
        let mut cache = ToolCatalogCache::new();
        let tools = vec![make_tool("read_file"), make_tool("write_file")];

        let first = cache.fingerprint_for(&tools);
        assert_eq!(cache.len(), 1);

        let second = cache.fingerprint_for(&tools);
        assert_eq!(cache.len(), 1, "second call should be a cache hit");
        assert_eq!(first.sha256_hex, second.sha256_hex);
    }

    #[test]
    fn tool_catalog_cache_different_tool_sets_dont_collide() {
        let mut cache = ToolCatalogCache::new();
        let a = vec![make_tool("read_file")];
        let b = vec![make_tool("write_file")];

        let entry_a = cache.fingerprint_for(&a);
        let entry_b = cache.fingerprint_for(&b);
        assert_eq!(cache.len(), 2);
        assert_ne!(entry_a.sha256_hex, entry_b.sha256_hex);
    }

    #[test]
    fn tool_catalog_cache_fingerprint_is_order_sensitive_like_the_provider_cache() {
        // The identity hash includes the input order so re-registering the
        // same set with a different permutation produces a separate cache
        // entry. The digest is now order-sensitive too: the provider KV cache
        // is order-sensitive, so a sorted fingerprint read a false-green while
        // the real prefix missed every turn (ops C1). Different wire order =
        // different prefix = different fingerprint.
        let mut cache = ToolCatalogCache::new();
        let a = vec![make_tool("read_file"), make_tool("write_file")];
        let b = vec![make_tool("write_file"), make_tool("read_file")];
        let entry_a = cache.fingerprint_for(&a);
        let entry_b = cache.fingerprint_for(&b);
        assert_ne!(entry_a.sha256_hex, entry_b.sha256_hex);
        assert_eq!(cache.len(), 2);
        // Re-requesting the original order returns the cached digest.
        let again = cache.fingerprint_for(&a);
        assert_eq!(again.sha256_hex, entry_a.sha256_hex);
    }

    #[test]
    fn tool_catalog_cache_detects_schema_change() {
        let mut cache = ToolCatalogCache::new();
        let tool_v1 = make_tool("t");
        let mut tool_v2 = make_tool("t");
        tool_v2.description = "updated".to_string();

        let entry_v1 = cache.fingerprint_for(&[tool_v1]);
        let entry_v2 = cache.fingerprint_for(&[tool_v2]);
        assert_ne!(entry_v1.sha256_hex, entry_v2.sha256_hex);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn tool_catalog_cache_respects_capacity() {
        let mut cache = ToolCatalogCache::with_capacity(2);
        cache.fingerprint_for(&[make_tool("a")]);
        cache.fingerprint_for(&[make_tool("b")]);
        cache.fingerprint_for(&[make_tool("c")]);
        assert_eq!(cache.len(), 2);
        // The first entry was evicted; a re-query for it should miss.
        let re_entry = cache.fingerprint_for(&[make_tool("a")]);
        // After the re-query, the cache has [b, c, a] — 3 entries? No,
        // capacity 2 means oldest is evicted when we insert the 3rd unique.
        // After inserting a, the cache holds the most recent 2: {c, a}.
        assert_eq!(cache.len(), 2);
        // The returned digest should match a fresh fingerprint of the same set.
        let fresh = cache.fingerprint_for(&[make_tool("a")]);
        assert_eq!(re_entry.sha256_hex, fresh.sha256_hex);
    }

    #[test]
    fn tool_catalog_cache_invalidate_clears_all() {
        let mut cache = ToolCatalogCache::new();
        cache.fingerprint_for(&[make_tool("a")]);
        cache.fingerprint_for(&[make_tool("b")]);
        cache.invalidate();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn tool_catalog_cache_empty_slice_uses_zero_capacity_path() {
        // Empty input is fine — should produce a stable, non-empty digest.
        let mut cache = ToolCatalogCache::new();
        let entry = cache.fingerprint_for(&[]);
        assert!(!entry.sha256_hex.is_empty());
        let again = cache.fingerprint_for(&[]);
        assert_eq!(entry.sha256_hex, again.sha256_hex);
    }

    #[test]
    fn compute_with_tool_cache_matches_compute_uncached() {
        // The cached and uncached paths must produce identical fingerprints
        // for the same inputs — otherwise we'd silently corrupt the prefix
        // cache and invalidate every request.
        let mut cache = ToolCatalogCache::new();
        let tools = vec![make_tool("alpha"), make_tool("beta")];

        let cached = PrefixFingerprint::compute_with_tool_cache("sys", Some(&tools), &mut cache);
        let uncached = PrefixFingerprint::compute("sys", Some(&tools));
        assert_eq!(cached.combined_sha256, uncached.combined_sha256);
        assert_eq!(cached.tools_sha256, uncached.tools_sha256);
    }

    #[test]
    fn manager_check_and_update_uses_cached_tool_fingerprint() {
        // After the first call populates the cache, subsequent calls with
        // the same tool list should not invalidate the prefix.
        let tools = vec![make_tool("t1")];
        let mut mgr = PrefixStabilityManager::new("sys", Some(&tools));
        assert!(mgr.check_and_update("sys", Some(&tools)).is_ok());
        assert!(mgr.check_and_update("sys", Some(&tools)).is_ok());
        assert_eq!(mgr.change_count(), 0);
    }
}

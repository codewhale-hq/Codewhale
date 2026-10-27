//! Session state management for the core engine.
//!
//! Tracks conversation history, token usage, and session metadata.

use crate::project_context::ProjectContext;
use crate::working_set::WorkingSet;
use codewhale_core::prefix_cache::PrefixStabilityManager;
use codewhale_execpolicy::ApprovalMode;
use codewhale_models::{Message, SystemPrompt, Usage};
use codewhale_runtime::prompt_zones::{AppendLog, FrozenPrefix};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;

/// Number of deferred schemas a conversation keeps in its active toolbox
/// before another admission evicts one. Tools used in the current or previous
/// turn are never evicted, so they can hold the toolbox above this bound. The
/// permanent `read`/`write`/`edit`/`bash`/`agent`/`tool_search` router surface
/// is not counted here.
pub(crate) const TOOL_ACTIVATION_CACHE_MAX_NAMES: usize = 8;
/// Added provider-wire schema bytes, under the same soft eviction rule
/// as the name bound. Unknown adapter measurements remain unknown.
pub(crate) const TOOL_ACTIVATION_CACHE_MAX_SCHEMA_BYTES: usize = 16 * 1024;

/// Bounded, process-local conversation cache for tools activated by
/// `tool_search`.
///
/// Only names are retained. Every turn revalidates them against the currently
/// filtered catalog, so a disconnected MCP server, changed allow/deny rule, or
/// mode switch cannot resurrect a tool from an older authority posture.
#[derive(Debug, Clone, Default)]
pub(crate) struct ToolActivationCache {
    /// Least-recently used at the front, most-recently used at the back.
    names: VecDeque<String>,
    last_use: HashMap<String, u64>,
    uses: u64,
    current_turn_floor: u64,
    previous_turn_floor: u64,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub(crate) struct ToolActivationDelta {
    pub(crate) admitted: Vec<String>,
    pub(crate) evicted: Vec<String>,
    pub(crate) rejected: Vec<String>,
    pub(crate) held: Vec<String>,
    pub(crate) schema_bytes: Option<usize>,
    pub(crate) exceeds_soft_target: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ToolActivationMeasurement {
    pub(crate) schema_bytes: Option<usize>,
    pub(crate) unavailable: Vec<String>,
}

pub(crate) type ToolActivationMeasurer<'a> =
    dyn FnMut(&[String]) -> Result<ToolActivationMeasurement, crate::tools::spec::ToolError> + 'a;

impl ToolActivationCache {
    /// Forget every deferred tool activated by the current conversation.
    ///
    /// `Op::SyncSession` calls this before installing another conversation's
    /// identity, history, and workspace. The cache is intentionally
    /// process-local, but it must still be conversation-local.
    pub(crate) fn clear(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn begin_turn(&mut self) {
        self.previous_turn_floor = self.current_turn_floor;
        self.current_turn_floor = self.uses;
    }

    pub(crate) fn record_use(&mut self, name: &str) -> bool {
        let Some(index) = self.names.iter().position(|cached| cached == name) else {
            return false;
        };
        if let Some(name) = self.names.remove(index) {
            self.uses += 1;
            self.last_use.insert(name.clone(), self.uses);
            self.names.push_back(name);
        }
        true
    }

    fn pinned(&self, name: &str) -> bool {
        self.last_use
            .get(name)
            .is_some_and(|used| *used > self.previous_turn_floor)
    }

    fn catalog_tool<'a>(
        catalog: &'a [codewhale_models::Tool],
        name: &str,
    ) -> Option<&'a codewhale_models::Tool> {
        catalog
            .iter()
            .find(|tool| tool.name == name && tool.defer_loading.unwrap_or(false))
    }

    #[cfg(test)]
    fn serialized_bytes(tool: &codewhale_models::Tool) -> usize {
        serde_json::to_vec(tool).map_or(usize::MAX, |bytes| bytes.len())
    }

    #[cfg(test)]
    fn total_serialized_bytes(&self, catalog: &[codewhale_models::Tool]) -> usize {
        self.names
            .iter()
            .filter_map(|name| Self::catalog_tool(catalog, name))
            .map(Self::serialized_bytes)
            .fold(0usize, usize::saturating_add)
    }

    fn exceeds_budget(
        names: &[String],
        measure: &mut ToolActivationMeasurer<'_>,
    ) -> Result<bool, crate::tools::spec::ToolError> {
        let measured = measure(names)?;
        Ok(names.len() > TOOL_ACTIVATION_CACHE_MAX_NAMES
            || measured
                .schema_bytes
                .is_some_and(|bytes| bytes > TOOL_ACTIVATION_CACHE_MAX_SCHEMA_BYTES)
            || !measured.unavailable.is_empty())
    }

    fn eviction_candidate(&self, keep: &[String]) -> Option<usize> {
        self.names
            .iter()
            .enumerate()
            .filter(|&(_, name)| !keep.contains(name) && !self.pinned(name))
            .min_by_key(|&(index, name)| (self.last_use.get(name).copied(), index))
            .map(|(index, _)| index)
    }

    pub(crate) fn revalidate_measured(
        &mut self,
        catalog: &[codewhale_models::Tool],
        measure: &mut ToolActivationMeasurer<'_>,
    ) -> Result<Vec<String>, crate::tools::spec::ToolError> {
        let mut next = self.clone();
        let mut evicted = Vec::new();
        for name in &self.names {
            let keep = if Self::catalog_tool(catalog, name).is_some() {
                !Self::exceeds_budget(std::slice::from_ref(name), measure)?
            } else {
                false
            };
            if !keep {
                next.names.retain(|cached| cached != name);
                next.last_use.remove(name);
                evicted.push(name.clone());
            }
        }
        *self = next;
        Ok(evicted)
    }

    #[cfg(test)]
    pub(crate) fn test_measure(
        catalog: &[codewhale_models::Tool],
        names: &[String],
    ) -> Result<ToolActivationMeasurement, crate::tools::spec::ToolError> {
        Ok(ToolActivationMeasurement {
            schema_bytes: Some(
                names
                    .iter()
                    .filter_map(|name| Self::catalog_tool(catalog, name))
                    .map(Self::serialized_bytes)
                    .fold(0usize, usize::saturating_add),
            ),
            unavailable: names
                .iter()
                .filter(|name| Self::catalog_tool(catalog, name).is_none())
                .cloned()
                .collect(),
        })
    }

    #[cfg(test)]
    pub(crate) fn revalidate(&mut self, catalog: &[codewhale_models::Tool]) -> Vec<String> {
        self.revalidate_measured(catalog, &mut |names| Self::test_measure(catalog, names))
            .unwrap()
    }

    #[cfg(test)]
    pub(crate) fn activate(
        &mut self,
        catalog: &[codewhale_models::Tool],
        requested: &[String],
    ) -> ToolActivationDelta {
        self.activate_measured(catalog, requested, &mut |names| {
            Self::test_measure(catalog, names)
        })
        .unwrap()
    }

    /// Admit requested deferred tools in search-result priority order. An
    /// oversized schema is rejected. Room comes from entries never used
    /// (oldest admission first), then from the least recently used; tools
    /// used in the current or previous turn are never evicted. When they
    /// leave no room, the best match is admitted over the bounds and the rest
    /// are held. The batch is stored best-match-last.
    pub(crate) fn activate_measured(
        &mut self,
        catalog: &[codewhale_models::Tool],
        requested: &[String],
        measure: &mut ToolActivationMeasurer<'_>,
    ) -> Result<ToolActivationDelta, crate::tools::spec::ToolError> {
        let mut next = self.clone();
        let delta = next.activate_inner(catalog, requested, measure)?;
        *self = next;
        Ok(delta)
    }

    fn activate_inner(
        &mut self,
        catalog: &[codewhale_models::Tool],
        requested: &[String],
        measure: &mut ToolActivationMeasurer<'_>,
    ) -> Result<ToolActivationDelta, crate::tools::spec::ToolError> {
        let mut delta = ToolActivationDelta {
            evicted: self.revalidate_measured(catalog, measure)?,
            ..ToolActivationDelta::default()
        };
        let mut seen = HashSet::new();
        let mut batch: Vec<String> = Vec::new();
        for name in requested.iter().filter(|name| seen.insert(name.as_str())) {
            let fits_alone = Self::catalog_tool(catalog, name).is_some()
                && !Self::exceeds_budget(std::slice::from_ref(name), measure)?;
            if !fits_alone {
                delta.rejected.push(name.clone());
                continue;
            }
            if self.names.contains(name) {
                batch.push(name.clone());
                continue;
            }
            let protected = self
                .names
                .iter()
                .filter(|cached| batch.contains(*cached) || self.pinned(cached))
                .chain(std::iter::once(name))
                .cloned()
                .collect::<Vec<_>>();
            if !batch.is_empty() && Self::exceeds_budget(&protected, measure)? {
                delta.held.push(name.clone());
                continue;
            }
            batch.push(name.clone());
            self.names.push_back(name.clone());
            while Self::exceeds_budget(&self.names.iter().cloned().collect::<Vec<_>>(), measure)? {
                let Some(index) = self.eviction_candidate(&batch) else {
                    break;
                };
                if let Some(evicted) = self.names.remove(index) {
                    self.last_use.remove(&evicted);
                    delta.evicted.push(evicted);
                }
            }
        }
        for name in batch.iter().rev() {
            if let Some(index) = self.names.iter().position(|cached| cached == name)
                && let Some(name) = self.names.remove(index)
            {
                self.names.push_back(name);
            }
        }

        let retained = self.names.iter().collect::<HashSet<_>>();
        delta.admitted = requested
            .iter()
            .filter(|name| retained.contains(name))
            .cloned()
            .collect();
        delta.evicted.sort();
        delta.evicted.dedup();
        delta.rejected.sort();
        delta.rejected.dedup();
        let names = self.names.iter().cloned().collect::<Vec<_>>();
        let measured = measure(&names)?;
        if !measured.unavailable.is_empty() {
            return Err(crate::tools::spec::ToolError::not_available(
                "Provider omitted an active deferred schema; toolbox admission was not changed",
            ));
        }
        delta.schema_bytes = measured.schema_bytes;
        delta.exceeds_soft_target = names.len() > TOOL_ACTIVATION_CACHE_MAX_NAMES
            || measured
                .schema_bytes
                .is_some_and(|bytes| bytes > TOOL_ACTIVATION_CACHE_MAX_SCHEMA_BYTES);
        Ok(delta)
    }

    pub(crate) fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }
}

/// Session state for the engine.
#[derive(Debug, Clone)]
pub struct Session {
    /// Model being used
    pub model: String,

    /// Reasoning-effort tier for DeepSeek thinking mode:
    /// `"off" | "low" | "medium" | "high" | "max"`. `None` lets the provider
    /// apply its own defaults.
    pub reasoning_effort: Option<String>,
    /// Whether the user selected automatic reasoning effort.
    pub reasoning_effort_auto: bool,

    /// Whether the user selected automatic model routing.
    pub auto_model: bool,

    /// Workspace directory
    pub workspace: PathBuf,

    /// System prompt (optional)
    pub system_prompt: Option<SystemPrompt>,
    pub instruction_assembly: Option<std::sync::Arc<crate::project_context::InstructionAssembly>>,
    /// True when `system_prompt` is a persisted/runtime-supplied prefix that
    /// should not be replaced by mode/context refreshes.
    pub system_prompt_override: bool,
    /// Hash of the last assembled stable system prompt. Used to avoid
    /// replacing `system_prompt` when unchanged.
    pub last_system_prompt_hash: Option<u64>,
    /// Reason the pinned prefix will move on the next model request, set by
    /// an explicit header-change op (`/model`, mode change, goal edit, session
    /// sync) when it actually alters the system-prompt bytes. Consumed by the
    /// turn loop's prefix check so a declared change re-pins under a logged
    /// reason while an undeclared mid-loop change is reported as drift and the
    /// pin holds. `None` means "no declared change since the last request".
    pub pending_prefix_change_reason: Option<String>,
    /// The explicit prompt inputs (model, mode, goal, route, translation,
    /// verbosity) the pinned system prompt was composed from. At each new user
    /// turn the engine recomposes: if these inputs are unchanged but the
    /// composed bytes differ, that is workspace/instruction/skills/memory
    /// drift and is delivered to the model as a `<context_update>` message,
    /// never by moving the pinned header.
    pub(crate) pinned_prompt_context: Option<crate::core::engine::NextTurnPromptContext>,
    /// Flat text of the session context the model has last been shown, either
    /// as the pinned header or through `<context_update>` messages. New-turn
    /// deltas are computed against this so an update is delivered exactly once.
    pub(crate) context_update_baseline: Option<String>,
    /// Host-persistence copy of the history checkpoint generated by context
    /// compaction. This is never part of the standing system prompt.
    pub compaction_summary_prompt: Option<SystemPrompt>,

    /// Conversation history (API format), backed by AppendLog (#2264).
    pub messages: AppendLog,

    /// Total tokens used in this session
    pub total_usage: SessionUsage,

    /// Whether shell execution is allowed
    pub allow_shell: bool,

    /// Whether to trust paths outside workspace
    pub trust_mode: bool,

    /// Whether the current session should auto-approve tool safety checks.
    pub auto_approve: bool,

    /// Live UI approval policy used to steer the system prompt.
    pub approval_mode: ApprovalMode,

    /// Notes file path
    pub notes_path: PathBuf,

    /// MCP config path
    pub mcp_config_path: PathBuf,

    /// Session ID (for tracking)
    pub id: String,

    /// Project context loaded from AGENTS.md, etc.
    pub project_context: Option<ProjectContext>,

    /// Repo-aware working set for context management.
    pub working_set: WorkingSet,

    /// Prefix-cache stability monitor (inspired by Reasonix's Pillar 1).
    /// Tracks the immutable prefix fingerprint and detects drift across turns.
    /// Set during engine construction; None until the first system prompt assembly.
    pub prefix_stability: Option<PrefixStabilityManager>,

    /// Three-zone immutable prefix baseline (#2264). Frozen on the first
    /// request of the session; verified against the current system+tool
    /// state before every subsequent request. None until the first turn.
    pub frozen_prefix: Option<FrozenPrefix>,

    /// Deferred tools explicitly discovered during this conversation. Names
    /// are revalidated against the live catalog before each request.
    pub(super) tool_activation_cache: ToolActivationCache,

    /// Monotonic counter bumped on every direct mutation of `messages`.
    /// Consumed by the engine token-estimate cache
    /// to memoize the per-turn token estimate without re-walking the message
    /// list. Defaults to 0; bumped in [`Session::add_message`],
    /// [`Session::replace_messages`], and at other mutation sites in
    /// `core/engine.rs`.
    pub messages_revision: u64,

    /// Provider-billed prompt tokens from the most recent parent-route
    /// request that still describes the live message list. Survives turn
    /// boundaries so the next send can compact on last-turn pressure
    /// (#5577). Cleared when history is rewritten (compaction, restore).
    pub(crate) latest_parent_input_tokens: Option<u32>,
}

/// Cumulative usage statistics for a session.
#[derive(Debug, Clone, Default)]
#[allow(clippy::struct_field_names)]
pub struct SessionUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Cache creation (write) tokens. `None` when never observed by the API —
    /// do NOT display as 0, which would be indistinguishable from "no writes".
    pub cache_creation_input_tokens: Option<u64>,
    /// Cache read (hit) tokens. `None` when never observed by the API —
    /// do NOT display as 0, which would be indistinguishable from "no hits".
    pub cache_read_input_tokens: Option<u64>,
}

impl SessionUsage {
    /// Add usage from a turn
    pub fn add(&mut self, usage: &Usage) {
        self.input_tokens += u64::from(usage.input_tokens);
        self.output_tokens += u64::from(usage.output_tokens);
        if let Some(tokens) = usage.prompt_cache_write_tokens {
            self.cache_creation_input_tokens =
                Some(self.cache_creation_input_tokens.unwrap_or(0) + u64::from(tokens));
        }
        if let Some(tokens) = usage.prompt_cache_hit_tokens {
            self.cache_read_input_tokens =
                Some(self.cache_read_input_tokens.unwrap_or(0) + u64::from(tokens));
        }
    }
}

impl Session {
    /// Create a new session
    pub fn new(
        model: String,
        workspace: PathBuf,
        allow_shell: bool,
        trust_mode: bool,
        notes_path: PathBuf,
        mcp_config_path: PathBuf,
    ) -> Self {
        Self {
            model,
            reasoning_effort: None,
            reasoning_effort_auto: false,
            auto_model: false,
            workspace,
            system_prompt: None,
            instruction_assembly: None,
            system_prompt_override: false,
            compaction_summary_prompt: None,
            messages: AppendLog::new(),
            total_usage: SessionUsage::default(),
            allow_shell,
            trust_mode,
            auto_approve: false,
            approval_mode: ApprovalMode::Suggest,
            notes_path,
            mcp_config_path,
            id: uuid::Uuid::new_v4().to_string(),
            project_context: None,
            last_system_prompt_hash: None,
            pending_prefix_change_reason: None,
            pinned_prompt_context: None,
            context_update_baseline: None,
            working_set: WorkingSet::default(),
            prefix_stability: None,
            frozen_prefix: None,
            tool_activation_cache: ToolActivationCache::default(),
            messages_revision: 0,
            latest_parent_input_tokens: None,
        }
    }

    /// Add a message to the conversation
    pub fn add_message(&mut self, message: Message) {
        self.messages.push(message);
        self.messages_revision = self.messages_revision.saturating_add(1);
    }

    /// Replace the entire message history. Used by session resume and
    /// compaction. Bumps `messages_revision` exactly once even when the new
    /// history has a different length, so downstream caches invalidate
    /// atomically.
    pub fn replace_messages(&mut self, messages: Vec<Message>) {
        self.messages = messages.into();
        self.messages_revision = self.messages_revision.saturating_add(1);
        self.latest_parent_input_tokens = None;
    }

    /// Bump `messages_revision` without otherwise mutating the message list.
    /// Reserved for sites that mutate the message list in place (e.g. an
    /// in-place rewrite of a content block). Most call sites do not need
    /// this — prefer [`add_message`](Self::add_message) and
    /// [`replace_messages`](Self::replace_messages).
    pub fn bump_messages_revision(&mut self) {
        self.messages_revision = self.messages_revision.saturating_add(1);
    }

    /// Rebuild the working set from current messages (best effort).
    pub fn rebuild_working_set(&mut self) {
        self.working_set
            .rebuild_from_messages(&self.messages, &self.workspace);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn deferred_tool(name: &str, description_bytes: usize) -> codewhale_models::Tool {
        codewhale_models::Tool {
            tool_type: None,
            name: name.to_string(),
            description: "x".repeat(description_bytes),
            input_schema: json!({"type": "object", "properties": {}}),
            allowed_callers: None,
            defer_loading: Some(true),
            input_examples: None,
            strict: None,
            cache_control: None,
        }
    }

    #[test]
    fn session_usage_cache_starts_none() {
        let usage = SessionUsage::default();
        assert!(usage.cache_creation_input_tokens.is_none());
        assert!(usage.cache_read_input_tokens.is_none());
    }

    #[test]
    fn session_usage_cache_remains_none_when_api_omits_cache() {
        let mut usage = SessionUsage::default();
        let api_usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            prompt_cache_hit_tokens: None,
            prompt_cache_miss_tokens: None,
            prompt_cache_write_tokens: None,
            reasoning_tokens: None,
            reasoning_replay_tokens: None,
            server_tool_use: None,

            input_audio_tokens: None,
            output_audio_tokens: None,

            request_usages: Vec::new(),
        };
        usage.add(&api_usage);
        assert!(usage.cache_creation_input_tokens.is_none());
        assert!(usage.cache_read_input_tokens.is_none());
    }

    #[test]
    fn session_usage_cache_accumulates_when_reported() {
        let mut usage = SessionUsage::default();
        let api_usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            prompt_cache_hit_tokens: Some(30),
            prompt_cache_miss_tokens: Some(50),
            prompt_cache_write_tokens: Some(20),
            reasoning_tokens: None,
            reasoning_replay_tokens: None,
            server_tool_use: None,

            input_audio_tokens: None,
            output_audio_tokens: None,

            request_usages: Vec::new(),
        };
        usage.add(&api_usage);
        assert_eq!(usage.cache_read_input_tokens, Some(30));
        assert_eq!(usage.cache_creation_input_tokens, Some(20));
        usage.add(&api_usage);
        assert_eq!(usage.cache_read_input_tokens, Some(60));
        assert_eq!(usage.cache_creation_input_tokens, Some(40));
    }

    #[test]
    fn session_usage_cache_preserves_explicit_zero() {
        let mut usage = SessionUsage::default();
        let api_usage = Usage {
            input_tokens: 100,
            output_tokens: 50,
            prompt_cache_hit_tokens: Some(0), // explicit zero from provider
            prompt_cache_miss_tokens: Some(50),
            prompt_cache_write_tokens: Some(1234),
            reasoning_tokens: None,
            reasoning_replay_tokens: None,
            server_tool_use: None,

            input_audio_tokens: None,
            output_audio_tokens: None,

            request_usages: Vec::new(),
        };
        usage.add(&api_usage);
        // 0 is a valid observed value, must NOT be converted to None
        assert_eq!(usage.cache_read_input_tokens, Some(0));
        assert_eq!(usage.cache_creation_input_tokens, Some(1234));
    }

    #[test]
    fn tool_activation_cache_retains_best_matches_on_count_overflow() {
        let catalog = (0..10)
            .map(|index| deferred_tool(&format!("tool_{index}"), 8))
            .collect::<Vec<_>>();
        let requested = catalog
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let mut cache = ToolActivationCache::default();
        let delta = cache.activate(&catalog, &requested);

        assert_eq!(cache.names().count(), TOOL_ACTIVATION_CACHE_MAX_NAMES);
        assert_eq!(
            cache.names().collect::<Vec<_>>(),
            vec![
                "tool_7", "tool_6", "tool_5", "tool_4", "tool_3", "tool_2", "tool_1", "tool_0"
            ]
        );
        assert_eq!(delta.admitted, requested[..TOOL_ACTIVATION_CACHE_MAX_NAMES]);
        assert_eq!(delta.held, vec!["tool_8", "tool_9"]);
    }

    #[test]
    fn best_match_survives_byte_overflow_eviction() {
        let mut catalog = vec![deferred_tool("big_best", 12_000)];
        for index in 0..4 {
            catalog.push(deferred_tool(&format!("small_{index}"), 1_500));
        }
        let requested = catalog
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        // Isolate byte overflow: every schema fits alone and the name count
        // fits, but the combined batch exceeds the current 16 KiB limit.
        assert!(catalog.iter().all(|tool| {
            ToolActivationCache::serialized_bytes(tool) <= TOOL_ACTIVATION_CACHE_MAX_SCHEMA_BYTES
        }));
        assert!(requested.len() < TOOL_ACTIVATION_CACHE_MAX_NAMES);
        assert!(
            catalog
                .iter()
                .map(ToolActivationCache::serialized_bytes)
                .sum::<usize>()
                > TOOL_ACTIVATION_CACHE_MAX_SCHEMA_BYTES
        );
        let mut cache = ToolActivationCache::default();
        let delta = cache.activate(&catalog, &requested);

        assert_eq!(delta.admitted.first().map(String::as_str), Some("big_best"));
        assert!(cache.names().any(|name| name == "big_best"));
        assert!(delta.admitted.len() < requested.len());
        assert_eq!(delta.admitted, requested[..delta.admitted.len()]);
        assert!(!delta.held.is_empty());
        assert!(delta.rejected.is_empty());
        assert!(cache.total_serialized_bytes(&catalog) <= TOOL_ACTIVATION_CACHE_MAX_SCHEMA_BYTES);
    }

    #[test]
    fn duplicate_does_not_lower_first_match_priority() {
        let catalog = (0..10)
            .map(|index| deferred_tool(&format!("tool_{index}"), 8))
            .collect::<Vec<_>>();
        let mut requested = catalog
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        requested.push("tool_0".to_string());
        let mut cache = ToolActivationCache::default();
        let delta = cache.activate(&catalog, &requested);

        assert_eq!(cache.names().count(), TOOL_ACTIVATION_CACHE_MAX_NAMES);
        assert_eq!(cache.names().filter(|name| *name == "tool_0").count(), 1);
        assert!(cache.names().any(|name| name == "tool_0"));
        assert!(
            !cache
                .names()
                .any(|name| name == "tool_8" || name == "tool_9")
        );
        // Admission reporting keeps the caller's order, including duplicates,
        // while the cache itself retains each name only once.
        let mut expected = requested[..TOOL_ACTIVATION_CACHE_MAX_NAMES].to_vec();
        expected.push("tool_0".to_string());
        assert_eq!(delta.admitted, expected);
    }

    #[test]
    fn touching_a_cached_tool_makes_it_most_recent() {
        let catalog = (0..9)
            .map(|index| deferred_tool(&format!("tool_{index}"), 8))
            .collect::<Vec<_>>();
        let first_eight = catalog[..8]
            .iter()
            .map(|tool| tool.name.clone())
            .collect::<Vec<_>>();
        let mut cache = ToolActivationCache::default();
        cache.activate(&catalog, &first_eight);
        // The initial batch puts its lowest-ranked match at the LRU front.
        // A later touch must protect it from the following batch's eviction.
        cache.activate(&catalog, &["tool_7".to_string()]);
        cache.activate(&catalog, &["tool_8".to_string()]);

        let names = cache.names().collect::<Vec<_>>();
        assert!(names.contains(&"tool_7"));
        assert!(!names.contains(&"tool_6"));
        assert_eq!(names.last().copied(), Some("tool_8"));
    }

    #[test]
    fn oversized_schema_is_never_admitted() {
        let catalog = vec![deferred_tool(
            "huge",
            TOOL_ACTIVATION_CACHE_MAX_SCHEMA_BYTES + 1,
        )];
        let mut cache = ToolActivationCache::default();
        let delta = cache.activate(&catalog, &["huge".to_string()]);

        assert_eq!(cache.names().count(), 0);
        assert_eq!(delta.rejected, vec!["huge"]);
    }

    #[test]
    fn revalidate_drops_removed_denied_or_eager_tools() {
        let catalog = vec![deferred_tool("kept", 8), deferred_tool("gone", 8)];
        let mut cache = ToolActivationCache::default();
        cache.activate(&catalog, &["kept".to_string(), "gone".to_string()]);
        let mut next_catalog = vec![deferred_tool("kept", 8), deferred_tool("gone", 8)];
        next_catalog[1].defer_loading = Some(false);

        let evicted = cache.revalidate(&next_catalog);
        assert_eq!(cache.names().collect::<Vec<_>>(), vec!["kept"]);
        assert_eq!(evicted, vec!["gone"]);
    }

    #[test]
    fn clearing_for_session_sync_forgets_all_activated_tools() {
        let catalog = vec![deferred_tool("one", 8), deferred_tool("two", 8)];
        let mut cache = ToolActivationCache::default();
        cache.activate(&catalog, &["one".to_string(), "two".to_string()]);

        cache.clear();

        assert_eq!(cache.names().count(), 0);
    }
}

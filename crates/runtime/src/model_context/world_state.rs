//! WorldState: ordered collection of ModelContext fragments with diff render.

use std::collections::BTreeMap;

use codewhale_models::SystemBlock;

use super::fragment::{FragmentId, FragmentRender, FragmentRole, ModelContextFragment};

/// Incremental render of WorldState against a previous snapshot.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorldStateDiff {
    /// Fragments whose content hash changed (or are new).
    pub updated: Vec<ModelContextFragment>,
    /// Markers that matched the previous snapshot byte-for-byte.
    pub retained: Vec<String>,
    /// Markers present previously and now cleared.
    pub cleared: Vec<String>,
}

/// Mutable mid-session context layer living below the constitution prefix.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorldState {
    fragments: BTreeMap<FragmentId, ModelContextFragment>,
}

impl WorldState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a fragment. Returns retain-unchanged when the hash
    /// matches the previous value for that id.
    pub fn upsert(&mut self, fragment: ModelContextFragment) -> FragmentRender {
        let previous = self.fragments.get(&fragment.id).cloned();
        let render = fragment.render_diff(previous.as_ref());
        if matches!(render, FragmentRender::Updated { .. }) {
            self.fragments.insert(fragment.id, fragment);
        }
        render
    }

    // Exported from codewhale-runtime, so clippy now asks for `is_empty`;
    // the move adds no API (runtime split, behavior-free).
    #[allow(clippy::len_without_is_empty)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    /// Full render of every fragment in stable `FragmentId` order.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn render_full(&self) -> String {
        self.fragments
            .values()
            .map(ModelContextFragment::render_marked)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Diff against a previous WorldState. Unchanged fragments are retained
    /// (listed, not reinjected into `updated`).
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn render_diff(&self, previous: Option<&WorldState>) -> WorldStateDiff {
        let Some(previous) = previous else {
            return WorldStateDiff {
                updated: self.fragments.values().cloned().collect(),
                retained: Vec::new(),
                cleared: Vec::new(),
            };
        };

        let mut diff = WorldStateDiff::default();
        for id in FragmentId::all() {
            match (previous.fragments.get(id), self.fragments.get(id)) {
                (Some(prev), Some(next)) => match next.render_diff(Some(prev)) {
                    FragmentRender::Unchanged { marker, .. } => diff.retained.push(marker),
                    FragmentRender::Updated { fragment } => diff.updated.push(fragment),
                    FragmentRender::Cleared { marker } => diff.cleared.push(marker),
                },
                (None, Some(next)) => diff.updated.push(next.clone()),
                (Some(prev), None) => diff.cleared.push(prev.marker.to_string()),
                (None, None) => {}
            }
        }
        diff
    }

    /// Convenience builders for the candidate volatile concerns.
    #[must_use]
    pub fn with_workspace(mut self, body: impl Into<String>) -> Self {
        self.upsert(ModelContextFragment::new(
            FragmentId::Workspace,
            FragmentRole::Workspace,
            body,
        ));
        self
    }

    #[must_use]
    pub fn with_permissions(mut self, body: impl Into<String>) -> Self {
        self.upsert(ModelContextFragment::new(
            FragmentId::Permissions,
            FragmentRole::Permissions,
            body,
        ));
        self
    }

    #[must_use]
    pub fn with_route(mut self, body: impl Into<String>) -> Self {
        self.upsert(ModelContextFragment::new(
            FragmentId::Route,
            FragmentRole::Route,
            body,
        ));
        self
    }

    #[must_use]
    pub fn with_agent_topology(mut self, body: impl Into<String>) -> Self {
        self.upsert(ModelContextFragment::new(
            FragmentId::AgentTopology,
            FragmentRole::AgentTopology,
            body,
        ));
        self
    }

    #[must_use]
    pub fn with_skills_tools(mut self, body: impl Into<String>) -> Self {
        self.upsert(ModelContextFragment::new(
            FragmentId::SkillsTools,
            FragmentRole::SkillsTools,
            body,
        ));
        self
    }

    #[must_use]
    pub fn with_session_relay(mut self, body: impl Into<String>) -> Self {
        self.upsert(ModelContextFragment::new(
            FragmentId::SessionRelay,
            FragmentRole::SessionRelay,
            body,
        ));
        self
    }

    #[must_use]
    pub fn with_project_instructions(mut self, body: impl Into<String>) -> Self {
        self.upsert(ModelContextFragment::new(
            FragmentId::ProjectInstructions,
            FragmentRole::ProjectInstructions,
            body,
        ));
        self
    }

    /// Enforce the hard caps for this WorldState. Returns an error if the
    /// fragment count or any fragment's byte/token size exceeds the core
    /// ceilings (`MAX_FRAGMENT_BYTES` / `MAX_FRAGMENT_TOKENS`).
    pub fn validate_caps(&self) -> Result<(), String> {
        use crate::model_context::fragment::{
            MAX_FRAGMENT_BYTES, MAX_FRAGMENT_TOKENS, MAX_FRAGMENTS_PER_CONTEXT,
        };
        if self.fragments.len() > MAX_FRAGMENTS_PER_CONTEXT {
            return Err(format!(
                "too many fragments: {} > {}",
                self.fragments.len(),
                MAX_FRAGMENTS_PER_CONTEXT
            ));
        }
        for fragment in self.fragments.values() {
            if fragment.content.len() > MAX_FRAGMENT_BYTES {
                return Err(format!(
                    "fragment {:?} exceeds byte ceiling: {} > {}",
                    fragment.id,
                    fragment.content.len(),
                    MAX_FRAGMENT_BYTES
                ));
            }
            let tokens = fragment.content.len().div_ceil(4);
            if tokens > MAX_FRAGMENT_TOKENS {
                return Err(format!(
                    "fragment {:?} exceeds token ceiling: {} > {}",
                    fragment.id, tokens, MAX_FRAGMENT_TOKENS
                ));
            }
        }
        Ok(())
    }
}

/// Constitution (cache-stable) + WorldState (volatile) assembly point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorldStateSnapshot {
    pub constitution: String,
    pub world_state: WorldState,
}

impl WorldStateSnapshot {
    /// Structured blocks: constitution first (cacheable), then each fragment.
    #[must_use]
    pub fn to_system_blocks(&self) -> Vec<SystemBlock> {
        let mut blocks = Vec::with_capacity(1 + self.world_state.len());
        blocks.push(SystemBlock {
            block_type: "text".to_string(),
            text: self.constitution.trim().to_string(),
            cache_control: None,
        });
        for fragment in self.world_state.fragments.values() {
            blocks.push(SystemBlock {
                block_type: "text".to_string(),
                text: fragment.render_marked(),
                cache_control: None,
            });
        }
        blocks
    }

    /// Flat text fallback for callers that still expect `SystemPrompt::Text`.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn render_text(&self) -> String {
        let world = self.world_state.render_full();
        if world.is_empty() {
            self.constitution.trim().to_string()
        } else {
            format!("{}\n\n{}", self.constitution.trim(), world)
        }
    }
}

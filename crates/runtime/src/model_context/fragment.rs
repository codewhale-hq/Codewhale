//! One typed, capped, marker-stable ModelContext fragment.
//!
//! Caps and identities are unified with `codewhale_core::fragments` — the
//! single `crates/core` owner for the bounded fragment system (issue #5264).
//! This crate re-exports the core caps so every injection goes through a
//! typed fragment with a stable marker and the hard caps
//! (per-fragment size, 10K-token ceiling, injected-item count) are enforced
//! in one place.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Re-export the core hard caps — single source of truth.
pub use codewhale_core::fragments::{
    DEFAULT_FRAGMENT_MAX_BYTES, MAX_FRAGMENT_BYTES, MAX_FRAGMENT_TOKENS, MAX_FRAGMENTS_PER_CONTEXT,
};

/// Stable identity for a WorldState concern. Markers are public contract —
/// do not rename without a migration note (prefix-cache + tests pin them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FragmentId {
    Workspace,
    Permissions,
    Route,
    AgentTopology,
    SkillsTools,
    TokenBudget,
    ProjectInstructions,
    Constitution,
}

impl FragmentId {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Permissions => "permissions",
            Self::Route => "route",
            Self::AgentTopology => "agent_topology",
            Self::SkillsTools => "skills_tools",
            Self::TokenBudget => "token_budget",
            Self::ProjectInstructions => "project_instructions",
            Self::Constitution => "constitution",
        }
    }

    /// Stable HTML-comment marker wrapping the fragment body.
    #[must_use]
    pub fn marker(self) -> &'static str {
        match self {
            Self::Workspace => "<!-- cw:ctx:workspace -->",
            Self::Permissions => "<!-- cw:ctx:permissions -->",
            Self::Route => "<!-- cw:ctx:route -->",
            Self::AgentTopology => "<!-- cw:ctx:agent_topology -->",
            Self::SkillsTools => "<!-- cw:ctx:skills_tools -->",
            Self::TokenBudget => "<!-- cw:ctx:token_budget -->",
            Self::ProjectInstructions => "<!-- cw:ctx:project_instructions -->",
            Self::Constitution => "<!-- cw:ctx:constitution -->",
        }
    }

    #[must_use]
    pub fn role(self) -> FragmentRole {
        match self {
            Self::Workspace => FragmentRole::Workspace,
            Self::Permissions => FragmentRole::Permissions,
            Self::Route => FragmentRole::Route,
            Self::AgentTopology => FragmentRole::AgentTopology,
            Self::SkillsTools => FragmentRole::SkillsTools,
            Self::TokenBudget => FragmentRole::TokenBudget,
            Self::ProjectInstructions => FragmentRole::ProjectInstructions,
            Self::Constitution => FragmentRole::Constitution,
        }
    }

    #[must_use]
    pub fn all() -> &'static [FragmentId] {
        &[
            Self::Workspace,
            Self::Permissions,
            Self::Route,
            Self::AgentTopology,
            Self::SkillsTools,
            Self::TokenBudget,
            Self::ProjectInstructions,
            Self::Constitution,
        ]
    }
}

/// Explicit role of a fragment relative to the cache-stable constitution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FragmentRole {
    /// Workspace / repo working-set facts (volatile across sessions).
    Workspace,
    /// Approval / permission posture.
    Permissions,
    /// Active route, model, and app mode.
    Route,
    /// Sub-agent topology and recent completion notices.
    AgentTopology,
    /// Skills, plugins, and tool availability summary.
    SkillsTools,
    /// Token budget and compaction status.
    TokenBudget,
    /// Project instructions (AGENTS.md + imported instruction files).
    ProjectInstructions,
    /// Codewhale-specific repo constitution.
    Constitution,
}

impl FragmentRole {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Permissions => "permissions",
            Self::Route => "route",
            Self::AgentTopology => "agent_topology",
            Self::SkillsTools => "skills_tools",
            Self::TokenBudget => "token_budget",
            Self::ProjectInstructions => "project_instructions",
            Self::Constitution => "constitution",
        }
    }
}

/// Result of comparing a fragment against its previous render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentRender {
    /// Content hash matches previous — retain bytes; do not reinject.
    Unchanged { marker: String, content_hash: u64 },
    /// New or changed content — inject the capped body.
    Updated { fragment: ModelContextFragment },
    /// Fragment was present before and is now absent.
    Cleared { marker: String },
}

/// One capped WorldState section with a stable marker and content hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelContextFragment {
    pub id: FragmentId,
    pub role: FragmentRole,
    pub marker: &'static str,
    pub max_bytes: usize,
    pub content: String,
    pub content_hash: u64,
}

impl ModelContextFragment {
    #[must_use]
    pub fn new(id: FragmentId, role: FragmentRole, raw: impl Into<String>) -> Self {
        Self::with_max_bytes(id, role, raw, DEFAULT_FRAGMENT_MAX_BYTES)
    }

    #[must_use]
    pub fn with_max_bytes(
        id: FragmentId,
        role: FragmentRole,
        raw: impl Into<String>,
        max_bytes: usize,
    ) -> Self {
        // Clamp to the global 10K-token ceiling so callers cannot opt out.
        let clamped_max = max_bytes.min(MAX_FRAGMENT_BYTES);
        let mut content = enforce_byte_cap(raw.into(), clamped_max);
        // Token-ceiling safety net (4 bytes ≈ 1 token).
        if content.len().div_ceil(4) > MAX_FRAGMENT_TOKENS {
            content = enforce_byte_cap(content, MAX_FRAGMENT_BYTES);
        }
        let content_hash = hash_content(&content);
        Self {
            id,
            role,
            marker: id.marker(),
            max_bytes: clamped_max,
            content,
            content_hash,
        }
    }

    /// Return whether `haystack` contains this fragment's stable marker.
    #[must_use]
    pub fn matches_text(&self, haystack: &str) -> bool {
        haystack.contains(self.marker)
    }

    /// Compare against a previous fragment of the same id.
    #[must_use]
    pub fn render_diff(&self, previous: Option<&Self>) -> FragmentRender {
        match previous {
            Some(prev) if prev.content_hash == self.content_hash && prev.marker == self.marker => {
                FragmentRender::Unchanged {
                    marker: self.marker.to_string(),
                    content_hash: self.content_hash,
                }
            }
            _ => FragmentRender::Updated {
                fragment: self.clone(),
            },
        }
    }

    /// Full render including the stable marker header.
    #[must_use]
    pub fn render_marked(&self) -> String {
        let rendered = format!("{}\n{}", self.marker, self.content.trim_end());
        debug_assert!(self.matches_text(&rendered));
        rendered
    }
}

fn hash_content(content: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    hasher.finish()
}

fn enforce_byte_cap(raw: String, max_bytes: usize) -> String {
    if max_bytes == 0 {
        return String::new();
    }
    if raw.len() <= max_bytes {
        return raw;
    }
    let omitted = raw.len().saturating_sub(max_bytes);
    let marker = format!("\n[…truncated: {omitted} bytes omitted]");
    if marker.len() >= max_bytes {
        return marker.chars().take(max_bytes).collect();
    }
    let keep = max_bytes.saturating_sub(marker.len());
    // Truncate on a char boundary.
    let mut end = keep;
    while end > 0 && !raw.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = raw[..end].to_string();
    out.push_str(&marker);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_diff_detects_change_and_retain() {
        let a = ModelContextFragment::new(FragmentId::Route, FragmentRole::Route, "m=a");
        let b = ModelContextFragment::new(FragmentId::Route, FragmentRole::Route, "m=a");
        let c = ModelContextFragment::new(FragmentId::Route, FragmentRole::Route, "m=b");
        assert!(matches!(
            b.render_diff(Some(&a)),
            FragmentRender::Unchanged { .. }
        ));
        assert!(matches!(
            c.render_diff(Some(&a)),
            FragmentRender::Updated { .. }
        ));
        assert!(matches!(
            a.render_diff(None),
            FragmentRender::Updated { .. }
        ));
    }
}

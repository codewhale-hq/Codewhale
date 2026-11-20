//! Typed incremental ModelContext / WorldState fragments.
//!
//! CodeWhale already assembles prompts most-static → most-volatile for prefix
//! caching (`prompts.rs`). This module adds the missing identity layer: each
//! volatile concern is a capped, marked fragment with `render_diff` so an
//! environment or agent-topology change does not rebuild unrelated material.
//!
//! The constitution / mode base stays outside WorldState and remains the
//! cache-stable prefix. WorldState owns only the mutable mid-session layers.

mod fragment;
mod world_state;

#[allow(unused_imports)] // public ModelContext surface for TUI-DOG-011 prompt cutover siblings
pub use fragment::{
    DEFAULT_FRAGMENT_MAX_BYTES, FragmentId, FragmentRender, FragmentRole, ModelContextFragment,
};
pub use world_state::{WorldState, WorldStateSnapshot};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_fragment_is_retained_not_reinjected() {
        let mut state = WorldState::new();
        let first = ModelContextFragment::new(
            FragmentId::Workspace,
            FragmentRole::Workspace,
            "repo: /tmp/demo\nbranch: main",
        );
        let render = state.upsert(first.clone());
        assert!(matches!(render, FragmentRender::Updated { .. }));

        let again = ModelContextFragment::new(
            FragmentId::Workspace,
            FragmentRole::Workspace,
            "repo: /tmp/demo\nbranch: main",
        );
        let render = state.upsert(again);
        assert_eq!(
            render,
            FragmentRender::Unchanged {
                marker: FragmentId::Workspace.marker().to_string(),
                content_hash: first.content_hash,
            }
        );

        let diff = state.render_diff(None);
        assert_eq!(diff.updated.len(), 1);
        assert!(diff.retained.is_empty());

        let previous = state.clone();
        let diff = state.render_diff(Some(&previous));
        assert!(diff.updated.is_empty());
        assert_eq!(
            diff.retained,
            vec![FragmentId::Workspace.marker().to_string()]
        );
    }

    #[test]
    fn hard_byte_cap_is_enforced_with_truncation_marker() {
        let oversized = "x".repeat(DEFAULT_FRAGMENT_MAX_BYTES + 64);
        let fragment = ModelContextFragment::new(
            FragmentId::AgentTopology,
            FragmentRole::AgentTopology,
            oversized,
        );
        assert!(fragment.content.len() <= DEFAULT_FRAGMENT_MAX_BYTES);
        assert!(
            fragment.content.contains("[…truncated:"),
            "truncated marker must remain visible: {}",
            fragment.content
        );
        assert_eq!(fragment.marker, FragmentId::AgentTopology.marker());
    }

    #[test]
    fn markers_are_stable_across_rebuilds() {
        let a = ModelContextFragment::new(
            FragmentId::Permissions,
            FragmentRole::Permissions,
            "approval: ask",
        );
        let b = ModelContextFragment::new(
            FragmentId::Permissions,
            FragmentRole::Permissions,
            "approval: auto",
        );
        assert_eq!(a.marker, b.marker);
        assert_eq!(a.marker, "<!-- cw:ctx:permissions -->");
        assert_ne!(a.content_hash, b.content_hash);
    }

    #[test]
    fn world_state_diff_updates_only_changed_fragments() {
        let mut previous = WorldState::new();
        previous.upsert(ModelContextFragment::new(
            FragmentId::Route,
            FragmentRole::Route,
            "model: deepseek-v4\nmode: agent",
        ));
        previous.upsert(ModelContextFragment::new(
            FragmentId::SessionRelay,
            FragmentRole::SessionRelay,
            "budget: 100000\ncompaction: idle",
        ));

        let mut next = previous.clone();
        next.upsert(ModelContextFragment::new(
            FragmentId::SessionRelay,
            FragmentRole::SessionRelay,
            "budget: 100000\ncompaction: pending",
        ));

        let diff = next.render_diff(Some(&previous));
        assert_eq!(diff.retained, vec![FragmentId::Route.marker().to_string()]);
        assert_eq!(diff.updated.len(), 1);
        assert_eq!(diff.updated[0].id, FragmentId::SessionRelay);
        assert!(diff.updated[0].content.contains("compaction: pending"));
    }

    #[test]
    fn snapshot_renders_constitution_then_world_state_blocks() {
        let mut state = WorldState::new();
        state.upsert(ModelContextFragment::new(
            FragmentId::Workspace,
            FragmentRole::Workspace,
            "pwd: /ws",
        ));
        state.upsert(ModelContextFragment::new(
            FragmentId::SkillsTools,
            FragmentRole::SkillsTools,
            "skills: 2 available",
        ));

        let snapshot = WorldStateSnapshot {
            constitution: "You are CodeWhale.".to_string(),
            world_state: state,
        };
        let blocks = snapshot.to_system_blocks();
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].text, "You are CodeWhale.");
        assert!(blocks[1].text.starts_with(FragmentId::Workspace.marker()));
        assert!(blocks[2].text.starts_with(FragmentId::SkillsTools.marker()));

        let text = snapshot.render_text();
        assert!(text.starts_with("You are CodeWhale."));
        assert!(text.contains(FragmentId::Workspace.marker()));
        assert!(text.contains(FragmentId::SkillsTools.marker()));
    }
}

//! Behavioural guards: positive statements of promises a later commit must not
//! quietly remove.
//!
//! Each guard cites the issue that created it. The contrast is the failure mode
//! in #7433: an inline test was renamed and its assertion inverted in place
//! (`2282c33f0`, `system_prompt_merges_workspace_and_configured_skills_dir` →
//! `system_prompt_carries_no_skills_index`), so the suite stayed green while the
//! documented behaviour disappeared. A positive statement in this file fails
//! when the behaviour goes away; it can only be deleted or rewritten, and either
//! is a one-line diff a reviewer sees.
//!
//! Keep one promise per guard. This is not a general test suite: put ordinary
//! behaviour tests next to the code they cover.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("resolve workspace root from CARGO_MANIFEST_DIR")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(repo_root().join(rel)).unwrap_or_else(|err| panic!("read {rel}: {err}"))
}

/// #7433 — an issue is not startable until a maintainer grants `agent-ready`.
///
/// The Agent task form may stay for maintainers, but it must not pre-authorise
/// an agent by applying the label at creation. The metadata contract test
/// (`.github/scripts/agent-task-metadata.test.sh`) and this guard fail together
/// if the line comes back.
#[test]
fn agent_task_form_does_not_self_apply_agent_ready() {
    let template = read(".github/ISSUE_TEMPLATE/agent-task.yml");
    assert!(
        template.contains("name: Agent task"),
        "the Agent task form moved or was deleted; update this guard deliberately (#7433)"
    );
    if let Some(line) = template
        .lines()
        .find(|line| line.trim_start().starts_with("labels:") && line.contains("agent-ready"))
    {
        panic!("the Agent task form self-applies agent-ready again (#7433): {line}");
    }
}

/// #7423 — synthesized tool-call ids never collide with literal ids.
///
/// `client/chat/tests/stream_decoder.rs` proves the behaviour for specific
/// streams. This guard keeps the allocation rule itself: literal ids seed the
/// assigned set, a fallback id is allocated against it, and a collision retries
/// with a suffix. A refactor that drops any half fails here even if the unit
/// tests move with it.
#[test]
fn chat_fallback_ids_are_allocated_against_literal_ids() {
    let source = read("crates/engine/src/client/chat.rs");
    assert!(
        source.contains("filter_map(|(_, id)| id.clone())"),
        "literal tool-call ids are no longer seeded into the assigned set (#7423)"
    );
    assert!(
        source.contains("synthesize_chat_call_id(ordinal, &mut assigned)"),
        "fallback ids are no longer allocated against the assigned set (#7423)"
    );
    assert!(
        source.contains("while !assigned.insert(id.clone())"),
        "fallback ids no longer retry on collision (#7423)"
    );
}

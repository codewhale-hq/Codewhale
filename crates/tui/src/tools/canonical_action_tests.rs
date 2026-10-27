use codewhale_execpolicy::canonical_action::*;
use serde_json::json;

/// Names the v0.9.3 consolidation retired from the model-facing catalog.
/// Each stays registered only as a hidden (`model_visible=false`)
/// replay alias or is gone entirely, so any one of them inside a
/// model-visible description or schema teaches a call the catalog never
/// offers. `retired_names_are_not_model_visible` pins that invariant.
///
/// `list_dir`, `file_search`, and `grep_files` are deliberately absent:
/// they are live, model-visible tools, not retired names
/// (#6747).
const RETIRED_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "edit_file",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "run_tests",
    "run_verifiers",
    "web_search",
    "fetch_url",
    "wait_for_dev_server",
    "exec_shell",
    "exec_shell_wait",
    "exec_shell_interact",
    "exec_shell_cancel",
];

fn full_native_registry(root: &std::path::Path) -> crate::tools::registry::ToolRegistry {
    use crate::tools::registry::ToolRegistryBuilder;
    use crate::tools::spec::ToolContext;

    ToolRegistryBuilder::new()
        .with_file_tools()
        .with_shell_tools()
        .with_search_tools()
        .with_git_tools()
        .with_git_history_tools()
        .with_test_runner_tool()
        .with_web_tools()
        .with_patch_tools()
        .with_handle_tools()
        .with_rlm_tool()
        .build(ToolContext::new(root.to_path_buf()))
}

/// Assert that model-facing `text` names only tools a model can call
/// (#6747): no hidden (`model_visible=false`) registry name and no
/// retired name, and any deferred tool it names comes with the
/// `tool_search` activation path. Shared by every site that writes
/// tool names into prompts, briefs, or tool output.
pub(crate) fn assert_text_names_only_callable_tools(label: &str, text: &str) {
    use crate::core::engine::tool_catalog::DEFAULT_ACTIVE_NATIVE_TOOLS;

    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = full_native_registry(tmp.path());
    let mut hidden: Vec<String> = RETIRED_TOOL_NAMES.iter().map(|n| n.to_string()).collect();
    let mut deferred = Vec::new();
    for tool in registry.all() {
        let name = tool.name().to_string();
        if !tool.model_visible() {
            hidden.push(name);
        } else if !DEFAULT_ACTIVE_NATIVE_TOOLS.contains(&name.as_str()) {
            deferred.push(name);
        }
    }
    // Snake-case names are unambiguous as bare words; short names such as
    // `File` or `Bash` only count as tool references inside backticks.
    let names = |name: &str, text: &str| {
        text.contains(&format!("`{name}`")) || (name.contains('_') && text.contains(name))
    };
    for name in &hidden {
        assert!(
            !names(name, text),
            "{label} names `{name}`, which the model-visible catalog never offers:\n{text}"
        );
    }
    for name in &deferred {
        if names(name, text) {
            assert!(
                text.contains("`tool_search`"),
                "{label} names deferred `{name}` without teaching `tool_search`:\n{text}"
            );
        }
    }
}

#[test]
fn retired_names_are_not_model_visible() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = full_native_registry(tmp.path());
    for tool in registry.all() {
        let name = tool.name().to_string();
        assert!(
            !(tool.model_visible() && RETIRED_TOOL_NAMES.contains(&name.as_str())),
            "`{name}` is model-visible, so it is live rather than retired"
        );
    }
}

#[test]
fn shared_prompt_text_names_only_callable_tools() {
    assert_text_names_only_callable_tools(
        "GOAL_CONTINUATION_PROMPT",
        crate::prompts::GOAL_CONTINUATION_PROMPT,
    );
    assert_text_names_only_callable_tools(
        "HANDLE_READ_ACTIVATION_HINT",
        crate::tools::handle::HANDLE_READ_ACTIVATION_HINT,
    );
}

/// The catalog is re-sent on every request, so a retired name in it is a
/// per-turn lie to every model. `verifier.rs` already guarded one such
/// description by hand; this covers the whole advertised surface at once.
#[test]
fn no_advertised_tool_teaches_a_retired_name() {
    use crate::tools::registry::ToolRegistryBuilder;
    use crate::tools::spec::ToolContext;

    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = ToolRegistryBuilder::new()
        .with_file_tools()
        .with_search_tools()
        .with_git_tools()
        .with_git_history_tools()
        .with_test_runner_tool()
        .with_web_tools()
        .with_patch_tools()
        .build(ToolContext::new(tmp.path().to_path_buf()));

    for tool in registry.to_api_tools() {
        let advertised = format!("{} {}", tool.description, tool.input_schema);
        for retired in RETIRED_TOOL_NAMES {
            assert!(
                !advertised.contains(retired),
                "tool `{}` advertises the retired name `{retired}`; \
                 name the canonical action form instead",
                tool.name
            );
        }
    }
}

#[test]
fn every_canonical_action_resolves_to_its_legacy_semantic_alias() {
    for (family, action, alias) in CANONICAL_ACTION_ALIASES {
        assert_eq!(
            canonical_action_alias(family, &json!({"action": action})),
            *alias,
            "{family}.{action}"
        );
    }
}

#[test]
fn lowercase_primitives_preserve_legacy_policy_names() {
    assert_eq!(canonical_action_alias("read", &json!({})), "read_file");
    assert_eq!(canonical_action_alias("write", &json!({})), "write_file");
    assert_eq!(canonical_action_alias("edit", &json!({})), "edit_file");
    assert_eq!(
        canonical_action_alias("bash", &json!({"command": "pwd"})),
        "exec_shell"
    );
    assert_eq!(
        canonical_action_alias("bash", &json!({"action": "cancel"})),
        "exec_shell_cancel"
    );
}

/// Execution refuses an actionless call; policy still needs a label for
/// it, and that label must stay the family's most conservative action.
#[test]
fn actionless_calls_keep_a_conservative_policy_label() {
    for (family, alias) in [
        ("Bash", "exec_shell"),
        ("File", "read_file"),
        ("Git", "git_status"),
        ("Run", "run_tests"),
        ("Web", "web_search"),
    ] {
        assert_eq!(
            canonical_action_alias(family, &json!({})),
            alias,
            "{family}"
        );
    }
}

#[test]
fn legacy_unknown_and_invalid_calls_keep_their_original_names() {
    for name in ["exec_shell", "read_file", "future_tool"] {
        assert_eq!(canonical_action_alias(name, &json!({})), name);
    }
    assert_eq!(
        canonical_action_alias("File", &json!({"action": "delete"})),
        "File"
    );
    assert_eq!(
        canonical_action_alias("Bash", &json!({"action": 42})),
        "exec_shell"
    );
}

/// A family with no execution default still resolves an explicit action.
/// Without this, `rlm{action:"eval"}` resolves to `rlm` and slips past every
/// deny list entry that names `rlm_eval`.
#[test]
fn a_family_without_a_default_still_resolves_an_explicit_action() {
    assert_eq!(
        canonical_action_alias("rlm", &json!({"action": "eval"})),
        "rlm_eval"
    );
    assert_eq!(
        canonical_action_alias("rlm", &json!({"action": "open", "url": "https://x.test/a"})),
        "rlm_open"
    );
    // No action, no default: nothing to resolve, and `RlmTool` will reject
    // the call on its own terms.
    assert_eq!(canonical_action_alias("rlm", &json!({})), "rlm");
    assert_eq!(
        canonical_action_alias("rlm", &json!({"action": "nope"})),
        "rlm"
    );
}

/// Every family named in the alias table must be recognised as a family, or
/// its actions are unprunable by the visibility filter.
#[test]
fn every_aliased_family_is_a_known_action_family() {
    for (family, _, _) in CANONICAL_ACTION_ALIASES {
        assert!(
            is_action_family(family),
            "{family} is aliased but not registered as an action family"
        );
    }
    assert!(!is_action_family("read_file"));
    assert!(!is_action_family("future_tool"));
}

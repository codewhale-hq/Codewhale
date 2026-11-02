/// Names the v0.9.3 consolidation retired from the model-facing catalog.
/// Each stays registered only as a hidden (`model_visible=false`)
/// replay alias or is gone entirely, so any one of them inside a
/// model-visible description or schema teaches a call the catalog never
/// offers. `retired_names_are_not_model_visible` pins that invariant.
///
/// `list_dir`, `file_search`, and `grep_files` are deliberately absent:
/// they are live, model-visible tools, not retired names
/// (#6747).
pub(super) const RETIRED_TOOL_NAMES: &[&str] = &[
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

pub(super) fn full_native_registry(root: &std::path::Path) -> crate::tools::registry::ToolRegistry {
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
pub fn assert_text_names_only_callable_tools(label: &str, text: &str) {
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

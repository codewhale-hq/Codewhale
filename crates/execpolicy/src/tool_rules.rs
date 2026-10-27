pub fn tool_denied(disallowed_tools: Option<&[String]>, tool_name: &str) -> bool {
    disallowed_tools.is_some_and(|rules| {
        tool_matches_any_rule(rules, tool_name)
            || (requires_raw_shell(tool_name) && tool_matches_any_rule(rules, "Bash"))
    })
}

/// Execution dependencies narrow denials only. Treating these as symmetric
/// aliases would also grant task/terminal execution to an allowlist of Bash.
fn requires_raw_shell(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "bash"
            | "exec_shell"
            | "exec_shell_interact"
            | "exec_interact"
            | "task_shell_start"
            | "task_gate_run"
            // These owners start a fresh execution and currently cannot
            // transport this command's deny ceiling. Fail closed until they
            // can preserve it; inspection and cancellation stay available.
            | "task_create"
            | "automation_create"
            | "automation_update"
            | "automation_resume"
            | "automation_run"
            | "terminal/run"
            | "terminal/send"
            | "terminal/reset"
            | "code_execution"
            | "js_execution"
            | "rlm_eval"
            // Launching an MCP server spawns an arbitrary local process.
            | "start_mcp_server"
            | "start_registry_mcp_server"
    )
}

pub fn tool_matches_any_rule(rules: &[String], tool_name: &str) -> bool {
    let tool_name = tool_name.to_ascii_lowercase();
    rules.iter().any(|rule| {
        let rule = rule.to_ascii_lowercase();
        let (rule_body, is_prefix) = rule
            .strip_suffix('*')
            .map_or((rule.as_str(), false), |prefix| (prefix, true));
        std::iter::once(tool_name.as_str())
            .chain(policy_tool_aliases(&tool_name).iter().copied())
            .any(|candidate| {
                if is_prefix {
                    candidate.starts_with(rule_body)
                } else {
                    candidate == rule_body
                }
            })
    })
}

fn policy_tool_aliases(name: &str) -> &'static [&'static str] {
    match name {
        "read" | "read_file" => &["read", "read_file", "file"],
        "write" | "write_file" => &["write", "write_file", "file"],
        "edit" | "edit_file" => &["edit", "edit_file", "file"],
        "file" => &[
            "file",
            "read",
            "read_file",
            "write",
            "write_file",
            "edit",
            "edit_file",
        ],
        "bash" | "exec_shell" => &["bash", "exec_shell"],
        "mcp_read_resource" | "read_mcp_resource" => &["mcp_read_resource", "read_mcp_resource"],
        _ => &[],
    }
}

//! Tool-call risk policy.
//!
//! Classifies a call so the engine's gates and the approval views agree on
//! how much the call can touch. Intentionally UI-free.

use codewhale_execpolicy::canonical_action::canonical_action_alias;
use codewhale_execpolicy::command_safety::is_parallel_readonly_command;
use serde_json::Value;

use super::ToolCategory;

/// Stakes-based variant for the takeover modal.
///
/// `RiskLevel::Benign` lets a single keystroke commit the approval.
/// `RiskLevel::Destructive` keeps stronger warning copy and styling
/// around approvals that can touch files, shell, or remote state.
///
/// Routing rules live in [`classify_risk`] - when in doubt, route to
/// `Destructive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Benign,
    Destructive,
}

/// Decide the stakes variant for an approval request.
///
/// The bias is conservative: a category we don't recognise routes to
/// `Destructive`, and any shell command that `command_safety` flags as
/// `Dangerous` is forced to `Destructive` even when the rest of the
/// request looks calm. The split lets the modal render stronger warning
/// copy on anything that can touch state outside this turn.
#[must_use]
pub fn classify_risk(tool_name: &str, category: ToolCategory, params: &Value) -> RiskLevel {
    let tool_name = canonical_action_alias(tool_name, params);
    match category {
        // Read paths and discovery.
        ToolCategory::Safe | ToolCategory::McpRead => RiskLevel::Benign,
        // Query-only network is benign; opening a URL pulls arbitrary
        // remote content, so it stays destructive.
        ToolCategory::Network => match tool_name {
            "web_search" | "wait_for_dev_server" | "registry_sync" => RiskLevel::Benign,
            // web_run is benign for search/query, but its `open`/`click`
            // actions fetch model-supplied URLs (arbitrary remote content) -
            // destructive, consistent with fetch_url.
            "web_run" => {
                let fetches_url = params
                    .get("open")
                    .and_then(Value::as_array)
                    .is_some_and(|a| !a.is_empty())
                    || params
                        .get("click")
                        .and_then(Value::as_array)
                        .is_some_and(|a| !a.is_empty());
                if fetches_url {
                    RiskLevel::Destructive
                } else {
                    RiskLevel::Benign
                }
            }
            _ => RiskLevel::Destructive,
        },
        // Shell stays destructive unless the existing command-safety analyzer
        // can prove the concrete command is read-only.
        ToolCategory::Shell => {
            if let Some(cmd) = params.get("command").and_then(Value::as_str)
                && is_parallel_readonly_command(cmd)
            {
                return RiskLevel::Benign;
            }
            RiskLevel::Destructive
        }
        // Sub-agent lifecycle: status/peek are inspection-only. Starts and
        // other actions keep the explicit-options keymap (the child's own
        // gates govern what it may do once running).
        ToolCategory::Agent => match params.get("action").and_then(Value::as_str) {
            Some("status" | "peek" | "list") => RiskLevel::Benign,
            _ => RiskLevel::Destructive,
        },
        // File writes, MCP actions, unclassified surfaces - all require
        // explicit confirmation.
        ToolCategory::FileWrite | ToolCategory::McpAction | ToolCategory::Unknown => {
            RiskLevel::Destructive
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::authority::{get_tool_category, get_tool_category_for_call};
    use serde_json::json;

    #[test]
    fn classifies_read_only_surfaces_as_benign() {
        for name in ["read_file", "list_dir", "list_mcp_tools", "web_search"] {
            let category = get_tool_category(name);
            assert_eq!(
                classify_risk(name, category, &json!({})),
                RiskLevel::Benign,
                "{name}"
            );
        }
    }

    #[test]
    fn classifies_stateful_or_unknown_surfaces_as_destructive() {
        for name in [
            "write_file",
            "edit_file",
            "apply_patch",
            "mcp_linear_save_issue",
            "fetch_url",
            "unknown_tool",
        ] {
            let category = get_tool_category(name);
            assert_eq!(
                classify_risk(name, category, &json!({})),
                RiskLevel::Destructive,
                "{name}"
            );
        }
    }

    #[test]
    fn shell_risk_uses_command_safety_analysis() {
        let category = get_tool_category("exec_shell");
        assert_eq!(
            classify_risk(
                "exec_shell",
                category,
                &json!({"command": "git status --short"})
            ),
            RiskLevel::Benign
        );
        assert_eq!(
            classify_risk(
                "exec_shell",
                category,
                &json!({"command": "rm -rf /tmp/example"})
            ),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn shell_exec_flags_are_not_benign() {
        let category = get_tool_category("exec_shell");
        for command in [
            "fd -x ./pwn.sh",
            "fd -uHtx ./pwn.sh",
            "rg --pre /tmp/evil.sh needle .",
            "git grep -O needle",
            "git grep -nO needle",
        ] {
            assert_eq!(
                classify_risk("exec_shell", category, &json!({"command": command})),
                RiskLevel::Destructive,
                "{command} should not be classified as benign"
            );
        }

        for command in [
            "fd -e rs .",
            "fd -H --type f src",
            "rg needle crates/",
            "git grep needle crates/",
            "git grep -n needle crates/",
        ] {
            assert_eq!(
                classify_risk("exec_shell", category, &json!({"command": command})),
                RiskLevel::Benign,
                "{command} should remain benign"
            );
        }
    }

    #[test]
    fn web_run_open_and_click_fetch_remote_content() {
        let category = get_tool_category("web_run");
        assert_eq!(
            classify_risk(
                "web_run",
                category,
                &json!({"search_query": [{"q": "rust"}]})
            ),
            RiskLevel::Benign
        );
        assert_eq!(
            classify_risk("web_run", category, &json!({"open": [{"ref_id": "x"}]})),
            RiskLevel::Destructive
        );
        assert_eq!(
            classify_risk(
                "web_run",
                category,
                &json!({"click": [{"ref_id": "x", "id": 1}]})
            ),
            RiskLevel::Destructive
        );
    }

    #[test]
    fn canonical_actions_keep_legacy_approval_categories_and_risk() {
        let cases = [
            ("Bash", "run", ToolCategory::Shell, RiskLevel::Destructive),
            ("Bash", "wait", ToolCategory::Shell, RiskLevel::Destructive),
            (
                "Bash",
                "interact",
                ToolCategory::Shell,
                RiskLevel::Destructive,
            ),
            (
                "Bash",
                "cancel",
                ToolCategory::Shell,
                RiskLevel::Destructive,
            ),
            ("File", "read", ToolCategory::Safe, RiskLevel::Benign),
            ("File", "list", ToolCategory::Safe, RiskLevel::Benign),
            ("File", "search_name", ToolCategory::Safe, RiskLevel::Benign),
            (
                "File",
                "search_content",
                ToolCategory::Safe,
                RiskLevel::Benign,
            ),
            (
                "File",
                "write",
                ToolCategory::FileWrite,
                RiskLevel::Destructive,
            ),
            (
                "File",
                "edit",
                ToolCategory::FileWrite,
                RiskLevel::Destructive,
            ),
            (
                "File",
                "patch",
                ToolCategory::FileWrite,
                RiskLevel::Destructive,
            ),
            ("Git", "status", ToolCategory::Safe, RiskLevel::Benign),
            ("Git", "diff", ToolCategory::Safe, RiskLevel::Benign),
            ("Git", "log", ToolCategory::Safe, RiskLevel::Benign),
            ("Git", "show", ToolCategory::Safe, RiskLevel::Benign),
            ("Git", "blame", ToolCategory::Safe, RiskLevel::Benign),
            ("Git", "commit_plan", ToolCategory::Safe, RiskLevel::Benign),
            (
                "Run",
                "tests",
                ToolCategory::Unknown,
                RiskLevel::Destructive,
            ),
            (
                "Run",
                "verifiers",
                ToolCategory::Unknown,
                RiskLevel::Destructive,
            ),
            ("Web", "search", ToolCategory::Network, RiskLevel::Benign),
            (
                "Web",
                "fetch",
                ToolCategory::Network,
                RiskLevel::Destructive,
            ),
            ("Web", "wait", ToolCategory::Network, RiskLevel::Benign),
        ];

        for (family, action, expected_category, expected_risk) in cases {
            let params = json!({"action": action});
            let category = get_tool_category_for_call(family, &params);
            assert_eq!(category, expected_category, "{family}.{action}");
            assert_eq!(
                classify_risk(family, category, &params),
                expected_risk,
                "{family}.{action}"
            );
        }
    }
}

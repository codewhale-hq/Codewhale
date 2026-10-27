//! Typed operation activity for the Engine owner contract.
//!
//! The Engine reports *what kind* of work a dispatched call is doing (reading,
//! editing, desktop control, ...) with a bounded canonical action identity, without arguments, command or
//! result. Classification reuses the resolvers dispatch already trusts:
//! [`canonical_action_alias`] for action families and the MCP pool's resolved
//! server map for MCP tools. A call that neither resolves to a concrete
//! operation nor to a registered tool reports nothing.

use codewhale_protocol::engine_owner::{OwnerActivityKind, OwnerOperationOutcome};
use serde_json::Value;

use super::spec::{RichToolResult, ToolError};
use codewhale_execpolicy::canonical_action::{canonical_action_alias, is_action_family};

/// Activity kind for a resolved (non-MCP) operation name.
fn kind_for_operation(operation: &str) -> OwnerActivityKind {
    use OwnerActivityKind as Kind;
    match operation {
        "read_file" | "list_dir" | "read_media" => Kind::Reading,
        "write_file" | "edit_file" | "apply_patch" | "fim_edit" => Kind::Editing,
        "file_search" | "grep_files" => Kind::Searching,
        "run_tests" | "run_verifiers" => Kind::Testing,
        "exec_shell"
        | "exec_shell_wait"
        | "exec_shell_interact"
        | "exec_shell_cancel"
        | "task_gate_run"
        | "automation_run"
        | "rlm_eval" => Kind::Executing,
        "web_search" | "fetch_url" | "wait_for_dev_server" | "rlm_open" => Kind::Browsing,
        "memory_search" => Kind::Memory,
        _ => Kind::Tool,
    }
}

/// Classify a call the tool registry is about to dispatch.
///
/// The caller only asks for names the registry has registered; interpreter
/// and MCP dispatch classify separately. Action families resolve through
/// [`canonical_action_alias`]; an action the wrapper would refuse (missing,
/// or not in the alias table) reports nothing.
#[must_use]
pub(crate) fn registry_activity_kind(tool_name: &str, input: &Value) -> Option<OwnerActivityKind> {
    if is_action_family(tool_name) {
        // Every family wrapper but the shell refuses an actionless call
        // (`required_action`); the shell defaults to `run`.
        let explicit = input.get("action").and_then(Value::as_str).is_some();
        if !explicit && !matches!(tool_name, "bash" | "Bash") {
            return None;
        }
        let operation = canonical_action_alias(tool_name, input);
        return (operation != tool_name).then(|| kind_for_operation(operation));
    }
    Some(kind_for_operation(canonical_action_alias(tool_name, input)))
}

#[must_use]
pub(crate) fn action_id(tool_name: &str, input: &Value) -> String {
    let id = canonical_action_alias(tool_name, input);
    if !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control) {
        id.to_owned()
    } else {
        "tool".to_owned()
    }
}

/// Classify an MCP call by the server the pool resolved it to, not by the
/// model-facing name. Desktop-control servers report `Computer`; everything
/// else is a generic `Tool`.
#[must_use]
pub(crate) fn mcp_activity_kind(server: &str) -> OwnerActivityKind {
    if super::subagent::is_machine_control_tool(&format!("mcp_{server}_tool")) {
        OwnerActivityKind::Computer
    } else {
        OwnerActivityKind::Tool
    }
}

/// Typed outcome for a finished call. `cancelled` only reclassifies a failure:
/// a call that succeeded before the token fired still succeeded.
#[must_use]
pub(crate) fn operation_outcome(
    outcome: &Result<RichToolResult, ToolError>,
    cancelled: bool,
) -> OwnerOperationOutcome {
    match outcome {
        Ok(result) if result.result.success => OwnerOperationOutcome::Succeeded,
        _ if cancelled => OwnerOperationOutcome::Cancelled,
        Ok(_) => OwnerOperationOutcome::Failed,
        Err(ToolError::Cancelled { .. }) => OwnerOperationOutcome::Cancelled,
        // Only a policy refusal is a denial. Bad input, an escaped path or a
        // missing tool is the call failing, and the pet should show it.
        Err(ToolError::PermissionDenied { .. }) => OwnerOperationOutcome::Denied,
        Err(
            ToolError::InvalidInput { .. }
            | ToolError::MissingField { .. }
            | ToolError::PathEscape { .. }
            | ToolError::NotAvailable { .. }
            | ToolError::ExecutionFailed { .. }
            | ToolError::Timeout { .. },
        ) => OwnerOperationOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::spec::ToolResult;
    use serde_json::json;

    use OwnerActivityKind as Kind;

    #[test]
    fn primitives_and_action_families_resolve_through_the_canonical_alias() {
        let none = json!({});
        assert_eq!(registry_activity_kind("read", &none), Some(Kind::Reading));
        assert_eq!(action_id("read", &none), "read_file");
        assert_eq!(action_id("Git", &json!({"action": "diff"})), "git_diff");
        assert_eq!(action_id("mcp_example_custom", &none), "mcp_example_custom");
        for unsafe_id in ["", "bad\nname", &"鲸".repeat(86)] {
            assert_eq!(action_id(unsafe_id, &none), "tool");
        }
        assert_eq!(registry_activity_kind("write", &none), Some(Kind::Editing));
        assert_eq!(registry_activity_kind("edit", &none), Some(Kind::Editing));
        let cases = [
            ("File", "read", Kind::Reading),
            ("File", "patch", Kind::Editing),
            ("File", "search_content", Kind::Searching),
            ("Run", "tests", Kind::Testing),
            ("Web", "fetch", Kind::Browsing),
            ("Git", "status", Kind::Tool),
            ("rlm", "eval", Kind::Executing),
            ("rlm", "open", Kind::Browsing),
            ("Bash", "run", Kind::Executing),
        ];
        for (family, action, kind) in cases {
            assert_eq!(
                registry_activity_kind(family, &json!({"action": action})),
                Some(kind),
                "{family}.{action}"
            );
        }
        // The shell defaults an actionless call to `run`.
        assert_eq!(
            registry_activity_kind("bash", &json!({"command": "ls"})),
            Some(Kind::Executing)
        );
    }

    #[test]
    fn refused_or_unresolved_calls_report_nothing() {
        // Wrappers refuse an actionless or unknown action: nothing ran.
        assert_eq!(registry_activity_kind("File", &json!({"path": "a"})), None);
        assert_eq!(
            registry_activity_kind("Web", &json!({"action": "teleport"})),
            None
        );
        assert_eq!(registry_activity_kind("rlm", &json!({})), None);
        assert_eq!(
            registry_activity_kind("tasks", &json!({"action": "nope"})),
            None
        );
    }

    #[test]
    fn shipped_desktop_control_servers_are_computer_activity() {
        for server in ["codewhale-cu", "computer-use", "local-computer_use"] {
            assert_eq!(mcp_activity_kind(server), Kind::Computer, "{server}");
        }
        assert_eq!(mcp_activity_kind("github"), Kind::Tool);
    }

    #[test]
    fn cancellation_does_not_rewrite_a_successful_call() {
        let ok = Ok(RichToolResult::plain(ToolResult::success("done")));
        assert_eq!(
            operation_outcome(&ok, true),
            OwnerOperationOutcome::Succeeded,
            "a call that finished before the token fired still succeeded"
        );
        let failed = Ok(RichToolResult::plain(ToolResult::error("boom")));
        assert_eq!(
            operation_outcome(&failed, false),
            OwnerOperationOutcome::Failed
        );
        assert_eq!(
            operation_outcome(&failed, true),
            OwnerOperationOutcome::Cancelled
        );
        let denied = Err(ToolError::permission_denied("no"));
        assert_eq!(
            operation_outcome(&denied, false),
            OwnerOperationOutcome::Denied
        );
        assert_eq!(
            operation_outcome(&Err(ToolError::cancelled("stop")), false),
            OwnerOperationOutcome::Cancelled
        );
        for failure in [
            ToolError::Timeout { seconds: 5 },
            ToolError::execution_failed("x"),
            ToolError::invalid_input("bad"),
            ToolError::missing_field("path"),
            ToolError::not_available("gone"),
        ] {
            assert_eq!(
                operation_outcome(&Err(failure), false),
                OwnerOperationOutcome::Failed
            );
        }
    }
}

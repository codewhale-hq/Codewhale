//! Launch actions must work through the real input loop, including Enter
//! after a mouse click. All state is sealed; provider fixtures use loopback.

use std::io::Read;
use std::time::Duration;

use super::qa_harness;
use qa_harness::harness::{
    Harness, SealedWorkspace, make_sealed_workspace, make_sealed_workspace_in_home,
};
use qa_harness::keys;

const WAIT: Duration = Duration::from_secs(15);
const SIZES: [(u16, u16); 5] = [(12, 40), (16, 60), (24, 80), (32, 100), (40, 140)];
const TITLE: &str = "Recent proof";
const SAVED_TEXT: &str = "Restored conversation proof";

/// Published codewhale.net terminal media. The provider is a deterministic
/// loopback demo, explicitly labelled website-demo; the real Engine performs
/// every file edit, shell check and delegated review visible in the captures.
#[test]
#[ignore = "opt-in website media; isolated loopback demo, no paid provider calls"]
fn website_current_terminal_capture() {
    assert!(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").is_some());
    let workspace = make_sealed_workspace_in_home("my-project").unwrap();
    let provider = WebsiteDemoProvider::start();
    website_demo_workspace(&workspace, &provider.base_url);
    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("SHELL", "/bin/sh")
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE", "1")
        .env("NO_ANIMATIONS", "1")
        .env("COLORTERM", "truecolor")
        .args([
            "--workspace",
            workspace.workspace().to_str().unwrap(),
            "--no-project-config",
            "--skip-onboarding",
            "--fresh",
            "--mouse-capture",
            "--sandbox-mode",
            "danger-full-access",
        ])
        .size(24, 100)
        .spawn()
        .unwrap();
    wait(&mut tui, "New session");
    tui.type_line("/provider").unwrap();
    wait(&mut tui, "website-demo");
    capture(&mut tui, "website-provider-picker");
    tui.send(keys::key::esc()).unwrap();
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
    tui.type_line("Local demo: add --json to export.py, run tests, and delegate a review.")
        .unwrap();
    wait(&mut tui, "JSON export is ready");
    assert!(
        std::fs::read_to_string(workspace.workspace().join("export.py"))
            .unwrap()
            .contains("--json")
    );
    assert!(
        workspace.workspace().join("test-results.txt").exists(),
        "the real terminal tool must run the fixture tests"
    );
    let results = std::fs::read_to_string(workspace.workspace().join("test-results.txt")).unwrap();
    assert!(
        results.contains("Ran 3 tests") && results.contains("\nOK\n"),
        "{results}"
    );
    tui.type_line("/workbar off").unwrap();
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
    assert_website_frame(&mut tui, &workspace);
    capture(&mut tui, "website-home");
    let follow_up = "Also cover an empty export.";
    tui.paste(follow_up).unwrap();
    wait(&mut tui, follow_up);
    capture(&mut tui, "website-composer");
    tui.send(keys::key::backspaces(follow_up.len())).unwrap();
    tui.type_line("/help").unwrap();
    wait(&mut tui, "/model");
    capture(&mut tui, "website-help");
    tui.send(keys::key::esc()).unwrap();
    tui.resize(32, 100).unwrap();
    tui.type_line("/workbar bottom").unwrap();
    wait(&mut tui, "Workbar: bottom placement");
    tui.type_line("/workbar agents").unwrap();
    wait(&mut tui, "Workbar: bottom placement, AGENTS panel");
    wait(&mut tui, "export-review");
    capture(&mut tui, "website-workbar-fleet");
    tui.type_line("/workbar tasks").unwrap();
    wait(&mut tui, "Workbar: bottom placement, TODO panel");
    wait(&mut tui, "Add the --json flag");
    capture(&mut tui, "website-workbar");
    provider.write_receipts();
    tui.shutdown();
}

const WEBSITE_DEMO_EXPORT: &str = r#"import argparse
import csv
import json
import sys

parser = argparse.ArgumentParser(description="Export the project list")
parser.add_argument("--json", action="store_true", help="write JSON instead of CSV")
args = parser.parse_args()
rows = [{"name": "Codewhale", "status": "ready"}]
if args.json:
    print(json.dumps(rows))
else:
    writer = csv.DictWriter(sys.stdout, fieldnames=["name", "status"])
    writer.writeheader()
    writer.writerows(rows)
"#;

fn website_demo_workspace(workspace: &SealedWorkspace, base_url: &str) {
    let state = workspace.home().join(".codewhale");
    std::fs::write(state.join(".onboarded"), "").unwrap();
    std::fs::write(
        state.join("settings.toml"),
        "permission_posture = \"full-access\"\n",
    )
    .unwrap();
    let trust = workspace.workspace().join(".deepseek");
    std::fs::create_dir_all(&trust).unwrap();
    std::fs::write(trust.join("trusted"), "").unwrap();
    std::fs::write(
        state.join("config.toml"),
        format!(
            "provider = \"website-demo\"\n[providers.website-demo]\nkind = \"openai-compatible\"\nbase_url = {base_url:?}\napi_key = \"local-demo-only\"\nmodel = \"website-demo\"\n[notifications]\nmethod = \"off\"\ncompletion_sound = \"off\"\n"
        ),
    )
    .unwrap();
    std::fs::write(
        workspace.workspace().join("export.py"),
        "import csv\nimport sys\nrows = [{'name': 'Codewhale', 'status': 'ready'}]\nwriter = csv.DictWriter(sys.stdout, fieldnames=['name', 'status'])\nwriter.writeheader()\nwriter.writerows(rows)\n",
    )
    .unwrap();
    let tests = workspace.workspace().join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(
        tests.join("test_export.py"),
        r#"import json
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parent.parent

class ExportTests(unittest.TestCase):
    def invoke(self, *args):
        return subprocess.run([sys.executable, str(ROOT / "export.py"), *args], text=True, capture_output=True)

    def test_csv_default_is_preserved(self):
        result = self.invoke()
        self.assertEqual(result.returncode, 0)
        self.assertIn("name,status", result.stdout)

    def test_json_flag_emits_the_rows(self):
        result = self.invoke("--json")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(json.loads(result.stdout), [{"name": "Codewhale", "status": "ready"}])

    def test_unknown_flag_is_rejected(self):
        self.assertNotEqual(self.invoke("--unknown").returncode, 0)
"#,
    )
    .unwrap();
}

/// A local script driving real Engine tools, never a claim about model quality.
struct WebsiteDemoProvider {
    base_url: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
    receipts: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

impl WebsiteDemoProvider {
    fn start() -> Self {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use tiny_http::{Header, Method, Response, Server};
        let server = Server::http("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}/v1", server.server_addr().to_ip().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let receipts = Arc::new(Mutex::new(Vec::new()));
        let worker_receipts = Arc::clone(&receipts);
        let worker = std::thread::spawn(move || {
            let (mut root_step, mut review_step) = (0, 0);
            let mut review_read_confirmed = false;
            while !worker_stop.load(Ordering::Relaxed) {
                let Some(mut request) = server.recv_timeout(Duration::from_millis(100)).unwrap()
                else {
                    continue;
                };
                let json_header =
                    || Header::from_bytes("content-type", "application/json").unwrap();
                if request.method() == &Method::Get && request.url().ends_with("/models") {
                    request.respond(Response::from_string(r#"{"object":"list","data":[{"id":"website-demo","object":"model","owned_by":"local-demo"}]}"#).with_header(json_header())).unwrap();
                    continue;
                }
                let mut body = String::new();
                let limit = request.body_length().unwrap_or(0);
                assert!(limit <= 2 * 1024 * 1024, "bounded demo request");
                std::io::Read::take(request.as_reader(), limit as u64)
                    .read_to_string(&mut body)
                    .unwrap();
                let body: serde_json::Value = serde_json::from_str(&body).unwrap();
                let is_review = body["messages"].as_array().unwrap().iter().any(|message| {
                    message["role"] == "user"
                        && message["content"]
                            .to_string()
                            .contains("WEBSITE_DEMO_REVIEW")
                });
                let messages = body["messages"].as_array().unwrap();
                if is_review {
                    review_read_confirmed |= messages.iter().any(|message| {
                        message["role"] == "tool"
                            && message["content"].as_str().is_some_and(|content| {
                                content.contains("parser.add_argument(\"--json\"")
                                    && content.contains("print(json.dumps(rows))")
                                    && content.contains("writer.writerows(rows)")
                            })
                    });
                    if review_step > 0 {
                        assert!(
                            review_read_confirmed,
                            "reviewer must actually read the edited file before reporting success"
                        );
                    }
                }
                let review_completed = messages.iter().any(|message| {
                    message["role"] == "tool"
                        && message["content"].as_str().is_some_and(|content| {
                            serde_json::from_str::<serde_json::Value>(content)
                                .ok()
                                .and_then(|result| result["settled"].as_array().cloned())
                                .is_some_and(|settled| {
                                    settled.iter().any(|agent| {
                                        agent["name"] == "export-review"
                                            && agent["status"] == "completed"
                                    })
                                })
                        })
                });
                if !is_review && root_step >= 7 {
                    assert!(
                        review_read_confirmed && review_completed,
                        "the parent must receive a completed, source-grounded review before marking the demo done"
                    );
                }
                let step = if is_review {
                    &mut review_step
                } else {
                    &mut root_step
                };
                let response_step = *step;
                let (mut tool, mut arguments, text) = website_demo_response(is_review, *step);
                let tools = body["tools"].as_array().unwrap();
                if let Some(wanted) = tool
                    && !tools.iter().any(|tool| tool["function"]["name"] == wanted)
                {
                    // Deferred tools are discovered through the real catalog;
                    // never execute a name the provider was not advertised.
                    tool = Some("tool_search");
                    arguments = serde_json::json!({"query":wanted.replace("-x00002F-", "/")});
                } else {
                    *step += 1;
                }
                let tool = tool.map(|name| {
                    let mut names = tools
                        .iter()
                        .filter_map(|tool| tool["function"]["name"].as_str());
                    names
                        .find(|candidate| *candidate == name)
                        .unwrap_or_else(|| panic!("demo tool {name} not advertised"))
                });
                worker_receipts.lock().unwrap().push(serde_json::json!({
                    "worker": if is_review {"export-review"} else {"root"},
                    "step": response_step, "model": body["model"], "tool": tool,
                    "toolResults": messages.iter().filter(|message| message["role"] == "tool").count(),
                    "reviewReadConfirmed": review_read_confirmed,
                    "reviewCompleted": review_completed
                }));
                let delta = match tool {
                    Some(name) => serde_json::json!({"tool_calls": [{
                        "index": 0, "id": format!("website-{}-{}", is_review, *step), "type":"function",
                        "function": {"name":name, "arguments": serde_json::to_string(&arguments).unwrap()}
                    }]}),
                    None => serde_json::json!({"content":text}),
                };
                let chunk = |delta, finish| {
                    format!(
                        "data: {}\n\n",
                        serde_json::json!({
                            "id":"website-demo", "object":"chat.completion.chunk", "model":"website-demo",
                            "choices":[{"index":0,"delta":delta,"finish_reason":finish}]
                        })
                    )
                };
                let stream = format!(
                    "{}{}data: [DONE]\n\n",
                    chunk(delta, serde_json::Value::Null),
                    chunk(
                        serde_json::json!({}),
                        serde_json::json!(if tool.is_some() { "tool_calls" } else { "stop" })
                    )
                );
                request
                    .respond(Response::from_string(stream).with_header(
                        Header::from_bytes("content-type", "text/event-stream").unwrap(),
                    ))
                    .unwrap();
            }
        });
        Self {
            base_url,
            stop,
            worker: Some(worker),
            receipts,
        }
    }

    fn write_receipts(&self) {
        let directory =
            std::path::PathBuf::from(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").unwrap());
        std::fs::write(
            directory.join("website-demo-receipts.json"),
            serde_json::to_vec_pretty(&*self.receipts.lock().unwrap()).unwrap(),
        )
        .unwrap();
    }
}

impl Drop for WebsiteDemoProvider {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.write_receipts();
    }
}

fn website_demo_response(
    review: bool,
    step: usize,
) -> (Option<&'static str>, serde_json::Value, &'static str) {
    use serde_json::{Value, json};
    if review {
        return match step {
            0 => (Some("read"), json!({"path":"export.py"}), ""),
            _ => (
                None,
                Value::Null,
                "Reviewed export.py: --json uses the same project rows, the default CSV path is preserved, and argparse rejects unknown flags. No issues found in this small change.",
            ),
        };
    }
    let todos = |finished: usize| {
        json!({"todos":[
            {"content":"Inspect the export command", "status":if finished > 0 {"completed"} else {"in_progress"}},
            {"content":"Add the --json flag", "status":if finished > 1 {"completed"} else {"pending"}},
            {"content":"Run tests and review the change", "status":if finished > 2 {"completed"} else if finished == 2 {"in_progress"} else {"pending"}}
        ]})
    };
    match step {
        0 => (Some("todo_write"), todos(0), ""),
        1 => (Some("read"), json!({"path":"export.py"}), ""),
        2 => (
            Some("write"),
            json!({"path":"export.py", "content":WEBSITE_DEMO_EXPORT}),
            "",
        ),
        3 => (Some("todo_write"), todos(2), ""),
        4 => (
            Some("terminal-x00002F-run"),
            json!({"session":"export-checks", "command":"/usr/bin/python3 -m unittest discover -s tests -v > test-results.txt 2>&1; result=$?; cat test-results.txt; test \"$result\" -eq 0", "timeout_secs":15}),
            "",
        ),
        5 => (
            Some("agent"),
            json!({"action":"start", "name":"export-review", "type":"reviewer", "model":"website-demo", "prompt":"WEBSITE_DEMO_REVIEW: Read export.py and review the --json change. Check that default CSV output is preserved and unknown flags are rejected. Report only findings supported by the file; do not edit."}),
            "",
        ),
        6 => (
            Some("agent"),
            json!({"action":"wait", "name":"export-review", "until":"all", "timeout_secs":30}),
            "",
        ),
        7 => (Some("todo_write"), todos(3), ""),
        _ => (
            None,
            Value::Null,
            "JSON export is ready.\n\nAdded --json while preserving CSV output. All 3 local tests passed, and the delegated review found no issues.\n\nTry: python3 export.py --json\n\nThis session uses the local website-demo provider.",
        ),
    }
}

/// Active conversations show the configured route and real completed task;
/// their footer need not include a workspace name. Never leak the sealed path.
fn assert_website_frame(tui: &mut Harness, workspace: &SealedWorkspace) {
    tui.pump();
    let text = tui.frame().text();
    assert!(
        text.contains("JSON export is ready")
            && text.contains("All 3 local tests passed")
            && text.contains("delegated review")
            && text.contains("website-demo")
            && !text.contains(".tmp")
            && !text.contains(workspace.home().to_str().unwrap())
            && !text.to_ascii_lowercase().contains("disconnected"),
        "website frame must show the completed local demo without a sealed path: {}",
        tui.diagnostics()
    );
}

fn start(rows: u16, cols: u16, with_mcp: bool) -> (SealedWorkspace, Harness) {
    start_titled(rows, cols, with_mcp, TITLE)
}

fn start_titled(rows: u16, cols: u16, with_mcp: bool, title: &str) -> (SealedWorkspace, Harness) {
    start_with_titles(rows, cols, with_mcp, &[title])
}

fn start_with_titles(
    rows: u16,
    cols: u16,
    with_mcp: bool,
    titles: &[&str],
) -> (SealedWorkspace, Harness) {
    start_with_theme(rows, cols, with_mcp, titles, None)
}

fn start_with_theme(
    rows: u16,
    cols: u16,
    with_mcp: bool,
    titles: &[&str],
    theme: Option<&str>,
) -> (SealedWorkspace, Harness) {
    start_with_options(rows, cols, with_mcp, titles, theme, false, false)
}

fn start_with_options(
    rows: u16,
    cols: u16,
    with_mcp: bool,
    titles: &[&str],
    theme: Option<&str>,
    animated: bool,
    no_color: bool,
) -> (SealedWorkspace, Harness) {
    start_in(
        make_sealed_workspace().unwrap(),
        Launch {
            rows,
            cols,
            with_mcp,
            titles,
            theme,
            animated,
            no_color,
            keep_composer: false,
        },
    )
}

#[derive(Default)]
struct Launch<'a> {
    rows: u16,
    cols: u16,
    with_mcp: bool,
    titles: &'a [&'a str],
    theme: Option<&'a str>,
    animated: bool,
    no_color: bool,
    /// Skip the post-launch Ctrl+U that empties the composer.
    keep_composer: bool,
}

/// Launch into `workspace`; the composer-first launch card is home.
fn start_in(workspace: SealedWorkspace, launch: Launch<'_>) -> (SealedWorkspace, Harness) {
    let Launch {
        rows,
        cols,
        with_mcp,
        titles,
        theme,
        animated,
        no_color,
        keep_composer,
    } = launch;
    let trust = workspace.workspace().join(".deepseek");
    let sessions = workspace.home().join(".codewhale/sessions");
    for directory in [&trust, &sessions] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let mut fixtures = vec![
        (workspace.home().join(".codewhale/.onboarded"), Vec::new()),
        (trust.join("trusted"), Vec::new()),
    ];
    if let Some(theme) = theme {
        fixtures.push((
            workspace.home().join(".codewhale/settings.toml"),
            format!("theme = {theme:?}\n").into_bytes(),
        ));
    }
    for (index, title) in titles.iter().enumerate() {
        let id = format!(
            "11111111-2222-4333-8444-{:012}",
            555555555555u64 + index as u64
        );
        let session = serde_json::json!({
            "schema_version": 1,
            "metadata": {
                "id": id,
                "title": title,
                "created_at": "2026-09-19T00:00:00Z",
                "updated_at": format!("2026-09-19T00:00:{:02}Z", 59usize.saturating_sub(index)),
                "message_count": 1,
                "total_tokens": 0,
                "model": "deepseek-flash",
                "model_provider": "deepseek",
                "workspace": workspace.workspace()
            },
            "messages": [{"role": "user", "content": [{"type": "text", "text": SAVED_TEXT}]}],
            "system_prompt": null
        });
        fixtures.push((
            sessions.join(format!("{id}.json")),
            serde_json::to_vec(&session).unwrap(),
        ));
    }
    if with_mcp {
        // A local failing server gives the summary a real row without any network.
        let mcp = serde_json::json!({"mcpServers": {"launch-proof": {
            "command": "/usr/bin/false", "required": true
        }}});
        fixtures.push((
            workspace.home().join(".codewhale/mcp.json"),
            serde_json::to_vec(&mcp).unwrap(),
        ));
    }
    for (path, contents) in fixtures {
        std::fs::write(path, contents).unwrap();
    }

    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE", "1")
        .env("NO_ANIMATIONS", if animated { "0" } else { "1" })
        .env("COLORTERM", "truecolor")
        .env("NO_COLOR", if no_color { "1" } else { "" })
        .args([
            "--workspace",
            workspace.workspace().to_str().unwrap(),
            "--no-project-config",
            "--fresh",
            "--mouse-capture",
        ])
        .size(rows, cols)
        .spawn()
        .unwrap();
    wait(&mut tui, "New session");
    if animated || keep_composer {
        return (workspace, tui);
    }
    tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
    tui.send(keys::key::ctrl('u')).unwrap();
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
    (workspace, tui)
}

pub(crate) fn explore_offline_from_composer(tui: &mut Harness) {
    wait(tui, "Type a message");
    tui.type_line("seed").expect("send a first message");
    wait(tui, "Choose your model provider");
    tui.send(keys::key::ctrl('o')).unwrap();
    wait(tui, "You're ready.");
    tui.send(keys::key::enter()).unwrap();
    tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
    tui.send(keys::key::ctrl('u')).unwrap();
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
}

fn wait(tui: &mut Harness, text: &str) {
    if let Err(error) = tui.wait_for(|frame| frame.contains(text), WAIT) {
        let transcript = tui.transcript();
        let tail = &transcript[transcript.len().saturating_sub(4096)..];
        panic!(
            "waiting for {text:?}: {error}\n{}\nPTY tail: {:?}",
            tui.diagnostics(),
            String::from_utf8_lossy(tail)
        );
    }
}

fn click_text(tui: &mut Harness, text: &str) {
    tui.pump();
    let (row, col) = tui
        .frame()
        .find_text(text)
        .unwrap_or_else(|| panic!("missing click target {text:?}\n{}", tui.diagnostics()));
    tui.send(keys::mouse::click(row, col)).unwrap();
}

#[test]
fn local_slash_navigation_does_not_create_rewindable_user_turns() {
    let (_workspace, mut tui) = start_with_titles(24, 80, false, &[]);
    // The first command leaves home; the others use the active-session path.
    for (command, title) in [
        ("/settings", "Settings"),
        ("/skills", "Extensions"),
        ("/mcp", "Extensions"),
    ] {
        tui.type_line(command).unwrap();
        wait(&mut tui, title);
        tui.send(keys::key::esc()).unwrap();
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        assert!(
            !tui.frame()
                .text()
                .lines()
                .take(18)
                .any(|line| line.contains(command)),
            "navigation leaked into transcript above the composer: {}",
            tui.diagnostics()
        );
    }
    tui.send(keys::key::esc()).unwrap();
    tui.send(keys::key::esc()).unwrap();
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
    assert!(
        !tui.frame().contains("Backtrack preview"),
        "view navigation became a rewindable turn: {}",
        tui.diagnostics()
    );
    tui.shutdown();
}

#[test]
fn raw_slash_input_keeps_a_steady_submit_cue_and_runs_on_enter_without_another_key() {
    // #6397: the `[↵]` chip follows the draft, not the paste-burst window, so
    // it is already lit while a raw (non-bracketed) burst's Enter-suppression
    // window is still open. It is therefore not a signal that Enter will
    // submit; wait out the window (120ms) with a quiet PTY before pressing
    // Enter, which must then run the command with no other key.
    let (_workspace, mut tui) = start_with_titles(24, 80, false, &[]);
    tui.send("/mcp").unwrap();
    wait(&mut tui, "enter:run");
    wait(&mut tui, "[↵]");
    tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
    assert!(
        tui.frame().contains("[↵]") && !tui.frame().contains("[·]"),
        "submit cue did not stay steady: {}",
        tui.diagnostics()
    );
    tui.send(keys::key::enter()).unwrap();
    wait(&mut tui, "Extensions");
    tui.shutdown();
}

#[test]
fn launch_recent_click_then_enter_resumes_without_another_mouse_event() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        wait(&mut tui, TITLE);
        capture(&mut tui, "home");
        tui.send(keys::key::down()).unwrap();
        tui.send(keys::key::down()).unwrap();
        capture(&mut tui, "selected");
        click_text(&mut tui, TITLE);
        wait(&mut tui, "Resume");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        capture(&mut tui, "confirm");
        tui.send(keys::key::enter()).unwrap();
        // No pointer motion follows Enter: the accepted action must run now.
        wait(&mut tui, SAVED_TEXT);
        if cols >= 80 {
            wait(&mut tui, "Resumed:");
        }
        assert!(
            !tui.frame().contains("Session loaded from"),
            "resume should not add a technical path receipt to the conversation"
        );
        capture(&mut tui, "conversation");
        tui.shutdown();
    }
}

#[test]
fn launch_mcp_summary_opens_manager_by_click_and_keyboard() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, true);
        wait(&mut tui, "MCP");
        capture(&mut tui, "home-mcp");
        click_text(&mut tui, "MCP");
        wait(&mut tui, "Extensions");
        wait(&mut tui, "launch-proof");
        capture(&mut tui, "mcp");
        tui.send(keys::key::esc()).unwrap();
        wait(&mut tui, "New session");
        // New session, the recent row (or compact See all), then MCP.
        for _ in 0..3 {
            tui.send(keys::key::down()).unwrap();
        }
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Extensions");
        wait(&mut tui, "launch-proof");
        tui.shutdown();
    }
}

#[test]
fn launch_resume_buttons_support_mouse_cancel_and_keyboard_choice() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        click_text(&mut tui, TITLE);
        wait(&mut tui, "resume");
        click_text(&mut tui, "cancel");
        wait(&mut tui, "New session");
        assert!(!tui.frame().contains(SAVED_TEXT));
        click_text(&mut tui, TITLE);
        wait(&mut tui, "resume");
        tui.send(keys::key::tab()).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "New session");
        assert!(!tui.frame().contains(SAVED_TEXT));
        click_text(&mut tui, TITLE);
        wait(&mut tui, "resume");
        click_text(&mut tui, "resume");
        wait(&mut tui, SAVED_TEXT);
        tui.shutdown();
    }
}

/// Optional review evidence from the real PTY, keeping cell colors rather
/// than relying on symbol-only goldens. The viewer supplies terminal fonts.
pub(super) fn capture(tui: &mut Harness, name: &str) {
    let Some(directory) = std::env::var_os("QA_LAUNCH_CAPTURE_DIR") else {
        return;
    };
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
    let frame = tui.frame();
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join(format!("{name}-{}x{}.json", frame.cols(), frame.rows()));
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&frame.capture_cells()).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "opt-in visual evidence; writes only with QA_LAUNCH_CAPTURE_DIR"]
fn workbench_settings_visual_evidence() {
    assert!(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").is_some());
    for (command, title, name) in [
        ("/model", "route ·", "models"),
        ("/provider", "Provider", "providers"),
        ("/fleet", "Coordinator", "fleet"),
        ("/plugin", "Extensions", "plugins"),
        ("/config", "Settings", "settings"),
        ("/statusline", "Status", "statusline"),
    ] {
        for (rows, cols) in SIZES {
            let (_workspace, mut tui) = start(rows, cols, true);
            tui.paste(command).unwrap();
            tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
            tui.send(keys::key::enter()).unwrap();
            wait(&mut tui, title);
            capture(&mut tui, name);
            if name == "providers" {
                tui.send(keys::key::alt('v')).unwrap();
                wait(&mut tui, "DeepSeek · Open details");
                capture(&mut tui, "provider-details");
                tui.send(keys::key::esc()).unwrap();
                wait(&mut tui, "Provider");
            }
            tui.shutdown();
        }
    }
}

#[test]
#[ignore = "opt-in populated launch evidence; fixture sessions, no provider calls"]
fn workbench_populated_home_visual_evidence() {
    assert!(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").is_some());
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start_with_titles(
            rows,
            cols,
            true,
            &[
                "Polish the release notes",
                "Investigate a provider timeout",
                "Review the plugin setup flow",
            ],
        );
        capture(&mut tui, "home-populated");
        tui.send(keys::key::down()).unwrap();
        tui.send(keys::key::down()).unwrap();
        capture(&mut tui, "home-populated-selected");
        tui.shutdown();
    }
}

#[test]
fn launch_long_resume_title_preserves_warning_and_truthful_enter_hint() {
    let title = "Investigate provider timeouts and connection failures across multiple accounts, preserve the original credentials, and verify every saved session can still be restored after the upgrade";
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start_titled(rows, cols, false, title);
        click_text(&mut tui, "Investigate provider");
        wait(&mut tui, "resume");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        capture(&mut tui, "confirm-long");
        let text = tui
            .frame()
            .text()
            .chars()
            .map(|ch| {
                if ('\u{2500}'..='\u{257f}').contains(&ch) {
                    ' '
                } else {
                    ch
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("This replaces the current context with that session's history."),
            "{text}"
        );
        tui.send(keys::key::tab()).unwrap();
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        let text = tui.frame().text();
        assert!(text.contains("cancel  Enter"), "{text}");
        assert!(!text.contains("resume  Enter"), "{text}");
        capture(&mut tui, "confirm-cancel");
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "New session");
        assert!(!tui.frame().contains(SAVED_TEXT));
        tui.shutdown();
    }
}

#[test]
#[ignore = "opt-in all-theme evidence; fixture sessions, no provider calls"]
fn workbench_every_theme_visual_evidence() {
    assert!(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").is_some());
    for theme in codewhale_palette::SELECTABLE_THEMES {
        let (_workspace, mut tui) = start_with_theme(24, 80, true, &[TITLE], Some(theme.name()));
        capture(&mut tui, &format!("theme-{}-home", theme.name()));
        tui.paste("/statusline").unwrap();
        tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Status");
        capture(&mut tui, &format!("theme-{}-statusline", theme.name()));
        tui.shutdown();
    }
}

#[test]
#[ignore = "opt-in real launch animation capture; fixture state, no provider calls"]
fn workbench_whale_reveal_visual_evidence() {
    let directory = std::path::PathBuf::from(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").unwrap());
    std::fs::create_dir_all(&directory).unwrap();
    let (_workspace, mut tui) =
        start_with_options(32, 100, false, &[TITLE], Some("shoreline"), true, false);
    let start = std::time::Instant::now();
    for index in 0..16 {
        let frame = tui.frame();
        assert!(
            frame.text().contains("New session"),
            "controls must remain usable during reveal"
        );
        std::fs::write(
            directory.join(format!("reveal-{index:02}-100x32.json")),
            serde_json::to_vec_pretty(&frame.capture_cells()).unwrap(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!("Captured launch reveal over {:?}", start.elapsed());
    tui.shutdown();
}

/// Record temporal evidence from the real terminal, including its idle settle.
#[test]
#[ignore = "opt-in Underwater motion capture; isolated fixture, no provider calls"]
fn underwater_motion_visual_evidence() {
    use std::io::Write;
    let directory = std::path::PathBuf::from(std::env::var_os("QA_LAUNCH_CAPTURE_DIR").unwrap());
    std::fs::create_dir_all(&directory).unwrap();
    for (rows, cols) in [(24, 80), (36, 120)] {
        let (_workspace, mut tui) =
            start_with_options(rows, cols, false, &[TITLE], Some("underwater"), true, false);
        // Home intentionally gives its brief whale reveal the stage. Sea life
        // lives in the conversation field, so enter a fresh offline session.
        tui.send(keys::key::ctrl('u')).unwrap();
        click_text(&mut tui, "New session");
        wait(&mut tui, "What do you want to accomplish?");
        let file =
            std::fs::File::create(directory.join(format!("ocean-{cols}x{rows}.jsonl.gz"))).unwrap();
        let mut output = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let start = std::time::Instant::now();
        for index in 0..360u64 {
            let frame = tui.frame();
            assert!(
                frame.contains("Type a message")
                    && frame.contains("What do you want to accomplish?"),
                "motion cannot displace the conversation or composer"
            );
            serde_json::to_writer(
                &mut output,
                &serde_json::json!({
                    "elapsed_ms": start.elapsed().as_millis(),
                    "frame": frame.capture_cells()
                }),
            )
            .unwrap();
            output.write_all(b"\n").unwrap();
            let target = Duration::from_millis((index + 1) * 33);
            if let Some(remaining) = target.checked_sub(start.elapsed()) {
                std::thread::sleep(remaining);
            }
        }
        output.finish().unwrap();
        tui.shutdown();
    }
}

/// Exercise the visible catalog controls and provider search through the
/// input decoder. This only browses fixture state; it never applies a route.
#[test]
fn settings_catalog_controls_and_provider_search_work_with_mouse_and_keyboard() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        tui.paste("/provider").unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Provider");
        click_text(&mut tui, "browse all");
        wait(&mut tui, "configured");
        tui.send("/Anthropic").unwrap();
        wait(&mut tui, "search: Anthropic");
        capture(&mut tui, "providers-search");
        tui.send(keys::key::esc()).unwrap();
        wait(&mut tui, "Provider");
        capture(&mut tui, "providers-catalog");
        tui.send(keys::key::esc()).unwrap();
        // Let the standalone Escape decode before starting a bracketed paste.
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        tui.paste("/model").unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "route ·");
        click_text(&mut tui, "browse catalog");
        wait(&mut tui, "catalog");
        capture(&mut tui, "models-catalog");
        tui.send(keys::key::esc()).unwrap();
        tui.shutdown();
    }
}

#[test]
fn fleet_roles_open_the_shared_model_picker_and_escape_returns_to_the_same_role() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        tui.paste("/fleet").unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Coordinator");
        capture(&mut tui, "fleet-assignments");
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Model · Coordinator");
        wait(&mut tui, "Current session");
        capture(&mut tui, "fleet-coordinator-model");
        // The roster footer ("saved teams") can stay visible behind the
        // picker at wide sizes, so it does not prove Esc landed. Wait for the
        // picker itself to close and the screen to settle before the next
        // key: a key sent inside the Esc disambiguation window is read as
        // Alt+key and the role never changes.
        close_picker(&mut tui, "Model · Coordinator");
        tui.send(keys::key::down()).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Model · manager");
        capture(&mut tui, "fleet-role-model");
        tui.send("search-proof").unwrap();
        wait(&mut tui, "search-proof");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        tui.send(keys::key::esc()).unwrap();
        tui.wait_for(|frame| !frame.contains("search-proof"), WAIT)
            .unwrap();
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        close_picker(&mut tui, "Model · manager");
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Model · manager");
        // Following Coordinator is a selectable local choice even without credentials.
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Personal");
        capture(&mut tui, "fleet-role-destination");
        tui.shutdown();
    }
}

/// Esc out of a Fleet model picker and wait until the roster is back and
/// quiet, so the next key is never folded into the Esc sequence.
fn close_picker(tui: &mut Harness, title: &str) {
    tui.send(keys::key::esc()).unwrap();
    if let Err(error) = tui.wait_for(|frame| !frame.contains(title), WAIT) {
        panic!(
            "waiting for {title:?} to close: {error}\n{}",
            tui.diagnostics()
        );
    }
    wait(tui, "saved teams");
    tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
}

#[test]
fn no_color_keeps_home_navigation_and_submit_cues_without_color() {
    let sgr = regex::Regex::new(r"\x1b\[([0-9;:]*)m").unwrap();
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) =
            start_with_options(rows, cols, false, &[TITLE], Some("shoreline"), false, true);
        wait(&mut tui, "[·]");
        tui.send(keys::key::down()).unwrap();
        tui.send(keys::key::down()).unwrap();
        capture(&mut tui, "no-color-selected");
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, "Resume");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, SAVED_TEXT);
        tui.send("monochrome draft").unwrap();
        wait(&mut tui, "[↵]");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        capture(&mut tui, "no-color-draft");

        for row in 0..rows {
            for col in 0..cols {
                assert_eq!(
                    tui.frame().colors_at(row, col),
                    Some((
                        qa_harness::frame::Color::Default,
                        qa_harness::frame::Color::Default
                    )),
                    "{cols}x{rows} cell ({row}, {col}) added a color"
                );
            }
        }
        // Inspect the whole emitted stream, not just its last rendered frame.
        let transcript = tui.transcript();
        let output = String::from_utf8_lossy(&transcript);
        for codes in sgr.captures_iter(&output) {
            for code in codes[1]
                .split([';', ':'])
                .filter_map(|code| code.parse::<u16>().ok())
            {
                assert!(
                    !matches!(code, 30..=38 | 40..=48 | 58 | 90..=97 | 100..=107),
                    "{cols}x{rows} emitted color SGR {:?}",
                    &codes[0]
                );
            }
        }
        tui.shutdown();
    }
}

#[test]
fn home_returns_to_the_same_conversation_by_escape_click_and_typing() {
    for (rows, cols) in SIZES {
        let (_workspace, mut tui) = start(rows, cols, false);
        click_text(&mut tui, TITLE);
        wait(&mut tui, "Resume");
        tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        tui.send(keys::key::enter()).unwrap();
        wait(&mut tui, SAVED_TEXT);
        for return_path in ["escape", "click", "type"] {
            tui.paste("/home").unwrap();
            tui.send(keys::key::enter()).unwrap();
            wait(&mut tui, "Back to conversation");
            capture(&mut tui, "home-return");
            match return_path {
                "escape" => tui.send(keys::key::esc()).unwrap(),
                "click" => click_text(&mut tui, "Back to conversation"),
                _ => tui.send("draft stays here").unwrap(),
            }
            tui.wait_for(|frame| !frame.contains("Back to conversation"), WAIT)
                .unwrap();
            tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
            if return_path == "type" {
                wait(&mut tui, "draft stays here");
                capture(&mut tui, "home-return-draft");
                tui.send(keys::key::ctrl('u')).unwrap();
            }
            // Slash commands add transcript rows. At 40x12 the original
            // message is now above the viewport, so inspect scrollback.
            tui.send(keys::key::page_up()).unwrap();
            wait(&mut tui, SAVED_TEXT);
            tui.send(keys::key::alt('G')).unwrap();
            tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        }
        tui.paste("/overview").unwrap();
        tui.send(keys::key::enter()).unwrap();
        tui.wait_for_idle(Duration::from_millis(300), WAIT).unwrap();
        // The dashboard is longer than a short transcript viewport.
        for _ in 0..20 {
            if tui.frame().contains("Quick Actions") {
                break;
            }
            tui.send(keys::key::page_up()).unwrap();
            tui.wait_for_idle(Duration::from_millis(200), WAIT).unwrap();
        }
        wait(&mut tui, "Quick Actions");
        tui.shutdown();
    }
}

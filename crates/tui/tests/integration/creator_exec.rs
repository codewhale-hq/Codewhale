use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codewhale_plugins::host_services::{AgentPosture, PluginApprover};
use codewhale_runtime::config::BundledSkillsConfig;
use serde_json::{Value, json};
use tempfile::TempDir;
use wait_timeout::ChildExt;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TEST_MODEL: &str = "acceptance-model";

fn discover_node_binary() -> PathBuf {
    for var in ["NODE_BINARY", "CODEWHALE_NODE_BINARY"] {
        if let Some(value) = std::env::var_os(var) {
            let candidate = PathBuf::from(value);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    let names: &[&str] = if cfg!(windows) {
        &["node.exe", "node.cmd", "node"]
    } else {
        &["node"]
    };
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }
    for candidate in [
        Path::new("/opt/homebrew/bin/node"),
        Path::new("/usr/local/bin/node"),
        Path::new("/usr/bin/node"),
        Path::new(r"C:\Program Files\nodejs\node.exe"),
    ] {
        if candidate.is_file() {
            return candidate.to_path_buf();
        }
    }
    panic!("node binary required for creator_exec tests");
}

fn tool_package_json() -> String {
    json!({
        "name": "@local/my-plugin",
        "version": "1.0.0",
        "private": true,
        "type": "module",
        "exports": { ".": "./index.js" },
        "dsh": { "bundle": { "patch": "./cordis.patch.yml" } }
    })
    .to_string()
}

fn tool_cordis_patch() -> &'static str {
    "- insert:\n    - id: my-plugin\n      name: '@local/my-plugin'\n      config: {}\n"
}

fn tool_index_js(reply: &str) -> String {
    format!(
        "export const inject = ['tools', 'storage']\n\
export function apply(ctx) {{\n  \
ctx.tools.register({{\n    \
name: 'loop_echo',\n    \
description: 'Echo from creator exec fixture.',\n    \
parameters: {{\n      \
type: 'object',\n      \
properties: {{ note: {{ type: 'string' }} }},\n      \
additionalProperties: false,\n    }},\n    \
execute: async (input) => {{\n      \
if (input && typeof input.note === 'string') {{\n        \
await ctx.storage.set('saved_note', input.note)\n      \
}}\n      \
return {{ reply: {reply:?}, saved: (await ctx.storage.get('saved_note')) ?? null }}\n    \
}},\n  }})\n\
}}\n"
    )
}

fn sse_chunk(value: Value) -> String {
    format!(
        "data: {}\n\n",
        serde_json::to_string(&value).expect("SSE JSON")
    )
}

fn tool_call_sse(id: &str, tool_name: &str, arguments: &str) -> String {
    [
        sse_chunk(json!({
            "id": "chatcmpl-tool",
            "object": "chat.completion.chunk",
            "model": TEST_MODEL,
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": tool_name,
                            "arguments": arguments
                        }
                    }]
                },
                "finish_reason": null
            }]
        })),
        sse_chunk(json!({
            "id": "chatcmpl-tool",
            "object": "chat.completion.chunk",
            "model": TEST_MODEL,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 2,
                "total_tokens": 12
            }
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .join("")
}

fn final_answer_sse(answer: &str) -> String {
    [
        sse_chunk(json!({
            "id": "chatcmpl-final",
            "object": "chat.completion.chunk",
            "model": TEST_MODEL,
            "choices": [{
                "index": 0,
                "delta": { "content": answer },
                "finish_reason": null
            }]
        })),
        sse_chunk(json!({
            "id": "chatcmpl-final",
            "object": "chat.completion.chunk",
            "model": TEST_MODEL,
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 8,
                "total_tokens": 28
            }
        })),
        "data: [DONE]\n\n".to_string(),
    ]
    .join("")
}

fn sse_response(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .insert_header("cache-control", "no-cache")
        .set_body_string(body.to_string())
}

fn tool_messages(body: &Value) -> Vec<String> {
    body.get("messages")
        .and_then(Value::as_array)
        .map(|msgs| {
            msgs.iter()
                .filter(|msg| msg.get("role").and_then(Value::as_str) == Some("tool"))
                .filter_map(|msg| {
                    msg.get("content")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn extract_skill_base_dir(text: &str) -> PathBuf {
    let marker = "Base directory for this skill: `";
    let start = text
        .find(marker)
        .unwrap_or_else(|| panic!("missing base dir marker in:\n{text}"))
        + marker.len();
    let rest = &text[start..];
    let end = rest
        .find('`')
        .unwrap_or_else(|| panic!("unclosed base dir backtick in:\n{text}"));
    PathBuf::from(&rest[..end])
}

struct FullAccessResponder {
    pkg_dir: PathBuf,
    captured: Arc<Mutex<Vec<Value>>>,
}

impl Respond for FullAccessResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().expect("request JSON");
        self.captured
            .lock()
            .expect("captured lock")
            .push(body.clone());
        let results = tool_messages(&body);
        let sse = match results.len() {
            0 => tool_call_sse(
                "call_1",
                "load_skill",
                r#"{"name":"cordis-plugin-development"}"#,
            ),
            1 => {
                let base_dir = extract_skill_base_dir(&results[0]);
                assert!(base_dir.ends_with("cordis-plugin-development"));
                tool_call_sse(
                    "call_2",
                    "read",
                    &json!({ "path": base_dir.join("templates/decoration/package.json") })
                        .to_string(),
                )
            }
            2 => {
                assert!(results[1].contains("dsh"), "{}", results[1]);
                tool_call_sse(
                    "call_3",
                    "write",
                    &json!({
                        "path": self.pkg_dir.join("package.json"),
                        "content": tool_package_json(),
                    })
                    .to_string(),
                )
            }
            3 => tool_call_sse(
                "call_4",
                "write",
                &json!({
                    "path": self.pkg_dir.join("cordis.patch.yml"),
                    "content": tool_cordis_patch(),
                })
                .to_string(),
            ),
            4 => tool_call_sse(
                "call_5",
                "write",
                &json!({
                    "path": self.pkg_dir.join("index.js"),
                    "content": tool_index_js("exec-v1"),
                })
                .to_string(),
            ),
            5 => tool_call_sse(
                "call_6",
                "plugin_manager",
                &json!({
                    "action": "install_bundle",
                    "target": self.pkg_dir,
                })
                .to_string(),
            ),
            6 => tool_call_sse("call_7", "loop_echo", r#"{"note":"from-exec"}"#),
            _ => final_answer_sse("creator exec ok"),
        };
        sse_response(&sse)
    }
}

struct AskDenyResponder {
    pkg_dir: PathBuf,
    captured: Arc<Mutex<Vec<Value>>>,
}

impl Respond for AskDenyResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().expect("request JSON");
        self.captured
            .lock()
            .expect("captured lock")
            .push(body.clone());
        let results = tool_messages(&body);
        let sse = if results.is_empty() {
            tool_call_sse(
                "call_deny",
                "plugin_manager",
                &json!({
                    "action": "install_bundle",
                    "target": self.pkg_dir,
                })
                .to_string(),
            )
        } else {
            final_answer_sse("denied as expected")
        };
        sse_response(&sse)
    }
}

fn prepare_hermetic_home(home: &Path, node: &Path) {
    let codewhale_dir = home.join(".codewhale");
    std::fs::create_dir_all(&codewhale_dir).expect("create .codewhale");
    std::fs::create_dir_all(home.join(".deepseek")).expect("create .deepseek");
    let skills_dir = codewhale_dir.join("skills");
    codewhale_skills::install_system_skills(&skills_dir, &BundledSkillsConfig::default())
        .expect("install system skills");
    let node_toml = toml::Value::String(node.to_string_lossy().into_owned()).to_string();
    let config_toml = format!(
        "[features]\nextension_host = true\nplugin_creator = true\n\n[extension_host]\nruntime = \"node\"\nnode = {node_toml}\n"
    );
    std::fs::write(codewhale_dir.join("config.toml"), config_toml).expect("write config.toml");
}

fn run_exec_binary(
    workspace: &Path,
    home: &Path,
    node: &Path,
    server_uri: &str,
    permission_mode: Option<&str>,
    exec_args: &[&str],
) -> std::process::Output {
    let mut command = Command::new(crate::binary::codewhale());
    command.env_clear();
    for key in [
        "PATHEXT",
        "SystemRoot",
        "SystemDrive",
        "WINDIR",
        "LOCALAPPDATA",
        "APPDATA",
        "PROGRAMDATA",
        "COMSPEC",
        "TEMP",
        "TMP",
        "TMPDIR",
        "TERM",
        "COLORTERM",
        "LANG",
        "LC_ALL",
    ] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let mut path_entries = Vec::new();
    if let Some(parent) = node.parent() {
        path_entries.push(parent.to_path_buf());
    }
    if let Some(existing) = std::env::var_os("PATH") {
        let mut seen: HashSet<PathBuf> = path_entries.iter().cloned().collect();
        for dir in std::env::split_paths(&existing) {
            if seen.insert(dir.clone()) {
                path_entries.push(dir);
            }
        }
    }
    let joined_path = std::env::join_paths(path_entries).expect("join PATH");

    command
        .current_dir(workspace)
        .arg("--workspace")
        .arg(workspace)
        .arg("--no-project-config");
    if let Some(mode) = permission_mode {
        command.arg("--permission-mode").arg(mode);
    }
    command
        .arg("exec")
        .arg("--conversation-mode")
        .arg("creator")
        .args(exec_args)
        .arg("--model")
        .arg(TEST_MODEL)
        .arg("--output-format")
        .arg("stream-json")
        .arg("create and call loop_echo")
        .env("PATH", joined_path)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local").join("share"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .env("CODEWHALE_HOME", home.join(".codewhale"))
        .env(
            "CODEWHALE_CONFIG_PATH",
            home.join(".codewhale").join("config.toml"),
        )
        .env(
            "DEEPSEEK_CONFIG_PATH",
            home.join(".deepseek").join("config.toml"),
        )
        .env("DEEPSEEK_API_KEY", "ci-test-key-not-real")
        .env("DEEPSEEK_BASE_URL", server_uri)
        .env("CODEWHALE_BASE_URL", server_uri)
        .env("DEEPSEEK_MODEL", TEST_MODEL)
        .env("CODEWHALE_MODEL", TEST_MODEL)
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE", "1")
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().expect("spawn codewhale exec");
    let mut stdout_pipe = child.stdout.take().expect("stdout pipe");
    let mut stderr_pipe = child.stderr.take().expect("stderr pipe");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stdout_pipe.read_to_end(&mut buf).map(|_| buf)
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stderr_pipe.read_to_end(&mut buf).map(|_| buf)
    });

    let status = match child
        .wait_timeout(Duration::from_secs(90))
        .expect("wait codewhale exec")
    {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let out = stdout_reader
                .join()
                .expect("stdout join")
                .unwrap_or_default();
            let err = stderr_reader
                .join()
                .expect("stderr join")
                .unwrap_or_default();
            panic!(
                "codewhale exec timed out\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&out),
                String::from_utf8_lossy(&err)
            );
        }
    };
    let stdout = stdout_reader
        .join()
        .expect("stdout join")
        .expect("read stdout");
    let stderr = stderr_reader
        .join()
        .expect("stderr join")
        .expect("read stderr");
    std::process::Output {
        status,
        stdout,
        stderr,
    }
}

fn stream_events(stdout: &[u8]) -> Vec<Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("bad JSONL {e}: {line}")))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creator_exec_full_access_installs_and_calls_its_tool() {
    let node = discover_node_binary();
    let home = TempDir::new().expect("home tempdir");
    let workspace = TempDir::new().expect("workspace tempdir");
    prepare_hermetic_home(home.path(), &node);
    let pkg_dir = workspace.path().join("my-plugin");

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "object": "list",
                    "data": [{ "id": TEST_MODEL, "object": "model" }]
                })),
        )
        .mount(&server)
        .await;

    let captured = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(FullAccessResponder {
            pkg_dir: pkg_dir.clone(),
            captured: Arc::clone(&captured),
        })
        .mount(&server)
        .await;

    let uri = server.uri();
    let ws_path = workspace.path().to_path_buf();
    let home_path = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        run_exec_binary(&ws_path, &home_path, &node, &uri, None, &["--auto"])
    })
    .await
    .expect("spawn_blocking join");

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "codewhale exec failed ({:?})\nstdout:\n{stdout_str}\nstderr:\n{stderr_str}",
        output.status.code()
    );

    let requests = captured.lock().expect("captured lock");
    assert!(!requests.is_empty());
    let first_tools = requests[0]["tools"]
        .as_array()
        .expect("tools on first request");
    assert!(
        first_tools
            .iter()
            .any(|t| t["function"]["name"] == "plugin_manager" || t["name"] == "plugin_manager"),
        "expected plugin_manager in first request: {}",
        requests[0]
    );
    assert!(
        requests.iter().all(|req| {
            !req["messages"]
                .to_string()
                .contains("\"name\":\"tool_search\"")
        }),
        "model must not call tool_search in creator full access"
    );
    drop(requests);

    let events = stream_events(&output.stdout);
    let loop_echo_result = events
        .iter()
        .find(|ev| {
            ev.get("type").and_then(Value::as_str) == Some("tool_result")
                && ev.get("name").and_then(Value::as_str) == Some("loop_echo")
        })
        .unwrap_or_else(|| panic!("missing loop_echo tool_result in:\n{stdout_str}"));
    assert_eq!(loop_echo_result["status"], json!("success"));
    let echo_output = loop_echo_result["output"].as_str().expect("output string");
    assert!(echo_output.contains("exec-v1"), "{echo_output}");
    assert!(echo_output.contains("from-exec"), "{echo_output}");

    let state_raw = std::fs::read_to_string(home.path().join(".codewhale/plugins/state.json"))
        .expect("read plugin state.json");
    let state: Value = serde_json::from_str(&state_raw).expect("parse state.json");
    let plugins = state["plugins"].as_object().expect("plugins object");
    let (_, entry) = plugins
        .iter()
        .find(|(id, _)| id.ends_with("my-plugin"))
        .expect("my-plugin state entry");
    assert_eq!(entry["enabled"], json!(true));
    let approver: PluginApprover =
        serde_json::from_value(entry["trust"]["approved_by"].clone()).expect("approved_by");
    match approver {
        PluginApprover::Agent(agent) => {
            assert_eq!(agent.posture, AgentPosture::FullAccess);
        }
        other => panic!("expected Agent FullAccess approval, got {other:?}"),
    }

    drop(server);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn creator_exec_ask_is_denied_and_installs_nothing() {
    let node = discover_node_binary();
    let home = TempDir::new().expect("home tempdir");
    let workspace = TempDir::new().expect("workspace tempdir");
    prepare_hermetic_home(home.path(), &node);
    let pkg_dir = workspace.path().join("my-plugin");
    std::fs::create_dir_all(&pkg_dir).expect("mkdir pkg");
    std::fs::write(pkg_dir.join("package.json"), tool_package_json()).expect("write pkg");
    std::fs::write(pkg_dir.join("cordis.patch.yml"), tool_cordis_patch()).expect("write patch");
    std::fs::write(pkg_dir.join("index.js"), tool_index_js("never")).expect("write index");

    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "object": "list",
                    "data": [{ "id": TEST_MODEL, "object": "model" }]
                })),
        )
        .mount(&server)
        .await;

    let captured = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(AskDenyResponder {
            pkg_dir: pkg_dir.clone(),
            captured: Arc::clone(&captured),
        })
        .mount(&server)
        .await;

    let uri = server.uri();
    let ws_path = workspace.path().to_path_buf();
    let home_path = home.path().to_path_buf();
    let output = tokio::task::spawn_blocking(move || {
        run_exec_binary(&ws_path, &home_path, &node, &uri, Some("ask"), &[])
    })
    .await
    .expect("spawn_blocking join");

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    let events = stream_events(&output.stdout);

    let pm_result = events
        .iter()
        .find(|ev| {
            ev.get("type").and_then(Value::as_str) == Some("tool_result")
                && ev.get("name").and_then(Value::as_str) == Some("plugin_manager")
        })
        .unwrap_or_else(|| {
            panic!("missing plugin_manager tool_result:\n{stdout_str}\n{stderr_str}")
        });
    assert_eq!(pm_result["status"], json!("error"), "{pm_result}");
    assert_eq!(
        pm_result["error_category"],
        json!("permission_denied"),
        "{pm_result}"
    );
    let pm_output = pm_result["output"]
        .as_str()
        .expect("plugin_manager error output");
    assert!(
        pm_output.contains("denied by user")
            && pm_output.contains("the call was not approved")
            && pm_output.contains("approval"),
        "expected permission_denied approval message in tool_result\nstdout:\n{stdout_str}\nstderr:\n{stderr_str}"
    );
    let meta_event = events
        .iter()
        .find(|ev| ev.get("type").and_then(Value::as_str) == Some("metadata"))
        .unwrap_or_else(|| panic!("missing metadata receipt:\n{stdout_str}\n{stderr_str}"));
    assert_eq!(
        meta_event["meta"]["approval_posture"],
        json!("ask"),
        "{meta_event}"
    );
    assert!(!home.path().join(".codewhale/plugins/state.json").exists());

    drop(server);
}

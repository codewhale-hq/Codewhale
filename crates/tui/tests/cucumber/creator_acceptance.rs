use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use codewhale_plugins::host_services::{AgentPosture, PersonVia, PluginApprover};
use codewhale_runtime::config::BundledSkillsConfig;
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::qa_harness::{
    harness::{Harness, make_sealed_workspace},
    keys,
};

const TEST_MODEL: &str = "acceptance-model";
const PTY_WAIT: Duration = Duration::from_secs(30);

fn discover_node_binary() -> PathBuf {
    for var in ["NODE_BINARY", "CODEWHALE_NODE_BINARY"] {
        if let Some(value) = std::env::var_os(var) {
            let candidate = PathBuf::from(value);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("node");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    for candidate in [
        Path::new("/opt/homebrew/bin/node"),
        Path::new("/usr/local/bin/node"),
        Path::new("/usr/bin/node"),
    ] {
        if candidate.is_file() {
            return candidate.to_path_buf();
        }
    }
    panic!("node binary required for creator_acceptance PTY tests");
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
description: 'Echo from creator PTY fixture.',\n    \
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

struct PtyFullAccessResponder {
    pkg_dir: PathBuf,
    captured: Arc<Mutex<Vec<Value>>>,
}

impl Respond for PtyFullAccessResponder {
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
                tool_call_sse(
                    "call_2",
                    "read",
                    &json!({ "path": base_dir.join("templates/decoration/package.json") })
                        .to_string(),
                )
            }
            2 => tool_call_sse(
                "call_3",
                "write",
                &json!({
                    "path": self.pkg_dir.join("package.json"),
                    "content": tool_package_json(),
                })
                .to_string(),
            ),
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
                    "content": tool_index_js("pty-v1"),
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
            6 => tool_call_sse("call_7", "loop_echo", r#"{"note":"from-pty-fa"}"#),
            _ => final_answer_sse("CREATOR_PTY_FULL_ACCESS_DONE"),
        };
        sse_response(&sse)
    }
}

struct PtyAskResponder {
    pkg_dir: PathBuf,
    captured: Arc<Mutex<Vec<Value>>>,
}

impl Respond for PtyAskResponder {
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
                tool_call_sse(
                    "call_2",
                    "read",
                    &json!({ "path": base_dir.join("templates/decoration/package.json") })
                        .to_string(),
                )
            }
            2 => tool_call_sse(
                "call_3",
                "plugin_manager",
                &json!({
                    "action": "install_bundle",
                    "target": self.pkg_dir,
                })
                .to_string(),
            ),
            3 => tool_call_sse("call_4", "loop_echo", r#"{"note":"from-pty-ask"}"#),
            _ => final_answer_sse("CREATOR_PTY_ASK_DONE"),
        };
        sse_response(&sse)
    }
}

fn prepare_pty_workspace(node: &Path) -> (super::qa_harness::harness::SealedWorkspace, String) {
    let workspace = make_sealed_workspace().expect("sealed workspace");
    let codewhale_dir = workspace.home().join(".codewhale");
    std::fs::create_dir_all(&codewhale_dir).expect("mkdir .codewhale");
    std::fs::write(codewhale_dir.join(".onboarded"), "").expect("write .onboarded");
    let skills_dir = codewhale_dir.join("skills");
    codewhale_skills::install_system_skills(&skills_dir, &BundledSkillsConfig::default())
        .expect("install system skills");
    let node_toml = toml::Value::String(node.to_string_lossy().into_owned()).to_string();
    let config_toml = format!(
        "[notifications]\nmethod = \"off\"\ncompletion_sound = \"off\"\n\n[features]\nextension_host = true\nplugin_creator = true\n\n[extension_host]\nruntime = \"node\"\nnode = {node_toml}\n"
    );
    std::fs::write(codewhale_dir.join("config.toml"), config_toml).expect("write config.toml");

    let trust = workspace.workspace().join(".deepseek");
    std::fs::create_dir_all(&trust).expect("mkdir .deepseek");
    std::fs::write(trust.join("trusted"), "").expect("write trusted");

    let mut dirs = Vec::new();
    if let Some(parent) = node.parent() {
        dirs.push(parent.to_path_buf());
    }
    for base in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        dirs.push(PathBuf::from(base));
    }
    let mut seen = HashSet::new();
    let deduped: Vec<PathBuf> = dirs
        .into_iter()
        .filter(|d| seen.insert(d.clone()))
        .collect();
    let path_env = std::env::join_paths(deduped)
        .expect("join PATH")
        .to_string_lossy()
        .into_owned();
    (workspace, path_env)
}

fn spawn_creator_tui(
    workspace: &super::qa_harness::harness::SealedWorkspace,
    path_env: &str,
    server_uri: &str,
    permission_mode: &str,
) -> Harness {
    let mut tui = Harness::builder(Harness::codewhale_binary())
        .cwd(workspace.workspace())
        .clear_env()
        .seal_home(workspace.home())
        .env("PATH", path_env)
        .env("DEEPSEEK_API_KEY", "ci-pty-test-key")
        .env("DEEPSEEK_BASE_URL", server_uri)
        .env("CODEWHALE_BASE_URL", server_uri)
        .env("DEEPSEEK_MODEL", TEST_MODEL)
        .env("CODEWHALE_MODEL", TEST_MODEL)
        .env("CODEWHALE_DISABLE_MODELS_DEV_FETCH", "1")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .env("CODEWHALE_DISABLE_LOCAL_OLLAMA_PROBE", "1")
        .env("NO_ANIMATIONS", "1")
        .env("RUST_LOG", "warn")
        .args([
            "--workspace",
            workspace.workspace().to_str().expect("workspace UTF-8"),
            "--no-project-config",
            "--skip-onboarding",
            "--fresh",
            "--conversation-mode",
            "creator",
            "--permission-mode",
            permission_mode,
        ])
        .size(40, 120)
        .spawn()
        .expect("start creator TUI");
    tui.wait_for_text("Creator mode", PTY_WAIT)
        .expect("startup Creator mode");
    tui.wait_for_idle(Duration::from_millis(250), Duration::from_secs(5))
        .expect("startup idle");
    tui
}

#[test]
fn creator_pty_full_access_installs_and_calls_its_tool() {
    let node = discover_node_binary();
    let (workspace, path_env) = prepare_pty_workspace(&node);
    let pkg_dir = workspace.workspace().join("my-plugin");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let server = rt.block_on(async {
        let srv = MockServer::start().await;
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
            .mount(&srv)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(PtyFullAccessResponder {
                pkg_dir: pkg_dir.clone(),
                captured: Arc::clone(&captured),
            })
            .mount(&srv)
            .await;
        srv
    });

    let mut tui = spawn_creator_tui(&workspace, &path_env, &server.uri(), "full-access");
    tui.type_line("create and run loop_echo")
        .expect("submit creator prompt");
    tui.wait_for_text("CREATOR_PTY_FULL_ACCESS_DONE", PTY_WAIT)
        .unwrap_or_else(|e| panic!("full access PTY did not finish: {e}\n{}", tui.diagnostics()));
    tui.pump();
    let frame = tui.frame().text();
    assert!(
        !frame.contains("Do you want to proceed?"),
        "unexpected approval prompt:\n{frame}"
    );
    tui.shutdown();

    let state_raw = std::fs::read_to_string(workspace.home().join(".codewhale/plugins/state.json"))
        .expect("read plugin state.json");
    let state: Value = serde_json::from_str(&state_raw).expect("parse state.json");
    let plugins = state["plugins"].as_object().expect("plugins map");
    let (_, entry) = plugins
        .iter()
        .find(|(id, _)| id.ends_with("my-plugin"))
        .expect("my-plugin entry");
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
    rt.shutdown_timeout(Duration::from_secs(5));
}

#[test]
fn creator_pty_ask_card_yes_installs_and_calls_its_tool() {
    let node = discover_node_binary();
    let (workspace, path_env) = prepare_pty_workspace(&node);
    let pkg_dir = workspace.workspace().join("my-plugin");
    std::fs::create_dir_all(&pkg_dir).expect("mkdir pkg");
    std::fs::write(pkg_dir.join("package.json"), tool_package_json()).expect("write pkg");
    std::fs::write(pkg_dir.join("cordis.patch.yml"), tool_cordis_patch()).expect("write patch");
    std::fs::write(pkg_dir.join("index.js"), tool_index_js("pty-ask-v1")).expect("write index");

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("tokio runtime");
    let captured = Arc::new(Mutex::new(Vec::new()));
    let server = rt.block_on(async {
        let srv = MockServer::start().await;
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
            .mount(&srv)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(PtyAskResponder {
                pkg_dir: pkg_dir.clone(),
                captured: Arc::clone(&captured),
            })
            .mount(&srv)
            .await;
        srv
    });

    let mut tui = spawn_creator_tui(&workspace, &path_env, &server.uri(), "ask");
    tui.type_line("install and run loop_echo")
        .expect("submit ask prompt");
    tui.wait_for(
        |frame| {
            let text = frame.text();
            text.contains("Allow once")
                && text.contains("Do you want to proceed?")
                && (text.contains("Plugin manager") || text.contains("plugin_manager"))
                && !text.contains("Use the loop_echo tool")
        },
        PTY_WAIT,
    )
    .unwrap_or_else(|e| {
        panic!(
            "first approval card did not appear: {e}\n{}",
            tui.diagnostics()
        )
    });
    tui.send(keys::key::ch('y'))
        .expect("approve plugin_manager card");

    tui.wait_for(
        |frame| {
            let text = frame.text();
            text.contains("Allow once")
                && text.contains("Do you want to proceed?")
                && text.contains("Use the loop_echo tool")
        },
        PTY_WAIT,
    )
    .unwrap_or_else(|e| {
        panic!(
            "loop_echo approval card did not appear: {e}\n{}",
            tui.diagnostics()
        )
    });
    tui.send(keys::key::ch('y'))
        .expect("approve loop_echo card");

    tui.wait_for_text("CREATOR_PTY_ASK_DONE", PTY_WAIT)
        .unwrap_or_else(|e| panic!("ask PTY did not finish: {e}\n{}", tui.diagnostics()));
    tui.shutdown();

    let state_raw = std::fs::read_to_string(workspace.home().join(".codewhale/plugins/state.json"))
        .expect("read plugin state.json");
    let state: Value = serde_json::from_str(&state_raw).expect("parse state.json");
    let plugins = state["plugins"].as_object().expect("plugins map");
    let (_, entry) = plugins
        .iter()
        .find(|(id, _)| id.ends_with("my-plugin"))
        .expect("my-plugin entry");
    assert_eq!(entry["enabled"], json!(true));
    let approver: PluginApprover =
        serde_json::from_value(entry["trust"]["approved_by"].clone()).expect("approved_by");
    assert_eq!(
        approver,
        PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        }
    );

    drop(server);
    rt.shutdown_timeout(Duration::from_secs(5));
}

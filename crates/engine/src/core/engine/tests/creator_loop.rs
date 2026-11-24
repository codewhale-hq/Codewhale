use super::*;
use crate::conversation_modes::{CREATOR, CreatorAvailabilityGuard, ModeRefusal, STANDARD};
use crate::core::authority::auto_review::AutoReviewPolicy;
use crate::core::ops::{TurnInputAuthorship, TurnSpec, UserInputProvenance};
use crate::extension_host::tier::{HostCohort, HostTier};
use crate::extension_host::{ExtensionHostManager, ExtensionHostOptions, TestManagerGuard};
use crate::llm_client::mock::{MockLlmClient, canned};
use crate::runtime_threads::manager::{
    CreateThreadRequest, RuntimeThreadManager, RuntimeThreadManagerConfig, StartTurnRequest,
    UpdateThreadRequest,
};
use crate::tools::approval_cache::CREATOR_HUMAN_APPROVAL_KEY_PREFIX;
use crate::tools::spec::{ToolError, ToolResult};
use codewhale_config::extension_host::ExtensionHostRuntime;
use codewhale_models::{MessageRequest, Tool};
use codewhale_plugins::PluginRegistry;
use codewhale_plugins::activation::TestPolicyGuard;
use codewhale_plugins::discovery::{DiscoveryConfig, discover_with_config};
use codewhale_plugins::host_services::{
    AgentPosture, PersonVia, PluginApprover, ReviewApproval, approver_label,
};
use codewhale_runtime::config::BundledSkillsConfig;
use std::collections::BTreeSet;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CREATOR_LOOP_EVENT_TIMEOUT: Duration = Duration::from_secs(60);

struct LoopFixture {
    _home: crate::test_support::SealedHome,
    _policy: TestPolicyGuard,
    _available: CreatorAvailabilityGuard,
    temp: tempfile::TempDir,
    workspace: PathBuf,
    user_plugins_dir: PathBuf,
    builtin_dir: PathBuf,
    state_path: PathBuf,
    node: PathBuf,
    ext_root: PathBuf,
    reg_config: DiscoveryConfig,
    manager: Arc<ExtensionHostManager>,
    _manager_guard: TestManagerGuard,
}

impl LoopFixture {
    fn new(test_name: &str, ext_host_on: bool) -> Self {
        let home = crate::test_support::SealedHome::new();
        let node = crate::extension_host::test_support::node_for_tests(test_name)
            .expect("node runtime required");
        let policy = TestPolicyGuard::extension_host(ext_host_on);
        let available = CreatorAvailabilityGuard::set(None);
        let skills_dir = codewhale_runtime::config::owned_skills_dir().expect("owned skills dir");
        codewhale_skills::install_system_skills(&skills_dir, &BundledSkillsConfig::default())
            .expect("install system skills");

        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("ws");
        let user_plugins_dir = temp.path().join("user-plugins");
        let workspace_plugins_dir = workspace.join(".codewhale/plugins");
        let builtin_dir = temp.path().join("builtin-plugins");
        let state_path = temp.path().join("state/plugin-state.json");
        let ext_root = temp.path().join("ext-root");
        fs::create_dir_all(&workspace).expect("workspace");
        fs::create_dir_all(&user_plugins_dir).expect("user plugins");
        fs::create_dir_all(&workspace_plugins_dir).expect("workspace plugins");
        fs::create_dir_all(&builtin_dir).expect("builtin plugins");
        fs::create_dir_all(&ext_root).expect("ext root");

        let reg_config = DiscoveryConfig {
            workspace: workspace.clone(),
            user_plugins_dir: user_plugins_dir.clone(),
            workspace_plugins_dir,
            builtin_plugin_dirs: vec![builtin_dir.clone()],
            state_path: state_path.clone(),
        };

        let manager = Self::new_manager(&node, &ext_root);
        let manager_guard = TestManagerGuard::install(Arc::clone(&manager));

        Self {
            _home: home,
            _policy: policy,
            _available: available,
            temp,
            workspace,
            user_plugins_dir,
            builtin_dir,
            state_path,
            node,
            ext_root,
            reg_config,
            manager,
            _manager_guard: manager_guard,
        }
    }

    fn new_manager(node: &Path, ext_root: &Path) -> Arc<ExtensionHostManager> {
        Arc::new(ExtensionHostManager::new(ExtensionHostOptions {
            runtime: ExtensionHostRuntime::Node,
            node_override: Some(node.to_path_buf()),
            root: Some(ext_root.to_path_buf()),
            ..Default::default()
        }))
    }

    fn discover(&self) -> PluginRegistry {
        discover_with_config(&self.reg_config)
    }

    fn engine_config(
        &self,
        registry: PluginRegistry,
        preset: Option<&str>,
        ext_host_on: bool,
    ) -> EngineConfig {
        let mut cfg = deterministic_engine_config(&self.workspace);
        cfg.features.enable(Feature::PluginCreator);
        if ext_host_on {
            cfg.features.enable(Feature::ExtensionHost);
        } else {
            cfg.features.disable(Feature::ExtensionHost);
        }
        cfg.plugin_registry = Some(Arc::new(registry));
        cfg.agent_preset = preset.map(str::to_string);
        cfg
    }
}

fn tool_package_json() -> String {
    serde_json::json!({
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
description: 'Echo from creator loop fixture.',\n    \
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

fn write_tool_package(dir: &Path, reply: &str) {
    fs::create_dir_all(dir).expect("tool package dir");
    fs::write(dir.join("package.json"), tool_package_json()).expect("package.json");
    fs::write(dir.join("cordis.patch.yml"), tool_cordis_patch()).expect("cordis.patch.yml");
    fs::write(dir.join("index.js"), tool_index_js(reply)).expect("index.js");
}

fn pinned_mcp_template_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../skills/assets/dsh-creator/skills/cordis-plugin-development/templates/mcp")
}

fn copy_dir_with_mcp_port(src: &Path, dst: &Path, port: u16) {
    fs::create_dir_all(dst).expect("copy dst");
    for entry in fs::read_dir(src).expect("read template dir") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        if entry.file_type().expect("file type").is_dir() {
            copy_dir_with_mcp_port(&src_path, &dst_path, port);
        } else if name.to_string_lossy() == "cordis.patch.yml" {
            let text = fs::read_to_string(&src_path).expect("read cordis.patch.yml");
            let updated = text.replacen(
                "http://127.0.0.1:3000/mcp",
                &format!("http://127.0.0.1:{port}/mcp"),
                1,
            );
            assert_ne!(text, updated, "template url line must match");
            fs::write(dst_path, updated).expect("write cordis.patch.yml");
        } else {
            fs::copy(&src_path, &dst_path).expect("copy file");
        }
    }
}

fn find_request_tool<'a>(req: &'a MessageRequest, name: &str) -> Option<&'a Tool> {
    req.tools
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find(|tool| tool.name == name)
}

fn request_tool_names(req: &MessageRequest) -> Vec<String> {
    req.tools
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|tool| tool.name.clone())
        .collect()
}

fn latest_tool_result_text(req: &MessageRequest) -> String {
    req.messages
        .iter()
        .rev()
        .flat_map(|msg| msg.content.iter().rev())
        .find_map(|block| match block {
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

fn extract_skill_base_dir(req: &MessageRequest) -> PathBuf {
    let text = latest_tool_result_text(req);
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

fn extract_connector_tool_name(text: &str, suffix: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(text).ok();
    if let Some(ref val) = parsed
        && let Some(matches) = val.get("matches").and_then(Value::as_array)
    {
        for item in matches {
            if let Some(name) = item.get("name").and_then(Value::as_str)
                && name.contains("mcp_plugin-")
                && name.ends_with(suffix)
            {
                return name.to_string();
            }
        }
    }
    text.split(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-'))
        .find(|token| token.starts_with("mcp_plugin-") && token.ends_with(suffix))
        .unwrap_or_else(|| panic!("expected connector tool ending with {suffix} in:\n{text}"))
        .to_string()
}

fn parse_tool_json(res: &Result<ToolResult, ToolError>) -> Value {
    let ok = res
        .as_ref()
        .unwrap_or_else(|err| panic!("tool call failed: {err}"));
    let body = match ok.content.strip_prefix("[approval]") {
        Some(rest) => rest
            .split_once("\n\n")
            .map_or(ok.content.as_str(), |(_, json_part)| json_part),
        None => ok.content.as_str(),
    };
    serde_json::from_str(body)
        .unwrap_or_else(|err| panic!("expected JSON tool content ({err}): {}", ok.content))
}

fn read_state_entry(state_path: &Path, selector: &str) -> Option<Value> {
    let raw = fs::read_to_string(state_path).ok()?;
    let json: Value = serde_json::from_str(&raw).ok()?;
    let plugins = json.get("plugins")?.as_object()?;
    plugins
        .iter()
        .find(|(id, _)| {
            id.as_str() == selector
                || id.ends_with(&format!("/{selector}"))
                || id.ends_with(&format!(":{selector}"))
        })
        .map(|(_, entry)| entry.clone())
}

fn parse_approver(val: Option<&Value>) -> Option<PluginApprover> {
    val.and_then(|v| serde_json::from_value::<PluginApprover>(v.clone()).ok())
}

fn walk_files_excluding(root: &Path, excluded: &[&Path]) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if excluded.iter().any(|prefix| path.starts_with(prefix)) {
                continue;
            }
            if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                stack.push(path);
            } else {
                out.insert(path);
            }
        }
    }
    out
}

#[derive(Clone, Copy)]
enum LoopApproval {
    PanicOnCard,
    ApproveUser,
    DenyUser,
}

struct CapturedLoopCard {
    tool_name: String,
    approval_key: String,
    approval_force_prompt: bool,
}

struct LoopOutcome {
    cards: Vec<CapturedLoopCard>,
    tool_results: Vec<(String, Result<ToolResult, ToolError>)>,
    tool_exec_ids: Vec<(String, String)>,
}

fn turn_op(content: &str, approval_mode: ApprovalMode, config: &Config) -> Op {
    let mut op = external_user_message_op(content, AppMode::Agent, config);
    if let Op::SendMessage(TurnSpec {
        approval_mode: turn_approval,
        auto_approve,
        trust_mode,
        provenance,
        input_authorship,
        ..
    }) = &mut op
    {
        *turn_approval = approval_mode;
        *auto_approve = approval_mode == ApprovalMode::Bypass;
        *trust_mode = approval_mode == ApprovalMode::Bypass;
        *provenance = UserInputProvenance::ExternalUser;
        *input_authorship = TurnInputAuthorship::Person(content.to_string());
    }
    op
}

fn auto_review_message_op(content: &str, config: &Config) -> Op {
    let mut op = turn_op(content, ApprovalMode::Auto, config);
    if let Op::SendMessage(TurnSpec {
        approval_mode,
        auto_approve,
        trust_mode,
        ..
    }) = &mut op
    {
        *approval_mode = ApprovalMode::Auto;
        *auto_approve = false;
        *trust_mode = false;
    }
    op
}

fn spawn_interactive(
    cfg: EngineConfig,
    mock: Arc<MockLlmClient>,
) -> (EngineHandle, tokio::task::JoinHandle<()>) {
    let config = Config::default();
    let (mut engine, mut handle) = Engine::new_with_model_client(cfg, &config, mock);
    engine.attach_owner_surface_for_tests(&mut handle);
    let task = tokio::spawn(engine.run());
    (handle, task)
}

async fn drive_turn(handle: &EngineHandle, op: Op, behavior: LoopApproval) -> LoopOutcome {
    handle.send(op).await.expect("send turn op");
    let mut cards = Vec::new();
    let mut tool_results = Vec::new();
    let mut tool_exec_ids = Vec::new();
    let mut rx = handle.rx_event.write().await;
    while let Some(event) = tokio::time::timeout(CREATOR_LOOP_EVENT_TIMEOUT, rx.recv())
        .await
        .expect("timed out waiting for engine event")
    {
        match event {
            Event::ApprovalRequired {
                id,
                tool_name,
                approval_key,
                approval_force_prompt,
                ..
            } => {
                cards.push(CapturedLoopCard {
                    tool_name: tool_name.clone(),
                    approval_key,
                    approval_force_prompt,
                });
                match behavior {
                    LoopApproval::PanicOnCard => {
                        panic!("unexpected approval card for {tool_name}");
                    }
                    LoopApproval::ApproveUser => {
                        handle.approve_tool_call(id).await.expect("approve user");
                    }
                    LoopApproval::DenyUser => {
                        handle.deny_tool_call(id).await.expect("deny user");
                    }
                }
            }
            Event::ToolCallComplete {
                id, name, result, ..
            } => {
                tool_exec_ids.push((name.clone(), id));
                tool_results.push((name, result));
            }
            Event::TurnComplete { status, error, .. } => {
                assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
                break;
            }
            _ => {}
        }
    }
    LoopOutcome {
        cards,
        tool_results,
        tool_exec_ids,
    }
}

async fn shutdown_engine(handle: EngineHandle, task: tokio::task::JoinHandle<()>) {
    handle.send(Op::Shutdown).await.expect("shutdown");
    tokio::time::timeout(CREATOR_LOOP_EVENT_TIMEOUT, task)
        .await
        .expect("shutdown timeout")
        .expect("engine task");
}

async fn spawn_loopback_mcp_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback mcp");
    let port = listener.local_addr().expect("mcp local addr").port();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let mut header_end = None;
                loop {
                    let Ok(n) = stream.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        break;
                    }
                }
                let Some(head_len) = header_end else {
                    return;
                };
                let head = String::from_utf8_lossy(&buf[..head_len]).into_owned();
                let content_length = head
                    .lines()
                    .find_map(|line| {
                        let (k, v) = line.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                while buf.len() < head_len + content_length {
                    let Ok(n) = stream.read(&mut tmp).await else {
                        return;
                    };
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&tmp[..n]);
                }
                let request_line = head.lines().next().unwrap_or("");
                if request_line.starts_with("GET ") {
                    let resp = "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(resp.as_bytes()).await;
                    return;
                }
                let body = &buf[head_len..head_len + content_length];
                let payload: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
                let method = payload.get("method").and_then(Value::as_str).unwrap_or("");
                let id = payload.get("id").cloned().unwrap_or(Value::Null);
                if method == "notifications/initialized" || id.is_null() {
                    let resp =
                        "HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = stream.write_all(resp.as_bytes()).await;
                    return;
                }
                let rpc_response = match method {
                    "initialize" => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "protocolVersion": "2025-03-26",
                            "capabilities": { "tools": { "listChanged": false } },
                            "serverInfo": { "name": "demo-mcp", "version": "1.0.0" }
                        }
                    }),
                    "tools/list" => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "tools": [{
                                "name": "demo_ping",
                                "description": "Loopback MCP ping.",
                                "inputSchema": {
                                    "type": "object",
                                    "properties": {},
                                    "additionalProperties": false
                                }
                            }]
                        }
                    }),
                    "resources/list" => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "resources": [] }
                    }),
                    "resources/templates/list" => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "resourceTemplates": [] }
                    }),
                    "prompts/list" => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "prompts": [] }
                    }),
                    "tools/call" => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": "connector-pong" }],
                            "isError": false
                        }
                    }),
                    _ => serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {}
                    }),
                };
                let sse_body = format!("event: message\ndata: {rpc_response}\n\n");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMcp-Session-Id: loop-mcp-session\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    sse_body.len(),
                    sse_body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    (port, handle)
}

fn install_reviewed_baseline_plugin(f: &LoopFixture, name: &str) {
    let reviewed_dir = f.user_plugins_dir.join(name);
    fs::create_dir_all(reviewed_dir.join("skills/base-skill")).expect("reviewed skill dir");
    fs::write(
        reviewed_dir.join("plugin.toml"),
        format!("schema_version = 1\n[plugin]\nname = {name:?}\nversion = \"1.0.0\"\n\n[skills]\npath = \"skills\"\n"),
    )
    .expect("reviewed plugin.toml");
    fs::write(
        reviewed_dir.join("skills/base-skill/SKILL.md"),
        "---\nname: base-skill\ndescription: reviewed baseline\n---\nBody\n",
    )
    .expect("reviewed SKILL.md");
    let mut reg = f.discover();
    reg.trust_and_enable_by(
        name,
        ReviewApproval {
            approved_by: PluginApprover::Person {
                via: PersonVia::Terminal,
            },
            host_services: None,
        },
        PluginApprover::Person {
            via: PersonVia::Terminal,
        },
    )
    .expect("enable reviewed plugin");
}

#[tokio::test]
async fn creator_full_access_loads_reads_writes_installs_and_calls_its_tool() {
    let f = LoopFixture::new(
        "creator_full_access_loads_reads_writes_installs_and_calls_its_tool",
        true,
    );
    install_reviewed_baseline_plugin(&f, "reviewed-baseline");
    let pkg_dir = f.workspace.join("my-plugin");

    let mock = Arc::new(MockLlmClient::new(Vec::new()));
    mock.push_turn(canned::tool_call_turn(
        "call-load-skill",
        "load_skill",
        r#"{"name":"cordis-plugin-development"}"#,
    ));
    mock.push_factory(|req| {
        let base_dir = extract_skill_base_dir(req);
        assert!(base_dir.ends_with("cordis-plugin-development"));
        let template_path = base_dir.join("templates/decoration/package.json");
        canned::tool_call_turn(
            "call-read-template",
            "read",
            &serde_json::json!({ "path": template_path }).to_string(),
        )
    });
    let pkg_dir_clone = pkg_dir.clone();
    mock.push_factory(move |req| {
        let read_result = latest_tool_result_text(req);
        assert!(read_result.contains("dsh"), "{read_result}");
        canned::tool_call_turn(
            "call-write-pkg",
            "write",
            &serde_json::json!({
                "path": pkg_dir_clone.join("package.json"),
                "content": tool_package_json(),
            })
            .to_string(),
        )
    });
    let pkg_dir_clone = pkg_dir.clone();
    mock.push_factory(move |_| {
        canned::tool_call_turn(
            "call-write-patch",
            "write",
            &serde_json::json!({
                "path": pkg_dir_clone.join("cordis.patch.yml"),
                "content": tool_cordis_patch(),
            })
            .to_string(),
        )
    });
    let pkg_dir_clone = pkg_dir.clone();
    mock.push_factory(move |_| {
        canned::tool_call_turn(
            "call-write-index",
            "write",
            &serde_json::json!({
                "path": pkg_dir_clone.join("index.js"),
                "content": tool_index_js("v1-echo"),
            })
            .to_string(),
        )
    });
    let pkg_dir_clone = pkg_dir.clone();
    mock.push_factory(move |_| {
        canned::tool_call_turn(
            "call-install",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": pkg_dir_clone,
            })
            .to_string(),
        )
    });
    mock.push_factory(|req| {
        let tool = find_request_tool(req, "loop_echo").expect("loop_echo available after install");
        assert!(!tool.defer_loading.unwrap_or(false));
        canned::tool_call_turn("call-loop-echo", "loop_echo", r#"{"note":"hello-fa"}"#)
    });
    mock.push_turn(canned::simple_text_turn("creator loop complete"));

    let config = Config::default();
    let (handle, task) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), true),
        Arc::clone(&mock),
    );
    let outcome = drive_turn(
        &handle,
        turn_op("create and run loop_echo", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;

    assert!(outcome.cards.is_empty());
    let requests = mock.captured_requests();
    let pm = find_request_tool(&requests[0], "plugin_manager")
        .expect("plugin_manager present on first request");
    assert!(!pm.defer_loading.unwrap_or(false));
    assert!(
        outcome
            .tool_results
            .iter()
            .all(|(name, _)| name != "tool_search")
    );

    let (_, install_res) = outcome
        .tool_results
        .iter()
        .find(|(name, _)| name == "plugin_manager")
        .expect("plugin_manager result");
    let install_json = parse_tool_json(install_res);
    assert_eq!(install_json["application"], serde_json::json!("applied"));

    let (_, echo_res) = outcome
        .tool_results
        .iter()
        .find(|(name, _)| name == "loop_echo")
        .expect("loop_echo result");
    let echo_ok = echo_res.as_ref().expect("loop_echo succeeded");
    assert!(echo_ok.success, "{}", echo_ok.content);
    assert!(echo_ok.content.contains("v1-echo"), "{}", echo_ok.content);
    assert!(echo_ok.content.contains("hello-fa"), "{}", echo_ok.content);

    let reg = f.discover();
    let loaded = reg
        .list()
        .into_iter()
        .find(|p| p.id.0 != "reviewed-baseline" && p.id.0.ends_with("my-plugin"))
        .expect("my-plugin discovered");
    assert!(loaded.enabled);
    let entry = read_state_entry(&f.state_path, &loaded.id.0).expect("state entry");
    assert_eq!(entry["enabled"], serde_json::json!(true));
    let approver = parse_approver(entry["trust"].get("approved_by")).expect("approved_by");
    match approver {
        PluginApprover::Agent(agent) => {
            assert_eq!(agent.posture, AgentPosture::FullAccess);
            assert_eq!(
                codewhale_plugins::host_services::PluginCohort::agent(&agent.workspace),
                codewhale_plugins::host_services::PluginCohort::agent(&f.workspace)
            );
            assert!(agent.thread_id.is_some());
            assert!(agent.turn_id.is_some());
            let install_exec_id = &outcome
                .tool_exec_ids
                .iter()
                .find(|(name, _)| name == "plugin_manager")
                .expect("plugin_manager execution id")
                .1;
            assert!(
                uuid::Uuid::parse_str(install_exec_id).is_ok(),
                "{install_exec_id}"
            );
            assert_eq!(
                agent.tool_call_id.as_deref(),
                Some(install_exec_id.as_str())
            );
        }
        other => panic!("expected Agent approval, got {other:?}"),
    }

    let report = f.manager.owner_report(&loaded.id.0).expect("owner report");
    let agent_cohort = HostCohort::agent(&f.workspace);
    assert_eq!(report.cohort.as_ref(), Some(&agent_cohort));
    assert_ne!(report.cohort, Some(HostCohort::reviewed()));

    shutdown_engine(handle, task).await;
    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_ask_one_card_yes_records_the_person() {
    let f = LoopFixture::new("creator_ask_one_card_yes_records_the_person", true);
    let pkg_dir = f.workspace.join("my-plugin");
    write_tool_package(&pkg_dir, "ask-echo");

    let mock = Arc::new(MockLlmClient::new(Vec::new()));
    mock.push_turn(canned::tool_call_turn(
        "call-load-skill",
        "load_skill",
        r#"{"name":"cordis-plugin-development"}"#,
    ));
    mock.push_factory(|req| {
        let base_dir = extract_skill_base_dir(req);
        canned::tool_call_turn(
            "call-read-template",
            "read",
            &serde_json::json!({
                "path": base_dir.join("templates/decoration/package.json"),
            })
            .to_string(),
        )
    });
    let pkg_dir_clone = pkg_dir.clone();
    mock.push_factory(move |_| {
        canned::tool_call_turn(
            "call-ask-install",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": pkg_dir_clone,
            })
            .to_string(),
        )
    });
    mock.push_factory(|_| {
        canned::tool_call_turn("call-ask-echo", "loop_echo", r#"{"note":"ask-ok"}"#)
    });
    mock.push_turn(canned::simple_text_turn("ask install complete"));

    let config = Config::default();
    let (handle, task) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), true),
        Arc::clone(&mock),
    );
    let outcome = drive_turn(
        &handle,
        turn_op("install and run in ask", ApprovalMode::Suggest, &config),
        LoopApproval::ApproveUser,
    )
    .await;

    assert_eq!(outcome.cards.len(), 2);
    assert_eq!(outcome.cards[0].tool_name, "plugin_manager");
    assert!(outcome.cards[0].approval_force_prompt);
    assert!(
        outcome.cards[0]
            .approval_key
            .starts_with(CREATOR_HUMAN_APPROVAL_KEY_PREFIX)
    );
    assert_eq!(outcome.cards[1].tool_name, "loop_echo");

    let (_, echo_res) = outcome
        .tool_results
        .iter()
        .find(|(name, _)| name == "loop_echo")
        .expect("loop_echo result");
    let echo_ok = echo_res.as_ref().expect("loop_echo ok");
    assert!(echo_ok.content.contains("ask-echo"), "{}", echo_ok.content);

    let reg = f.discover();
    let loaded = reg
        .list()
        .into_iter()
        .find(|p| p.id.0.ends_with("my-plugin"))
        .expect("my-plugin discovered");
    let entry = read_state_entry(&f.state_path, &loaded.id.0).expect("state entry");
    assert_eq!(
        parse_approver(entry["trust"].get("approved_by")),
        Some(PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        })
    );
    let report = f.manager.owner_report(&loaded.id.0).expect("owner report");
    assert_eq!(report.cohort, Some(HostCohort::reviewed()));

    shutdown_engine(handle, task).await;
    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_ask_card_no_installs_nothing_and_tells_the_model() {
    let f = LoopFixture::new(
        "creator_ask_card_no_installs_nothing_and_tells_the_model",
        true,
    );
    let pkg_dir = f.workspace.join("my-plugin");
    write_tool_package(&pkg_dir, "never-runs");

    let warmup = f.manager.attach(Arc::new(f.discover()));
    let _ = warmup.sync().await;
    let before_files = walk_files_excluding(f.temp.path(), &[&f.workspace, &f.ext_root]);

    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-ask-deny",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": pkg_dir,
            })
            .to_string(),
        ),
        canned::simple_text_turn("user rejected install"),
    ]));

    let config = Config::default();
    let (handle, task) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), true),
        Arc::clone(&mock),
    );
    let outcome = drive_turn(
        &handle,
        turn_op("install my-plugin", ApprovalMode::Suggest, &config),
        LoopApproval::DenyUser,
    )
    .await;
    shutdown_engine(handle, task).await;

    assert_eq!(outcome.cards.len(), 1);
    assert_eq!(outcome.cards[0].tool_name, "plugin_manager");
    let err = outcome.tool_results[0]
        .1
        .as_ref()
        .expect_err("expected denial error");
    assert!(matches!(err, ToolError::PermissionDenied { .. }), "{err:?}");

    let expected_msg = crate::core::engine::dispatch::format_tool_error(err, "plugin_manager");
    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        latest_tool_result_text(&requests[1]),
        format!("Error: {expected_msg}")
    );

    assert_eq!(f.discover().list().len(), 0);
    assert!(!f.state_path.exists());
    let after_files = walk_files_excluding(f.temp.path(), &[&f.workspace, &f.ext_root]);
    assert_eq!(after_files, before_files);

    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_connection_template_against_a_local_connector() {
    let f = LoopFixture::new(
        "creator_connection_template_against_a_local_connector",
        true,
    );
    let (port, server_handle) = spawn_loopback_mcp_server().await;
    let mcp_dir = f.workspace.join("demo-mcp");
    copy_dir_with_mcp_port(&pinned_mcp_template_dir(), &mcp_dir, port);

    let mock = Arc::new(MockLlmClient::new(Vec::new()));
    mock.push_turn(canned::tool_call_turn(
        "call-install-mcp",
        "plugin_manager",
        &serde_json::json!({
            "action": "install_bundle",
            "target": mcp_dir,
        })
        .to_string(),
    ));
    mock.push_turn(canned::simple_text_turn("connector installed"));
    mock.push_turn(canned::tool_call_turn(
        "call-search-mcp",
        "tool_search",
        r#"{"query":"mcp demo_ping"}"#,
    ));
    mock.push_factory(|req| {
        let tool_name = extract_connector_tool_name(&latest_tool_result_text(req), "demo_ping");
        canned::tool_call_turn("call-mcp-ping", &tool_name, "{}")
    });
    mock.push_turn(canned::simple_text_turn("connector called"));

    let config = Config::default();
    let (handle, task) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), true),
        Arc::clone(&mock),
    );
    let turn1 = drive_turn(
        &handle,
        turn_op("install demo-mcp", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    let install_json = parse_tool_json(&turn1.tool_results[0].1);
    assert_eq!(install_json["application"], serde_json::json!("applied"));
    let warnings = install_json["warnings"].to_string();
    assert!(warnings.contains("mcp_plugin-"), "{warnings}");
    assert!(warnings.contains("demo_ping"), "{warnings}");

    let turn2 = drive_turn(
        &handle,
        turn_op("call demo_ping", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    shutdown_engine(handle, task).await;
    server_handle.abort();

    let (_, ping_res) = turn2
        .tool_results
        .iter()
        .find(|(name, _)| name.ends_with("demo_ping"))
        .expect("demo_ping result");
    let ping_ok = ping_res.as_ref().expect("demo_ping succeeded");
    assert!(
        ping_ok.content.contains("connector-pong"),
        "{}",
        ping_ok.content
    );

    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_edit_remove_install_runs_the_new_code_without_restart() {
    let f = LoopFixture::new(
        "creator_edit_remove_install_runs_the_new_code_without_restart",
        true,
    );
    let pkg_dir = f.workspace.join("my-plugin");
    write_tool_package(&pkg_dir, "v1-echo");

    let mock = Arc::new(MockLlmClient::new(Vec::new()));
    mock.push_turn(canned::tool_call_turn(
        "call-install-v1",
        "plugin_manager",
        &serde_json::json!({
            "action": "install_bundle",
            "target": pkg_dir,
        })
        .to_string(),
    ));
    mock.push_turn(canned::tool_call_turn("call-echo-v1", "loop_echo", "{}"));
    mock.push_turn(canned::simple_text_turn("v1 done"));

    let mock_clone = Arc::clone(&mock);
    let pkg_dir_clone = pkg_dir.clone();
    let config = Config::default();
    let (handle, task) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), true),
        mock_clone,
    );
    let turn1 = drive_turn(
        &handle,
        turn_op("install v1 and run", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    let v1_install = parse_tool_json(&turn1.tool_results[0].1);
    let bundle_id = v1_install["plugin"]
        .as_str()
        .or_else(|| v1_install["id"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            f.discover()
                .list()
                .into_iter()
                .find(|p| p.id.0.ends_with("my-plugin"))
                .expect("installed bundle")
                .id
                .0
                .clone()
        });
    let v1_echo = turn1.tool_results[1].1.as_ref().expect("v1 echo ok");
    assert!(v1_echo.content.contains("v1-echo"), "{}", v1_echo.content);

    fs::write(pkg_dir_clone.join("index.js"), tool_index_js("v2-echo")).expect("write v2");

    mock.push_turn(canned::tool_call_turn(
        "call-remove",
        "plugin_manager",
        &serde_json::json!({
            "action": "remove_bundle",
            "target": bundle_id,
        })
        .to_string(),
    ));
    mock.push_turn(canned::tool_call_turn(
        "call-reinstall-v2",
        "plugin_manager",
        &serde_json::json!({
            "action": "install_bundle",
            "target": pkg_dir_clone,
        })
        .to_string(),
    ));
    mock.push_turn(canned::tool_call_turn("call-echo-v2", "loop_echo", "{}"));
    mock.push_turn(canned::simple_text_turn("v2 done"));

    let turn2 = drive_turn(
        &handle,
        turn_op("reinstall and run v2", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    shutdown_engine(handle, task).await;

    let v2_echo = turn2.tool_results[2].1.as_ref().expect("v2 echo ok");
    assert!(v2_echo.content.contains("v2-echo"), "{}", v2_echo.content);

    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_restart_keeps_the_plugin_on_and_the_agent_record() {
    let f = LoopFixture::new(
        "creator_restart_keeps_the_plugin_on_and_the_agent_record",
        true,
    );
    let pkg_dir = f.workspace.join("my-plugin");
    write_tool_package(&pkg_dir, "restart-echo");

    let config = Config::default();
    let mock1 = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-install",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": pkg_dir,
            })
            .to_string(),
        ),
        canned::tool_call_turn("call-echo-1", "loop_echo", "{}"),
        canned::simple_text_turn("first run done"),
    ]));
    let (h1, t1) = spawn_interactive(f.engine_config(f.discover(), Some(CREATOR), true), mock1);
    let o1 = drive_turn(
        &h1,
        turn_op("install and run", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    shutdown_engine(h1, t1).await;
    assert!(
        o1.tool_results[1]
            .1
            .as_ref()
            .expect("echo 1 ok")
            .content
            .contains("restart-echo")
    );

    let LoopFixture {
        _home,
        _policy,
        _available,
        temp,
        workspace,
        user_plugins_dir,
        builtin_dir,
        state_path,
        node,
        ext_root,
        reg_config,
        manager,
        _manager_guard,
    } = f;
    manager.shutdown().await;
    drop(_manager_guard);
    drop(manager);

    let reg2 = discover_with_config(&reg_config);
    let loaded = reg2
        .list()
        .into_iter()
        .find(|p| p.id.0.ends_with("my-plugin"))
        .expect("plugin persists after restart");
    assert!(loaded.enabled);
    assert!(matches!(
        loaded.approved_by,
        Some(PluginApprover::Agent(ref agent)) if agent.posture == AgentPosture::FullAccess
    ));

    let manager2 = LoopFixture::new_manager(&node, &ext_root);
    let _guard2 = TestManagerGuard::install(Arc::clone(&manager2));

    let mut cfg2 = deterministic_engine_config(&workspace);
    cfg2.features.enable(Feature::PluginCreator);
    cfg2.features.enable(Feature::ExtensionHost);
    cfg2.plugin_registry = Some(Arc::new(reg2));
    cfg2.agent_preset = Some(CREATOR.to_string());

    let mock2 = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-echo-2", "loop_echo", "{}"),
        canned::simple_text_turn("second run done"),
    ]));
    let (h2, t2) = spawn_interactive(cfg2, mock2);
    let o2 = drive_turn(
        &h2,
        turn_op("call after restart", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    shutdown_engine(h2, t2).await;

    let echo2 = o2.tool_results[0].1.as_ref().expect("echo 2 ok");
    assert!(echo2.content.contains("restart-echo"), "{}", echo2.content);

    let _ = (user_plugins_dir, builtin_dir, state_path, temp);
    manager2.shutdown().await;
}

#[tokio::test]
async fn creator_person_review_clears_the_label_and_moves_host_and_data() {
    let f = LoopFixture::new(
        "creator_person_review_clears_the_label_and_moves_host_and_data",
        true,
    );
    let pkg_dir = f.workspace.join("my-plugin");
    write_tool_package(&pkg_dir, "review-echo");
    let config = Config::default();

    let mock1 = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-install",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": pkg_dir,
            })
            .to_string(),
        ),
        canned::tool_call_turn(
            "call-write-storage",
            "loop_echo",
            r#"{"note":"persisted-before-review"}"#,
        ),
        canned::simple_text_turn("wrote storage"),
    ]));
    let (h1, t1) = spawn_interactive(f.engine_config(f.discover(), Some(CREATOR), true), mock1);
    let o1 = drive_turn(
        &h1,
        turn_op("install and store", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;

    let echo1 = o1.tool_results[1].1.as_ref().expect("echo1 ok");
    assert!(
        echo1.content.contains("persisted-before-review"),
        "{}",
        echo1.content
    );

    let mut reg = f.discover();
    let plugin_id = reg
        .list()
        .into_iter()
        .find(|p| p.id.0.ends_with("my-plugin"))
        .expect("installed plugin")
        .id
        .0
        .clone();
    assert_eq!(
        f.manager.owner_report(&plugin_id).and_then(|r| r.cohort),
        Some(HostCohort::agent(&f.workspace))
    );
    shutdown_engine(h1, t1).await;

    reg.trust_and_enable_by(
        &plugin_id,
        ReviewApproval {
            approved_by: PluginApprover::Person {
                via: PersonVia::Terminal,
            },
            host_services: None,
        },
        PluginApprover::Person {
            via: PersonVia::Terminal,
        },
    )
    .expect("person review");

    let refreshed = f.discover();
    let reviewed_plugin = refreshed.get(&plugin_id).expect("plugin remains");
    assert_eq!(
        approver_label(reviewed_plugin.approved_by.as_ref()),
        "approved by you"
    );
    let entry = read_state_entry(&f.state_path, &plugin_id).expect("state entry");
    assert_eq!(
        parse_approver(entry["trust"].get("approved_by")),
        Some(PluginApprover::Person {
            via: PersonVia::Terminal,
        })
    );
    let history = entry["review_history"]
        .as_array()
        .expect("review history array");
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0]["approved_by"]["kind"],
        serde_json::json!("agent")
    );
    assert_eq!(
        history[1]["approved_by"]["kind"],
        serde_json::json!("person")
    );

    let attachment = f.manager.attach(Arc::new(f.discover()));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let _ = attachment.sync().await;
        if f.manager
            .owner_report(&plugin_id)
            .is_some_and(|report| report.cohort == Some(HostCohort::reviewed()))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "plugin did not migrate to reviewed host: {:?}",
            f.manager
                .owner_report(&plugin_id)
                .map(|r| (r.cohort, r.tools, r.diagnostics))
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(attachment);

    let mock2 = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("call-read-storage", "loop_echo", "{}"),
        canned::simple_text_turn("read storage"),
    ]));
    let (h2, t2) = spawn_interactive(f.engine_config(f.discover(), Some(CREATOR), true), mock2);
    let o2 = drive_turn(
        &h2,
        turn_op("read storage after review", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;

    let echo2 = o2.tool_results[0].1.as_ref().expect("echo2 ok");
    assert!(
        echo2.content.contains("persisted-before-review"),
        "{}",
        echo2.content
    );
    assert_eq!(
        f.manager.owner_report(&plugin_id).and_then(|r| r.cohort),
        Some(HostCohort::reviewed())
    );

    shutdown_engine(h2, t2).await;
    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_strict_org_policy_answers_overridden() {
    let f = LoopFixture::new("creator_strict_org_policy_answers_overridden", true);
    let pkg_dir = f.workspace.join("my-plugin");
    write_tool_package(&pkg_dir, "blocked-by-policy");
    fs::create_dir_all(f.state_path.parent().expect("state dir")).expect("mkdir state");
    fs::write(
        f.state_path.with_file_name("managed-policy.json"),
        r#"{"schema_version":1,"allowed_plugins":[],"allow_unlisted":false}"#,
    )
    .expect("write managed-policy.json");

    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-install-policy",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": pkg_dir,
            })
            .to_string(),
        ),
        canned::simple_text_turn("policy overridden"),
    ]));

    let config = Config::default();
    let (handle, task) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), true),
        Arc::clone(&mock),
    );
    let outcome = drive_turn(
        &handle,
        turn_op("install under strict policy", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    shutdown_engine(handle, task).await;

    let install_json = parse_tool_json(&outcome.tool_results[0].1);
    assert_eq!(install_json["application"], serde_json::json!("overridden"));
    assert_eq!(install_json["enabled"], serde_json::json!(false));

    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_full_access_target_outside_the_workspace_asks_the_person() {
    let f = LoopFixture::new(
        "creator_full_access_target_outside_the_workspace_asks_the_person",
        true,
    );
    let outside_dir = f.temp.path().join("outside/ext-plugin");
    write_tool_package(&outside_dir, "outside-echo");
    let config = Config::default();

    let deny_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-out-deny",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": outside_dir,
            })
            .to_string(),
        ),
        canned::simple_text_turn("outside denied"),
    ]));
    let (h1, t1) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), true),
        deny_mock,
    );
    let deny_outcome = drive_turn(
        &h1,
        turn_op("install outside dir", ApprovalMode::Bypass, &config),
        LoopApproval::DenyUser,
    )
    .await;
    shutdown_engine(h1, t1).await;

    assert_eq!(deny_outcome.cards.len(), 1);
    assert_eq!(deny_outcome.cards[0].tool_name, "plugin_manager");
    assert!(deny_outcome.cards[0].approval_force_prompt);
    assert!(matches!(
        deny_outcome.tool_results[0].1,
        Err(ToolError::PermissionDenied { .. })
    ));
    assert_eq!(f.discover().list().len(), 0);

    let allow_mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-out-allow",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": outside_dir,
            })
            .to_string(),
        ),
        canned::simple_text_turn("outside approved"),
    ]));
    let (h2, t2) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), true),
        allow_mock,
    );
    let allow_outcome = drive_turn(
        &h2,
        turn_op("install outside dir yes", ApprovalMode::Bypass, &config),
        LoopApproval::ApproveUser,
    )
    .await;
    shutdown_engine(h2, t2).await;

    assert_eq!(allow_outcome.cards.len(), 1);
    assert_eq!(allow_outcome.cards[0].tool_name, "plugin_manager");
    let install_json = parse_tool_json(&allow_outcome.tool_results[0].1);
    assert_eq!(install_json["application"], serde_json::json!("applied"));

    let reg = f.discover();
    let loaded = reg
        .list()
        .into_iter()
        .find(|p| p.id.0.ends_with("my-plugin"))
        .expect("outside plugin installed");
    let entry = read_state_entry(&f.state_path, &loaded.id.0).expect("outside state entry");
    assert_eq!(
        parse_approver(entry["trust"].get("approved_by")),
        Some(PluginApprover::Person {
            via: PersonVia::ApprovalCard,
        })
    );

    f.manager.shutdown().await;
}

#[tokio::test]
async fn standard_mode_finds_plugin_manager_by_search() {
    let f = LoopFixture::new("standard_mode_finds_plugin_manager_by_search", true);

    let mock = Arc::new(MockLlmClient::new(Vec::new()));
    mock.push_turn(canned::simple_text_turn("warm"));
    mock.push_turn(canned::tool_call_turn(
        "call-search-pm",
        "tool_search",
        r#"{"query":"plugin_manager","match":"regex"}"#,
    ));
    mock.push_turn(canned::simple_text_turn("found plugin_manager"));

    let config = Config::default();
    let (handle, task) =
        spawn_interactive(f.engine_config(f.discover(), None, true), Arc::clone(&mock));
    let _ = drive_turn(
        &handle,
        turn_op("warm", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    let mut creator_ready = false;
    for _ in 0..400 {
        if f.manager
            .live_tool_names()
            .iter()
            .any(|name| name == "plugin_manager")
        {
            creator_ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        creator_ready,
        "plugin_manager did not register within timeout; live tools: {:?}",
        f.manager.live_tool_names()
    );
    let outcome = drive_turn(
        &handle,
        turn_op("find plugin_manager", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    shutdown_engine(handle, task).await;

    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 3);
    assert!(find_request_tool(&requests[0], "plugin_manager").is_none());
    assert!(find_request_tool(&requests[1], "plugin_manager").is_none());
    let search_res = outcome.tool_results[0].1.as_ref().expect("tool_search ok");
    assert!(
        search_res.content.contains("plugin_manager"),
        "{}",
        search_res.content
    );
    assert!(
        find_request_tool(&requests[2], "plugin_manager").is_some(),
        "plugin_manager available after tool_search"
    );

    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_mode_is_fixed_after_the_first_turn() {
    let f = LoopFixture::new("creator_mode_is_fixed_after_the_first_turn", true);
    let mut cfg = Config::default();
    cfg.set_feature("plugin_creator", true)
        .expect("enable plugin_creator");
    cfg.set_feature("extension_host", true)
        .expect("enable extension_host");
    let rt_dir = f.temp.path().join("rt");
    let manager = RuntimeThreadManager::open(
        cfg.clone(),
        f.workspace.clone(),
        RuntimeThreadManagerConfig {
            data_dir: rt_dir.join("data"),
            task_data_dir: rt_dir.join("tasks"),
            sessions_dir: Some(rt_dir.join("sessions")),
            max_active_threads: 4,
            mcp_auth_context: Default::default(),
        },
    )
    .expect("open runtime thread manager");

    let create_req: CreateThreadRequest = serde_json::from_value(serde_json::json!({
        "workspace": f.workspace,
        "agent_preset": CREATOR,
    }))
    .expect("create request");
    let thread = manager
        .create_thread(create_req)
        .await
        .expect("create creator thread");

    let mock = Arc::new(MockLlmClient::new(vec![
        canned::simple_text_turn("first turn complete"),
        canned::simple_text_turn("second turn complete"),
    ]));
    let (handle, task) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), true),
        Arc::clone(&mock),
    );
    manager
        .install_test_engine(&thread.id, handle.clone())
        .await
        .expect("install test engine");

    let mut rx = handle.rx_event.write().await;
    let start_req: StartTurnRequest =
        serde_json::from_value(serde_json::json!({ "prompt": "turn one" })).expect("start turn");
    manager
        .start_turn(&thread.id, start_req)
        .await
        .expect("start first turn");
    while let Some(event) = tokio::time::timeout(CREATOR_LOOP_EVENT_TIMEOUT, rx.recv())
        .await
        .expect("wait first turn")
    {
        if let Event::TurnComplete { status, error, .. } = event {
            assert_eq!(status, TurnOutcomeStatus::Completed, "{error:?}");
            break;
        }
    }
    drop(rx);

    let err = manager
        .update_thread_with_shell_policy(
            &thread.id,
            UpdateThreadRequest {
                agent_preset: Some(STANDARD.to_string()),
                ..Default::default()
            },
            None,
            None,
        )
        .await
        .expect_err("mode change after first turn must refuse");
    let refusal = err
        .downcast_ref::<ModeRefusal>()
        .unwrap_or_else(|| panic!("expected ModeRefusal, got {err:?}"));
    assert_eq!(refusal.code, ModeRefusal::MODE_LOCKED);

    let second = drive_turn(
        &handle,
        turn_op("turn two", ApprovalMode::Bypass, &cfg),
        LoopApproval::PanicOnCard,
    )
    .await;
    assert!(second.cards.is_empty());

    let requests = mock.captured_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        request_tool_names(&requests[0]),
        request_tool_names(&requests[1])
    );
    assert!(
        find_request_tool(&requests[0], "plugin_manager")
            .is_some_and(|tool| !tool.defer_loading.unwrap_or(false))
    );
    assert!(
        find_request_tool(&requests[1], "plugin_manager")
            .is_some_and(|tool| !tool.defer_loading.unwrap_or(false))
    );

    shutdown_engine(handle, task).await;
    drop(manager);
    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_with_the_extension_host_switch_off_makes_a_connection_and_says_why_a_tool_does_not_run()
 {
    let f = LoopFixture::new(
        "creator_with_the_extension_host_switch_off_makes_a_connection_and_says_why_a_tool_does_not_run",
        false,
    );
    let (port, server_handle) = spawn_loopback_mcp_server().await;
    let mcp_dir = f.workspace.join("demo-mcp");
    copy_dir_with_mcp_port(&pinned_mcp_template_dir(), &mcp_dir, port);
    let pkg_dir = f.workspace.join("my-plugin");
    write_tool_package(&pkg_dir, "blocked-ext-off");

    let mock = Arc::new(MockLlmClient::new(Vec::new()));
    mock.push_turn(canned::tool_call_turn(
        "call-install-mcp",
        "plugin_manager",
        &serde_json::json!({
            "action": "install_bundle",
            "target": mcp_dir,
        })
        .to_string(),
    ));
    mock.push_turn(canned::simple_text_turn("mcp installed"));
    mock.push_turn(canned::tool_call_turn(
        "call-search-mcp",
        "tool_search",
        r#"{"query":"mcp demo_ping"}"#,
    ));
    mock.push_factory(|req| {
        let tool_name = extract_connector_tool_name(&latest_tool_result_text(req), "demo_ping");
        canned::tool_call_turn("call-mcp-ping", &tool_name, "{}")
    });
    mock.push_turn(canned::simple_text_turn("mcp called"));
    mock.push_turn(canned::tool_call_turn(
        "call-install-tool",
        "plugin_manager",
        &serde_json::json!({
            "action": "install_bundle",
            "target": pkg_dir,
        })
        .to_string(),
    ));
    mock.push_turn(canned::simple_text_turn("native install reported off"));

    let config = Config::default();
    let (handle, task) = spawn_interactive(
        f.engine_config(f.discover(), Some(CREATOR), false),
        Arc::clone(&mock),
    );
    let t1 = drive_turn(
        &handle,
        turn_op("install mcp", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    assert_eq!(
        parse_tool_json(&t1.tool_results[0].1)["application"],
        serde_json::json!("applied")
    );

    let t2 = drive_turn(
        &handle,
        turn_op("call mcp", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    let (_, ping_res) = t2
        .tool_results
        .iter()
        .find(|(name, _)| name.ends_with("demo_ping"))
        .expect("demo_ping result");
    assert!(
        ping_res
            .as_ref()
            .expect("mcp succeeded")
            .content
            .contains("connector-pong")
    );

    let t3 = drive_turn(
        &handle,
        turn_op("install tool pkg", ApprovalMode::Bypass, &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    shutdown_engine(handle, task).await;
    server_handle.abort();

    let tool_install = parse_tool_json(&t3.tool_results[0].1);
    assert_eq!(tool_install["application"], serde_json::json!("failed"));
    let text = tool_install.to_string();
    assert!(
        text.contains(
            "the extension host is switched off in this process, so native plugin code did not start"
        ),
        "{text}"
    );
    assert_eq!(f.manager.tier_spawn_attempts(HostTier::Plugin), 0);
    assert!(f.manager.host_pid().is_none());

    f.manager.shutdown().await;
}

#[tokio::test]
async fn creator_auto_review_reviewer_yes_installs_and_records_the_agent_auto_review() {
    let f = LoopFixture::new(
        "creator_auto_review_reviewer_yes_installs_and_records_the_agent_auto_review",
        true,
    );
    let pkg_dir = f.workspace.join("my-plugin");
    write_tool_package(&pkg_dir, "ar-echo");
    let (_, expected_review_hash) =
        codewhale_plugins::install::preview_dsh(&pkg_dir).expect("preview_dsh");

    let config = Config::default();
    let mock = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "call-ar-install",
            "plugin_manager",
            &serde_json::json!({
                "action": "install_bundle",
                "target": pkg_dir,
            })
            .to_string(),
        ),
        canned::tool_call_turn("call-ar-echo", "loop_echo", r#"{"note":"ar-ok"}"#),
        canned::simple_text_turn("auto review done"),
    ]));
    mock.push_message_response(guardian_fixture_response(
        r#"{"risk_level":"low","decision":"allow","reason":"clean workspace tool bundle"}"#,
    ));
    mock.push_message_response(guardian_fixture_response(
        r#"{"risk_level":"low","decision":"allow","reason":"safe echo call"}"#,
    ));

    let mut engine_cfg = f.engine_config(f.discover(), Some(CREATOR), true);
    engine_cfg.auto_review_policy = AutoReviewPolicy::from_config(&config);
    let (handle, task) = spawn_interactive(engine_cfg, Arc::clone(&mock));
    let outcome = drive_turn(
        &handle,
        auto_review_message_op("install and run under auto review", &config),
        LoopApproval::PanicOnCard,
    )
    .await;
    shutdown_engine(handle, task).await;

    assert!(outcome.cards.is_empty());
    let install_json = parse_tool_json(&outcome.tool_results[0].1);
    assert_eq!(install_json["application"], serde_json::json!("applied"));
    let echo_ok = outcome.tool_results[1].1.as_ref().expect("loop_echo ok");
    assert!(echo_ok.content.contains("ar-echo"), "{}", echo_ok.content);
    assert!(echo_ok.content.contains("ar-ok"), "{}", echo_ok.content);

    let reg = f.discover();
    let loaded = reg
        .list()
        .into_iter()
        .find(|p| p.id.0.ends_with("my-plugin"))
        .expect("installed plugin");
    let reviewer_saw_hash = mock.captured_requests().iter().any(|req| {
        serde_json::to_string(req)
            .is_ok_and(|raw| raw.contains("plugin_review") && raw.contains(&expected_review_hash))
    });
    assert!(
        reviewer_saw_hash,
        "expected content hash in reviewer request"
    );

    let entry = read_state_entry(&f.state_path, &loaded.id.0).expect("state entry");
    match parse_approver(entry["trust"].get("approved_by")) {
        Some(PluginApprover::Agent(agent)) => {
            assert_eq!(agent.posture, AgentPosture::AutoReview);
            let install_exec_id = &outcome
                .tool_exec_ids
                .iter()
                .find(|(name, _)| name == "plugin_manager")
                .expect("plugin_manager execution id")
                .1;
            assert!(
                uuid::Uuid::parse_str(install_exec_id).is_ok(),
                "{install_exec_id}"
            );
            assert_eq!(
                agent.tool_call_id.as_deref(),
                Some(install_exec_id.as_str())
            );
        }
        other => panic!("expected Agent AutoReview approval, got {other:?}"),
    }

    f.manager.shutdown().await;
}

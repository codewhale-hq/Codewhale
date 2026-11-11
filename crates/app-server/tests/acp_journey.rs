mod acp_support;

use acp_support::{Client, fixture_config, text_of, updates};
use anyhow::{Result, anyhow};
use codewhale_app_server::acp_server::capture_frontend;
use codewhale_engine::llm_client::mock::{MockLlmClient, canned};
use codewhale_engine::runtime_threads::RuntimeTurnStatus;
use codewhale_engine::runtime_threads::manager::{
    RuntimeThreadManager, RuntimeThreadManagerConfig,
};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};

#[test]
fn editor_drives_a_session_through_permission_cancel_and_load() -> Result<()> {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(120), journey())
                        .await
                        .map_err(|_| anyhow!("journey exceeded 120s"))?
                })
        })?
        .join()
        .map_err(|_| anyhow!("journey thread panicked"))?
}

async fn journey() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let _home = codewhale_engine::test_support::SealedHome::at(dir.path());
    let workspace = dir.path().join("workspace");
    std::fs::create_dir(&workspace)?;
    let runtime_dir = dir.path().join("runtime");
    let config = fixture_config();
    let plugins = Arc::new(codewhale_plugins::PluginRegistry::empty(&workspace));
    let manager = Arc::new(RuntimeThreadManager::open_host(
        config.clone(),
        workspace.clone(),
        RuntimeThreadManagerConfig {
            mcp_auth_context: Default::default(),
            data_dir: runtime_dir.clone(),
            task_data_dir: runtime_dir,
            sessions_dir: None,
            max_active_threads: 4,
        },
        plugins,
        codewhale_engine::core::engine::EngineHostProfile::Acp,
        None,
        None,
    )?);
    let provider = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn(
            "provider-id-not-authority",
            "write",
            r#"{"path":"allowed.txt","content":"approved"}"#,
        ),
        canned::simple_text_turn("settled after approval"),
        canned::tool_call_turn(
            "provider-id-two",
            "write",
            r#"{"path":"cancelled.txt","content":"never"}"#,
        ),
        canned::simple_text_turn("resumed in the real session"),
    ]));
    manager.set_test_model_client(provider.clone());
    let sessions_dir = codewhale_engine::session_manager::default_sessions_dir()?;
    let frontend = capture_frontend(
        config,
        "fixture-model".into(),
        workspace.clone(),
        Arc::clone(&manager),
        sessions_dir,
        None,
        None,
    )?;

    let (mut client, agent_read, agent_write) = Client::connect(false);
    let first_connection =
        frontend.serve(Box::new(BufReader::new(agent_read)), Box::new(agent_write));
    let first_script = async {
        let result = async {
        let (initialized, _) = client
            .request(
                "initialize",
                json!({"protocolVersion":1,"clientCapabilities":{"terminal":false}}),
            )
            .await?;
        let capabilities = &initialized["result"]["agentCapabilities"];
        assert_eq!(initialized["result"]["protocolVersion"], 1);
        assert_eq!(capabilities["loadSession"], true);
        assert_eq!(capabilities["promptCapabilities"]["image"], false);
        assert_eq!(capabilities["promptCapabilities"]["audio"], false);
        assert_eq!(capabilities["mcpCapabilities"]["http"], false);
        assert_eq!(capabilities["mcpCapabilities"]["sse"], false);
        assert!(capabilities["sessionCapabilities"]["list"].is_object());
        assert!(capabilities["sessionCapabilities"].get("resume").is_none());
        let (relative, _) = client
            .request("session/new", json!({"cwd":"relative/dir"}))
            .await?;
        assert_eq!(relative["error"]["code"], -32602);
        let (mcp, _) = client
            .request(
                "session/new",
                json!({"cwd":workspace,"mcpServers":[{"name":"x","command":"x","args":[],"env":[]}]}),
            )
            .await?;
        assert_eq!(mcp["error"]["code"], -32602);
        let (resume, _) = client
            .request("session/resume", json!({"sessionId":"x","cwd":workspace}))
            .await?;
        assert_eq!(resume["error"]["code"], -32601);
        let (created, _) = client
            .request("session/new", json!({"cwd":workspace}))
            .await?;
        let session = created["result"]["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow!("session/new returned no sessionId"))?
            .to_string();
        let (image, _) = client
            .request(
                "session/prompt",
                json!({"sessionId":session,"prompt":[
                    {"type":"text","text":"look"},
                    {"type":"image","data":"AAAA","mimeType":"image/png"}
                ]}),
            )
            .await?;
        assert_eq!(image["error"]["code"], -32602);
        assert_eq!(
            provider.call_count(),
            0,
            "a refused prompt never reaches the provider"
        );
        client.next_id += 1;
        let prompt_id = client.next_id;
        client
            .send(
                json!({"jsonrpc":"2.0","id":prompt_id,"method":"session/prompt","params":{
                "sessionId":session,"prompt":[{"type":"text","text":"write under Ask"}]}}),
            )
            .await?;
        let (permission, before) = client.until_permission().await?;
        assert_eq!(permission["params"]["sessionId"], session.as_str());
        assert_eq!(permission["params"]["toolCall"]["kind"], "edit");
        let kinds: Vec<_> = permission["params"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .map(|option| option["kind"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(kinds, ["allow_once", "reject_once"]);
        assert!(
            !updates(&before, "tool_call").is_empty(),
            "the tool call is announced before approval is requested"
        );
        assert!(
            updates(&before, "tool_call_update")
                .iter()
                .all(|frame| frame.pointer("/params/update/status") != Some(&json!("in_progress"))),
            "nothing dispatches before the person decides"
        );
        assert!(!workspace.join("allowed.txt").exists());
        client
            .send(json!({"jsonrpc":"2.0","id":permission["id"],
                "result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}}))
            .await?;
        let (done, after) = client.until_response(prompt_id).await?;
        assert_eq!(done["result"]["stopReason"], "end_turn");
        assert_eq!(
            std::fs::read_to_string(workspace.join("allowed.txt"))?,
            "approved"
        );
        assert!(
            updates(&after, "tool_call_update")
                .iter()
                .any(|frame| frame.pointer("/params/update/status") == Some(&json!("completed")))
        );
        assert_eq!(
            text_of(&after, "agent_message_chunk"),
            "settled after approval"
        );
        client.next_id += 1;
        let cancel_id = client.next_id;
        client
            .send(
                json!({"jsonrpc":"2.0","id":cancel_id,"method":"session/prompt","params":{
                "sessionId":session,"prompt":"write then cancel"}}),
            )
            .await?;
        let (second, _) = client.until_permission().await?;
        client
            .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":session}}))
            .await?;
        client
            .send(json!({"jsonrpc":"2.0","id":second["id"],
                "result":{"outcome":{"outcome":"cancelled"}}}))
            .await?;
        let (cancelled, _) = client.until_response(cancel_id).await?;
        assert_eq!(cancelled["result"]["stopReason"], "cancelled");
        assert!(!workspace.join("cancelled.txt").exists());
        let (listed, _) = client
            .request("session/list", json!({"cwd":workspace}))
            .await?;
        assert!(
            listed["result"]["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["sessionId"] == session.as_str())
        );
        client.request("shutdown", json!({})).await?;
        Ok::<_, anyhow::Error>(session)
    }
        .await;
        if result.is_err() {
            let _ = client.write.shutdown().await;
        }
        result
    };
    let (served, session) = tokio::join!(first_connection, first_script);
    served?;
    let session = session?;

    let thread = manager
        .list_threads(Default::default(), None)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no runtime thread was created"))?;
    let detail = manager.get_thread_detail(&thread.id).await?;
    assert!(
        detail.pending_approvals.is_empty(),
        "cancel settles the approval"
    );
    assert_eq!(detail.turns.len(), 2);
    assert_eq!(detail.turns[0].status, RuntimeTurnStatus::Completed);
    assert_eq!(detail.turns[1].status, RuntimeTurnStatus::Interrupted);

    let (mut client, agent_read, agent_write) = Client::connect(false);
    let second_connection =
        frontend.serve(Box::new(BufReader::new(agent_read)), Box::new(agent_write));
    let second_script = async {
        client
            .request(
                "initialize",
                json!({"protocolVersion":1,"clientCapabilities":{}}),
            )
            .await?;
        let (missing, _) = client
            .request(
                "session/load",
                json!({"sessionId":"no-such-session","cwd":workspace,"mcpServers":[]}),
            )
            .await?;
        assert_eq!(missing["error"]["code"], -32602);
        let (loaded, replay) = client
            .request(
                "session/load",
                json!({"sessionId":session,"cwd":workspace,"mcpServers":[]}),
            )
            .await?;
        assert_eq!(loaded["result"]["sessionId"], session.as_str());
        assert!(text_of(&replay, "user_message_chunk").contains("write under Ask"));
        assert!(text_of(&replay, "agent_message_chunk").contains("settled after approval"));
        assert!(!updates(&replay, "tool_call").is_empty());
        let (resumed, frames) = client
            .request(
                "session/prompt",
                json!({"sessionId":session,"prompt":"continue"}),
            )
            .await?;
        assert_eq!(resumed["result"]["stopReason"], "end_turn");
        assert_eq!(
            text_of(&frames, "agent_message_chunk"),
            "resumed in the real session"
        );
        client.request("shutdown", json!({})).await?;
        Ok::<_, anyhow::Error>(())
    };
    let (served, script) = tokio::join!(second_connection, second_script);
    served?;
    script?;

    let requests = provider.captured_requests();
    let last = serde_json::to_string(requests.last().ok_or_else(|| anyhow!("no model request"))?)?;
    assert!(
        last.contains("write under Ask") && last.contains("settled after approval"),
        "the loaded session continues from its real history"
    );
    manager.shutdown_and_wait().await?;
    Ok(())
}

const MCP_FIXTURE: &str = r#"import readline from 'node:readline';
readline.createInterface({input:process.stdin}).on('line', line => {
  const r = JSON.parse(line); if (r.id === undefined) return;
  let result = {};
  if (r.method === 'initialize') result = {protocolVersion:'2024-11-05',capabilities:{tools:{}},serverInfo:{name:'fixture',version:'1'}};
  if (r.method === 'tools/list') result = {tools:[{name:'echo',description:'Echo text',inputSchema:{type:'object',properties:{text:{type:'string'}},required:['text']}}]};
  if (r.method === 'tools/call') result = {content:[{type:'text',text:'mcp-echo:' + r.params.arguments.text}]};
  process.stdout.write(JSON.stringify({jsonrpc:'2.0',id:r.id,result}) + '\n');
});
"#;

#[test]
fn plugin_and_mcp_tools_run_as_acp_tool_calls_through_the_permission_dial() -> Result<()> {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(120), extension_journey())
                        .await
                        .map_err(|_| anyhow!("extension journey exceeded 120s"))?
                })
        })?
        .join()
        .map_err(|_| anyhow!("extension journey thread panicked"))?
}

async fn extension_journey() -> Result<()> {
    use codewhale_engine::extension_host::test_support::{FixturePlugins, node_for_tests};
    let Some(node) = node_for_tests("plugin_and_mcp_tools_run_as_acp_tool_calls") else {
        return Ok(());
    };
    let _policy = codewhale_plugins::activation::TestPolicyGuard::extension_host(true);
    let fixture = FixturePlugins::new(&["plugin-context"]).await;
    let host = fixture.manager(node.clone());
    let _host = codewhale_engine::extension_host::TestManagerGuard::install(Arc::clone(&host));
    let dir = tempfile::tempdir()?;
    let _home = codewhale_engine::test_support::SealedHome::at(dir.path());
    let workspace = fixture.workspace().to_path_buf();
    let server = dir.path().join("fixture-mcp.mjs");
    std::fs::write(&server, MCP_FIXTURE)?;
    let mcp_config = dir.path().join("mcp.json");
    std::fs::write(
        &mcp_config,
        serde_json::to_vec(&json!({"servers":{"fixture":{"command":node,"args":[server]}}}))?,
    )?;
    let runtime_dir = dir.path().join("runtime");
    let mut config = fixture_config();
    config.set_feature("mcp", true).unwrap();
    config.set_feature("extension_host", true).unwrap();
    config.mcp_config_path = Some(mcp_config.to_string_lossy().into_owned());
    let manager = Arc::new(RuntimeThreadManager::open_host(
        config.clone(),
        workspace.clone(),
        RuntimeThreadManagerConfig {
            mcp_auth_context: Default::default(),
            data_dir: runtime_dir.clone(),
            task_data_dir: runtime_dir,
            sessions_dir: None,
            max_active_threads: 4,
        },
        fixture.registry(),
        codewhale_engine::core::engine::EngineHostProfile::Acp,
        None,
        None,
    )?);
    let provider = Arc::new(MockLlmClient::new(vec![
        canned::tool_call_turn("plugin-call", "ctx_probe", "{}"),
        canned::tool_call_turn("mcp-call", "mcp_fixture_echo", r#"{"text":"from-acp"}"#),
        canned::simple_text_turn("both extension tools ran"),
    ]));
    manager.set_test_model_client(provider.clone());
    let frontend = capture_frontend(
        config,
        "fixture-model".into(),
        workspace.clone(),
        Arc::clone(&manager),
        codewhale_engine::session_manager::default_sessions_dir()?,
        None,
        None,
    )?;
    let (mut client, agent_read, agent_write) = Client::connect(false);
    let connection = frontend.serve(Box::new(BufReader::new(agent_read)), Box::new(agent_write));
    let script = async {
        let result = async {
            let (initialized, _) = client
                .request(
                    "initialize",
                    json!({"protocolVersion":1,"clientCapabilities":{}}),
                )
                .await?;
            let meta = &initialized["result"]["agentCapabilities"]["_meta"]["codewhale"];
            assert_eq!(meta["plugins"], true);
            assert_eq!(meta["configuredMcpServers"], true);
            assert_eq!(meta["clientMcpServers"], false);
            let (created, _) = client
                .request("session/new", json!({"cwd":workspace}))
                .await?;
            let session = created["result"]["sessionId"]
                .as_str()
                .ok_or_else(|| anyhow!("session/new returned no sessionId"))?
                .to_string();
            client.next_id += 1;
            let prompt_id = client.next_id;
            client
                .send(json!({"jsonrpc":"2.0","id":prompt_id,"method":"session/prompt","params":{
                    "sessionId":session,"prompt":[{"type":"text","text":"use both extension tools"}]}}))
                .await?;
            let mut asked = Vec::new();
            let mut frames = Vec::new();
            let done = loop {
                let frame = client.recv().await?;
                if frame["method"] == "session/request_permission" {
                    asked.push(
                        frame["params"]["toolCall"]["title"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    );
                    client
                        .send(json!({"jsonrpc":"2.0","id":frame["id"],
                            "result":{"outcome":{"outcome":"selected","optionId":"allow-once"}}}))
                        .await?;
                } else if frame["id"] == json!(prompt_id) && frame.get("method").is_none() {
                    break frame;
                } else {
                    frames.push(frame);
                }
            };
            assert_eq!(done["result"]["stopReason"], "end_turn");
            assert!(
                asked.iter().any(|title| title.starts_with("ctx_probe")),
                "the plugin tool asks through the permission dial: {asked:?} {frames:#?}"
            );
            assert!(
                asked
                    .iter()
                    .any(|title| title.starts_with("mcp_fixture_echo")),
                "the MCP tool asks through the permission dial: {asked:?}"
            );
            let announced: Vec<_> = updates(&frames, "tool_call")
                .iter()
                .filter_map(|frame| frame.pointer("/params/update/title")?.as_str())
                .map(str::to_string)
                .collect();
            assert!(announced.iter().any(|title| title == "ctx_probe"), "{announced:?}");
            assert!(
                announced.iter().any(|title| title == "mcp_fixture_echo"),
                "{announced:?}"
            );
            let completed = updates(&frames, "tool_call_update")
                .iter()
                .filter(|frame| {
                    frame.pointer("/params/update/status") == Some(&json!("completed"))
                })
                .count();
            assert_eq!(completed, 2, "both extension calls complete: {frames:#?}");
            let rendered = serde_json::to_string(&frames)?;
            assert!(rendered.contains("mcp-echo:from-acp"), "{rendered}");
            let probed = updates(&frames, "tool_call_update")
                .iter()
                .filter_map(|frame| frame.pointer("/params/update/content/0/content/text"))
                .filter_map(Value::as_str)
                .filter_map(|text| serde_json::from_str::<Value>(&text[text.find('{')?..]).ok())
                .find_map(|result| Some(PathBuf::from(result.get("workspace")?.as_str()?)))
                .ok_or_else(|| anyhow!("the plugin result did not reach the editor: {rendered}"))?;
            assert_eq!(probed.canonicalize()?, workspace.canonicalize()?);
            assert_eq!(
                text_of(&frames, "agent_message_chunk"),
                "both extension tools ran"
            );
            client.request("shutdown", json!({})).await?;
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if result.is_err() {
            let _ = client.write.shutdown().await;
        }
        result
    };
    let (served, scripted) = tokio::join!(connection, script);
    served?;
    scripted?;
    let first = serde_json::to_string(
        provider
            .captured_requests()
            .first()
            .ok_or_else(|| anyhow!("no model request"))?,
    )?;
    assert!(
        first.contains("ctx_probe") && first.contains("mcp_fixture_echo"),
        "the first model request carries both extension tools"
    );
    manager.shutdown_and_wait().await?;
    host.shutdown().await;
    Ok(())
}

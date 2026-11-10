use anyhow::{Result, anyhow};
use codewhale_app_server::acp_server::capture_frontend;
use codewhale_engine::llm_client::mock::{MockLlmClient, canned};
use codewhale_engine::runtime_threads::RuntimeTurnStatus;
use codewhale_engine::runtime_threads::manager::{
    RuntimeThreadManager, RuntimeThreadManagerConfig,
};
use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig, SnapshotsConfig};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

const STEP: Duration = Duration::from_secs(30);

fn fixture_config() -> Config {
    let mut config = Config::default();
    config.provider = Some("acp-fixture".into());
    config.providers = Some(ProvidersConfig {
        custom: HashMap::from([(
            "acp-fixture".into(),
            ProviderConfig {
                kind: Some("openai-compatible".into()),
                base_url: Some("http://127.0.0.1:18181/v1".into()),
                model: Some("fixture-model".into()),
                api_key: Some("local-test-key".into()),
                ..Default::default()
            },
        )]),
        ..Default::default()
    });
    config.snapshots = Some(SnapshotsConfig {
        enabled: false,
        ..Default::default()
    });
    config.set_feature("mcp", false).unwrap();
    config.set_feature("subagents", false).unwrap();
    config
}

struct Client {
    lines: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    write: WriteHalf<DuplexStream>,
    next_id: u64,
}

impl Client {
    async fn send(&mut self, message: Value) -> Result<()> {
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        self.write.write_all(&line).await?;
        self.write.flush().await?;
        Ok(())
    }
    async fn recv(&mut self) -> Result<Value> {
        let line = tokio::time::timeout(STEP, self.lines.next_line())
            .await
            .map_err(|_| anyhow!("agent produced no frame within {STEP:?}"))??
            .ok_or_else(|| anyhow!("agent closed the transport"))?;
        Ok(serde_json::from_str(&line)?)
    }
    async fn request(&mut self, method: &str, params: Value) -> Result<(Value, Vec<Value>)> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        let mut frames = Vec::new();
        loop {
            let frame = self.recv().await?;
            if frame["id"] == json!(id) && frame.get("method").is_none() {
                return Ok((frame, frames));
            }
            frames.push(frame);
        }
    }
    async fn until_permission(&mut self) -> Result<(Value, Vec<Value>)> {
        let mut frames = Vec::new();
        loop {
            let frame = self.recv().await?;
            if frame["method"] == "session/request_permission" {
                return Ok((frame, frames));
            }
            frames.push(frame);
        }
    }
    async fn until_response(&mut self, id: u64) -> Result<(Value, Vec<Value>)> {
        let mut frames = Vec::new();
        loop {
            let frame = self.recv().await?;
            if frame["id"] == json!(id) && frame.get("method").is_none() {
                return Ok((frame, frames));
            }
            frames.push(frame);
        }
    }
}

fn updates<'a>(frames: &'a [Value], kind: &str) -> Vec<&'a Value> {
    frames
        .iter()
        .filter(|frame| frame.pointer("/params/update/sessionUpdate") == Some(&json!(kind)))
        .collect()
}

fn text_of(frames: &[Value], kind: &str) -> String {
    updates(frames, kind)
        .iter()
        .filter_map(|frame| frame.pointer("/params/update/content/text")?.as_str())
        .collect()
}

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

    let connect = || {
        let (client, agent) = tokio::io::duplex(1 << 20);
        let (agent_read, agent_write) = tokio::io::split(agent);
        let (client_read, client_write) = tokio::io::split(client);
        (
            Client {
                lines: BufReader::new(client_read).lines(),
                write: client_write,
                next_id: 0,
            },
            agent_read,
            agent_write,
        )
    };

    let (mut client, agent_read, agent_write) = connect();
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

    let (mut client, agent_read, agent_write) = connect();
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

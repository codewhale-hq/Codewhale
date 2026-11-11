mod acp_support;
#[path = "../../engine/tests/support/acp_conformance.rs"]
mod conformance;

use acp_support::{Client, fixture_config, text_of, updates};
use agent_client_protocol_schema::ProtocolVersion;
use agent_client_protocol_schema::v1 as acp;
use anyhow::{Result, anyhow};
use codewhale_app_server::acp_server::capture_frontend;
use codewhale_engine::llm_client::mock::{MockLlmClient, canned};
use codewhale_engine::runtime_threads::manager::{
    RuntimeThreadManager, RuntimeThreadManagerConfig,
};
use conformance::{Allowed, Conformance, Side};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncWriteExt, BufReader};

const KNOWN: &[Allowed] = &[Allowed {
    side: Side::Agent,
    frame: "session/load response",
    problem: "field `.sessionId` is not in the official type",
    reason: "session/load keeps the session/new body with its root sessionId through the 0.10.3 tag; official LoadSessionResponse has no sessionId",
}];

fn wire<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).expect("official type serializes")
}

async fn prompt_to_end(
    client: &mut Client,
    params: Value,
    answer: Value,
) -> Result<(Value, Vec<Value>)> {
    client.next_id += 1;
    let id = client.next_id;
    client
        .send(json!({"jsonrpc":"2.0","id":id,"method":"session/prompt","params":params}))
        .await?;
    let mut frames = Vec::new();
    loop {
        let frame = client.recv().await?;
        if frame["id"] == json!(id) && frame.get("method").is_none() {
            return Ok((frame, frames));
        }
        if frame["method"] == "session/request_permission" {
            client
                .send(json!({"jsonrpc":"2.0","id":frame["id"],"result":answer}))
                .await?;
        }
        frames.push(frame);
    }
}

#[test]
fn every_frame_the_acp_server_writes_is_an_official_v1_shape() -> Result<()> {
    std::thread::Builder::new()
        .stack_size(64 << 20)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(120), journey())
                        .await
                        .map_err(|_| anyhow!("conformance journey exceeded 120s"))?
                })
        })?
        .join()
        .map_err(|_| anyhow!("conformance journey thread panicked"))?
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
            "plan-call",
            "todo_write",
            r#"{"todos":[{"content":"write the file","status":"in_progress"}]}"#,
        ),
        canned::tool_call_turn(
            "write-call",
            "write",
            r#"{"path":"allowed.txt","content":"approved"}"#,
        ),
        canned::simple_text_turn("settled after approval"),
        canned::tool_call_turn(
            "cancel-call",
            "write",
            r#"{"path":"cancelled.txt","content":"never"}"#,
        ),
        canned::simple_text_turn("unused"),
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
    let mut observed = Conformance::checking(&[Side::Agent]);

    let (mut client, agent_read, agent_write) = Client::connect(true);
    let first_connection =
        frontend.serve(Box::new(BufReader::new(agent_read)), Box::new(agent_write));
    let first_script = async {
        let result = async {
            let (initialized, _) = client
                .request(
                    "initialize",
                    wire(
                        acp::InitializeRequest::new(ProtocolVersion::V1)
                            .client_capabilities(
                                acp::ClientCapabilities::new()
                                    .fs(acp::FileSystemCapabilities::new())
                                    .terminal(false),
                            )
                            .client_info(acp::Implementation::new("conformance", "1.0.0")),
                    ),
                )
                .await?;
            assert_eq!(initialized["result"]["protocolVersion"], 1);
            client
                .send(json!({"jsonrpc":"2.0","method":"$/cancel_request","params":wire(acp::CancelRequestNotification::new(999_i64))}))
                .await?;
            let (unknown, _) = client.request("session/not_a_method", json!({})).await?;
            assert_eq!(unknown["error"]["code"], -32601);
            let (invalid, _) = client
                .request("session/set_mode", json!({"modeId":"agent"}))
                .await?;
            assert_eq!(invalid["error"]["code"], -32602);
            let (relative, _) = client
                .request("session/new", wire(acp::NewSessionRequest::new("relative/dir")))
                .await?;
            assert_eq!(relative["error"]["code"], -32602);
            let (created, _) = client
                .request("session/new", wire(acp::NewSessionRequest::new(&workspace)))
                .await?;
            let session = created["result"]["sessionId"]
                .as_str()
                .ok_or_else(|| anyhow!("session/new returned no sessionId"))?
                .to_string();
            let (mode, _) = client
                .request(
                    "session/set_mode",
                    wire(acp::SetSessionModeRequest::new(session.clone(), "agent")),
                )
                .await?;
            assert!(mode.get("result").is_some(), "{mode}");
            let (option, _) = client
                .request(
                    "session/set_config_option",
                    wire(acp::SetSessionConfigOptionRequest::new(
                        session.clone(),
                        "model",
                        "fixture-model",
                    )),
                )
                .await?;
            assert!(option.get("result").is_some(), "{option}");
            let (image, _) = client
                .request(
                    "session/prompt",
                    wire(acp::PromptRequest::new(
                        session.clone(),
                        vec![acp::ContentBlock::Image(acp::ImageContent::new(
                            "AAAA",
                            "image/png",
                        ))],
                    )),
                )
                .await?;
            assert_eq!(image["error"]["code"], -32602);
            let allow = wire(acp::RequestPermissionResponse::new(
                acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(
                    "allow-once",
                )),
            ));
            let (done, frames) = prompt_to_end(
                &mut client,
                wire(acp::PromptRequest::new(
                    session.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new(
                        "plan, then write under Ask",
                    ))],
                )),
                allow,
            )
            .await?;
            assert_eq!(done["result"]["stopReason"], "end_turn");
            assert!(!updates(&frames, "tool_call").is_empty());
            assert_eq!(text_of(&frames, "agent_message_chunk"), "settled after approval");

            client.next_id += 1;
            let cancel_id = client.next_id;
            client
                .send(json!({"jsonrpc":"2.0","id":cancel_id,"method":"session/prompt","params":wire(acp::PromptRequest::new(
                    session.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("write then cancel"))],
                ))}))
                .await?;
            let (permission, _) = client.until_permission().await?;
            client
                .send(json!({"jsonrpc":"2.0","method":"session/cancel","params":wire(acp::CancelNotification::new(session.clone()))}))
                .await?;
            client
                .send(json!({"jsonrpc":"2.0","id":permission["id"],"result":wire(acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled))}))
                .await?;
            let (cancelled, _) = client.until_response(cancel_id).await?;
            assert_eq!(cancelled["result"]["stopReason"], "cancelled");
            let (listed, _) = client
                .request(
                    "session/list",
                    wire(acp::ListSessionsRequest::new().cwd(workspace.clone())),
                )
                .await?;
            assert!(listed["result"]["sessions"].is_array());
            Ok::<_, anyhow::Error>(session)
        }
        .await;
        let _ = client.write.shutdown().await;
        result
    };
    let (served, session) = tokio::join!(first_connection, first_script);
    served?;
    let session = session?;
    for (sent, frame) in client.log.take().unwrap_or_default() {
        observed.observe(if sent { Side::Client } else { Side::Agent }, &frame);
    }

    let (mut client, agent_read, agent_write) = Client::connect(true);
    let second_connection =
        frontend.serve(Box::new(BufReader::new(agent_read)), Box::new(agent_write));
    let second_script = async {
        let result = async {
            client
                .request(
                    "initialize",
                    wire(acp::InitializeRequest::new(ProtocolVersion::V1)),
                )
                .await?;
            let (missing, _) = client
                .request(
                    "session/load",
                    wire(acp::LoadSessionRequest::new("no-such-session", &workspace)),
                )
                .await?;
            assert_eq!(missing["error"]["code"], -32602);
            let (loaded, replay) = client
                .request(
                    "session/load",
                    wire(acp::LoadSessionRequest::new(session.clone(), &workspace)),
                )
                .await?;
            assert!(loaded.get("result").is_some(), "{loaded}");
            assert!(text_of(&replay, "user_message_chunk").contains("plan, then write under Ask"));
            assert!(!updates(&replay, "tool_call_update").is_empty());
            Ok::<_, anyhow::Error>(())
        }
        .await;
        let _ = client.write.shutdown().await;
        result
    };
    let (served, script) = tokio::join!(second_connection, second_script);
    served?;
    script?;
    for (sent, frame) in client.log.take().unwrap_or_default() {
        observed.observe(if sent { Side::Client } else { Side::Agent }, &frame);
    }
    manager.shutdown_and_wait().await?;

    assert!(
        observed.frames() > 20,
        "only {} frames observed",
        observed.frames()
    );
    observed.assert_only(KNOWN);
    Ok(())
}

use anyhow::{Result, anyhow};
use codewhale_runtime::config::{Config, ProviderConfig, ProvidersConfig, SnapshotsConfig};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

pub const STEP: Duration = Duration::from_secs(30);

pub fn fixture_config() -> Config {
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

pub struct Client {
    pub lines: tokio::io::Lines<BufReader<ReadHalf<DuplexStream>>>,
    pub write: WriteHalf<DuplexStream>,
    pub next_id: u64,
    pub log: Option<Vec<(bool, Value)>>,
}

impl Client {
    pub fn connect(record: bool) -> (Self, ReadHalf<DuplexStream>, WriteHalf<DuplexStream>) {
        let (client, agent) = tokio::io::duplex(1 << 20);
        let (agent_read, agent_write) = tokio::io::split(agent);
        let (client_read, client_write) = tokio::io::split(client);
        (
            Client {
                lines: BufReader::new(client_read).lines(),
                write: client_write,
                next_id: 0,
                log: record.then(Vec::new),
            },
            agent_read,
            agent_write,
        )
    }
    pub async fn send(&mut self, message: Value) -> Result<()> {
        let mut line = serde_json::to_vec(&message)?;
        line.push(b'\n');
        self.write.write_all(&line).await?;
        self.write.flush().await?;
        if let Some(log) = &mut self.log {
            log.push((true, message));
        }
        Ok(())
    }
    pub async fn recv(&mut self) -> Result<Value> {
        let line = tokio::time::timeout(STEP, self.lines.next_line())
            .await
            .map_err(|_| anyhow!("agent produced no frame within {STEP:?}"))??
            .ok_or_else(|| anyhow!("agent closed the transport"))?;
        let frame: Value = serde_json::from_str(&line)?;
        if let Some(log) = &mut self.log {
            log.push((false, frame.clone()));
        }
        Ok(frame)
    }
    pub async fn request(&mut self, method: &str, params: Value) -> Result<(Value, Vec<Value>)> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        self.until_response(id).await
    }
    pub async fn until_permission(&mut self) -> Result<(Value, Vec<Value>)> {
        let mut frames = Vec::new();
        loop {
            let frame = self.recv().await?;
            if frame["method"] == "session/request_permission" {
                return Ok((frame, frames));
            }
            frames.push(frame);
        }
    }
    pub async fn until_response(&mut self, id: u64) -> Result<(Value, Vec<Value>)> {
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

pub fn updates<'a>(frames: &'a [Value], kind: &str) -> Vec<&'a Value> {
    frames
        .iter()
        .filter(|frame| frame.pointer("/params/update/sessionUpdate") == Some(&json!(kind)))
        .collect()
}

pub fn text_of(frames: &[Value], kind: &str) -> String {
    updates(frames, kind)
        .iter()
        .filter_map(|frame| frame.pointer("/params/update/content/text")?.as_str())
        .collect()
}

use std::path::Path;
use std::time::Duration;

use axum::Json;
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::task::JoinSet;

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_PROBE_LINE_BYTES: usize = 256 * 1024;
const MAX_TEXT_CHARS: usize = 80;
const MAX_AUTH_METHODS: usize = 8;

use codewhale_engine::fleet::agents::{
    AGENTS, ExecutorUnavailable, ExternalAgent, below_minimum, cli_version, foreign_command,
    resolve,
};

fn clean(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_TEXT_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

async fn acp_initialize(path: &Path, args: &[&str]) -> Option<Value> {
    let mut child = foreign_command(path, args).spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let request = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false
            },
            "clientInfo": {
                "name": "codewhale",
                "title": "Codewhale",
                "version": env!("CARGO_PKG_VERSION")
            }
        }
    });
    let mut line = request.to_string();
    line.push('\n');
    stdin.write_all(line.as_bytes()).await.ok()?;
    stdin.flush().await.ok()?;
    let mut lines = crate::stdio::BoundedLines::new(BufReader::new(stdout));
    let reply = tokio::time::timeout(PROBE_TIMEOUT, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.len() > MAX_PROBE_LINE_BYTES {
                return None;
            }
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return None;
            }
            if message.get("id") == Some(&json!(1)) {
                let result = message.get("result")?;
                return (result.get("protocolVersion").and_then(Value::as_u64) == Some(1))
                    .then(|| result.clone());
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    drop(stdin);
    let _ = child.start_kill();
    let _ = child.wait().await;
    reply
}

fn auth_methods(result: &Value) -> Vec<Value> {
    result
        .get("authMethods")
        .and_then(Value::as_array)
        .map(|methods| {
            methods
                .iter()
                .take(MAX_AUTH_METHODS)
                .filter_map(|method| {
                    let id = clean(method.get("id")?.as_str()?);
                    let name = method
                        .get("name")
                        .and_then(Value::as_str)
                        .map_or_else(|| id.clone(), clean);
                    let kind = method
                        .get("type")
                        .and_then(Value::as_str)
                        .map_or("agent", clean_kind);
                    Some(json!({ "id": id, "name": name, "type": kind }))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn clean_kind(kind: &str) -> &'static str {
    if kind == "terminal" {
        "terminal"
    } else {
        "agent"
    }
}

async fn describe(agent: &'static ExternalAgent) -> Value {
    let Some(path) = resolve(agent.binary).await else {
        return json!({
            "id": agent.id,
            "name": agent.name,
            "state": "not_installed",
            "version": null,
            "acp": null,
            "auth_methods": [],
            "website": agent.website,
            "min_version": agent.min_version,
            "upgrade": agent.upgrade,
            "login": agent.login,
            "adapter": agent.adapter,
            "adapter_installed": false,
            "needs_update": false,
            "runnable": false,
            "depth": null,
            "auth_status": "unknown",
            "reason_code": ExecutorUnavailable::NOT_INSTALLED,
            "reason": format!(
                "{} is not installed.{}",
                agent.name,
                agent
                    .upgrade
                    .map(|command| format!(" Install it with `{command}`."))
                    .unwrap_or_default()
            ),
        });
    };
    let (version, initialized, adapter_installed) = tokio::join!(
        cli_version(&path),
        async {
            match agent.acp_args {
                Some(args) => acp_initialize(&path, args).await,
                None => None,
            }
        },
        async {
            match agent.adapter_binary {
                Some(binary) => resolve(binary).await.is_some(),
                None => false,
            }
        }
    );
    let acp = match (&initialized, agent.acp_args) {
        (Some(_), _) => "native",
        (None, Some(_)) => "unreachable",
        (None, None) => "adapter",
    };
    let reported = initialized
        .as_ref()
        .and_then(|result| result.pointer("/agentInfo/version"))
        .and_then(Value::as_str)
        .map(clean);
    let version = reported.or(version);
    let needs_update = below_minimum(version.as_deref(), agent.min_version);
    let runnable = !needs_update && (acp == "native" || (acp == "adapter" && adapter_installed));
    let (reason_code, reason) = if needs_update {
        (
            Some(ExecutorUnavailable::BELOW_MIN_VERSION),
            Some(format!(
                "Version {} is installed; Codewhale needs {} or newer to drive it.",
                version.as_deref().unwrap_or("unknown"),
                agent.min_version.unwrap_or("a newer one")
            )),
        )
    } else if acp == "unreachable" {
        (
            Some(ExecutorUnavailable::NO_ACP),
            Some("It did not answer the Agent Client Protocol handshake.".to_string()),
        )
    } else if acp == "adapter" && !adapter_installed {
        let adapter = agent.adapter.unwrap_or("its ACP adapter");
        (
            Some(ExecutorUnavailable::ADAPTER_MISSING),
            Some(format!(
                "It runs through the ACP adapter {adapter}, which is not installed. Install it with `npm i -g {adapter}`."
            )),
        )
    } else {
        (None, None)
    };
    json!({
        "id": agent.id,
        "name": agent.name,
        "state": "installed",
        "needs_update": needs_update,
        "min_version": agent.min_version,
        "upgrade": agent.upgrade,
        "login": agent.login,
        "adapter": agent.adapter,
        "version": version,
        "acp": acp,
        "auth_methods": initialized.as_ref().map(auth_methods).unwrap_or_default(),
        "website": agent.website,
        "adapter_installed": adapter_installed,
        "runnable": runnable,
        "depth": if runnable { "driven" } else { "observed" },
        "auth_status": "unknown",
        "reason_code": reason_code,
        "reason": reason,
    })
}

pub(super) async fn list_external_agents() -> Json<Value> {
    let mut tasks = JoinSet::new();
    for agent in AGENTS {
        tasks.spawn(describe(agent));
    }
    let mut found = Vec::with_capacity(AGENTS.len());
    while let Some(result) = tasks.join_next().await {
        if let Ok(entry) = result {
            found.push(entry);
        }
    }
    found.sort_by_key(|entry| {
        AGENTS
            .iter()
            .position(|agent| entry.get("id").and_then(Value::as_str) == Some(agent.id))
            .unwrap_or(usize::MAX)
    });
    Json(json!({ "agents": found }))
}

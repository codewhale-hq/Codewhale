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
const PROBE_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const SILENT_HANDSHAKE: &str = "It did not answer the Agent Client Protocol handshake.";

use codewhale_engine::fleet::agents::{
    AGENTS, AgentQuota, ExecutorUnavailable, ExternalAgent, QuotaGate, QuotaSource, QuotaUnknown,
    below_minimum, cli_version, foreign_command, local_signin_quota, not_installed, resolve,
};
use codewhale_runtime::process_tree::ProcessTree;
use codewhale_runtime::settings::{AgentSettings, Settings};

#[derive(Debug)]
enum Handshake {
    Answered(Value),
    Refused(String),
    Silent,
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_TEXT_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

async fn acp_initialize(path: &Path, args: &[&str]) -> Handshake {
    let mut command = foreign_command(path, args);
    #[cfg(unix)]
    command.process_group(0);
    let Ok(mut child) = command.spawn() else {
        return Handshake::Silent;
    };
    let Ok(tree) = ProcessTree::attach_tokio(&child) else {
        return Handshake::Silent;
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Handshake::Silent;
    };
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
    let mut lines = crate::stdio::BoundedLines::new(BufReader::new(stdout));
    let handshake = tokio::time::timeout(PROBE_TIMEOUT, async {
        if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
            return Handshake::Silent;
        }
        while let Ok(Some(line)) = lines.next_line().await {
            if line.len() > MAX_PROBE_LINE_BYTES {
                return Handshake::Refused(
                    "It answered the Agent Client Protocol handshake with a line larger than Codewhale reads."
                        .to_string(),
                );
            }
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Handshake::Refused(
                    "It answered without JSON-RPC 2.0, which the Agent Client Protocol requires."
                        .to_string(),
                );
            }
            if message.get("id") != Some(&json!(1)) {
                continue;
            }
            let Some(result) = message.get("result") else {
                return Handshake::Refused(
                    "It refused the Agent Client Protocol handshake.".to_string(),
                );
            };
            return match result.get("protocolVersion").and_then(Value::as_u64) {
                Some(1) => Handshake::Answered(result.clone()),
                Some(version) => Handshake::Refused(format!(
                    "It speaks Agent Client Protocol version {version}; Codewhale drives version 1."
                )),
                None => Handshake::Refused(
                    "It answered the Agent Client Protocol handshake without a protocol version."
                        .to_string(),
                ),
            };
        }
        Handshake::Silent
    })
    .await
    .unwrap_or(Handshake::Silent);
    drop(stdin);
    drop(tree);
    let _ = child.start_kill();
    let _ = tokio::time::timeout(PROBE_REAP_TIMEOUT, child.wait()).await;
    handshake
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
            "reason": not_installed(agent).message,
        });
    };
    let (version, handshake, adapter_installed) = tokio::join!(
        cli_version(&path),
        async {
            match agent.acp_args {
                Some(args) => acp_initialize(&path, args).await,
                None => Handshake::Silent,
            }
        },
        async {
            match agent.adapter_binary {
                Some(binary) => resolve(binary).await.is_some(),
                None => false,
            }
        }
    );
    let (initialized, refusal) = match handshake {
        Handshake::Answered(result) => (Some(result), None),
        Handshake::Refused(reason) => (None, Some(reason)),
        Handshake::Silent => (None, None),
    };
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
    let version = version.or(reported);
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
            Some(refusal.unwrap_or_else(|| SILENT_HANDSHAKE.to_string())),
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

fn quota_json(quota: &AgentQuota) -> Value {
    match quota {
        AgentQuota::Known { source, windows } => json!({
            "status": "known",
            "source": source.as_str(),
            "windows": windows
                .iter()
                .map(|window| json!({
                    "name": window.name,
                    "used_percent": window.used_percent,
                    "resets_at": window.resets_at,
                }))
                .collect::<Vec<_>>(),
        }),
        AgentQuota::Unknown(reason) => json!({
            "status": "unknown",
            "reason": reason.as_str(),
        }),
    }
}

async fn agent_quota_report(agents: &AgentSettings) -> Value {
    let mut entries = Vec::with_capacity(AGENTS.len());
    for agent in AGENTS {
        let choice = agents.quota(agent.id);
        let gate = QuotaGate {
            enabled: choice.enabled,
            consented: choice.consented_at.is_some(),
        };
        let quota = local_signin_quota(agent, gate).await;
        entries.push(json!({
            "id": agent.id,
            "name": agent.name,
            "quota": quota_json(&quota),
        }));
    }
    for id in agents
        .quota
        .keys()
        .filter(|id| !AGENTS.iter().any(|agent| agent.id == id.as_str()))
    {
        entries.push(json!({
            "id": id,
            "name": null,
            "quota": quota_json(&AgentQuota::Unknown(QuotaUnknown::UnknownAgent)),
        }));
    }
    json!({
        "default_source": QuotaSource::AgentReported.as_str(),
        "agents": entries,
    })
}

pub(super) async fn get_external_agent_quota() -> Json<Value> {
    #[cfg(any(test, feature = "test-support"))]
    let env_ticket = codewhale_engine::test_support::env_scope_ticket();
    let agents = tokio::task::spawn_blocking(move || {
        #[cfg(any(test, feature = "test-support"))]
        let _membership = codewhale_engine::test_support::join_env_scope(env_ticket);
        Settings::load_read_only()
    })
    .await
    .ok()
    .and_then(Result::ok)
    .map(|settings| settings.agents)
    .unwrap_or_default();
    Json(agent_quota_report(&agents).await)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::Instant;

    fn script(dir: &Path, name: &str, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nread request\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[tokio::test]
    async fn handshake_keeps_answered_unsupported_and_silent_agents_distinct() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("agent dir ünï");
        std::fs::create_dir_all(&dir).unwrap();
        let answer = |reply: &str| format!("echo 'log line'; echo '{reply}'; sleep 30");
        let native = script(
            &dir,
            "native",
            &answer(
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"version":"2.0.0"},"authMethods":[{"id":"login","name":"Log in","type":"terminal"}]}}"#,
            ),
        );
        let Handshake::Answered(result) = acp_initialize(&native, &["acp"]).await else {
            panic!("a v1 answer must count as native ACP");
        };
        assert_eq!(auth_methods(&result)[0]["type"], "terminal");

        let cases = [
            (
                r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":2}}"#,
                "It speaks Agent Client Protocol version 2; Codewhale drives version 1.",
            ),
            (
                r#"{"id":1,"result":{"protocolVersion":1}}"#,
                "It answered without JSON-RPC 2.0, which the Agent Client Protocol requires.",
            ),
            (
                r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no"}}"#,
                "It refused the Agent Client Protocol handshake.",
            ),
        ];
        for (index, (reply, expected)) in cases.into_iter().enumerate() {
            let agent = script(&dir, &format!("refusing {index}"), &answer(reply));
            match acp_initialize(&agent, &[]).await {
                Handshake::Refused(reason) => assert_eq!(reason, expected),
                other => panic!("{reply}: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn silent_handshake_is_bounded_and_kills_the_agent_tree() {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("grandchild.pid");
        let silent = script(
            root.path(),
            "silent",
            &format!("sleep 300 & echo $! > '{}'; sleep 300", pid_file.display()),
        );
        let started = Instant::now();
        assert!(matches!(
            acp_initialize(&silent, &[]).await,
            Handshake::Silent
        ));
        assert!(started.elapsed() < PROBE_TIMEOUT + PROBE_REAP_TIMEOUT + Duration::from_secs(2));
        let grandchild =
            codewhale_runtime::process_tree::read_pid_file(&pid_file, Duration::from_secs(5));
        assert!(codewhale_runtime::process_tree::wait_for_pid_exit(
            grandchild,
            Duration::from_secs(5)
        ));
    }
}

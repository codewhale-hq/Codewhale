use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::Json;
use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

const PROBE_TIMEOUT: Duration = SETUP_TIMEOUT;
const MAX_CONCURRENT_DISCOVERY_PROBES: usize = 4;
static DISCOVERY_PROBES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_DISCOVERY_PROBES)));
const MAX_PROBE_LINE_BYTES: usize = 256 * 1024;
const MAX_TEXT_CHARS: usize = 80;
const MAX_AUTH_METHODS: usize = 8;
const PROBE_REAP_TIMEOUT: Duration = Duration::from_secs(2);
const SILENT_HANDSHAKE: &str = "It did not answer the Agent Client Protocol handshake.";
const VERSION_TIMEOUT_REASON: &str = "Its version could not be read because `--version` timed out.";

use codewhale_engine::fleet::agents::{
    AGENTS, AgentQuota, CustomExternalAgent, ExecutorUnavailable, ExternalAgent, QuotaGate,
    QuotaSource, QuotaUnknown, SETUP_TIMEOUT, VersionProbe, below_minimum,
    cli_version_probe_with_env, custom_agents, foreign_command, local_signin_quota, not_installed,
    resolve, resolve_custom_binary,
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

async fn acp_initialize(path: &Path, args: &[&str], env: &BTreeMap<String, String>) -> Handshake {
    acp_initialize_with_timeout(path, args, env, PROBE_TIMEOUT).await
}

async fn acp_initialize_with_timeout(
    path: &Path,
    args: &[&str],
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> Handshake {
    let mut command = foreign_command(path, args);
    command.envs(env);
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
    let handshake = tokio::time::timeout(timeout, async {
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

struct DiscoveryOutcome {
    acp: &'static str,
    version: Option<String>,
    needs_update: bool,
    runnable: bool,
    reason_code: Option<&'static str>,
    reason: Option<String>,
}

fn unpack_version(probe: VersionProbe, initialized: Option<&Value>) -> (Option<String>, bool) {
    let (cli_version, timed_out) = match probe {
        VersionProbe::Known(version) => (Some(version), false),
        VersionProbe::TimedOut => (None, true),
        VersionProbe::Unknown => (None, false),
    };
    let reported = initialized
        .and_then(|result| result.pointer("/agentInfo/version"))
        .and_then(Value::as_str)
        .map(clean)
        .filter(|version| !version.is_empty());
    let version = cli_version.or(reported);
    let version_timed_out = version.is_none() && timed_out;
    (version, version_timed_out)
}

fn append_version_timeout(message: String, version_timed_out: bool) -> String {
    if version_timed_out {
        format!("{message} {VERSION_TIMEOUT_REASON}")
    } else {
        message
    }
}

fn classify_installed(
    version_probe: VersionProbe,
    initialized: Option<&Value>,
    refusal: Option<String>,
    has_acp_args: bool,
    min_version: Option<&str>,
    adapter: Option<&str>,
    adapter_installed: bool,
) -> DiscoveryOutcome {
    let acp = match (initialized, has_acp_args) {
        (Some(_), _) => "native",
        (None, true) => "unreachable",
        (None, false) => "adapter",
    };
    let (version, version_timed_out) = unpack_version(version_probe, initialized);
    let needs_update = below_minimum(version.as_deref(), min_version);
    let runnable = !needs_update && (acp == "native" || (acp == "adapter" && adapter_installed));
    let (reason_code, reason) = if needs_update {
        (
            Some(ExecutorUnavailable::BELOW_MIN_VERSION),
            Some(format!(
                "Version {} is installed; Codewhale needs {} or newer to drive it.",
                version.as_deref().unwrap_or("unknown"),
                min_version.unwrap_or("a newer one")
            )),
        )
    } else if acp == "unreachable" {
        let message = refusal.unwrap_or_else(|| SILENT_HANDSHAKE.to_string());
        (
            Some(ExecutorUnavailable::NO_ACP),
            Some(append_version_timeout(message, version_timed_out)),
        )
    } else if acp == "adapter" && !adapter_installed {
        let adapter = adapter.unwrap_or("its ACP adapter");
        let message = format!(
            "It runs through the ACP adapter {adapter}, which is not installed. Install it with `npm i -g {adapter}`."
        );
        (
            Some(ExecutorUnavailable::ADAPTER_MISSING),
            Some(append_version_timeout(message, version_timed_out)),
        )
    } else if version_timed_out {
        (None, Some(VERSION_TIMEOUT_REASON.to_string()))
    } else {
        (None, None)
    };
    DiscoveryOutcome {
        acp,
        version,
        needs_update,
        runnable,
        reason_code,
        reason,
    }
}

async fn describe_custom(agent: CustomExternalAgent) -> Value {
    let Some(path) = resolve_custom_binary(&agent.binary).await else {
        return json!({
            "id": agent.id,
            "name": agent.name,
            "state": "not_installed",
            "version": null,
            "acp": null,
            "auth_methods": [],
            "website": agent.website,
            "min_version": null,
            "upgrade": null,
            "login": null,
            "adapter": null,
            "adapter_installed": false,
            "needs_update": false,
            "runnable": false,
            "depth": null,
            "auth_status": "unknown",
            "custom": true,
            "reason_code": ExecutorUnavailable::NOT_INSTALLED,
            "reason": format!("{} (`{}`) is not installed.", agent.name, agent.binary),
        });
    };
    let _permit = Arc::clone(&DISCOVERY_PROBES).acquire_owned().await.ok();
    let arg_refs: Vec<&str> = agent.args.iter().map(String::as_str).collect();
    let (version_probe, handshake) = tokio::join!(
        cli_version_probe_with_env(&path, &agent.env),
        acp_initialize(&path, &arg_refs, &agent.env),
    );
    let (initialized, refusal) = match handshake {
        Handshake::Answered(result) => (Some(result), None),
        Handshake::Refused(reason) => (None, Some(reason)),
        Handshake::Silent => (None, None),
    };
    let outcome = classify_installed(
        version_probe,
        initialized.as_ref(),
        refusal,
        true,
        None,
        None,
        false,
    );
    json!({
        "id": agent.id,
        "name": agent.name,
        "state": "installed",
        "needs_update": outcome.needs_update,
        "min_version": null,
        "upgrade": null,
        "login": null,
        "adapter": null,
        "version": outcome.version,
        "acp": outcome.acp,
        "auth_methods": initialized.as_ref().map(auth_methods).unwrap_or_default(),
        "website": agent.website,
        "adapter_installed": false,
        "runnable": outcome.runnable,
        "depth": if outcome.runnable { "driven" } else { "observed" },
        "auth_status": "unknown",
        "custom": true,
        "reason_code": outcome.reason_code,
        "reason": outcome.reason,
    })
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
    let _permit = Arc::clone(&DISCOVERY_PROBES).acquire_owned().await.ok();
    let env = agent.process_env();
    let (version_probe, handshake, adapter_installed) = tokio::join!(
        cli_version_probe_with_env(&path, &env),
        async {
            match agent.acp_args {
                Some(args) => acp_initialize(&path, args, &env).await,
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
    let outcome = classify_installed(
        version_probe,
        initialized.as_ref(),
        refusal,
        agent.acp_args.is_some(),
        agent.min_version,
        agent.adapter,
        adapter_installed,
    );
    json!({
        "id": agent.id,
        "name": agent.name,
        "state": "installed",
        "needs_update": outcome.needs_update,
        "min_version": agent.min_version,
        "upgrade": agent.upgrade,
        "login": agent.login,
        "adapter": agent.adapter,
        "version": outcome.version,
        "acp": outcome.acp,
        "auth_methods": initialized.as_ref().map(auth_methods).unwrap_or_default(),
        "website": agent.website,
        "adapter_installed": adapter_installed,
        "runnable": outcome.runnable,
        "depth": if outcome.runnable { "driven" } else { "observed" },
        "auth_status": "unknown",
        "reason_code": outcome.reason_code,
        "reason": outcome.reason,
    })
}

pub(super) async fn list_external_agents() -> Json<Value> {
    let customs = custom_agents();
    let custom_ids: Vec<String> = customs.iter().map(|c| c.id.clone()).collect();
    let mut tasks = JoinSet::new();
    for agent in AGENTS {
        tasks.spawn(describe(agent));
    }
    for custom in customs {
        tasks.spawn(describe_custom(custom));
    }
    let mut found = Vec::with_capacity(AGENTS.len() + custom_ids.len());
    while let Some(result) = tasks.join_next().await {
        if let Ok(entry) = result {
            found.push(entry);
        }
    }
    found.sort_by_key(|entry| {
        let id = entry.get("id").and_then(Value::as_str).unwrap_or_default();
        AGENTS
            .iter()
            .position(|agent| agent.id == id)
            .or_else(|| {
                custom_ids
                    .iter()
                    .position(|cid| cid == id)
                    .map(|idx| AGENTS.len() + idx)
            })
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
    async fn discovery_passes_registered_env_to_version_and_initialize() {
        let root = tempfile::tempdir().unwrap();
        let path = script(
            root.path(),
            "agent",
            r#"
            test "$AUGMENT_DISABLE_AUTO_UPDATE" = 1 || exit 3
            test "$DROID_DISABLE_AUTO_UPDATE" = true || exit 4
            test "$FACTORY_DROID_AUTO_UPDATE_ENABLED" = false || exit 5
            if [ "$1" = --version ]; then echo "agent 1.2.3"; exit; fi
            test "$1" = acp || exit 6
            echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}'
        "#,
        );
        let mut env = codewhale_engine::fleet::agents::find("auggie")
            .unwrap()
            .process_env();
        env.extend(
            codewhale_engine::fleet::agents::find("factory-droid")
                .unwrap()
                .process_env(),
        );
        let row = describe_custom(CustomExternalAgent {
            id: "local".into(),
            name: "Local".into(),
            binary: path.display().to_string(),
            args: vec!["acp".into()],
            website: String::new(),
            env,
        })
        .await;
        assert_eq!(row["version"], "1.2.3");
        assert_eq!(row["runnable"], true);
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
        let Handshake::Answered(result) = acp_initialize(&native, &["acp"], &BTreeMap::new()).await
        else {
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
            match acp_initialize(&agent, &[], &BTreeMap::new()).await {
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
        let timeout = Duration::from_secs(2);
        let started = Instant::now();
        assert!(matches!(
            acp_initialize_with_timeout(&silent, &[], &BTreeMap::new(), timeout).await,
            Handshake::Silent
        ));
        assert!(started.elapsed() < timeout + PROBE_REAP_TIMEOUT + Duration::from_secs(2));
        let grandchild =
            codewhale_runtime::process_tree::read_pid_file(&pid_file, Duration::from_secs(5));
        assert!(codewhale_runtime::process_tree::wait_for_pid_exit(
            grandchild,
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn version_timeout_is_distinct_from_no_acp_and_known_versions_check_minimum() {
        assert_eq!(PROBE_TIMEOUT, SETUP_TIMEOUT);
        assert_eq!(MAX_CONCURRENT_DISCOVERY_PROBES, 4);

        let answered_no_version = json!({"protocolVersion": 1});
        let timed_out = classify_installed(
            VersionProbe::TimedOut,
            Some(&answered_no_version),
            None,
            true,
            Some("1.0.0"),
            None,
            false,
        );
        assert_eq!(timed_out.version, None);
        assert!(timed_out.runnable);
        assert!(!timed_out.needs_update);
        assert_eq!(timed_out.reason_code, None);
        assert_eq!(timed_out.reason.as_deref(), Some(VERSION_TIMEOUT_REASON));

        let answered_old = json!({"protocolVersion": 1, "agentInfo": {"version": "0.209.1"}});
        let old_from_handshake = classify_installed(
            VersionProbe::TimedOut,
            Some(&answered_old),
            None,
            true,
            Some("0.249.0"),
            None,
            false,
        );
        assert_eq!(old_from_handshake.version.as_deref(), Some("0.209.1"));
        assert!(old_from_handshake.needs_update);
        assert!(!old_from_handshake.runnable);
        assert_eq!(
            old_from_handshake.reason_code,
            Some(ExecutorUnavailable::BELOW_MIN_VERSION)
        );

        let both_timed_out = classify_installed(
            VersionProbe::TimedOut,
            None,
            None,
            true,
            Some("0.249.0"),
            None,
            false,
        );
        assert_eq!(both_timed_out.version, None);
        assert!(!both_timed_out.runnable);
        assert_eq!(
            both_timed_out.reason_code,
            Some(ExecutorUnavailable::NO_ACP)
        );
        assert!(
            both_timed_out
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains(SILENT_HANDSHAKE)
                    && reason.contains(VERSION_TIMEOUT_REASON))
        );
    }
}

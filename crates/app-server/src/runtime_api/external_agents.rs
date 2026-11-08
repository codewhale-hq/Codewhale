use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use axum::Json;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinSet;

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PROBE_LINE_BYTES: usize = 256 * 1024;
const MAX_TEXT_CHARS: usize = 80;
const MAX_AUTH_METHODS: usize = 8;

struct Agent {
    id: &'static str,
    name: &'static str,
    binary: &'static str,
    acp_args: Option<&'static [&'static str]>,
    website: &'static str,
}

const AGENTS: &[Agent] = &[
    Agent {
        id: "claude",
        name: "Claude Agent",
        binary: "claude",
        acp_args: None,
        website: "https://github.com/agentclientprotocol/claude-agent-acp",
    },
    Agent {
        id: "codex",
        name: "Codex",
        binary: "codex",
        acp_args: None,
        website: "https://github.com/agentclientprotocol/codex-acp",
    },
    Agent {
        id: "gemini",
        name: "Gemini CLI",
        binary: "gemini",
        acp_args: Some(&["--acp"]),
        website: "https://geminicli.com",
    },
    Agent {
        id: "opencode",
        name: "OpenCode",
        binary: "opencode",
        acp_args: Some(&["acp"]),
        website: "https://opencode.ai",
    },
    Agent {
        id: "kimi",
        name: "Kimi Code",
        binary: "kimi",
        acp_args: Some(&["acp"]),
        website: "https://moonshotai.github.io/kimi-code/",
    },
    Agent {
        id: "mistral-vibe",
        name: "Mistral Vibe",
        binary: "vibe-acp",
        acp_args: Some(&[]),
        website: "https://mistral.ai/products/vibe",
    },
    Agent {
        id: "qoder",
        name: "Qoder",
        binary: "qodercli",
        acp_args: Some(&["--acp"]),
        website: "https://qoder.com",
    },
    Agent {
        id: "codebuddy",
        name: "CodeBuddy",
        binary: "codebuddy",
        acp_args: Some(&["--acp"]),
        website: "https://www.codebuddy.cn/cli/",
    },
    Agent {
        id: "grok",
        name: "Grok",
        binary: "grok",
        acp_args: Some(&["agent", "stdio"]),
        website: "https://x.ai/cli",
    },
];

const USER_BIN_DIRS: &[&str] = &[
    ".local/bin",
    ".npm-global/bin",
    ".opencode/bin",
    ".kimi-code/bin",
    ".bun/bin",
    ".cargo/bin",
];
const SYSTEM_BIN_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

fn resolve(binary: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend(USER_BIN_DIRS.iter().map(|dir| home.join(dir)));
    }
    dirs.extend(SYSTEM_BIN_DIRS.iter().map(PathBuf::from));
    dirs.into_iter()
        .map(|dir| dir.join(binary))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

fn foreign_command(path: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(path);
    command
        .args(args)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("CODEWHALE_")
            || name.ends_with("_API_KEY")
            || name.ends_with("_AUTH_TOKEN")
            || name.ends_with("_ACCESS_KEY")
            || name.ends_with("_SECRET_KEY")
        {
            command.env_remove(name);
        }
    }
    command
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control())
        .take(MAX_TEXT_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

fn version_token(text: &str) -> Option<String> {
    text.split(|ch: char| ch.is_whitespace() || ch == '(' || ch == ')' || ch == ',')
        .map(|token| token.trim_start_matches('v'))
        .find(|token| {
            token.chars().next().is_some_and(|ch| ch.is_ascii_digit()) && token.contains('.')
        })
        .map(clean)
}

async fn cli_version(path: &Path) -> Option<String> {
    let mut command = foreign_command(path, &["--version"]);
    command.stdin(Stdio::null());
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    version_token(text.lines().next().unwrap_or_default())
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
    let mut lines = BufReader::new(stdout).lines();
    let reply = tokio::time::timeout(PROBE_TIMEOUT, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.len() > MAX_PROBE_LINE_BYTES {
                return None;
            }
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message.get("id") == Some(&json!(1)) {
                return message.get("result").cloned();
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

async fn describe(agent: &'static Agent) -> Value {
    let Some(path) = resolve(agent.binary) else {
        return json!({
            "id": agent.id,
            "name": agent.name,
            "state": "not_installed",
            "version": null,
            "acp": null,
            "auth_methods": [],
            "website": agent.website,
            "runnable": false,
        });
    };
    let (version, initialized) = tokio::join!(cli_version(&path), async {
        match agent.acp_args {
            Some(args) => acp_initialize(&path, args).await,
            None => None,
        }
    });
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
    json!({
        "id": agent.id,
        "name": agent.name,
        "state": "installed",
        "version": reported.or(version),
        "acp": acp,
        "auth_methods": initialized.as_ref().map(auth_methods).unwrap_or_default(),
        "website": agent.website,
        "runnable": false,
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

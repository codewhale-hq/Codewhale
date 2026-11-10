use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub const FIXTURE_AGENT_ENV: &str = "CODEWHALE_ACP_FIXTURE_AGENT";
pub const FIXTURE_AGENT_ID: &str = "fixture";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_VERSION_CHARS: usize = 80;
const MAX_VERSION_BYTES: u64 = 8192;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct ExecutorUnavailable {
    pub code: &'static str,
    pub message: String,
}

impl ExecutorUnavailable {
    pub const UNKNOWN_AGENT: &'static str = "unknown_agent";
    pub const NOT_INSTALLED: &'static str = "not_installed";
    pub const ADAPTER_MISSING: &'static str = "adapter_missing";
    pub const BELOW_MIN_VERSION: &'static str = "below_min_version";
    pub const NEEDS_SIGN_IN: &'static str = "needs_sign_in";
    pub const NO_ACP: &'static str = "no_acp";
    pub const EXECUTOR_NOT_SUPPORTED: &'static str = "executor_not_supported";
    pub const ENVIRONMENT_NOT_SUPPORTED: &'static str = "environment_not_supported";

    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
pub struct ExternalAgent {
    pub id: &'static str,
    pub name: &'static str,
    pub binary: &'static str,
    pub acp_args: Option<&'static [&'static str]>,
    pub website: &'static str,
    pub min_version: Option<&'static str>,
    pub upgrade: Option<&'static str>,
    pub login: Option<&'static str>,
    pub adapter: Option<&'static str>,
    pub adapter_binary: Option<&'static str>,
    pub adapter_cli_env: Option<&'static str>,
}

pub const AGENTS: &[ExternalAgent] = &[
    ExternalAgent {
        id: "claude",
        name: "Claude Agent",
        binary: "claude",
        acp_args: None,
        website: "https://github.com/agentclientprotocol/claude-agent-acp",
        min_version: Some("2.1.226"),
        upgrade: Some("npm i -g @anthropic-ai/claude-code@latest"),
        login: Some("claude auth login"),
        adapter: Some("@agentclientprotocol/claude-agent-acp"),
        adapter_binary: Some("claude-agent-acp"),
        adapter_cli_env: Some("CLAUDE_CODE_EXECUTABLE"),
    },
    ExternalAgent {
        id: "codex",
        name: "Codex",
        binary: "codex",
        acp_args: None,
        website: "https://github.com/agentclientprotocol/codex-acp",
        min_version: Some("0.160.0"),
        upgrade: Some("npm i -g @openai/codex@latest"),
        login: Some("codex login"),
        adapter: Some("@agentclientprotocol/codex-acp"),
        adapter_binary: Some("codex-acp"),
        adapter_cli_env: Some("CODEX_PATH"),
    },
    ExternalAgent {
        id: "dsh",
        name: "DeepSeek Harness",
        binary: "dsh",
        acp_args: Some(&["acp"]),
        website: "https://github.com/deepseek-ai/deepseek-harness",
        min_version: None,
        upgrade: Some("npm i -g @deepseek-ai/dsh@latest"),
        login: None,
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "antigravity",
        name: "Antigravity",
        binary: "agy",
        acp_args: Some(&["acp"]),
        website: "https://antigravity.google/docs/cli/acp/",
        min_version: Some("1.5.0"),
        upgrade: Some("agy update"),
        login: Some("agy acp --login"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "opencode",
        name: "OpenCode",
        binary: "opencode",
        acp_args: Some(&["acp"]),
        website: "https://opencode.ai",
        min_version: Some("1.15.5"),
        upgrade: Some("npm i -g opencode-ai@latest"),
        login: Some("opencode auth login"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "kimi",
        name: "Kimi Code",
        binary: "kimi",
        acp_args: Some(&["acp"]),
        website: "https://moonshotai.github.io/kimi-code/",
        min_version: None,
        upgrade: None,
        login: Some("kimi login"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "mistral-vibe",
        name: "Mistral Vibe",
        binary: "vibe-acp",
        acp_args: Some(&[]),
        website: "https://mistral.ai/products/vibe",
        min_version: None,
        upgrade: None,
        login: None,
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "qoder",
        name: "Qoder",
        binary: "qodercli",
        acp_args: Some(&["--acp"]),
        website: "https://qoder.com",
        min_version: None,
        upgrade: None,
        login: None,
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "codebuddy",
        name: "CodeBuddy",
        binary: "codebuddy",
        acp_args: Some(&["--acp"]),
        website: "https://www.codebuddy.cn/cli/",
        min_version: None,
        upgrade: None,
        login: None,
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "grok",
        name: "Grok",
        binary: "grok",
        acp_args: Some(&["agent", "stdio"]),
        website: "https://x.ai/cli",
        min_version: None,
        upgrade: None,
        login: None,
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
];

const FIXTURE_AGENT: ExternalAgent = ExternalAgent {
    id: FIXTURE_AGENT_ID,
    name: "ACP fixture agent",
    binary: "node",
    acp_args: None,
    website: "",
    min_version: None,
    upgrade: None,
    login: None,
    adapter: None,
    adapter_binary: None,
    adapter_cli_env: None,
};

pub fn find(id: &str) -> Option<&'static ExternalAgent> {
    if id == FIXTURE_AGENT_ID {
        return fixture_script().map(|_| &FIXTURE_AGENT);
    }
    AGENTS.iter().find(|agent| agent.id == id)
}

fn fixture_script() -> Option<PathBuf> {
    std::env::var_os(FIXTURE_AGENT_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

const USER_BIN_DIRS: &[&str] = &[
    ".local/bin",
    ".npm-global/bin",
    ".opencode/bin",
    ".kimi-code/bin",
    ".bun/bin",
    ".cargo/bin",
];
const SYSTEM_BIN_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

pub async fn resolve(binary: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.extend(USER_BIN_DIRS.iter().map(|dir| home.join(dir)));
    }
    dirs.extend(SYSTEM_BIN_DIRS.iter().map(PathBuf::from));
    for dir in dirs {
        let candidate = dir.join(binary);
        if is_executable(&candidate).await {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
async fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
async fn is_executable(path: &Path) -> bool {
    tokio::fs::metadata(path)
        .await
        .is_ok_and(|meta| meta.is_file())
}

pub fn foreign_command(path: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(path);
    command
        .args(args)
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command.env_clear().envs(
        codewhale_runtime::child_env::sanitized_runtime_env_from(std::env::vars_os())
            .into_iter()
            .filter(|(name, _)| !name.to_string_lossy().starts_with("CODEWHALE_")),
    );
    command
}

pub fn version_token(text: &str) -> Option<String> {
    text.split(|ch: char| ch.is_whitespace() || ch == '(' || ch == ')' || ch == ',')
        .map(|token| token.trim_start_matches('v'))
        .find(|token| {
            token.chars().next().is_some_and(|ch| ch.is_ascii_digit()) && token.contains('.')
        })
        .map(|token| {
            token
                .chars()
                .filter(|ch| !ch.is_control())
                .take(MAX_VERSION_CHARS)
                .collect::<String>()
                .trim()
                .to_string()
        })
}

pub async fn cli_version(path: &Path) -> Option<String> {
    let mut command = foreign_command(path, &["--version"]);
    command.stdin(Stdio::null());
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    let bytes = tokio::time::timeout(VERSION_TIMEOUT, async {
        let mut bytes = Vec::new();
        stdout
            .take(MAX_VERSION_BYTES + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() as u64 > MAX_VERSION_BYTES {
            return Err(std::io::Error::other("agent version output is too large"));
        }
        child.wait().await?;
        Ok::<_, std::io::Error>(bytes)
    })
    .await
    .ok()?
    .ok()?;
    let text = String::from_utf8_lossy(&bytes);
    version_token(text.lines().next().unwrap_or_default())
}

fn version_core(text: &str) -> Option<(u64, u64, u64)> {
    let core: String = text
        .trim_start_matches('v')
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect();
    let mut parts = core.trim_end_matches('.').split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    parts.next().is_none().then_some((major, minor, patch))
}

pub fn below_minimum(version: Option<&str>, minimum: Option<&str>) -> bool {
    match (
        version.and_then(version_core),
        minimum.and_then(version_core),
    ) {
        (Some(found), Some(floor)) => found < floor,
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalAgentLaunch {
    pub agent_id: String,
    pub agent_name: String,
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

pub async fn launch(id: &str) -> Result<ExternalAgentLaunch, ExecutorUnavailable> {
    let agent = find(id).ok_or_else(|| {
        ExecutorUnavailable::new(
            ExecutorUnavailable::UNKNOWN_AGENT,
            format!("No external agent is named `{id}`."),
        )
    })?;
    if agent.id == FIXTURE_AGENT_ID {
        let script = fixture_script().ok_or_else(|| {
            ExecutorUnavailable::new(
                ExecutorUnavailable::UNKNOWN_AGENT,
                "The fixture agent is not configured.",
            )
        })?;
        let node = resolve("node").await.ok_or_else(|| {
            ExecutorUnavailable::new(
                ExecutorUnavailable::NOT_INSTALLED,
                "The fixture agent needs `node` on PATH.",
            )
        })?;
        return Ok(ExternalAgentLaunch {
            agent_id: agent.id.to_string(),
            agent_name: agent.name.to_string(),
            program: node,
            args: vec![script.display().to_string()],
            env: BTreeMap::new(),
        });
    }
    let cli = resolve(agent.binary).await.ok_or_else(|| {
        let install = agent
            .upgrade
            .map(|command| format!(" Install it with `{command}`."))
            .unwrap_or_default();
        ExecutorUnavailable::new(
            ExecutorUnavailable::NOT_INSTALLED,
            format!("{} is not installed.{install}", agent.name),
        )
    })?;
    if agent.min_version.is_some() {
        let version = cli_version(&cli).await;
        if below_minimum(version.as_deref(), agent.min_version) {
            let update = agent
                .upgrade
                .map(|command| format!(" Update it with `{command}`."))
                .unwrap_or_default();
            return Err(ExecutorUnavailable::new(
                ExecutorUnavailable::BELOW_MIN_VERSION,
                format!(
                    "{} {} is installed; Codewhale needs {} or newer to drive it.{update}",
                    agent.name,
                    version.as_deref().unwrap_or("unknown"),
                    agent.min_version.unwrap_or("a newer one"),
                ),
            ));
        }
    }
    match (agent.acp_args, agent.adapter_binary) {
        (Some(args), _) => Ok(ExternalAgentLaunch {
            agent_id: agent.id.to_string(),
            agent_name: agent.name.to_string(),
            program: cli,
            args: args.iter().map(|arg| (*arg).to_string()).collect(),
            env: BTreeMap::new(),
        }),
        (None, Some(adapter_binary)) => {
            let adapter = resolve(adapter_binary).await.ok_or_else(|| {
                ExecutorUnavailable::new(ExecutorUnavailable::ADAPTER_MISSING, format!(
                    "{} runs through the ACP adapter {}, which is not installed. Install it with `npm i -g {}`; Codewhale never downloads it for you.",
                    agent.name,
                    agent.adapter.unwrap_or(adapter_binary),
                    agent.adapter.unwrap_or(adapter_binary),
                ))
            })?;
            let env = agent
                .adapter_cli_env
                .map(|name| BTreeMap::from([(name.to_string(), cli.display().to_string())]))
                .unwrap_or_default();
            Ok(ExternalAgentLaunch {
                agent_id: agent.id.to_string(),
                agent_name: agent.name.to_string(),
                program: adapter,
                args: Vec::new(),
                env,
            })
        }
        (None, None) => Err(ExecutorUnavailable::new(
            ExecutorUnavailable::NO_ACP,
            format!("{} has no ACP mode Codewhale can run.", agent.name),
        )),
    }
}

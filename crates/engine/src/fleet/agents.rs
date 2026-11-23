use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

pub const FIXTURE_AGENT_ENV: &str = "CODEWHALE_ACP_FIXTURE_AGENT";
pub const FIXTURE_AGENT_ID: &str = "fixture";
pub const CUSTOM_AGENTS_ENV: &str = "CODEWHALE_CUSTOM_ACP_AGENTS";
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(60);
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_VERSION_CHARS: usize = 80;
const MAX_VERSION_BYTES: usize = 8192;
const MAX_VERSION_STDERR_BYTES: usize = 64 * 1024;
const WINDOWS_SPAWNABLE_EXTENSIONS: &[&str] = &[".com", ".exe", ".bat", ".cmd"];
#[cfg(any(windows, test))]
const WINDOWS_DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

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

impl ExternalAgent {
    pub fn process_env(&self) -> BTreeMap<String, String> {
        let vars: &[(&str, &str)] = match self.id {
            "auggie" => &[("AUGMENT_DISABLE_AUTO_UPDATE", "1")],
            "factory-droid" => &[
                ("DROID_DISABLE_AUTO_UPDATE", "true"),
                ("FACTORY_DROID_AUTO_UPDATE_ENABLED", "false"),
            ],
            _ => &[],
        };
        vars.iter()
            .map(|&(key, value)| (key.into(), value.into()))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct CustomExternalAgent {
    pub id: String,
    pub name: String,
    pub binary: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub website: String,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
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
        id: "cursor",
        name: "Cursor",
        binary: "cursor-agent",
        acp_args: Some(&["acp"]),
        website: "https://cursor.com/docs/cli/acp",
        min_version: None,
        upgrade: None,
        login: Some("cursor-agent login"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "cline",
        name: "Cline",
        binary: "cline",
        acp_args: Some(&["--acp"]),
        website: "https://cline.bot/cli",
        min_version: Some("3.0.73"),
        upgrade: Some("npm i -g cline@latest"),
        login: Some("cline auth"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "devin",
        name: "Devin",
        binary: "devin",
        acp_args: Some(&["acp"]),
        website: "https://docs.devin.ai/cli",
        min_version: None,
        upgrade: None,
        login: Some("devin login"),
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
        id: "goose",
        name: "Goose",
        binary: "goose",
        acp_args: Some(&["acp"]),
        website: "https://block.github.io/goose/",
        min_version: Some("1.56.0"),
        upgrade: None,
        login: Some("goose configure"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "junie",
        name: "Junie",
        binary: "junie",
        acp_args: Some(&["--acp=true"]),
        website: "https://junie.jetbrains.com",
        min_version: None,
        upgrade: None,
        login: Some("junie login"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "amp",
        name: "Amp",
        binary: "amp-acp",
        acp_args: Some(&[]),
        website: "https://github.com/tao12345666333/amp-acp",
        min_version: None,
        upgrade: None,
        login: Some("amp login"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "auggie",
        name: "Auggie CLI",
        binary: "auggie",
        acp_args: Some(&["--acp"]),
        website: "https://www.augmentcode.com/",
        min_version: Some("0.36.0"),
        upgrade: Some("npm i -g @augmentcode/auggie@latest"),
        login: Some("auggie login"),
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
        upgrade: Some("kimi upgrade"),
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
        upgrade: Some("qodercli update"),
        login: Some("qodercli login"),
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
        upgrade: Some("grok update"),
        login: Some("grok login"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "qwen-code",
        name: "Qwen Code",
        binary: "qwen",
        acp_args: Some(&["--acp"]),
        website: "https://github.com/QwenLM/qwen-code",
        min_version: Some("0.6.1"),
        upgrade: Some("npm i -g @qwen-code/qwen-code@latest"),
        login: None,
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "copilot",
        name: "GitHub Copilot CLI",
        binary: "copilot",
        acp_args: Some(&["--acp", "--stdio"]),
        website: "https://docs.github.com/copilot/reference/copilot-cli-reference/acp-server",
        min_version: None,
        upgrade: Some("copilot update"),
        login: Some("copilot login"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "muse",
        name: "Muse Code",
        binary: "muse",
        acp_args: None,
        website: "https://dev.meta.ai/products/muse-code",
        min_version: None,
        upgrade: None,
        login: Some("muse login"),
        adapter: Some("@bex-co/muse-code-acp"),
        adapter_binary: Some("muse-code-acp"),
        adapter_cli_env: Some("MUSE_CODE_EXECUTABLE"),
    },
    ExternalAgent {
        id: "minimax-code",
        name: "MiniMax Code",
        binary: "minimax-code",
        acp_args: Some(&["acp"]),
        website: "https://agent.minimax.io",
        min_version: Some("0.2.7"),
        upgrade: Some("npm i -g @minimax-ai/code@latest"),
        login: None,
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "deepagents",
        name: "DeepAgents",
        binary: "deepagents-acp",
        acp_args: Some(&[]),
        website: "https://docs.langchain.com/oss/javascript/deepagents/overview",
        min_version: Some("0.1.7"),
        upgrade: Some("npm i -g deepagents-acp@latest"),
        login: None,
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "factory-droid",
        name: "Factory Droid",
        binary: "droid",
        acp_args: Some(&["exec", "--output-format", "acp-daemon"]),
        website: "https://factory.ai/product/cli",
        min_version: Some("0.249.0"),
        upgrade: Some("npm update -g @factory/cli"),
        login: Some("droid login"),
        adapter: None,
        adapter_binary: None,
        adapter_cli_env: None,
    },
    ExternalAgent {
        id: "pi-acp",
        name: "Pi",
        binary: "pi",
        acp_args: None,
        website: "https://github.com/svkozak/pi-acp",
        min_version: None,
        upgrade: None,
        login: None,
        adapter: Some("pi-acp"),
        adapter_binary: Some("pi-acp"),
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

pub fn custom_agents() -> Vec<CustomExternalAgent> {
    std::env::var(CUSTOM_AGENTS_ENV)
        .ok()
        .map_or_else(Vec::new, |raw| parse_custom_agents(&raw))
}

fn parse_custom_agents(raw: &str) -> Vec<CustomExternalAgent> {
    if raw.len() > 64 * 1024 {
        return Vec::new();
    }
    let Ok(parsed) = serde_json::from_str::<Vec<CustomExternalAgent>>(raw) else {
        return Vec::new();
    };
    let mut seen = std::collections::BTreeSet::new();
    for builtin in AGENTS {
        seen.insert(builtin.id.to_string());
    }
    seen.insert(FIXTURE_AGENT_ID.to_string());
    parsed
        .into_iter()
        .filter(|entry| {
            let id = entry.id.as_str();
            !id.is_empty()
                && id.len() <= 64
                && id
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
                && !entry.name.trim().is_empty()
                && !entry.binary.trim().is_empty()
                && !entry.name.chars().any(char::is_control)
                && !entry.binary.chars().any(char::is_control)
                && entry.args.iter().all(|arg| !arg.contains('\0'))
                && (entry.website.is_empty()
                    || url::Url::parse(&entry.website).is_ok_and(|url| {
                        url.scheme() == "https"
                            && url.has_host()
                            && url.username().is_empty()
                            && url.password().is_none()
                    }))
                && entry.env.iter().all(|(key, value)| {
                    !key.is_empty()
                        && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                        && !key.to_ascii_uppercase().starts_with("CODEWHALE_")
                        && super::host::is_safe_env_key(key)
                        && !value.contains('\0')
                })
                && seen.insert(id.to_string())
        })
        .take(32)
        .collect()
}

pub fn find_custom(id: &str) -> Option<CustomExternalAgent> {
    custom_agents().into_iter().find(|entry| entry.id == id)
}

pub fn find(id: &str) -> Option<&'static ExternalAgent> {
    if id == FIXTURE_AGENT_ID {
        return fixture_script().map(|_| &FIXTURE_AGENT);
    }
    AGENTS.iter().find(|agent| agent.id == id)
}

pub fn agent_meta(id: &str) -> Option<(String, Option<&'static str>)> {
    if let Some(agent) = find(id) {
        return Some((agent.name.to_string(), agent.login));
    }
    find_custom(id).map(|custom| (custom.name, None))
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
    ".cursor/bin",
    ".devin/bin",
    ".bun/bin",
    ".cargo/bin",
];
#[cfg(not(windows))]
const SYSTEM_BIN_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

fn search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    let home = std::env::var_os("HOME");
    #[cfg(windows)]
    let home = home.or_else(|| std::env::var_os("USERPROFILE"));
    if let Some(home) = home.map(PathBuf::from) {
        dirs.extend(USER_BIN_DIRS.iter().map(|dir| home.join(dir)));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    #[cfg(not(windows))]
    dirs.extend(SYSTEM_BIN_DIRS.iter().map(PathBuf::from));
    dirs.retain(|dir| dir.is_absolute());
    dirs
}

fn candidates(dir: &Path, binary: &str, windows_pathext: Option<&OsStr>) -> Vec<PathBuf> {
    let Some(pathext) = windows_pathext else {
        return vec![dir.join(binary)];
    };
    let spawnable = |ext: &str| {
        WINDOWS_SPAWNABLE_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext))
    };
    if let Some(ext) = Path::new(binary).extension().and_then(OsStr::to_str) {
        return if spawnable(&format!(".{ext}")) {
            vec![dir.join(binary)]
        } else {
            Vec::new()
        };
    }
    let mut extensions: Vec<String> = Vec::new();
    for ext in pathext.to_string_lossy().split(';').map(str::trim) {
        if spawnable(ext) && !extensions.iter().any(|seen| seen.eq_ignore_ascii_case(ext)) {
            extensions.push(ext.to_string());
        }
    }
    extensions
        .iter()
        .map(|ext| dir.join(format!("{binary}{ext}")))
        .collect()
}

fn windows_pathext() -> Option<std::ffi::OsString> {
    #[cfg(windows)]
    {
        Some(std::env::var_os("PATHEXT").unwrap_or_else(|| WINDOWS_DEFAULT_PATHEXT.into()))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

async fn resolve_in(
    dirs: Vec<PathBuf>,
    binary: &str,
    windows_pathext: Option<&OsStr>,
) -> Option<PathBuf> {
    let mut components = Path::new(binary).components();
    if !matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    ) {
        return None;
    }
    for dir in dirs {
        for candidate in candidates(&dir, binary, windows_pathext) {
            if is_executable(&candidate).await {
                return Some(candidate);
            }
        }
    }
    None
}

pub async fn resolve(binary: &str) -> Option<PathBuf> {
    let pathext = windows_pathext();
    resolve_in(search_dirs(), binary, pathext.as_deref()).await
}

pub async fn resolve_custom_binary(binary: &str) -> Option<PathBuf> {
    let path = Path::new(binary);
    if path.is_absolute() {
        return is_executable(path).await.then(|| path.to_path_buf());
    }
    resolve(binary).await
}

pub fn is_batch_shim(path: &Path) -> bool {
    path.extension()
        .and_then(OsStr::to_str)
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
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

pub fn install_hint(agent: &ExternalAgent) -> String {
    let runs_itself = |command: &str| command.split_whitespace().next() == Some(agent.binary);
    match agent.upgrade {
        Some(command) if !runs_itself(command) => format!(" Install it with `{command}`."),
        _ if !agent.website.is_empty() => format!(" See {} to install it.", agent.website),
        _ => String::new(),
    }
}

pub fn not_installed(agent: &ExternalAgent) -> ExecutorUnavailable {
    ExecutorUnavailable::new(
        ExecutorUnavailable::NOT_INSTALLED,
        format!("{} is not installed.{}", agent.name, install_hint(agent)),
    )
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
                .trim_end_matches('.')
                .to_string()
        })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionProbe {
    Known(String),
    TimedOut,
    Unknown,
}

async fn probe_cli_version_with_timeout(
    path: &Path,
    env: &BTreeMap<String, String>,
    timeout: Duration,
) -> VersionProbe {
    let mut command = foreign_command(path, &["--version"]);
    command.envs(env);
    let Ok(run) = codewhale_runtime::process_tree::contained_output_with_input_bounded(
        &mut command,
        Vec::new(),
        MAX_VERSION_BYTES,
        MAX_VERSION_STDERR_BYTES,
        tokio::time::sleep(timeout),
    )
    .await
    else {
        return VersionProbe::Unknown;
    };
    if run.stopped {
        return VersionProbe::TimedOut;
    }
    let text = String::from_utf8_lossy(&run.output.stdout);
    match version_token(text.lines().next().unwrap_or_default()) {
        Some(version) => VersionProbe::Known(version),
        None => VersionProbe::Unknown,
    }
}

pub async fn cli_version_probe_with_env(
    path: &Path,
    env: &BTreeMap<String, String>,
) -> VersionProbe {
    probe_cli_version_with_timeout(path, env, VERSION_TIMEOUT).await
}

pub async fn cli_version(path: &Path) -> Option<String> {
    cli_version_with_env(path, &BTreeMap::new()).await
}

pub async fn cli_version_with_env(path: &Path, env: &BTreeMap<String, String>) -> Option<String> {
    match cli_version_probe_with_env(path, env).await {
        VersionProbe::Known(version) => Some(version),
        VersionProbe::TimedOut | VersionProbe::Unknown => None,
    }
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
    let Some(agent) = find(id) else {
        if let Some(custom) = find_custom(id) {
            let program = resolve_custom_binary(&custom.binary).await.ok_or_else(|| {
                ExecutorUnavailable::new(
                    ExecutorUnavailable::NOT_INSTALLED,
                    format!("{} (`{}`) is not installed.", custom.name, custom.binary),
                )
            })?;
            return Ok(ExternalAgentLaunch {
                agent_id: custom.id,
                agent_name: custom.name,
                program,
                args: custom.args,
                env: custom.env,
            });
        }
        return Err(ExecutorUnavailable::new(
            ExecutorUnavailable::UNKNOWN_AGENT,
            format!("No external agent is named `{id}`."),
        ));
    };
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
    let cli = resolve(agent.binary)
        .await
        .ok_or_else(|| not_installed(agent))?;
    let mut env = agent.process_env();
    if agent.min_version.is_some() {
        let version = cli_version_with_env(&cli, &env).await;
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
            env,
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
            if let Some(key) = agent.adapter_cli_env.filter(|_| !is_batch_shim(&cli)) {
                env.insert(key.to_string(), cli.display().to_string());
            }
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuotaGate {
    pub enabled: bool,
    pub consented: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaSource {
    AgentReported,
    LocalSignin,
}

impl QuotaSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AgentReported => "agent_reported",
            Self::LocalSignin => "local_signin",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaUnknown {
    FlagOff,
    ConsentMissing,
    NotImplemented,
    UnknownAgent,
}

impl QuotaUnknown {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FlagOff => "flag_off",
            Self::ConsentMissing => "consent_missing",
            Self::NotImplemented => "not_implemented",
            Self::UnknownAgent => "unknown_agent",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct QuotaWindow {
    pub name: String,
    pub used_percent: Option<f64>,
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentQuota {
    Known {
        source: QuotaSource,
        windows: Vec<QuotaWindow>,
    },
    Unknown(QuotaUnknown),
}

pub enum LocalSigninReader {}

fn local_signin_reader(_agent: &ExternalAgent) -> Option<LocalSigninReader> {
    None
}

pub async fn local_signin_quota(agent: &ExternalAgent, gate: QuotaGate) -> AgentQuota {
    if !gate.enabled {
        return AgentQuota::Unknown(QuotaUnknown::FlagOff);
    }
    if !gate.consented {
        return AgentQuota::Unknown(QuotaUnknown::ConsentMissing);
    }
    match local_signin_reader(agent) {
        None => AgentQuota::Unknown(QuotaUnknown::NotImplemented),
        Some(reader) => match reader {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn quota_gate_reads_nothing_without_flag_and_consent_and_logs_nothing() {
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let writer = buffer.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(move || CaptureWriter(writer.clone()))
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let cases = [
            (QuotaGate::default(), QuotaUnknown::FlagOff),
            (
                QuotaGate {
                    enabled: false,
                    consented: true,
                },
                QuotaUnknown::FlagOff,
            ),
            (
                QuotaGate {
                    enabled: true,
                    consented: false,
                },
                QuotaUnknown::ConsentMissing,
            ),
            (
                QuotaGate {
                    enabled: true,
                    consented: true,
                },
                QuotaUnknown::NotImplemented,
            ),
        ];
        for agent in AGENTS {
            assert!(local_signin_reader(agent).is_none(), "{}", agent.id);
            for (gate, reason) in cases {
                assert_eq!(
                    local_signin_quota(agent, gate).await,
                    AgentQuota::Unknown(reason),
                    "{} {gate:?}",
                    agent.id
                );
            }
        }
        assert!(buffer.lock().unwrap().is_empty());
    }

    struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CaptureWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn names(paths: &[PathBuf]) -> Vec<String> {
        paths
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn windows_lookup_tries_only_spawnable_pathext_suffixes_in_order() {
        let dir = Path::new("C:/Users/Ünï Person/AppData/Roaming/npm");
        let pathext = OsStr::new(".COM;.EXE;.BAT;.CMD;.VBS;.JS;.PS1;.exe;;");
        assert_eq!(
            names(&candidates(dir, "claude", Some(pathext))),
            ["claude.COM", "claude.EXE", "claude.BAT", "claude.CMD"]
        );
        assert_eq!(
            names(&candidates(
                dir,
                "node",
                Some(OsStr::new(WINDOWS_DEFAULT_PATHEXT))
            )),
            ["node.COM", "node.EXE", "node.BAT", "node.CMD"]
        );
        assert_eq!(
            names(&candidates(dir, "node.exe", Some(pathext))),
            ["node.exe"]
        );
        assert!(candidates(dir, "cli.js", Some(pathext)).is_empty());
        assert!(candidates(dir, "claude", Some(OsStr::new(".PS1;.JS"))).is_empty());
        assert_eq!(names(&candidates(dir, "claude", None)), ["claude"]);
    }

    #[test]
    fn install_hint_never_offers_a_self_update_as_the_install() {
        let agy = find("antigravity").unwrap();
        assert_eq!(
            not_installed(agy).message,
            "Antigravity is not installed. See https://antigravity.google/docs/cli/acp/ to install it."
        );
        let claude = find("claude").unwrap();
        assert_eq!(
            install_hint(claude),
            " Install it with `npm i -g @anthropic-ai/claude-code@latest`."
        );
        assert_eq!(
            not_installed(claude).code,
            ExecutorUnavailable::NOT_INSTALLED
        );
    }

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_skips_unspawnable_files_and_handles_spaces_and_unicode() {
        let root = tempfile::tempdir().unwrap();
        let plain = root.path().join("not executable");
        let shims = root.path().join("dir with space/ünïcode bin");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::create_dir_all(&shims).unwrap();
        std::fs::write(plain.join("claude"), "").unwrap();
        let unix = script(&shims, "claude", "exit 0");
        let windows = script(&shims, "claude.CMD", "exit 0");
        let dirs = vec![plain.clone(), shims.clone()];
        assert_eq!(resolve_in(dirs.clone(), "claude", None).await, Some(unix));
        let found = resolve_in(
            dirs.clone(),
            "claude",
            Some(OsStr::new(WINDOWS_DEFAULT_PATHEXT)),
        )
        .await
        .unwrap();
        assert!(
            found
                .to_string_lossy()
                .eq_ignore_ascii_case(&windows.to_string_lossy()),
            "{}",
            found.display()
        );
        for hostile in ["", ".", "..", "../claude", "bin/claude", "/bin/sh"] {
            assert_eq!(
                resolve_in(dirs.clone(), hostile, None).await,
                None,
                "{hostile}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_version_reads_a_bounded_answer_from_paths_with_spaces() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("spaced ünï dir");
        std::fs::create_dir_all(&dir).unwrap();
        let good = script(
            &dir,
            "agent one",
            "echo \"agent v1.5.2 (build)\"; echo noise >&2",
        );
        assert_eq!(cli_version(&good).await.as_deref(), Some("1.5.2"));
        let sentence = script(&dir, "sentence", "echo \"GitHub Copilot CLI 1.0.82.\"");
        assert_eq!(cli_version(&sentence).await.as_deref(), Some("1.0.82"));
        let flood = script(
            &dir,
            "flood",
            "echo 9.9.9; head -c 20000 /dev/zero | tr '\\0' '1'; echo",
        );
        assert_eq!(cli_version(&flood).await, None);
        let failed = script(&dir, "failed", "exit 3");
        assert_eq!(cli_version(&failed).await, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cli_version_stops_a_hung_cli_and_everything_it_started() {
        let root = tempfile::tempdir().unwrap();
        let pid_file = root.path().join("grandchild.pid");
        let hung = script(
            root.path(),
            "hung",
            &format!("sleep 300 & echo $! > '{}'; sleep 300", pid_file.display()),
        );
        let timeout = Duration::from_secs(2);
        let started = std::time::Instant::now();
        assert_eq!(
            probe_cli_version_with_timeout(&hung, &BTreeMap::new(), timeout).await,
            VersionProbe::TimedOut
        );
        assert!(started.elapsed() < timeout + Duration::from_secs(4));
        let grandchild =
            codewhale_runtime::process_tree::read_pid_file(&pid_file, Duration::from_secs(5));
        assert!(codewhale_runtime::process_tree::wait_for_pid_exit(
            grandchild,
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn custom_registry_keeps_the_first_valid_id_and_reserves_builtins() {
        let entries = serde_json::json!([
            {"id":"opencode","name":"Replacement","binary":"replacement"},
            {"id":"fixture","name":"Fixture","binary":"fixture"},
            {"id":" trim ","name":"Whitespace","binary":"agent"},
            {"id":"bad","name":"Bad link","binary":"agent","website":"file:///local"},
            {"id":"bad","name":"Secret override","binary":"agent","env":{"API_TOKEN":"fixture"}},
            {"id":"bad","name":"Internal override","binary":"agent","env":{"codewhale_home":"local"}},
            {"id":"desk","name":"Desk","binary":"local-acp","args":["acp"],"env":{"AGENT_PROFILE":"desk"}},
            {"id":"desk","name":"Duplicate","binary":"other"}
        ]);
        let parsed = parse_custom_agents(&entries.to_string());
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].id, "desk");
        assert_eq!(parsed[0].args, ["acp"]);
        assert_eq!(parsed[0].env["AGENT_PROFILE"], "desk");
    }

    #[test]
    fn registry_rows_are_unique_with_verified_commands() {
        assert_eq!(AGENTS.len(), 24);
        assert!(find("gemini").is_none());
        let mut ids: Vec<&str> = AGENTS.iter().map(|agent| agent.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), AGENTS.len());
        for agent in AGENTS {
            assert!(
                agent.acp_args.is_some() || agent.adapter_binary.is_some(),
                "{}",
                agent.id
            );
            assert_eq!(
                Path::new(agent.binary).components().count(),
                1,
                "{}",
                agent.id
            );
        }
        for required in [
            "claude",
            "codex",
            "dsh",
            "antigravity",
            "cursor",
            "cline",
            "devin",
            "opencode",
            "goose",
            "junie",
            "amp",
            "auggie",
            "kimi",
            "mistral-vibe",
            "qoder",
            "codebuddy",
            "grok",
            "qwen-code",
            "copilot",
            "muse",
            "minimax-code",
            "deepagents",
            "factory-droid",
            "pi-acp",
        ] {
            assert!(find(required).is_some(), "missing {required}");
        }
        let grok = find("grok").unwrap();
        assert_eq!(
            (grok.login, grok.upgrade),
            (Some("grok login"), Some("grok update"))
        );
        let muse = find("muse").unwrap();
        assert_eq!(
            (
                muse.binary,
                muse.adapter,
                muse.adapter_binary,
                muse.adapter_cli_env,
                muse.login
            ),
            (
                "muse",
                Some("@bex-co/muse-code-acp"),
                Some("muse-code-acp"),
                Some("MUSE_CODE_EXECUTABLE"),
                Some("muse login"),
            )
        );
    }
}

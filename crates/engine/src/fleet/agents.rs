use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

pub const FIXTURE_AGENT_ENV: &str = "CODEWHALE_ACP_FIXTURE_AGENT";
pub const FIXTURE_AGENT_ID: &str = "fixture";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
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
                .to_string()
        })
}

pub async fn cli_version(path: &Path) -> Option<String> {
    let mut command = foreign_command(path, &["--version"]);
    let run = codewhale_runtime::process_tree::contained_output_with_input_bounded(
        &mut command,
        Vec::new(),
        MAX_VERSION_BYTES,
        MAX_VERSION_STDERR_BYTES,
        tokio::time::sleep(VERSION_TIMEOUT),
    )
    .await
    .ok()?;
    if run.stopped {
        return None;
    }
    let text = String::from_utf8_lossy(&run.output.stdout);
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
    let cli = resolve(agent.binary)
        .await
        .ok_or_else(|| not_installed(agent))?;
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
                .filter(|_| !is_batch_shim(&cli))
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
        let started = std::time::Instant::now();
        assert_eq!(cli_version(&hung).await, None);
        assert!(started.elapsed() < VERSION_TIMEOUT + Duration::from_secs(4));
        let grandchild =
            codewhale_runtime::process_tree::read_pid_file(&pid_file, Duration::from_secs(5));
        assert!(codewhale_runtime::process_tree::wait_for_pid_exit(
            grandchild,
            Duration::from_secs(5)
        ));
    }

    #[test]
    fn registry_rows_are_unique_with_verified_commands() {
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
        let grok = find("grok").unwrap();
        assert_eq!(
            (grok.login, grok.upgrade),
            (Some("grok login"), Some("grok update"))
        );
    }
}

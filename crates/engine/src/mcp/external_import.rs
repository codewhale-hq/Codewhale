//! Consent-gated external MCP imports.
//!
//! Discovery can scan `~/.claude.json`, project `.mcp.json`, and marketplace
//! manifests. Import approval saves connectors OFF; a separate enable action
//! is required before any connection. Provenance
//! (source path + content hash) is shown before import. `enabled=false` and
//! `disabled=true` on a source entry are hard blocks — those candidates never
//! become managed connectors even after a blanket approval.
//!
//! Design (Kimi session_a75a393a-a984-4f35-98d0-b78cfbdcf23f): keep discovery
//! pure and independent of the TUI; merge approved servers through the same
//! config write path as `/mcp add`.

use std::collections::HashMap;
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use codewhale_mcp::config::{McpConfig, McpServerConfig};

/// Where an import candidate came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalMcpSourceKind {
    ClaudeJson,
    ProjectMcpJson,
    Marketplace,
}

impl ExternalMcpSourceKind {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ClaudeJson => "claude.json",
            Self::ProjectMcpJson => ".mcp.json",
            Self::Marketplace => "marketplace",
        }
    }
}

/// One discovered server before consent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportCandidate {
    pub name: String,
    pub source_kind: ExternalMcpSourceKind,
    pub source_path: PathBuf,
    /// Hex sha256 of this server's name and entry as parsed from the source.
    /// It covers exactly what an approval imports, so review tokens and
    /// decisions survive unrelated rewrites of the same file (Claude Code
    /// rewrites `~/.claude.json` on nearly every run).
    pub content_hash: String,
    pub summary: String,
    /// When true the entry is present but must never connect.
    pub hard_blocked: bool,
    pub block_reason: Option<String>,
    pub server: McpServerConfig,
}

/// User decision for one candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportDecision {
    Approve,
    Decline,
    Skip,
}

/// Durable consent / decline record keyed by source path + hash.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ImportConsentStore {
    #[serde(default)]
    pub entries: HashMap<String, ConsentEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentEntry {
    pub source_path: String,
    pub content_hash: String,
    pub decision: ImportDecision,
    pub decided_at_unix: u64,
    pub servers: Vec<String>,
}

fn consent_key(path: &Path, hash: &str) -> String {
    format!("{}::{hash}", path.display())
}

/// Discover candidates from well-known external locations. Never connects.
pub fn discover_external_sources(
    home: &Path,
    workspace: &Path,
    marketplace_paths: &[PathBuf],
) -> Vec<ImportCandidate> {
    let mut out = Vec::new();
    let claude = home.join(".claude.json");
    if claude.is_file() {
        out.extend(discover_from_json_file(
            &claude,
            ExternalMcpSourceKind::ClaudeJson,
        ));
    }
    let project_mcp = workspace.join(".mcp.json");
    if project_mcp.is_file() {
        out.extend(discover_from_json_file(
            &project_mcp,
            ExternalMcpSourceKind::ProjectMcpJson,
        ));
    }
    for path in marketplace_paths {
        if path.is_file() {
            out.extend(discover_from_json_file(
                path,
                ExternalMcpSourceKind::Marketplace,
            ));
        }
    }
    out
}

fn discover_from_json_file(path: &Path, kind: ExternalMcpSourceKind) -> Vec<ImportCandidate> {
    checked_source(path, kind)
        .map(|(candidates, _)| candidates)
        .unwrap_or_default()
}

/// Most per-entry problems reported for one source; the rest are counted.
const MAX_PROBLEMS_PER_SOURCE: usize = 20;

/// Read one source. A file-level failure is an `Err`; an entry that fails
/// validation is reported in the second list (content-free) and skipped, so
/// one unsupported entry cannot hide its valid siblings. At most
/// [`MAX_PROBLEMS_PER_SOURCE`] entries are named, then one summary line.
fn checked_source(
    path: &Path,
    kind: ExternalMcpSourceKind,
) -> anyhow::Result<(Vec<ImportCandidate>, Vec<String>)> {
    super::validate_mcp_config_path(path)?;
    // `~/.claude.json` is Claude Code's whole state file (project history,
    // caches), routinely past the 1 MiB MCP config bound. Use the bound
    // `/import-claude` already reads it with.
    let max_bytes = match kind {
        ExternalMcpSourceKind::ClaudeJson => codewhale_net::import_claude::MAX_SOURCE_BYTES,
        _ => super::MAX_MCP_CONFIG_BYTES,
    };
    let Some(raw) = super::read_bounded_mcp_config_file(path, max_bytes)? else {
        return Ok((Vec::new(), Vec::new()));
    };
    let value: Value = serde_json::from_str(&raw)
        .map_err(|_| anyhow::anyhow!("Source is not valid JSON; contents omitted"))?;
    anyhow::ensure!(
        value
            .get("mcpServers")
            .or_else(|| value.get("servers"))
            .is_some_and(Value::is_object)
            || value.is_array(),
        "Source has no supported MCP server map"
    );
    let mut out = Vec::new();
    let mut problems = Vec::new();
    let mut unreported = 0usize;
    let mut report = |problem: String| {
        if problems.len() < MAX_PROBLEMS_PER_SOURCE {
            problems.push(problem);
        } else {
            unreported += 1;
        }
    };
    for (name, config) in extract_servers_map(&value) {
        if !(super::mcp_name_is_command_safe(&name) && name.len() <= 128) {
            report("An entry with an unsupported server name was skipped".to_string());
            continue;
        }
        let hash = entry_hash(&name, &config);
        let server = match checked_entry(config, value.is_array()) {
            Ok(server) => server,
            Err(error) => {
                report(format!("Server '{name}' was skipped: {error}"));
                continue;
            }
        };
        let hard_blocked = !server.is_enabled();
        out.push(ImportCandidate {
            summary: server_summary(&name, &server),
            name,
            source_kind: kind.clone(),
            source_path: path.to_path_buf(),
            content_hash: hash,
            hard_blocked,
            block_reason: hard_blocked.then(|| "Disabled at its source; cannot import".into()),
            server,
        });
    }
    if unreported > 0 {
        problems.push(format!("{unreported} more entries were skipped"));
    }
    Ok((out, problems))
}

fn entry_hash(name: &str, entry: &Value) -> String {
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend(entry.to_string().into_bytes());
    hex_sha256(&bytes)
}

/// Validate one server entry. Error messages never echo entry contents.
fn checked_entry(mut config: Value, from_array: bool) -> anyhow::Result<McpServerConfig> {
    if let Some(map) = config.as_object_mut() {
        if from_array {
            map.remove("name");
        }
        translate_entry_type(map)?;
    }
    let fields = config
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Invalid MCP entry; contents omitted"))?;
    const ALLOWED: &[&str] = &[
        "command",
        "args",
        "env",
        "cwd",
        "url",
        "allow_private_network",
        "transport",
        "connect_timeout",
        "execute_timeout",
        "read_timeout",
        "disabled",
        "enabled",
        "required",
        "enabled_tools",
        "disabled_tools",
        "headers",
        "env_headers",
        "env_http_headers",
        "bearer_token_env_var",
        "scopes",
        "oauth",
        "oauth_resource",
    ];
    anyhow::ensure!(
        fields.keys().all(|key| ALLOWED.contains(&key.as_str())),
        "Entry contains unsupported MCP fields; review it at its source"
    );
    if let Some(oauth) = fields.get("oauth").filter(|v| !v.is_null()) {
        anyhow::ensure!(
            oauth.as_object().is_some_and(|map| map
                .keys()
                .all(|key| matches!(key.as_str(), "client_id" | "issuer" | "client_metadata_url"))),
            "Entry contains unsupported OAuth fields"
        );
    }
    let server: McpServerConfig = serde_json::from_value(config)
        .map_err(|_| anyhow::anyhow!("Invalid MCP entry; contents omitted"))?;
    anyhow::ensure!(
        server.command.is_some() != server.url.is_some(),
        "MCP entry must have one target"
    );
    if let Some(command) = &server.command {
        anyhow::ensure!(
            !command.trim().is_empty() && !command.chars().any(char::is_control),
            "Invalid MCP command"
        );
    }
    if let Some(url) = &server.url {
        let parsed = reqwest::Url::parse(url).map_err(|_| anyhow::anyhow!("Invalid MCP URL"))?;
        anyhow::ensure!(
            matches!(parsed.scheme(), "http" | "https")
                && parsed.host_str().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none(),
            "Unsupported MCP URL"
        );
    }
    super::validate_mcp_transport(server.transport.as_deref())
        .map_err(|_| anyhow::anyhow!("Unsupported MCP transport"))?;
    Ok(server)
}

/// Claude Code writes `"type": "stdio" | "http" | "sse"` on every entry. Fold
/// it into Codewhale's shape: the target field already selects stdio versus
/// HTTP, and only legacy SSE needs an explicit `transport`. A `type` that
/// contradicts the entry's target or transport is refused, never guessed.
fn translate_entry_type(map: &mut serde_json::Map<String, Value>) -> anyhow::Result<()> {
    let Some(kind) = map.remove("type") else {
        return Ok(());
    };
    let kind = kind
        .as_str()
        .map(|kind| kind.trim().to_ascii_lowercase())
        .ok_or_else(|| anyhow::anyhow!("Unsupported MCP entry type"))?;
    let transport = map
        .get("transport")
        .map(|transport| transport.as_str().map(str::trim));
    let is_sse = transport.is_some_and(|t| t.is_some_and(|t| t.eq_ignore_ascii_case("sse")));
    match kind.as_str() {
        "stdio" => anyhow::ensure!(
            map.contains_key("command") && transport.is_none(),
            "MCP entry type does not match its target"
        ),
        "http" | "streamable-http" => anyhow::ensure!(
            map.contains_key("url") && !is_sse,
            "MCP entry type does not match its target"
        ),
        "sse" => {
            anyhow::ensure!(
                map.contains_key("url") && (transport.is_none() || is_sse),
                "MCP entry type does not match its target"
            );
            map.insert("transport".to_string(), Value::String("sse".to_string()));
        }
        _ => anyhow::bail!("Unsupported MCP entry type"),
    }
    Ok(())
}

fn extract_servers_map(value: &Value) -> Vec<(String, Value)> {
    // Claude / team: { "mcpServers": { name: {...} } }
    // Marketplace catalog: { "servers": { name: {...} } } or array of {name, ...}
    if let Some(map) = value
        .get("mcpServers")
        .or_else(|| value.get("servers"))
        .and_then(|v| v.as_object())
    {
        return map.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(arr) = value.as_array() {
        let mut out = Vec::new();
        for item in arr {
            let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
                continue;
            };
            out.push((name.to_string(), item.clone()));
        }
        return out;
    }
    Vec::new()
}

fn destination(server: &McpServerConfig) -> String {
    if let Some(url) = &server.url {
        return reqwest::Url::parse(url)
            .map(|url| url.origin().ascii_serialization())
            .unwrap_or_else(|_| "Invalid URL".into());
    }
    server
        .command
        .as_deref()
        .and_then(|command| Path::new(command).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unknown command".into())
}

fn server_summary(name: &str, server: &McpServerConfig) -> String {
    format!(
        "{name} — {} ({} arguments; credential values hidden)",
        destination(server),
        server.args.len()
    )
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Record decisions against the latest consent document under the shared
/// process lock. Malformed history is never silently replaced with empty state.
pub fn persist_decisions(
    path: &Path,
    candidates: &[ImportCandidate],
    decisions: &HashMap<String, ImportDecision>,
    now_unix: u64,
) -> anyhow::Result<()> {
    super::validate_mcp_config_path(path)?;
    codewhale_config::with_config_write_lock(path, |path| {
        let original = super::read_mcp_config_file(path)?;
        let mut raw: Value = match original.as_deref() {
            Some(raw) => serde_json::from_str(raw)
                .map_err(|_| anyhow::anyhow!("Invalid MCP consent history; contents omitted"))?,
            None => serde_json::json!({}),
        };
        anyhow::ensure!(raw.is_object(), "MCP consent history must be an object");
        let mut store: ImportConsentStore = if original.is_none() {
            ImportConsentStore::default()
        } else {
            serde_json::from_value(raw.clone())
                .map_err(|_| anyhow::anyhow!("Invalid MCP consent history; contents omitted"))?
        };
        let before = serde_json::to_value(&store)?;
        record_decisions(&mut store, candidates, decisions, now_unix);
        let after = serde_json::to_value(&store)?;
        super::apply_json_delta(&mut raw, &before, &after);
        let rendered = serde_json::to_vec_pretty(&raw)?;
        if rendered.len() as u64 > super::MAX_MCP_CONFIG_BYTES {
            anyhow::bail!("MCP consent history exceeds size limit");
        }
        codewhale_util::utils::write_atomic(path, &rendered)?;
        Ok(())
    })
}

/// Filter candidates that still need a user decision for this content hash.
#[allow(dead_code)] // used by future selector UI + unit tests
pub fn candidates_needing_consent(
    candidates: &[ImportCandidate],
    store: &ImportConsentStore,
) -> Vec<ImportCandidate> {
    candidates
        .iter()
        .filter(|c| {
            let key = consent_key(&c.source_path, &c.content_hash);
            match store.entries.get(&key) {
                Some(entry) if entry.decision == ImportDecision::Decline => false,
                Some(entry) if entry.decision == ImportDecision::Approve => {
                    // Re-prompt only when the specific server was not part of
                    // the prior approval list (partial approval).
                    !entry.servers.iter().any(|s| s == &c.name)
                }
                _ => true,
            }
        })
        .cloned()
        .collect()
}

/// Apply approvals: returns servers to merge into user mcp.json.
/// Hard-blocked candidates are never returned even if decision is Approve.
#[cfg(any(test, feature = "test-support"))]
pub fn apply_approved(
    candidates: &[ImportCandidate],
    decisions: &HashMap<String, ImportDecision>,
) -> Vec<(String, McpServerConfig, ImportCandidate)> {
    let mut out = Vec::new();
    for candidate in candidates {
        let decision = decisions
            .get(&candidate.name)
            .copied()
            .unwrap_or(ImportDecision::Skip);
        if decision != ImportDecision::Approve {
            continue;
        }
        if candidate.hard_blocked {
            continue;
        }
        out.push((
            candidate.name.clone(),
            candidate.server.clone(),
            candidate.clone(),
        ));
    }
    out
}

/// Record decisions in the consent store (including declines).
pub fn record_decisions(
    store: &mut ImportConsentStore,
    candidates: &[ImportCandidate],
    decisions: &HashMap<String, ImportDecision>,
    now_unix: u64,
) {
    // Group by source path + entry hash: one record per reviewed entry.
    let mut by_source: HashMap<(PathBuf, String), Vec<(&ImportCandidate, ImportDecision)>> =
        HashMap::new();
    for candidate in candidates {
        let decision = decisions
            .get(&candidate.name)
            .copied()
            .unwrap_or(ImportDecision::Skip);
        if decision == ImportDecision::Skip {
            continue;
        }
        by_source
            .entry((
                candidate.source_path.clone(),
                candidate.content_hash.clone(),
            ))
            .or_default()
            .push((candidate, decision));
    }
    for ((path, hash), group) in by_source {
        // If any approval exists, record Approve with the approved names;
        // pure decline groups record Decline.
        let any_approve = group.iter().any(|(_, d)| *d == ImportDecision::Approve);
        let decision = if any_approve {
            ImportDecision::Approve
        } else {
            ImportDecision::Decline
        };
        let servers: Vec<String> = group
            .iter()
            .filter(|(_, d)| *d == ImportDecision::Approve)
            .filter(|(c, _)| !c.hard_blocked)
            .map(|(c, _)| c.name.clone())
            .collect();
        let key = consent_key(&path, &hash);
        store.entries.insert(
            key,
            ConsentEntry {
                source_path: path.display().to_string(),
                content_hash: hash,
                decision,
                decided_at_unix: now_unix,
                servers,
            },
        );
    }
}

/// Merge approved servers into an existing McpConfig. Does not touch
/// hard-blocked entries. Returns names that were newly inserted.
#[cfg(any(test, feature = "test-support"))]
pub fn merge_approved_into_config(
    config: &mut McpConfig,
    approved: &[(String, McpServerConfig, ImportCandidate)],
) -> Vec<String> {
    let mut inserted = Vec::new();
    for (name, server, _) in approved {
        if config.servers.contains_key(name) {
            continue;
        }
        // Defense in depth: never insert disabled servers.
        if !server.is_enabled() {
            continue;
        }
        let mut server = server.clone();
        server.enabled = false;
        server.disabled = true;
        config.servers.insert(name.clone(), server);
        inserted.push(name.clone());
    }
    inserted
}

/// Human-readable provenance block for the selector / status panel.
#[cfg(any(test, feature = "test-support"))]
pub fn format_candidates_for_display(candidates: &[ImportCandidate]) -> String {
    if candidates.is_empty() {
        return "No external MCP sources found (or all already decided for current content)."
            .to_string();
    }
    let mut lines = vec![
        "External MCP import candidates (nothing is installed until you approve):".to_string(),
        String::new(),
    ];
    for (idx, c) in candidates.iter().enumerate() {
        let status = if c.hard_blocked { "BLOCKED" } else { "pending" };
        lines.push(format!(
            "  {}. [{}] {} — provenance: {} ({})",
            idx + 1,
            status,
            c.summary,
            c.source_kind.as_str(),
            c.source_path.display()
        ));
        lines.push(format!(
            "     content_hash: {}",
            &c.content_hash[..12.min(c.content_hash.len())]
        ));
        if let Some(reason) = &c.block_reason {
            lines.push(format!("     {reason}"));
        }
    }
    lines.push(String::new());
    lines.push(
        "Run /mcp import for a current reviewed approval token. Imports stay off until enabled separately."
            .to_string(),
    );
    lines.join("\n")
}

/// One authority shared by the native API and terminal import UI. Sources are
/// selected here; callers cannot supply arbitrary source paths to the API.
pub struct ImportContext<'a> {
    pub workspace: &'a Path,
    pub mcp_path: &'a Path,
    pub plugins: &'a codewhale_plugins::PluginRegistry,
    pub home: PathBuf,
    pub codewhale_home: PathBuf,
}
impl<'a> ImportContext<'a> {
    pub fn new(
        workspace: &'a Path,
        mcp_path: &'a Path,
        plugins: &'a codewhale_plugins::PluginRegistry,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            workspace,
            mcp_path,
            plugins,
            home: codewhale_paths::user_home()
                .ok_or_else(|| anyhow::anyhow!("Home directory unavailable"))?,
            codewhale_home: codewhale_config::codewhale_home()?,
        })
    }
    fn discover(&self) -> (Vec<ImportCandidate>, Vec<ImportProblem>) {
        let sources = [
            (
                self.home.join(".claude.json"),
                ExternalMcpSourceKind::ClaudeJson,
            ),
            (
                self.workspace.join(".mcp.json"),
                ExternalMcpSourceKind::ProjectMcpJson,
            ),
            (
                self.codewhale_home.join("mcp-marketplace.json"),
                ExternalMcpSourceKind::Marketplace,
            ),
        ];
        let mut candidates = Vec::new();
        let mut problems = Vec::new();
        for (path, kind) in sources {
            match checked_source(&path, kind.clone()) {
                Ok((found, skipped)) => {
                    candidates.extend(found);
                    problems.extend(skipped.into_iter().map(|message| ImportProblem {
                        source_kind: kind.clone(),
                        message,
                    }));
                }
                Err(_) => problems.push(ImportProblem { source_kind: kind,
                    message: "Source could not be safely read or contains unsupported configuration; review it at its source".into() }),
            }
        }
        (candidates, problems)
    }
    fn merged(&self) -> anyhow::Result<McpConfig> {
        super::load_config_with_workspace_and_plugins(self.mcp_path, self.workspace, self.plugins)
    }
}

#[derive(Debug, Serialize)]
pub struct ImportProblem {
    pub source_kind: ExternalMcpSourceKind,
    pub message: String,
}
#[derive(Debug, Serialize)]
pub struct ReviewedImport {
    pub id: String,
    pub name: String,
    pub source_kind: ExternalMcpSourceKind,
    pub source_path: PathBuf,
    pub content_hash: String,
    pub transport: &'static str,
    pub destination: String,
    pub argument_count: usize,
    pub env_keys: Vec<String>,
    pub header_keys: Vec<String>,
    pub credential_configured: bool,
    pub hard_blocked: bool,
    pub conflict: bool,
    pub review_token: String,
}
#[derive(Debug, Serialize)]
pub struct ImportPreview {
    pub revision: String,
    pub candidates: Vec<ReviewedImport>,
    pub problems: Vec<ImportProblem>,
}
#[derive(Debug, Serialize)]
pub struct ImportReceipt {
    pub name: String,
    pub decision: ImportDecision,
    pub imported: bool,
    pub enabled: bool,
    pub revision: String,
    pub consent_recorded: bool,
    pub warning: Option<String>,
}
fn candidate_id(candidate: &ImportCandidate) -> String {
    hex_sha256(
        format!(
            "{}\0{}\0{}",
            candidate.source_kind.as_str(),
            candidate.source_path.display(),
            candidate.name
        )
        .as_bytes(),
    )
}
fn source_blocked(context: &ImportContext<'_>, candidate: &ImportCandidate) -> bool {
    candidate.hard_blocked
        || (candidate.source_kind == ExternalMcpSourceKind::ProjectMcpJson
            && !codewhale_runtime::config::is_workspace_trusted(context.workspace))
}

pub fn preview_imports(context: &ImportContext<'_>) -> anyhow::Result<ImportPreview> {
    codewhale_config::with_config_write_lock(context.mcp_path, |path| {
        let revision = super::read_config_revision(path)?;
        let merged = context.merged()?;
        let (candidates, problems) = context.discover();
        let candidates = candidates
            .into_iter()
            .map(|candidate| {
                let id = candidate_id(&candidate);
                let server = &candidate.server;
                let mut env_keys: Vec<_> = server.env.keys().cloned().collect();
                env_keys.sort();
                let mut header_keys: Vec<_> = server
                    .headers
                    .keys()
                    .chain(server.env_headers.keys())
                    .cloned()
                    .collect();
                header_keys.sort();
                header_keys.dedup();
                ReviewedImport {
                    review_token: format!(
                        "mcp-import-v1:{id}:{}:{revision}",
                        candidate.content_hash
                    ),
                    id,
                    transport: if server.url.is_some() {
                        "http"
                    } else {
                        "stdio"
                    },
                    destination: destination(server),
                    argument_count: server.args.len(),
                    env_keys,
                    header_keys,
                    credential_configured: !server.env.is_empty()
                        || !server.headers.is_empty()
                        || !server.env_headers.is_empty()
                        || server.bearer_token_env_var.is_some()
                        || server.oauth.is_some()
                        || server.oauth_resource.is_some(),
                    hard_blocked: source_blocked(context, &candidate),
                    conflict: merged.servers.contains_key(&candidate.name),
                    name: candidate.name,
                    source_kind: candidate.source_kind,
                    source_path: candidate.source_path,
                    content_hash: candidate.content_hash,
                }
            })
            .collect();
        Ok(ImportPreview {
            revision,
            candidates,
            problems,
        })
    })
}

/// Re-read exact reviewed bytes inside the same config transaction as insertion.
/// Nothing connects here. Consent follows a successful write and cannot turn a
/// completed import into a false failed-write receipt.
pub fn apply_reviewed_import(
    context: &ImportContext<'_>,
    id: &str,
    hash: &str,
    revision: &str,
    decision: ImportDecision,
) -> anyhow::Result<ImportReceipt> {
    anyhow::ensure!(
        matches!(decision, ImportDecision::Approve | ImportDecision::Decline),
        "Choose approve or decline"
    );
    let (candidate, revision) = super::mutate_config(context.mcp_path, Some(revision), |config| {
        let (candidates, _) = context.discover();
        let candidate = candidates
            .into_iter()
            .find(|candidate| candidate_id(candidate) == id)
            .ok_or_else(|| {
                anyhow::anyhow!("Reviewed source is unavailable; refresh the import preview")
            })?;
        anyhow::ensure!(
            candidate.content_hash == hash,
            "Source changed; refresh the import preview"
        );
        if decision == ImportDecision::Approve {
            anyhow::ensure!(
                !source_blocked(context, &candidate),
                "Source is disabled or its workspace is untrusted"
            );
            anyhow::ensure!(
                !context.merged()?.servers.contains_key(&candidate.name)
                    && !config.servers.contains_key(&candidate.name),
                "A managed, project or plugin connector already uses this name"
            );
            let mut server = candidate.server.clone();
            server.enabled = false;
            server.disabled = true;
            config.servers.insert(candidate.name.clone(), server);
        }
        Ok(candidate)
    })?;
    let decisions = HashMap::from([(candidate.name.clone(), decision)]);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_secs());
    let consent_recorded = persist_decisions(
        &context.codewhale_home.join("mcp-import-consent.json"),
        std::slice::from_ref(&candidate),
        &decisions,
        now,
    )
    .is_ok();
    Ok(ImportReceipt { name: candidate.name, decision, imported: decision == ImportDecision::Approve,
        enabled: false, revision, consent_recorded,
        warning: (!consent_recorded).then(|| "The decision could not be added to import history; the configuration receipt above is authoritative".into()),
    })
}

pub fn parse_review_token(token: &str) -> anyhow::Result<(&str, &str, &str)> {
    let parts: Vec<_> = token.split(':').collect();
    anyhow::ensure!(
        parts.len() == 4
            && parts[0] == "mcp-import-v1"
            && parts[1..3]
                .iter()
                .all(|value| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
            && (parts[3] == "mcp-v1-absent"
                || parts[3].strip_prefix("mcp-v1-").is_some_and(
                    |hash| hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                )),
        "Approval needs a reviewed token, not a name. Run /mcp import and copy its approve or decline command"
    );
    Ok((parts[1], parts[2], parts[3]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_claude_json(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join(".claude.json");
        fs::write(&path, body).unwrap();
        path
    }

    fn with_import_context(test: impl FnOnce(&ImportContext<'_>)) {
        let _env = crate::test_support::lock_test_env();
        let root = tempdir().unwrap();
        let home = root.path().join("home");
        let workspace = root.path().join("workspace");
        let state = root.path().join("state");
        for path in [&home, &workspace, &state] {
            fs::create_dir_all(path).unwrap();
        }
        let path = state.join("mcp.json");
        let plugins = codewhale_plugins::PluginRegistry::empty(&workspace);
        test(&ImportContext {
            workspace: &workspace,
            mcp_path: &path,
            plugins: &plugins,
            home,
            codewhale_home: state,
        });
    }

    #[test]
    fn reviewed_import_rejects_changed_source_and_stale_revision() {
        with_import_context(|context| {
            let source =
                write_claude_json(&context.home, r#"{"mcpServers":{"x":{"command":"echo"}}}"#);
            let preview = preview_imports(context).unwrap();
            let row = &preview.candidates[0];
            fs::write(&source, r#"{"mcpServers":{"x":{"command":"changed"}}}"#).unwrap();
            assert!(
                apply_reviewed_import(
                    context,
                    &row.id,
                    &row.content_hash,
                    &preview.revision,
                    ImportDecision::Approve
                )
                .unwrap_err()
                .to_string()
                .contains("Source changed")
            );
            assert!(!context.mcp_path.exists());
            let preview = preview_imports(context).unwrap();
            let row = &preview.candidates[0];
            fs::write(context.mcp_path, r#"{"servers":{}}"#).unwrap();
            assert!(
                apply_reviewed_import(
                    context,
                    &row.id,
                    &row.content_hash,
                    &preview.revision,
                    ImportDecision::Approve
                )
                .unwrap_err()
                .is::<super::super::McpRevisionConflict>()
            );
        });
    }

    #[test]
    fn reviewed_import_is_off_and_reports_history_failure_after_commit() {
        with_import_context(|context| {
            write_claude_json(
                &context.home,
                r#"{"mcpServers":{"x":{"command":"never-launch-this"}}}"#,
            );
            let preview = preview_imports(context).unwrap();
            let row = &preview.candidates[0];
            fs::write(
                context.codewhale_home.join("mcp-import-consent.json"),
                "invalid history",
            )
            .unwrap();
            let receipt = apply_reviewed_import(
                context,
                &row.id,
                &row.content_hash,
                &preview.revision,
                ImportDecision::Approve,
            )
            .unwrap();
            assert!(receipt.imported);
            assert!(!receipt.enabled);
            assert!(!receipt.consent_recorded);
            assert!(receipt.warning.is_some());
            assert!(
                !super::super::load_config(context.mcp_path).unwrap().servers["x"].is_enabled()
            );
            let next = preview_imports(context).unwrap();
            assert!(next.candidates[0].conflict);
            assert!(
                apply_reviewed_import(
                    context,
                    &row.id,
                    &row.content_hash,
                    &next.revision,
                    ImportDecision::Approve
                )
                .is_err()
            );
        });
    }

    #[test]
    fn reviewed_decline_does_not_create_config_and_preview_hides_values() {
        with_import_context(|context| {
            write_claude_json(
                &context.home,
                r#"{"mcpServers":{"x":{"url":"https://example.test/private-secret?key=secret-value","headers":{"Authorization":"header-secret"},"env":{"TOKEN":"env-secret"}}}}"#,
            );
            let preview = preview_imports(context).unwrap();
            let rendered = serde_json::to_string(&preview).unwrap();
            for secret in [
                "private-secret",
                "secret-value",
                "header-secret",
                "env-secret",
            ] {
                assert!(!rendered.contains(secret));
            }
            let row = &preview.candidates[0];
            assert_eq!(row.destination, "https://example.test");
            assert!(row.credential_configured);
            assert!(parse_review_token(&row.review_token).is_ok());
            assert!(parse_review_token("x").is_err());
            let receipt = apply_reviewed_import(
                context,
                &row.id,
                &row.content_hash,
                &preview.revision,
                ImportDecision::Decline,
            )
            .unwrap();
            assert!(!receipt.imported);
            assert!(receipt.consent_recorded);
            assert_eq!(receipt.revision, preview.revision);
            assert!(!context.mcp_path.exists());
        });
    }

    #[test]
    fn reviewed_import_blocks_disabled_and_untrusted_project_sources() {
        with_import_context(|context| {
            write_claude_json(
                &context.home,
                r#"{"mcpServers":{"disabled":{"command":"echo","disabled":true}}}"#,
            );
            fs::write(
                context.workspace.join(".mcp.json"),
                r#"{"mcpServers":{"project":{"command":"echo"}}}"#,
            )
            .unwrap();
            let preview = preview_imports(context).unwrap();
            assert_eq!(preview.candidates.len(), 2);
            for row in preview.candidates {
                assert!(row.hard_blocked);
                assert!(
                    apply_reviewed_import(
                        context,
                        &row.id,
                        &row.content_hash,
                        &preview.revision,
                        ImportDecision::Approve
                    )
                    .is_err()
                );
            }
            assert!(!context.mcp_path.exists());
        });
    }

    #[test]
    fn reviewed_discovery_rejects_oversize_and_symlink_sources() {
        with_import_context(|context| {
            let path = context.home.join(".claude.json");
            fs::write(
                &path,
                vec![b' '; codewhale_net::import_claude::MAX_SOURCE_BYTES as usize + 1],
            )
            .unwrap();
            let preview = preview_imports(context).unwrap();
            assert!(preview.candidates.is_empty());
            assert_eq!(preview.problems.len(), 1);
            #[cfg(unix)]
            {
                fs::remove_file(&path).unwrap();
                let target = context.home.join("target.json");
                fs::write(&target, r#"{"mcpServers":{"x":{"command":"echo"}}}"#).unwrap();
                std::os::unix::fs::symlink(&target, &path).unwrap();
                let preview = preview_imports(context).unwrap();
                assert!(preview.candidates.is_empty());
                assert_eq!(preview.problems.len(), 1);
            }
        });
    }

    #[test]
    fn claude_code_entries_with_type_import_and_bad_siblings_are_skipped_alone() {
        with_import_context(|context| {
            write_claude_json(
                &context.home,
                r#"{"mcpServers":{
                    "local":{"type":"stdio","command":"npx","args":["-y","pkg"]},
                    "remote":{"type":"http","url":"https://mcp.example.test/mcp"},
                    "legacy":{"type":"sse","url":"https://sse.example.test/sse"},
                    "odd":{"type":"sse","command":"npx"},
                    "extra":{"command":"echo","unknownField":"secret-value"}
                }}"#,
            );
            let preview = preview_imports(context).unwrap();
            let mut names: Vec<_> = preview.candidates.iter().map(|c| c.name.as_str()).collect();
            names.sort_unstable();
            assert_eq!(names, ["legacy", "local", "remote"]);
            let messages: Vec<_> = preview
                .problems
                .iter()
                .map(|p| p.message.as_str())
                .collect();
            assert_eq!(messages.len(), 2, "{messages:?}");
            assert!(messages.iter().any(|m| m.contains("'odd'")));
            assert!(messages.iter().any(|m| m.contains("'extra'")));
            assert!(
                !serde_json::to_string(&preview)
                    .unwrap()
                    .contains("secret-value")
            );

            let candidates = discover_external_sources(&context.home, context.workspace, &[]);
            let legacy = candidates.iter().find(|c| c.name == "legacy").unwrap();
            assert_eq!(legacy.server.transport.as_deref(), Some("sse"));
            let remote = candidates.iter().find(|c| c.name == "remote").unwrap();
            assert_eq!(remote.server.transport, None);
        });
    }

    #[test]
    fn skipped_entries_from_one_source_are_capped() {
        with_import_context(|context| {
            let entries: Vec<String> = (0..MAX_PROBLEMS_PER_SOURCE + 5)
                .map(|i| format!(r#""bad{i}":{{"x":1}}"#))
                .collect();
            write_claude_json(
                &context.home,
                &format!(
                    r#"{{"mcpServers":{{{},"ok":{{"command":"echo"}}}}}}"#,
                    entries.join(",")
                ),
            );
            let preview = preview_imports(context).unwrap();
            assert_eq!(preview.candidates.len(), 1);
            assert_eq!(preview.problems.len(), MAX_PROBLEMS_PER_SOURCE + 1);
            assert_eq!(
                preview.problems.last().unwrap().message,
                "5 more entries were skipped"
            );
        });
    }

    #[test]
    fn unrelated_source_rewrites_keep_review_tokens_and_declines() {
        with_import_context(|context| {
            let source = write_claude_json(
                &context.home,
                r#"{"numStartups":1,"mcpServers":{"x":{"command":"echo"},"y":{"command":"echo"}}}"#,
            );
            let preview = preview_imports(context).unwrap();
            // Claude Code rewrites its state file on every run.
            fs::write(
                &source,
                r#"{"numStartups":2,"mcpServers":{"x":{"command":"echo"},"y":{"command":"echo"}}}"#,
            )
            .unwrap();
            let row = preview.candidates.iter().find(|c| c.name == "x").unwrap();
            let receipt = apply_reviewed_import(
                context,
                &row.id,
                &row.content_hash,
                &preview.revision,
                ImportDecision::Approve,
            )
            .unwrap();
            assert!(receipt.imported);

            let candidates = discover_from_json_file(&source, ExternalMcpSourceKind::ClaudeJson);
            let mut store = ImportConsentStore::default();
            let decisions = HashMap::from([("y".to_string(), ImportDecision::Decline)]);
            record_decisions(&mut store, &candidates, &decisions, 1);
            fs::write(
                &source,
                r#"{"numStartups":3,"mcpServers":{"x":{"command":"echo"},"y":{"command":"echo"}}}"#,
            )
            .unwrap();
            let refreshed = discover_from_json_file(&source, ExternalMcpSourceKind::ClaudeJson);
            let needing: Vec<_> = candidates_needing_consent(&refreshed, &store)
                .into_iter()
                .map(|c| c.name)
                .collect();
            // `y` stays declined; `x` was never decided in this store.
            assert_eq!(needing, ["x"]);
        });
    }

    #[test]
    fn claude_json_larger_than_the_mcp_config_bound_is_discovered() {
        with_import_context(|context| {
            let padding = "x".repeat(super::super::MAX_MCP_CONFIG_BYTES as usize * 3 / 2);
            write_claude_json(
                &context.home,
                &format!(r#"{{"history":"{padding}","mcpServers":{{"x":{{"command":"echo"}}}}}}"#),
            );
            let preview = preview_imports(context).unwrap();
            assert!(preview.problems.is_empty());
            assert_eq!(preview.candidates.len(), 1);
            assert_eq!(preview.candidates[0].name, "x");
        });
    }

    #[test]
    fn consent_transaction_preserves_other_sources_and_unknown_fields() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("consent.json");
        fs::write(&path, r#"{"entries":{},"extension":{"owner":"external"}}"#).unwrap();
        for name in ["first", "second"] {
            let source = dir.path().join(format!("{name}.json"));
            fs::write(
                &source,
                format!(r#"{{"mcpServers":{{"{name}":{{"command":"echo"}}}}}}"#),
            )
            .unwrap();
            let candidates = discover_from_json_file(&source, ExternalMcpSourceKind::ClaudeJson);
            let decisions = HashMap::from([(name.to_string(), ImportDecision::Approve)]);
            persist_decisions(&path, &candidates, &decisions, 1).unwrap();
        }
        let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["extension"]["owner"], "external");
        assert_eq!(raw["entries"].as_object().unwrap().len(), 2);
        fs::write(&path, "malformed-sensitive-history").unwrap();
        let error = persist_decisions(&path, &[], &HashMap::new(), 2).unwrap_err();
        assert!(!error.to_string().contains("malformed-sensitive-history"));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "malformed-sensitive-history"
        );
    }

    #[test]
    fn disabled_imported_server_never_merges() {
        let dir = tempdir().unwrap();
        let body = r#"{
            "mcpServers": {
                "ok": { "command": "npx", "args": ["-y", "good"], "enabled": true },
                "blocked": { "command": "npx", "args": ["-y", "bad"], "enabled": false }
            }
        }"#;
        write_claude_json(dir.path(), body);
        let candidates = discover_external_sources(dir.path(), dir.path(), &[]);
        assert_eq!(candidates.len(), 2);
        let blocked = candidates.iter().find(|c| c.name == "blocked").unwrap();
        assert!(blocked.hard_blocked);

        let mut decisions = HashMap::new();
        decisions.insert("ok".into(), ImportDecision::Approve);
        decisions.insert("blocked".into(), ImportDecision::Approve);
        let approved = apply_approved(&candidates, &decisions);
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].0, "ok");

        let mut config = McpConfig::default();
        let inserted = merge_approved_into_config(&mut config, &approved);
        assert_eq!(inserted, vec!["ok".to_string()]);
        assert!(!config.servers.contains_key("blocked"));
        assert!(!config.servers["ok"].is_enabled());
    }

    #[test]
    fn declined_consent_skips_reprompt_until_hash_changes() {
        let dir = tempdir().unwrap();
        let path = write_claude_json(
            dir.path(),
            r#"{"mcpServers":{"x":{"command":"echo","enabled":true}}}"#,
        );
        let candidates = discover_from_json_file(&path, ExternalMcpSourceKind::ClaudeJson);
        let mut store = ImportConsentStore::default();
        let mut decisions = HashMap::new();
        decisions.insert("x".into(), ImportDecision::Decline);
        record_decisions(&mut store, &candidates, &decisions, 1);
        let needing = candidates_needing_consent(&candidates, &store);
        assert!(needing.is_empty(), "declined should not re-prompt");

        // Content change → new hash → re-prompt.
        fs::write(
            &path,
            r#"{"mcpServers":{"x":{"command":"echo","args":["changed"],"enabled":true}}}"#,
        )
        .unwrap();
        let refreshed = discover_from_json_file(&path, ExternalMcpSourceKind::ClaudeJson);
        let needing = candidates_needing_consent(&refreshed, &store);
        assert_eq!(needing.len(), 1);
    }

    #[test]
    fn provenance_display_includes_source_and_hash() {
        let dir = tempdir().unwrap();
        write_claude_json(
            dir.path(),
            r#"{"mcpServers":{"hf":{"url":"https://example.com/mcp","enabled":true}}}"#,
        );
        let candidates = discover_external_sources(dir.path(), dir.path(), &[]);
        let text = format_candidates_for_display(&candidates);
        assert!(text.contains("provenance:"));
        assert!(text.contains("claude.json"));
        assert!(text.contains("content_hash:"));
        assert!(text.contains("nothing is installed until you approve"));
    }

    #[test]
    fn project_mcp_json_and_marketplace_are_discovered() {
        let home = tempdir().unwrap();
        let workspace = tempdir().unwrap();
        fs::write(
            workspace.path().join(".mcp.json"),
            r#"{"mcpServers":{"team":{"command":"uvx","args":["team-mcp"]}}}"#,
        )
        .unwrap();
        let market = home.path().join("market.json");
        fs::write(
            &market,
            r#"{"servers":{"shop":{"url":"https://market.example/mcp"}}}"#,
        )
        .unwrap();
        let candidates = discover_external_sources(home.path(), workspace.path(), &[market]);
        let names: Vec<_> = candidates.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"team"));
        assert!(names.contains(&"shop"));
    }
}

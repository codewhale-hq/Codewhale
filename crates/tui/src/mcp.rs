//! Async MCP (Model Context Protocol) Implementation
//!
//! This module provides full async support for MCP servers with:
//! - Connection pooling for server reuse
//! - Automatic tool discovery via `tools/list`
//! - Configurable timeouts per-server and globally

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::future::Future;
use std::io::{Read, Seek};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use futures_util::FutureExt;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

pub mod external_import;
mod headers;
mod http;
pub(crate) mod http_client;
pub mod oauth;
pub(crate) mod process_broker;
pub(crate) mod sse;
mod stdio;
mod streamable_http;
pub(crate) mod wire;

use self::http::HttpTransport;
use self::http_client::McpHttpAuth;
#[cfg(all(test, unix))]
use self::process_broker::STDIO_SHUTDOWN_GRACE;
use self::sse::SseTransport;
use self::stdio::StdioTransport;
pub(crate) use self::wire::MAX_MCP_RESPONSE_BYTES;
pub(crate) use self::wire::read_line_capped;
use self::wire::{
    is_mcp_connection_lost_error, is_mcp_session_rejected_error, is_mcp_stale_session_body,
};
use crate::network_policy::{Decision, NetworkPolicyDecider, host_from_url};
use crate::utils::write_atomic;

// === Error diagnostics helpers (#71) ===

/// Bytes of a non-2xx response body to surface in connection errors.
pub(crate) const ERROR_BODY_PREVIEW_BYTES: usize = 200;

/// Newest dated MCP protocol revision Codewhale advertises at `initialize` and
/// answers as the native MCP server.
pub(crate) const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
/// Dated MCP revisions accepted during negotiation, newest first. A peer
/// answering or requesting any of these continues the handshake.
pub(crate) const MCP_SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &[MCP_PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];
/// Revisions a *server* may answer our client `initialize` with, newest first.
/// Servers built on current SDKs (Pi, OMP and the 2025-11-25 TypeScript and
/// Python SDKs) answer `2025-11-25` even when offered an older revision; the
/// message shapes our client uses are unchanged in that revision, so ending
/// the handshake there only turns a working server into a failed row. Our own
/// MCP server still negotiates from `MCP_SUPPORTED_PROTOCOL_VERSIONS` alone.
pub(crate) const MCP_CLIENT_ACCEPTED_PROTOCOL_VERSIONS: &[&str] = &[
    "2025-11-25",
    MCP_PROTOCOL_VERSION,
    "2025-03-26",
    "2024-11-05",
];

fn validate_mcp_config_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        anyhow::bail!("MCP config path cannot be empty");
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        anyhow::bail!("MCP config path cannot contain '..' components");
    }
    Ok(())
}

/// Refuse to write a workspace's `.codewhale/mcp.json` through a link: when
/// the file lives under a `<workspace>/.codewhale`, no component from the
/// workspace down may be a link. The user's own home config (`~/.codewhale`) is
/// exempt, because users relocate it on purpose. Writers only: reading a linked
/// file is still allowed, as before.
fn reject_linked_workspace_state_path(path: &Path) -> Result<()> {
    let Some(root) = path
        .ancestors()
        .find(|ancestor| ancestor.file_name() == Some(std::ffi::OsStr::new(".codewhale")))
        .and_then(Path::parent)
    else {
        return Ok(());
    };
    if crate::config::effective_home_dir().is_some_and(|home| home == root) {
        return Ok(());
    }
    crate::fleet::files::reject_linked_path(root, path)
        .with_context(|| format!("MCP config {} is not safe to write", path.display()))
}

/// Expand `${NAME}` placeholders in an MCP config value from the process
/// environment. This lets secrets (API keys, bearer tokens, …) be supplied
/// through environment variables instead of being written in cleartext into
/// the MCP config file on disk.
///
/// On a missing or malformed placeholder the error names only the offending
/// variable, never the surrounding value, so a secret-bearing string is never
/// echoed into logs or error output.
fn expand_env_placeholders_with(
    value: &str,
    environment: Option<&crate::plugins::HostEnvironment>,
) -> Result<String> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            anyhow::bail!("unterminated environment placeholder in MCP config value");
        };
        let name = &after[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            anyhow::bail!("invalid environment placeholder in MCP config value");
        }
        let env_value = environment
            .map_or_else(|| std::env::var(name), |env| env.var(name))
            .with_context(|| {
                format!("environment variable {name} required by MCP config is not set")
            })?;
        out.push_str(&env_value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
fn expand_env_placeholders(value: &str) -> Result<String> {
    expand_env_placeholders_with(value, None)
}

/// Expand `${NAME}` placeholders across every value of an MCP config map
/// (e.g. the stdio child `env`). `context` only labels expansion errors so a
/// failure can be attributed to the right map.
fn expand_env_placeholders_map_with_environment(
    values: &HashMap<String, String>,
    context: &str,
    environment: Option<&crate::plugins::HostEnvironment>,
) -> Result<HashMap<String, String>> {
    let mut expanded = HashMap::with_capacity(values.len());
    for (key, value) in values {
        expanded.insert(
            key.clone(),
            expand_env_placeholders_with(value, environment)
                .with_context(|| format!("failed to expand MCP {context} value for {key}"))?,
        );
    }
    Ok(expanded)
}

#[cfg(test)]
fn expand_env_placeholders_map(
    values: &HashMap<String, String>,
    context: &str,
) -> Result<HashMap<String, String>> {
    expand_env_placeholders_map_with_environment(values, context, None)
}

fn expanded_mcp_stdio_env(config: &McpServerConfig) -> Result<HashMap<String, String>> {
    let environment = config
        .reviewed_plugin
        .as_ref()
        .map(|source| source.host_environment.as_ref());
    expand_env_placeholders_map_with_environment(&config.env, "env", environment)
}

/// Mirror the exact expanded and sanitized environment applied by the MCP
/// stdio spawn path, without constructing or starting a process.
fn mcp_stdio_child_env(config: &McpServerConfig) -> Result<Vec<(OsString, OsString)>> {
    let expanded_env = expanded_mcp_stdio_env(config)?;
    let overrides = crate::child_env::string_map_env(&expanded_env);
    Ok(if let Some(source) = config.reviewed_plugin.as_ref() {
        // Plugin reviews name every extra environment source explicitly. Do
        // not silently widen that consent to the compatibility-oriented MCP
        // bootstrap namespace (for example NPM_CONFIG_*).
        crate::child_env::sanitized_plugin_mcp_env_from(
            source.host_environment.entries().iter().cloned(),
            overrides,
        )
    } else {
        crate::child_env::sanitized_mcp_env(overrides)
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpCommandAvailability {
    Available,
    Missing,
    NotApplicable,
    NotChecked,
}

impl McpCommandAvailability {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Missing => "missing",
            Self::NotApplicable => "not_applicable",
            Self::NotChecked => "not_checked",
        }
    }
}

pub(crate) fn is_relative_stdio_path_arg(value: &str) -> bool {
    if value.is_empty() || value.starts_with('-') || value.contains("://") || value.starts_with('~')
    {
        return false;
    }
    let looks_like_path = value.contains('/') || value.contains('\\');
    if !looks_like_path {
        return false;
    }
    let bytes = value.as_bytes();
    let windows_absolute = value.starts_with("\\\\")
        || (bytes.len() >= 3 && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/'));
    !Path::new(value).is_absolute() && !windows_absolute
}

fn env_value<'a>(env: &'a [(OsString, OsString)], name: &str) -> Option<&'a OsStr> {
    env.iter()
        .rev()
        .find(|(key, _)| {
            #[cfg(windows)]
            {
                key.to_string_lossy().eq_ignore_ascii_case(name)
            }
            #[cfg(not(windows))]
            {
                key == OsStr::new(name)
            }
        })
        .map(|(_, value)| value.as_os_str())
}

#[cfg(unix)]
fn spawnable_command_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.is_file()
        && fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(windows)]
fn spawnable_command_file(path: &Path) -> bool {
    path.is_file() || (path.extension().is_none() && path.with_extension("exe").is_file())
}

#[cfg(not(any(unix, windows)))]
fn spawnable_command_file(path: &Path) -> bool {
    path.is_file()
}

fn path_candidate(dir: &Path, name: &str, cwd: Option<&Path>) -> PathBuf {
    #[cfg(unix)]
    {
        // Unix performs PATH lookup after applying Command::current_dir. That
        // includes empty PATH entries, which mean the child's current dir.
        if dir.is_relative()
            && let Some(cwd) = cwd
        {
            return cwd.join(dir).join(name);
        }
    }
    #[cfg(not(unix))]
    let _ = cwd;
    dir.join(name)
}

fn command_availability_on_path(
    name: &str,
    env: &[(OsString, OsString)],
    cwd: Option<&Path>,
) -> McpCommandAvailability {
    let Some(path) = env_value(env, "PATH") else {
        // On Unix execvp falls back to an OS-defined path. On Windows Rust's
        // resolver still checks system and parent locations. We cannot prove a
        // miss without reproducing platform internals, so remain conservative.
        return McpCommandAvailability::NotChecked;
    };
    for dir in std::env::split_paths(path) {
        let candidate = path_candidate(&dir, name, cwd);
        if spawnable_command_file(&candidate) {
            return McpCommandAvailability::Available;
        }
    }

    #[cfg(windows)]
    {
        // Windows Command resolution also checks the running executable's
        // directory, system directories, and the parent PATH after an explicit
        // child PATH. A static miss in the child PATH is therefore not proof
        // that spawn will fail. PATHEXT is intentionally not consulted: Rust
        // only supplies an omitted `.exe`; `.cmd`/`.bat` must be explicit.
        return McpCommandAvailability::NotChecked;
    }
    #[cfg(not(windows))]
    {
        McpCommandAvailability::Missing
    }
}

/// Inspect an MCP stdio command using the same expanded, sanitized environment
/// as the real spawn path, without starting the configured process.
pub(crate) fn static_mcp_command_availability(
    server: &McpServerConfig,
) -> Result<McpCommandAvailability> {
    if server.url.is_some() {
        return Ok(McpCommandAvailability::NotApplicable);
    }
    let Some(cmd) = server.command.as_deref() else {
        return Ok(McpCommandAvailability::NotChecked);
    };
    if cmd.is_empty() {
        return Ok(McpCommandAvailability::Missing);
    }

    // StdioTransport expands every configured env value before spawning, even
    // when the command itself is absolute. Mirror that failure boundary here.
    let child_env = mcp_stdio_child_env(server)?;
    let path = Path::new(cmd);
    let is_absolute = path.is_absolute() || cmd.starts_with('/');
    if is_absolute {
        return Ok(if spawnable_command_file(path) {
            McpCommandAvailability::Available
        } else {
            McpCommandAvailability::Missing
        });
    }

    if is_relative_stdio_path_arg(cmd) {
        let Some(cwd) = server.cwd.as_deref() else {
            return Ok(McpCommandAvailability::NotChecked);
        };
        return Ok(if spawnable_command_file(&cwd.join(path)) {
            McpCommandAvailability::Available
        } else {
            McpCommandAvailability::Missing
        });
    }

    Ok(command_availability_on_path(
        cmd,
        &child_env,
        server.cwd.as_deref(),
    ))
}

/// Mask a URL so any embedded credentials in the userinfo portion (e.g.
/// `https://user:secret@host`) are replaced with `***`. Failures fall back to
/// the original string so we don't lose context — we never want masking to
/// produce an empty error.
pub(crate) fn mask_url_secrets(url: &str) -> String {
    if let Ok(parsed) = reqwest::Url::parse(url) {
        let mut clone = parsed.clone();
        if !parsed.username().is_empty() || parsed.password().is_some() {
            let _ = clone.set_username("***");
            let _ = clone.set_password(Some("***"));
        }
        if parsed.query().is_some() {
            clone.set_query(Some("***"));
        }
        clone.set_fragment(None);
        return clone.to_string();
    }
    url.to_string()
}

/// Redact the userinfo segment (`username[:password]@…` portion) from
/// a proxy URL so it can be safely included in `tracing::warn!` output
/// without leaking the
/// password into the on-disk log. URLs without userinfo are returned
/// unchanged. Garbage input (no `://` scheme separator) is also returned
/// unchanged — the malformed-URL warning path is the only caller, so an
/// unparseable input is already the failure case.
fn redact_proxy_userinfo(proxy_url: &str) -> String {
    let Some(scheme_end) = proxy_url.find("://") else {
        return proxy_url.to_string();
    };
    let after_scheme = scheme_end + 3;
    // The userinfo segment ends at the next `@`, but only if that `@`
    // comes before the next `/`, `?`, or `#` (otherwise the `@` is in a
    // path / query and the URL has no userinfo at all).
    let rest = &proxy_url[after_scheme..];
    let at_idx = rest.find('@');
    let path_idx = rest.find(['/', '?', '#']);
    let userinfo_end = match (at_idx, path_idx) {
        (Some(a), Some(p)) if a < p => Some(a),
        (Some(a), None) => Some(a),
        _ => None,
    };
    if let Some(end) = userinfo_end {
        let mut out = String::with_capacity(proxy_url.len());
        out.push_str(&proxy_url[..after_scheme]);
        out.push_str("***@");
        out.push_str(&rest[end + 1..]);
        out
    } else {
        proxy_url.to_string()
    }
}

fn redact_values_after_ascii_needle(
    output: &mut String,
    needle: &str,
    terminates: impl Fn(char) -> bool,
) {
    let needle = needle.as_bytes();
    let mut search_from = 0_usize;
    while search_from.saturating_add(needle.len()) <= output.len() {
        let Some(relative) = output.as_bytes()[search_from..]
            .windows(needle.len())
            .position(|candidate| candidate.eq_ignore_ascii_case(needle))
        else {
            break;
        };
        let value_start = search_from + relative + needle.len();
        let value_end = output[value_start..]
            .char_indices()
            .find(|(_, ch)| terminates(*ch))
            .map_or(output.len(), |(offset, _)| value_start + offset);
        if value_end == value_start {
            if value_start == output.len() {
                break;
            }
            // The empty value is already safe. Advance over its ASCII
            // separator so a second occurrence later in the body is found.
            search_from = value_start + 1;
            continue;
        }
        output.replace_range(value_start..value_end, "***");
        search_from = value_start + 3;
    }
}

/// Mask obvious token-like substrings in a body excerpt before surfacing it.
/// Every occurrence is replaced, not only the first one.
fn redact_body_preview(body: &str) -> String {
    let mut out = body.to_string();
    redact_values_after_ascii_needle(&mut out, "bearer ", |ch| {
        ch.is_whitespace() || ch == '"' || ch == ','
    });
    for needle in ["api_key=", "apikey=", "api-key=", "token="] {
        redact_values_after_ascii_needle(&mut out, needle, |ch| {
            ch.is_whitespace() || ch == '&' || ch == '"' || ch == ','
        });
    }
    out
}

/// Read at most `max_bytes` of a reqwest response body and produce a
/// single-line excerpt suitable for an error message. The stream is dropped as
/// soon as the cap is reached, so an unbounded or never-ending error response
/// cannot make diagnostics retain the entire body. Best-effort — if the body
/// can't be read, returns the literal string `<no body>`.
pub(crate) async fn bounded_body_excerpt(response: reqwest::Response, max_bytes: usize) -> String {
    use futures_util::StreamExt;

    let declared_truncated = response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64);
    let mut stream = response.bytes_stream();
    let mut body = Vec::with_capacity(max_bytes.min(8 * 1024));
    let mut truncated = declared_truncated;

    while body.len() < max_bytes {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = max_bytes - body.len();
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
        if body.len() == max_bytes {
            // For a chunked response there is no length that proves EOF. Stop
            // now rather than polling an attacker-controlled stream again.
            truncated = true;
        }
    }

    if body.is_empty() {
        return "<no body>".to_string();
    }

    let one_line = String::from_utf8_lossy(&body).replace(['\n', '\r'], " ");
    let suffix = if truncated { "…" } else { "" };
    format!("{}{}", redact_body_preview(&one_line), suffix)
}

fn invalid_json_preview(bytes: &[u8]) -> String {
    let body_text = String::from_utf8_lossy(bytes);
    if body_text.is_empty() {
        return "<empty>".to_string();
    }

    let trimmed: String = body_text.chars().take(ERROR_BODY_PREVIEW_BYTES).collect();
    let suffix = if body_text.chars().count() > ERROR_BODY_PREVIEW_BYTES {
        "…"
    } else {
        ""
    };
    let one_line = trimmed.replace(['\n', '\r'], " ");
    format!("{}{}", redact_body_preview(&one_line), suffix)
}

// === Configuration Types ===

/// Full MCP configuration from mcp.json
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct McpConfig {
    #[serde(default)]
    pub timeouts: McpTimeouts,
    #[serde(default, alias = "mcpServers")]
    pub servers: HashMap<String, McpServerConfig>,
}

/// Global timeout configuration
#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[allow(clippy::struct_field_names)]
pub struct McpTimeouts {
    #[serde(default = "default_connect_timeout")]
    pub connect_timeout: u64,
    #[serde(default = "default_execute_timeout")]
    pub execute_timeout: u64,
    #[serde(default = "default_read_timeout")]
    pub read_timeout: u64,
}

/// Covers spawn, `initialize` and the first `tools/list` together. A cold
/// `uvx`/`npx` start resolves and downloads the package inside this window,
/// which routinely takes longer than 10 s; 30 s matches Codex, opencode, OMP
/// and Claude Code's `MCP_TIMEOUT` default.
fn default_connect_timeout() -> u64 {
    30
}
// 30 minutes: an MCP tool call legitimately runs minutes — builds, test
// suites, scrapes, remote jobs. The old 60s default returned "timed out" to
// the model for healthy-but-slow tools, which then retried and compounded
// the cost. Per-server and global `execute_timeout` overrides still win.
// Scope note: `prompts/get` also routes through `effective_execute_timeout`,
// so it inherits this default; its server-side template work is normally
// fast, but the override knob is the intended way to keep it tight.
fn default_execute_timeout() -> u64 {
    1800
}
fn default_read_timeout() -> u64 {
    120
}

impl Default for McpTimeouts {
    fn default() -> Self {
        Self {
            connect_timeout: default_connect_timeout(),
            execute_timeout: default_execute_timeout(),
            read_timeout: default_read_timeout(),
        }
    }
}

/// Configuration for a single MCP server
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    pub url: Option<String>,
    /// Explicit operator authority for private DNS names at this exact origin.
    /// Ignored for model-added runtime servers.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_private_network: bool,
    /// Optional explicit HTTP transport override.
    ///
    /// By default URL-based MCP servers use Streamable HTTP first and fall
    /// back to legacy SSE only when the server rejects Streamable HTTP with
    /// a known incompatible status. Set this to `"sse"` for legacy SSE
    /// endpoints that must start with a long-lived GET endpoint discovery
    /// stream and cannot accept an initial POST to the configured URL.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default)]
    pub connect_timeout: Option<u64>,
    #[serde(default)]
    pub execute_timeout: Option<u64>,
    #[serde(default)]
    pub read_timeout: Option<u64>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lazy: bool,
    #[serde(default)]
    pub enabled_tools: Vec<String>,
    #[serde(default)]
    pub disabled_tools: Vec<String>,
    /// Extra HTTP headers sent with every request to this MCP server.
    /// Only the HTTP transports (streamable HTTP today; SSE in a
    /// follow-up) honor this — `command`-based stdio servers ignore it.
    ///
    /// Mirrors the `headers` field that Claude Code, Codex, and
    /// OpenCode already accept in their MCP config formats. Use it to
    /// authenticate against gateways that require a Bearer token or
    /// API key, e.g.:
    ///
    /// ```jsonc
    /// "huggingface": {
    ///     "url": "https://huggingface.co/api/mcp",
    ///     "headers": { "Authorization": "Bearer ${HF_TOKEN}" }
    /// }
    /// ```
    ///
    /// Header keys and values are passed through as-is — we do not
    /// substitute environment variables in v0.8.31. If you store a
    /// real token here, the value lives in plain text in
    /// `~/.deepseek/mcp.json`; treat that file with the same care
    /// as any other secret-bearing config.
    #[serde(default)]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
    /// HTTP headers whose values are read from environment variables at request
    /// time. This keeps common bearer/API-token integrations out of mcp.json.
    #[serde(default, alias = "env_http_headers")]
    #[serde(skip_serializing_if = "HashMap::is_empty")]
    pub env_headers: HashMap<String, String>,
    /// Environment variable containing a bearer token. When present and set,
    /// CodeWhale sends `Authorization: Bearer <value>` for URL-based servers.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bearer_token_env_var: Option<String>,
    /// OAuth scopes requested during `codewhale mcp login`.
    #[serde(default)]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// OAuth client override for MCP servers that require a pre-registered
    /// public client instead of dynamic registration.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpServerOAuthConfig>,
    /// Optional RFC 8707 resource parameter appended to the authorization URL.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_resource: Option<String>,
    /// In-memory provenance for MCP servers contributed by a reviewed plugin
    /// bundle. This is never deserialized from or serialized into user config:
    /// only the trusted plugin merge adapter may attach it.
    #[serde(skip)]
    pub(crate) reviewed_plugin: Option<ReviewedPluginMcpSource>,
    /// Only the runtime registration boundary can attach this provenance.
    #[serde(skip)]
    pub(crate) runtime_added: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewedPluginMcpSource {
    authority: crate::plugins::types::PluginAuthority,
    approved_remote_endpoint: Option<String>,
    approved_remote_origin: Option<String>,
    host_environment: Arc<crate::plugins::HostEnvironment>,
    native_mcp: Option<crate::extension_host::native_mcp::NativeMcpRef>,
}

impl ReviewedPluginMcpSource {
    /// The plugin bundle that contributes this server. The panel names it
    /// rather than parsing the synthesized `plugin-<len>-<plugin>-<server>`
    /// key, which is an encoding detail and not a contract.
    pub(crate) fn plugin_name(&self) -> &str {
        &self.authority.plugin_name
    }

    fn from_authority(
        authority: crate::plugins::types::PluginAuthority,
        remote_endpoint: Option<&str>,
        host_environment: Arc<crate::plugins::HostEnvironment>,
    ) -> Result<Self> {
        let (approved_remote_endpoint, approved_remote_origin) = match remote_endpoint {
            Some(endpoint) => reviewed_remote_endpoint_identity(endpoint)
                .map(|(endpoint, origin)| (Some(endpoint), Some(origin)))?,
            None => (None, None),
        };
        Ok(Self {
            authority,
            approved_remote_endpoint,
            approved_remote_origin,
            host_environment,
            native_mcp: None,
        })
    }

    pub(crate) fn validate_before_stdio_spawn(&self, server_name: &str) -> Result<()> {
        self.validate_before_use(server_name, "spawn")
    }

    pub(crate) fn prepare_stdio_launch(
        &self,
        server_name: &str,
        command: &str,
        args: &[String],
        cwd: Option<&Path>,
    ) -> Result<ReviewedStdioLaunch> {
        self.validate_before_stdio_spawn(server_name)?;
        let staged_root = self
            .authority
            .staged_manifest
            .parent()
            .context("reviewed plugin stage manifest has no parent")?;
        let validated = crate::plugins::manifest::PluginManifest::validate_from_path(
            &self.authority.staged_manifest,
        )
        .map_err(|_| anyhow::anyhow!("reviewed plugin stage could not be opened for launch"))?;
        if validated.content_hash != self.authority.content_hash
            || validated.capability_hash != self.authority.capability_hash
        {
            anyhow::bail!("reviewed plugin stage changed before stdio launch");
        }

        let mut launch = ReviewedStdioLaunch {
            command: std::ffi::OsString::from(command),
            args: args.iter().map(std::ffi::OsString::from).collect(),
            cwd: cwd.map(Path::to_path_buf),
            opened_files: Vec::new(),
            #[cfg(unix)]
            cwd_fd: None,
        };
        if Path::new(command).is_absolute() {
            launch.bind_command(staged_root, Path::new(command), &validated.file_hashes)?;
        }
        // Darwin descriptor paths lose Node's module filename, package.json
        // context and relative-import directory (#5916). Keep the staged path
        // for .js/.cjs and multi-file .mjs entries, after bind_file verifies
        // their bytes. Node reopens these paths; this is not atomic descriptor
        // execution. Sibling modules already use the reviewed staged paths.
        #[cfg(target_os = "macos")]
        let node_entry_index = is_node_command(command)
            .then(|| node_script_entry_index(args))
            .flatten()
            .filter(|&index| {
                let path = Path::new(&args[index]);
                path.is_absolute()
                    && path.starts_with(staged_root)
                    && path.extension().is_some_and(|extension| {
                        matches!(extension.to_str(), Some("mjs" | "js" | "cjs"))
                    })
            });
        #[cfg(target_os = "macos")]
        let node_entry_keeps_path = node_entry_index.is_some_and(|index| {
            node_entry_needs_staged_path(
                staged_root,
                Path::new(&args[index]),
                &validated.file_hashes,
            )
        });
        for (index, argument) in args.iter().enumerate() {
            let path = Path::new(argument);
            if path.is_absolute() && path.starts_with(staged_root) && path.is_file() {
                let bound = launch.bind_file(staged_root, path, &validated.file_hashes)?;
                #[cfg(target_os = "macos")]
                if node_entry_keeps_path && node_entry_index == Some(index) {
                    continue;
                }
                launch.args[index] = bound;
            }
        }
        #[cfg(target_os = "macos")]
        if let Some(entry_index) = node_entry_index
            && !node_entry_keeps_path
        {
            launch.args = node_esm_descriptor_args(&launch.args, entry_index);
        }
        if let Some(cwd) = cwd {
            if !cwd.starts_with(staged_root) {
                anyhow::bail!("reviewed plugin stdio cwd escaped its staged root");
            }
            launch.bind_cwd(cwd)?;
        }
        // A final authority pass detects source/stage and capability drift
        // while handles were opened. Retained Node entry paths and imports
        // are reopened after this check; their owner-only, read-only stage is
        // not an atomic handle binding or an OS sandbox.
        self.validate_before_stdio_spawn(server_name)?;
        Ok(launch)
    }

    fn required_capability(&self) -> crate::plugins::activation::PluginActivationCapability {
        if self.native_mcp.is_some() {
            return crate::plugins::activation::PluginActivationCapability::Native;
        }
        if self.approved_remote_endpoint.is_some() {
            crate::plugins::activation::PluginActivationCapability::McpRemote
        } else {
            crate::plugins::activation::PluginActivationCapability::McpStdio
        }
    }

    pub(crate) fn validate_before_use(&self, server_name: &str, operation: &str) -> Result<()> {
        if let Some(native) = self.native_mcp.as_ref() {
            native.validate().map_err(anyhow::Error::msg)?;
        }
        let remediation = format!(
            "Run `/plugin reload`, inspect `/plugin show {0}`, then repeat the displayed trust command and `/plugin enable {0}` before retrying",
            self.authority.plugin_name
        );
        crate::plugins::registry::verify_plugin_component_authority(
            &self.authority,
            self.required_capability(),
        )
        .map_err(|reason| {
            anyhow::anyhow!(
                "Refusing to {operation} MCP server '{server_name}' from plugin bundle `{}`: {reason}. {remediation}",
                self.authority.plugin_name
            )
        })
    }

    fn validate_remote_endpoint(&self, server_name: &str, endpoint: &str) -> Result<()> {
        let (endpoint, origin) = reviewed_remote_endpoint_identity(endpoint)?;
        if self.approved_remote_endpoint.as_deref() != Some(endpoint.as_str())
            || self.approved_remote_origin.as_deref() != Some(origin.as_str())
        {
            anyhow::bail!(
                "Refusing MCP server '{server_name}': its remote endpoint no longer matches the reviewed plugin origin"
            );
        }
        Ok(())
    }

    fn catalog_is_current(&self) -> bool {
        if self
            .native_mcp
            .as_ref()
            .is_some_and(|native| native.validate().is_err())
        {
            return false;
        }

        // Catalog exposure is an authority boundary too: stale tool, prompt,
        // or resource descriptions can steer the model even when the later
        // operation would be denied. Revalidate both the mutable reviewed
        // source and the Codewhale-owned stage before publishing any entry.
        crate::plugins::registry::verify_plugin_component_authority(
            &self.authority,
            self.required_capability(),
        )
        .is_ok()
    }
}

/// Preserve .js package type lookup, .cjs module semantics and multi-file
/// .mjs relative imports. A lone .mjs retains the existing descriptor launch.
#[cfg(target_os = "macos")]
fn node_entry_needs_staged_path(
    staged_root: &Path,
    entry: &Path,
    file_hashes: &std::collections::BTreeMap<PathBuf, String>,
) -> bool {
    let Ok(entry) = entry.strip_prefix(staged_root) else {
        return false;
    };
    if entry
        .extension()
        .is_some_and(|extension| matches!(extension.to_str(), Some("js" | "cjs")))
    {
        return true;
    }
    file_hashes.keys().any(|path| {
        path != entry
            && path.extension().is_some_and(|extension| {
                matches!(
                    extension.to_string_lossy().as_ref(),
                    "mjs" | "js" | "cjs" | "node" | "wasm"
                )
            })
    })
}

/// Find the script operand, never a preload's value or an argument belonging
/// to an earlier script. Unknown option layouts get no Node-specific rewrite;
/// the generic reviewed-file binder still applies.
#[cfg(target_os = "macos")]
fn node_script_entry_index(args: &[impl AsRef<std::ffi::OsStr>]) -> Option<usize> {
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        let argument = argument.as_ref().to_str()?;
        match argument {
            "--" => return (index + 1 < args.len()).then_some(index + 1),
            "-" | "-e" | "--eval" | "-p" | "--print" | "--run" | "--test" | "-c" | "--check"
            | "-i" | "--interactive" | "--input-type" => return None,
            "-r"
            | "--require"
            | "--import"
            | "--loader"
            | "--experimental-loader"
            | "-C"
            | "--conditions"
            | "--max-old-space-size"
            | "--stack-size" => index += 2,
            "--no-warnings"
            | "--trace-warnings"
            | "--trace-deprecation"
            | "--no-deprecation"
            | "--enable-source-maps"
            | "--preserve-symlinks"
            | "--preserve-symlinks-main"
            | "--abort-on-uncaught-exception"
            | "--expose-gc"
            | "--jitless" => index += 1,
            _ if argument.starts_with("--eval=")
                || argument.starts_with("--print=")
                || argument.starts_with("--run=")
                || argument.starts_with("--input-type=") =>
            {
                return None;
            }
            _ if argument.starts_with("--") && argument.contains('=') => index += 1,
            _ if argument.starts_with('-') => return None,
            _ => return Some(index),
        }
    }
    None
}

fn is_node_command(command: &str) -> bool {
    Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, "node" | "nodejs" | "node.exe" | "nodejs.exe"))
}

/// Rewrite a Node launch so a reviewed `.mjs` entrypoint keeps ESM semantics
/// after Darwin's reviewed-launch binding replaced its staged path with an
/// inherited `/dev/fd/N` descriptor.
///
/// Node determines the entrypoint module type from its filename and a
/// descriptor path has no extension, so `node /dev/fd/N` exits without
/// evaluating the module. `--experimental-default-type=module` used to fix
/// that but was removed from current Node releases (Node 25 rejects it as a
/// bad option, which killed the child before the MCP handshake). `--import`
/// has loaded its specifier as an ES module on every supported release, so
/// the reviewed bytes are imported by descriptor once, `-e ""` supplies an
/// empty main, and the descriptor path is echoed after `--` so
/// `process.argv[1]` and the script's own arguments keep the file-mode shape.
/// Node options that preceded the entrypoint stay in front; anything after it
/// is passed through untouched. When the `.mjs` file is not the first
/// positional argument it is not the entrypoint and the launch is left alone.
#[cfg(target_os = "macos")]
fn node_esm_descriptor_args(
    args: &[std::ffi::OsString],
    entry_index: usize,
) -> Vec<std::ffi::OsString> {
    if node_script_entry_index(args) != Some(entry_index) {
        return args.to_vec();
    }
    let bound_entry = args[entry_index].clone();
    let prefix_end = entry_index - usize::from(entry_index > 0 && args[entry_index - 1] == "--");
    let mut rewritten: Vec<std::ffi::OsString> = args[..prefix_end].to_vec();
    rewritten.push(std::ffi::OsString::from("--import"));
    rewritten.push(bound_entry.clone());
    rewritten.push(std::ffi::OsString::from("-e"));
    rewritten.push(std::ffi::OsString::from(""));
    rewritten.push(std::ffi::OsString::from("--"));
    rewritten.push(bound_entry);
    rewritten.extend(args[entry_index + 1..].iter().cloned());
    rewritten
}

pub(crate) struct ReviewedStdioLaunch {
    pub(crate) command: std::ffi::OsString,
    pub(crate) args: Vec<std::ffi::OsString>,
    pub(crate) cwd: Option<PathBuf>,
    /// Kept for the child lifetime. Windows opens deny write/delete sharing;
    /// Unix normally uses inherited descriptors; macOS Node entries needing
    /// module path context are hash-checked here, then reopened by path.
    pub(crate) opened_files: Vec<fs::File>,
    #[cfg(unix)]
    pub(crate) cwd_fd: Option<fs::File>,
}

impl ReviewedStdioLaunch {
    fn bind_command(
        &mut self,
        staged_root: &Path,
        path: &Path,
        expected_hashes: &std::collections::BTreeMap<PathBuf, String>,
    ) -> Result<()> {
        let bound_path = self.bind_file(staged_root, path, expected_hashes)?;
        #[cfg(not(target_os = "macos"))]
        {
            self.command = bound_path;
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            use std::os::unix::fs::FileExt as _;

            // Darwin devfs deliberately rejects execve("/dev/fd/N"). Bind
            // reviewed scripts by running the interpreter declared in their
            // exact hashed shebang and passing the inherited descriptor as
            // input. Native Mach-O bundle commands have no fexecve/execveat
            // equivalent on Darwin, so fail closed and require the manifest
            // to name a bare interpreter with the bundle file as an argument.
            let file = self
                .opened_files
                .last()
                .context("reviewed command handle disappeared")?;
            let mut prefix = [0_u8; 4_096];
            let read = file
                .read_at(&mut prefix, 0)
                .context("read reviewed command shebang")?;
            let prefix = &prefix[..read];
            let line_end = prefix
                .iter()
                .position(|byte| *byte == b'\n')
                .unwrap_or(prefix.len());
            let line = std::str::from_utf8(&prefix[..line_end])
                .context("reviewed script shebang is not UTF-8")?;
            let shebang = line.strip_prefix("#!").map(str::trim).filter(|s| !s.is_empty())
                .context(
                    "Darwin cannot execute a reviewed native bundle command by descriptor; use a shebang script or declare a bare interpreter command plus the script argument",
                )?;
            let mut words = shlex::split(shebang)
                .context("reviewed script shebang could not be parsed safely")?;
            let interpreter = words
                .first()
                .filter(|word| Path::new(word).is_absolute())
                .context("reviewed script shebang interpreter must be absolute")?
                .clone();
            words.remove(0);
            let mut args = words
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            args.push(bound_path);
            args.append(&mut self.args);
            self.command = std::ffi::OsString::from(interpreter);
            self.args = args;
            Ok(())
        }
    }

    fn bind_file(
        &mut self,
        staged_root: &Path,
        path: &Path,
        expected_hashes: &std::collections::BTreeMap<PathBuf, String>,
    ) -> Result<std::ffi::OsString> {
        let relative = path
            .strip_prefix(staged_root)
            .context("reviewed plugin executable escaped its staged root")?;
        let expected = expected_hashes
            .get(relative)
            .context("reviewed plugin executable is absent from its byte inventory")?;
        let mut file = open_reviewed_launch_file(path)?;
        let mut hasher = sha2::Sha256::new();
        hasher.update(b"codewhale-plugin-file-bytes-v1\0");
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .context("read reviewed launch file")?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        if &actual != expected {
            anyhow::bail!("reviewed plugin executable bytes changed before spawn");
        }
        file.seek(std::io::SeekFrom::Start(0))
            .context("rewind reviewed launch file after verification")?;

        #[cfg(unix)]
        let launch_path = {
            use std::os::fd::AsRawFd as _;
            let fd = file.as_raw_fd();
            // SAFETY: `fd` is owned by `file`; clearing only FD_CLOEXEC keeps
            // that same descriptor available across the imminent exec.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0
            {
                anyhow::bail!("failed to inherit reviewed plugin executable descriptor");
            }
            #[cfg(target_os = "linux")]
            let prefix = "/proc/self/fd";
            #[cfg(not(target_os = "linux"))]
            let prefix = "/dev/fd";
            std::ffi::OsString::from(format!("{prefix}/{fd}"))
        };

        #[cfg(not(unix))]
        let launch_path = path.as_os_str().to_os_string();

        self.opened_files.push(file);
        Ok(launch_path)
    }

    fn bind_cwd(&mut self, cwd: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(cwd)
                .context("open reviewed plugin cwd without following links")?;
            self.cwd_fd = Some(file);
            self.cwd = None;
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
            let file = fs::OpenOptions::new()
                .read(true)
                .share_mode(0x0000_0001) // FILE_SHARE_READ only
                .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
                .open(cwd)
                .context("open reviewed plugin cwd without write/delete sharing")?;
            let metadata = file
                .metadata()
                .context("inspect reviewed plugin cwd handle")?;
            if !metadata.is_dir() || metadata.file_attributes() & 0x0000_0400 != 0 {
                anyhow::bail!("reviewed plugin cwd is a reparse point or non-directory");
            }
            self.opened_files.push(file);
        }
        Ok(())
    }
}

#[cfg(unix)]
fn open_reviewed_launch_file(path: &Path) -> Result<fs::File> {
    crate::plugins::manifest::open_bundle_file(path)
        .context("open reviewed launch file without following links")
}

#[cfg(windows)]
fn open_reviewed_launch_file(path: &Path) -> Result<fs::File> {
    crate::plugins::manifest::open_bundle_file(path)
        .context("open reviewed launch file without links, hard links, or write/delete sharing")
}

#[cfg(all(not(unix), not(windows)))]
fn open_reviewed_launch_file(path: &Path) -> Result<fs::File> {
    fs::File::open(path).context("open reviewed launch file")
}

fn reviewed_remote_endpoint_identity(endpoint: &str) -> Result<(String, String)> {
    let endpoint =
        reqwest::Url::parse(endpoint).context("reviewed plugin MCP endpoint is invalid")?;
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        anyhow::bail!("reviewed plugin MCP endpoint must not contain user information");
    }
    if endpoint.query().is_some() || endpoint.fragment().is_some() {
        anyhow::bail!("reviewed plugin MCP endpoint must not contain a query or fragment");
    }
    let origin = reviewed_remote_origin(&endpoint)
        .ok_or_else(|| anyhow::anyhow!("reviewed plugin MCP endpoint has an unsafe origin"))?;
    Ok((endpoint.to_string(), origin))
}

fn reviewed_remote_origin(endpoint: &reqwest::Url) -> Option<String> {
    if !endpoint.username().is_empty() || endpoint.password().is_some() {
        return None;
    }
    let host = endpoint.host_str()?;
    let allowed_scheme = endpoint.scheme() == "https"
        || (endpoint.scheme() == "http"
            && (host.eq_ignore_ascii_case("localhost")
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|address| address.is_loopback())));
    allowed_scheme.then(|| endpoint.origin().ascii_serialization())
}

fn reviewed_redirect_matches_origin(endpoint: &reqwest::Url, approved_origin: &str) -> bool {
    reviewed_remote_origin(endpoint).as_deref() == Some(approved_origin)
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpServerOAuthConfig {
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

fn default_enabled() -> bool {
    true
}

impl McpServerConfig {
    pub fn effective_connect_timeout(&self, global: &McpTimeouts) -> u64 {
        self.connect_timeout.unwrap_or(global.connect_timeout)
    }

    pub fn effective_execute_timeout(&self, global: &McpTimeouts) -> u64 {
        self.execute_timeout.unwrap_or(global.execute_timeout)
    }

    pub fn effective_read_timeout(&self, global: &McpTimeouts) -> u64 {
        self.read_timeout.unwrap_or(global.read_timeout)
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled && !self.disabled
    }

    pub fn is_tool_enabled(&self, tool_name: &str) -> bool {
        let allowed = if self.enabled_tools.is_empty() {
            true
        } else {
            self.enabled_tools.iter().any(|t| t == tool_name)
        };
        if !allowed {
            return false;
        }
        !self.disabled_tools.iter().any(|t| t == tool_name)
    }
}

// === MCP Tool Definition ===

/// Tool discovered from an MCP server
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpTool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "inputSchema", default)]
    pub input_schema: serde_json::Value,
    /// Behaviour hints the server declares (MCP `ToolAnnotations`). They are
    /// claims, not proof: only a reviewed plugin's hints relax approval, and
    /// only toward what the plugin review already covers (CW-11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<McpToolAnnotations>,
}

/// The subset of MCP `ToolAnnotations` the approval path reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct McpToolAnnotations {
    #[serde(
        rename = "readOnlyHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub read_only_hint: Option<bool>,
    #[serde(
        rename = "destructiveHint",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub destructive_hint: Option<bool>,
}

/// How the approval path may treat one model-visible MCP tool, from its
/// server's declared annotations (CW-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolApprovalHint {
    /// A reviewed, enabled plugin declares the tool read-only and not
    /// destructive: it runs without a prompt, like the built-in read tools.
    TrustedReadOnly,
    /// The server declares the tool destructive: session-wide auto-approve
    /// does not cover it, so each call keeps its prompt.
    Destructive,
}

/// Annotation-derived approval hints for the MCP tools of every live
/// catalog, keyed by model tool name. Filled where the catalog is built
/// (`McpPool::to_api_tools`, once per turn) and read by the side-effect-free
/// call preparation, which has no pool handle.
///
/// Known limitation: the map is process-wide. Two pools in one process that
/// expose the same model tool name from different servers overwrite each
/// other's hint; the last catalog built wins. Plugin servers carry
/// synthesized `plugin-…` names, so this needs a user server deliberately
/// named like a plugin server.
static MCP_TOOL_APPROVAL_HINTS: std::sync::LazyLock<RwLock<HashMap<String, McpToolApprovalHint>>> =
    std::sync::LazyLock::new(|| RwLock::new(HashMap::new()));

/// The approval hint recorded for a model-visible MCP tool name, if any.
#[must_use]
pub fn mcp_tool_approval_hint(model_tool_name: &str) -> Option<McpToolApprovalHint> {
    MCP_TOOL_APPROVAL_HINTS.read().get(model_tool_name).copied()
}

#[cfg(test)]
pub(crate) fn set_mcp_tool_approval_hint_for_test(
    model_tool_name: &str,
    hint: Option<McpToolApprovalHint>,
) {
    let mut hints = MCP_TOOL_APPROVAL_HINTS.write();
    match hint {
        Some(hint) => hints.insert(model_tool_name.to_string(), hint),
        None => hints.remove(model_tool_name),
    };
}

fn approval_hint_for(tool: &McpTool, reviewed_plugin: bool) -> Option<McpToolApprovalHint> {
    let annotations = tool.annotations.unwrap_or_default();
    // An absent destructiveHint defaults to true in the MCP spec, but only
    // when readOnlyHint is false; a read-only tool is not destructive.
    if annotations.destructive_hint == Some(true) {
        return Some(McpToolApprovalHint::Destructive);
    }
    (reviewed_plugin && annotations.read_only_hint == Some(true))
        .then_some(McpToolApprovalHint::TrustedReadOnly)
}

const MCP_TOOL_DESCRIPTION_MAX_CHARS: usize = 80;

/// Format an optional MCP tool description for terminal list surfaces.
///
/// CLI and TUI callers share this helper so both stay single-line and truncate
/// on Unicode scalar boundaries rather than slicing UTF-8 bytes.
pub(crate) fn format_mcp_tool_description(description: Option<&str>) -> String {
    let Some(first_line) = description
        .and_then(|description| description.split(['\r', '\n']).next())
        .map(str::trim)
        .filter(|description| !description.is_empty())
    else {
        return String::new();
    };

    let mut chars = first_line.chars();
    let summary: String = chars
        .by_ref()
        .take(MCP_TOOL_DESCRIPTION_MAX_CHARS)
        .collect();
    if chars.next().is_some() {
        format!(": {summary}...")
    } else {
        format!(": {summary}")
    }
}

/// Resource discovered from an MCP server
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpResource {
    pub uri: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

/// Resource template discovered from an MCP server
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpResourceTemplate {
    #[serde(rename = "uriTemplate")]
    pub uri_template: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(rename = "mimeType", default)]
    pub mime_type: Option<String>,
}

/// Fail-closed RFC 6570 subset used only as an authorization check. Literal,
/// simple (`{id}`), and reserved (`{+path}`) expansions cover the common MCP
/// resource templates. More elaborate operators remain listable but are not
/// callable until their expansion semantics are implemented exactly.
///
/// `None` is the fail-closed answer: a template this subset cannot express
/// matches nothing.
fn resource_template_pattern(template: &str) -> Option<String> {
    let mut pattern = String::from("^");
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        pattern.push_str(&regex::escape(&rest[..start]));
        let end = rest[start + 1..].find('}')?;
        let expression = &rest[start + 1..start + 1 + end];
        let (reserved, variables) = match expression.strip_prefix('+') {
            Some(variables) => (true, variables),
            None => (false, expression),
        };
        if variables.is_empty()
            || variables.split(',').any(|variable| {
                variable.is_empty()
                    || !variable
                        .chars()
                        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
            })
        {
            return None;
        }
        let atom = if reserved { ".+" } else { "[^/?#]+" };
        for (index, _) in variables.split(',').enumerate() {
            if index > 0 {
                pattern.push(',');
            }
            pattern.push_str(atom);
        }
        rest = &rest[start + end + 2..];
    }
    if rest.contains('}') {
        return None;
    }
    pattern.push_str(&regex::escape(rest));
    pattern.push('$');
    Some(pattern)
}

/// `template`'s anchored pattern, compiled once and reused.
///
/// This runs per URI per advertised template, while the template itself is
/// fixed by the server's listing, so compiling it on every call was pure
/// repetition. `None` still means "matches nothing" (#6213 T7).
fn compiled_resource_template(template: &str) -> Option<Arc<regex::Regex>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<String, Option<Arc<regex::Regex>>>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .entry(template.to_string())
        .or_insert_with(|| {
            resource_template_pattern(template)
                .and_then(|pattern| regex::Regex::new(&pattern).ok())
                .map(Arc::new)
        })
        .clone()
}

fn resource_uri_matches_template(uri: &str, template: &str) -> bool {
    compiled_resource_template(template).is_some_and(|regex| regex.is_match(uri))
}

/// Prompt discovered from an MCP server
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpPrompt {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub arguments: Vec<McpPromptArgument>,
}

/// Argument for an MCP prompt
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpPromptArgument {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

// === Connection State ===

/// State of an MCP connection
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Connecting,
    Ready,
    Disconnected,
}

/// MCP server capabilities advertised in the initialize response.
///
/// Each flag records presence of the corresponding MCP capability object. The
/// surrounding [`McpServerCapabilityMetadata`] preserves the important
/// distinction between an advertised empty set and a legacy server that did
/// not send capability metadata at all.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct McpServerCapabilities {
    pub tools: bool,
    pub resources: bool,
    pub prompts: bool,
}

impl McpServerCapabilities {
    fn from_initialize_response(response: &serde_json::Value) -> Option<Self> {
        let capabilities = response.get("result")?.get("capabilities")?.as_object()?;
        Some(Self {
            tools: capabilities.contains_key("tools"),
            resources: capabilities.contains_key("resources"),
            prompts: capabilities.contains_key("prompts"),
        })
    }
}

/// Provenance-aware capability metadata for a manager snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpServerCapabilityMetadata {
    /// The server supplied a spec-shaped `capabilities` object at initialize.
    Advertised(McpServerCapabilities),
    /// A connected legacy server omitted metadata, so bounded discovery probes
    /// remain enabled for backward compatibility.
    LegacyFallback,
    /// The server has not completed initialization, so no truthful capability
    /// claim can be made yet.
    #[default]
    NotObserved,
}

fn response_result<'a>(
    response: &'a serde_json::Value,
    method: &str,
    suppress_server_details: bool,
) -> Result<Option<&'a serde_json::Value>> {
    if let Some(error) = response.get("error") {
        if suppress_server_details {
            anyhow::bail!(
                "Reviewed plugin MCP server returned an error in '{method}' (server details suppressed to protect environment-backed credentials)"
            );
        }
        anyhow::bail!("MCP error in '{method}': {error}");
    }
    Ok(response.get("result"))
}

async fn run_optional_discovery<F, T>(
    server: &str,
    method: &str,
    timeout: Duration,
    discovery: F,
) -> Option<T>
where
    F: Future<Output = Result<T>>,
{
    match tokio::time::timeout(timeout, discovery).await {
        Ok(Ok(value)) => return Some(value),
        Ok(Err(error)) => {
            tracing::warn!(
                target: "mcp",
                server,
                method,
                error = %error,
                "optional MCP discovery failed; continuing with available capabilities"
            );
        }
        Err(error) => {
            tracing::warn!(
                target: "mcp",
                server,
                method,
                ?timeout,
                error = %error,
                "optional MCP discovery timed out; continuing with available capabilities"
            );
        }
    }
    None
}

// === McpConnection - Async Connection Management ===

// === Transport Trait ===

#[async_trait::async_trait]
pub(crate) trait McpTransport: Send + Sync {
    async fn send(&mut self, msg: Vec<u8>) -> Result<()>;
    /// The SDK stdio adapter keeps the actual person-decision inside Rust.
    /// Existing transports retain their current byte send and attestation path.
    async fn send_decided(
        &mut self,
        msg: Vec<u8>,
        _decision: Option<&crate::core::engine::HumanDecision>,
        _timeout: Duration,
    ) -> Result<()> {
        self.send(msg).await
    }

    async fn recv(&mut self) -> Result<Vec<u8>>;

    /// Record the protocol revision negotiated at `initialize`. Only the
    /// Streamable HTTP transport uses it (the `MCP-Protocol-Version` header on
    /// subsequent requests); stdio and legacy SSE have no header channel, so
    /// the default is a no-op.
    fn set_protocol_version(&mut self, _version: &str) {}

    /// The last non-empty line the server wrote to stderr, for naming why a
    /// handshake was refused. Only stdio children have a stderr; a reviewed
    /// plugin's is never retained.
    async fn last_stderr_line(&self) -> Option<String> {
        None
    }

    /// Synchronous, best-effort liveness probe consulted by
    /// [`McpConnection::is_ready`] so a crashed stdio child stops reading
    /// as "ready" before the next call fails (#6187). Must never block and
    /// never spawn — a contended lock reads as alive; the next call observes
    /// the death. The default is "alive"; Streamable HTTP has no long-lived
    /// channel to observe, while legacy SSE reports its closed event stream.
    fn probe_dead(&self) -> bool {
        false
    }

    /// Graceful shutdown — stdio transports send SIGTERM to the child and
    /// give it a brief window to exit before tokio's `kill_on_drop` fires
    /// SIGKILL as the backstop. Default is a no-op for non-stdio transports
    /// that have no child process. Whalescale#420.
    async fn shutdown(&mut self) {}
}

const MAX_MCP_CATALOG_PAGES: usize = 64;
const MAX_MCP_CATALOG_ITEMS: usize = 4_096;
const MAX_MCP_CATALOG_BYTES: usize = 32 * 1024 * 1024;

/// One retained catalog generation, shared by every advertised family.
struct McpCatalogBudget {
    pages: usize,
    items: usize,
    bytes: usize,
    // Cursor namespaces are independent across list methods.
    seen_cursors: HashSet<(&'static str, String)>,
    // Optional-method recovery cannot turn a budget refusal into success.
    refused: Option<String>,
}

impl McpCatalogBudget {
    fn new() -> Self {
        Self {
            pages: 0,
            items: 0,
            bytes: 0,
            seen_cursors: HashSet::new(),
            refused: None,
        }
    }

    fn ensure_available(&self) -> Result<()> {
        if let Some(reason) = self.refused.as_ref() {
            anyhow::bail!("{reason}");
        }
        Ok(())
    }

    fn observe_page(
        &mut self,
        method: &'static str,
        result: &serde_json::Value,
        item_count: usize,
    ) -> Result<Option<String>> {
        self.ensure_available()?;
        self.pages = self.pages.saturating_add(1);
        self.items = self.items.saturating_add(item_count);
        self.bytes = self.bytes.saturating_add(serde_json::to_vec(result)?.len());
        let refusal = if self.pages > MAX_MCP_CATALOG_PAGES {
            Some(format!(
                "{method} exceeded the {MAX_MCP_CATALOG_PAGES}-page catalogue limit"
            ))
        } else if self.items > MAX_MCP_CATALOG_ITEMS {
            Some(format!(
                "{method} exceeded the {MAX_MCP_CATALOG_ITEMS}-item catalogue limit"
            ))
        } else if self.bytes > MAX_MCP_CATALOG_BYTES {
            Some(format!(
                "{method} exceeded the {MAX_MCP_CATALOG_BYTES}-byte aggregate catalogue limit"
            ))
        } else {
            None
        };
        if let Some(reason) = refusal {
            self.refused = Some(reason);
            self.ensure_available()?;
        }
        let cursor = result
            .get("nextCursor")
            .and_then(|value| value.as_str())
            .map(str::to_owned);
        if let Some(cursor) = cursor.as_ref()
            && !self.seen_cursors.insert((method, cursor.clone()))
        {
            self.refused = Some(format!("{method} repeated pagination cursor; aborting"));
            self.ensure_available()?;
        }
        Ok(cursor)
    }
}

pub(crate) fn is_legacy_sse_transport(config: &McpServerConfig) -> bool {
    config
        .transport
        .as_deref()
        .map(|transport| transport.trim().eq_ignore_ascii_case("sse"))
        .unwrap_or(false)
}

pub fn validate_mcp_transport(transport: Option<&str>) -> Result<()> {
    let Some(transport) = transport else {
        return Ok(());
    };
    if transport.trim().eq_ignore_ascii_case("sse") {
        return Ok(());
    }
    anyhow::bail!("Unsupported MCP transport '{transport}'. Supported values: sse");
}

fn response_id_matches(id: Option<&serde_json::Value>, expected_id: &str) -> bool {
    let Some(id) = id else {
        return false;
    };
    if id.as_str() == Some(expected_id) {
        return true;
    }
    id.as_u64()
        .map(|id| id.to_string() == expected_id)
        .unwrap_or(false)
}

// === McpConnection - Async Connection Management ===

/// Manages a single async connection to an MCP server
pub struct McpConnection {
    name: String,
    transport: Box<dyn McpTransport>,
    tools: Vec<McpTool>,
    resources: Vec<McpResource>,
    resource_templates: Vec<McpResourceTemplate>,
    prompts: Vec<McpPrompt>,
    request_id: AtomicU64,
    state: ConnectionState,
    config: McpServerConfig,
    server_capabilities: Option<McpServerCapabilities>,
    /// Sanitized `instructions` from this connection's `initialize` result.
    /// A reconnect builds a new connection, so guidance never outlives the
    /// handshake that supplied it.
    instructions: Option<String>,
    discovery_timeout: Duration,
    read_timeout_secs: u64,
    cancel_token: tokio_util::sync::CancellationToken,
    authority_revocation_reason: Arc<std::sync::Mutex<Option<String>>>,
    authority_watch: Option<tokio::task::JoinHandle<()>>,
    /// Pool catalog generation that created/last authorized this connection.
    /// Directly constructed test connections use zero until inserted.
    catalog_generation: u64,
    /// Key this host shares with the built-in Computer Use plugin over the
    /// connection itself, to attest a person's card decision on a call.
    decision_key: Option<[u8; 32]>,
}

struct PendingAuthorityWatch {
    handle: Option<tokio::task::JoinHandle<()>>,
    cancel: tokio_util::sync::CancellationToken,
    armed: bool,
}

impl PendingAuthorityWatch {
    fn start(
        source: ReviewedPluginMcpSource,
        cancel: tokio_util::sync::CancellationToken,
        reason_slot: Arc<std::sync::Mutex<Option<String>>>,
    ) -> Self {
        let task_cancel = cancel.clone();
        // The watch stays per-connection by design (#6211 R7a): it is born
        // with the connect attempt (covering the pre-insertion window) and
        // dies with the connection, so a watched server can neither be
        // missed nor leak. A pool-level task would need the pool lock —
        // held across in-flight calls — and regress the mid-call trip this
        // exists for. What moves is the check itself: synchronous
        // state fs has no place on the executor at 20Hz, so it runs on the
        // blocking pool while the 50ms revocation cadence is unchanged.
        let source = Arc::new(source);
        let handle = tokio::spawn(async move {
            loop {
                let source = Arc::clone(&source);
                let check = tokio::task::spawn_blocking(move || {
                    crate::plugins::registry::verify_plugin_state_authority(&source.authority)
                })
                .await;
                let reason = match check {
                    Ok(Err(reason)) => Some(reason),
                    Ok(Ok(())) => None,
                    Err(_) => {
                        Some("plugin authority check failed to run; failing closed".to_string())
                    }
                };
                if let Some(reason) = reason {
                    if let Ok(mut slot) = reason_slot.lock() {
                        *slot = Some(reason);
                    }
                    task_cancel.cancel();
                    break;
                }
                tokio::select! {
                    _ = task_cancel.cancelled() => break,
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                }
            }
        });
        Self {
            handle: Some(handle),
            cancel,
            armed: true,
        }
    }

    fn disarm(mut self) -> tokio::task::JoinHandle<()> {
        self.armed = false;
        self.handle.take().expect("authority watch must exist")
    }
}

impl Drop for PendingAuthorityWatch {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
            if let Some(handle) = self.handle.take() {
                handle.abort();
            }
        }
    }
}

/// Total request ceiling handed to the HTTP client: it must cover the longest
/// request the connection carries (`tools/call` at the execute budget), so a
/// raised `execute_timeout` governs HTTP servers too. This is a ceiling for
/// the transport, not the read knob.
///
/// Streamable HTTP reads the reply inside the POST; `call_method` bounds the
/// send and the receive with the request's own budget, so this ceiling is
/// only the transport's outer safety net for requests without one.
fn http_request_ceiling_secs(config: &McpServerConfig, global: &McpTimeouts) -> u64 {
    config
        .effective_read_timeout(global)
        .max(config.effective_execute_timeout(global))
}

async fn prepare_mcp_http_client(
    name: &str,
    config: &McpServerConfig,
    global_timeouts: &McpTimeouts,
    network_policy: Option<&NetworkPolicyDecider>,
    cancel_token: &tokio_util::sync::CancellationToken,
    connect_timeout_secs: u64,
) -> Result<http_client::McpHttpClient> {
    let url = config.url.as_deref().context("MCP HTTP URL absent")?;
    // Per-domain network policy gate (#135). Only the HTTP/SSE transport
    // is gated; STDIO MCP servers run as local subprocesses and never
    // touch the network from this code path.
    if let Some(decider) = network_policy
        && let Some(host) = host_from_url(url)
    {
        match decider.evaluate(&host, "mcp") {
            Decision::Allow => {}
            Decision::Deny => {
                anyhow::bail!(
                    "MCP server '{name}' connection to '{host}' blocked by network policy"
                );
            }
            Decision::Prompt => {
                anyhow::bail!(
                    "MCP server '{name}' connection to '{host}' requires approval; \
             re-run after `/network allow {host}` or set network.default = \"allow\" in config"
                );
            }
        }
    }
    let client = http_client::McpHttpClient::new(
        url,
        config.runtime_added,
        config.reviewed_plugin.is_some(),
        config.allow_private_network,
        network_policy,
        Duration::from_secs(connect_timeout_secs),
        // Transport total-request ceiling, not the read knob: it must
        // cover the longest request this connection carries
        // (`tools/call` at the execute budget), so a raised
        // `execute_timeout` governs HTTP servers too. The read knob
        // itself stays intact for the connection-level waits below.
        Duration::from_secs(http_request_ceiling_secs(config, global_timeouts)),
    )?;
    let oauth_runtime = if config.reviewed_plugin.is_some() {
        None
    } else {
        match oauth::build_default_headers(&config.headers, &config.env_headers) {
            Ok(default_headers) => {
                let prepared = tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        anyhow::bail!(
                    "MCP OAuth setup cancelled after plugin authority changed"
                        )
                    }
                    prepared = oauth::McpOAuthRuntime::from_server_config_with_client(
                        name,
                        config,
                        default_headers,
                        client.clone(),
                    ) => prepared,
                };
                match prepared {
                    Ok(runtime) => runtime,
                    Err(err) => {
                        if config.reviewed_plugin.is_some() {
                            tracing::warn!(
                                target: "mcp",
                                server = %name,
                                "failed to prepare reviewed plugin MCP OAuth runtime; provider details suppressed; continuing without stored OAuth token"
                            );
                        } else {
                            tracing::warn!(
                                target: "mcp",
                                server = %name,
                                error = %err,
                                "failed to prepare MCP OAuth runtime; continuing without stored OAuth token"
                            );
                        }
                        None
                    }
                }
            }
            Err(err) => {
                if config.reviewed_plugin.is_some() {
                    tracing::warn!(
                        target: "mcp",
                        server = %name,
                        "failed to prepare reviewed plugin MCP OAuth headers; details suppressed; continuing without stored OAuth token"
                    );
                } else {
                    tracing::warn!(
                        target: "mcp",
                        server = %name,
                        error = %err,
                        "failed to prepare MCP OAuth default headers; continuing without stored OAuth token"
                    );
                }
                None
            }
        }
    };
    let client = client.with_mcp_auth(McpHttpAuth::from_config(name, config, oauth_runtime));
    Ok(client)
}

impl McpConnection {
    /// Connect to an MCP server and initialize it.
    ///
    /// `network_policy` (added in v0.7.0 for #135) is consulted for HTTP/SSE
    /// transports only — STDIO transports are unaffected. Pass `None` to
    /// match pre-v0.7.0 permissive behavior.
    #[cfg(test)]
    pub async fn connect_with_policy(
        name: String,
        config: McpServerConfig,
        global_timeouts: &McpTimeouts,
        network_policy: Option<&NetworkPolicyDecider>,
    ) -> Result<Self> {
        Self::connect_with_backend(
            name,
            config,
            global_timeouts,
            network_policy,
            McpBackend::Rust,
        )
        .await
    }
    pub(crate) async fn connect_with_backend(
        name: String,
        config: McpServerConfig,
        global_timeouts: &McpTimeouts,
        network_policy: Option<&NetworkPolicyDecider>,
        backend: McpBackend,
    ) -> Result<Self> {
        let connect_timeout_secs = config.effective_connect_timeout(global_timeouts);
        let read_timeout_secs = config.effective_read_timeout(global_timeouts);
        let cancel_token = tokio_util::sync::CancellationToken::new();
        let authority_revocation_reason = Arc::new(std::sync::Mutex::new(None));
        if let Some(source) = config.reviewed_plugin.as_ref() {
            source.validate_before_use(&name, "connect")?;
            if let Some(url) = config.url.as_deref() {
                source.validate_remote_endpoint(&name, url)?;
            }
        }
        // Start the cross-process generation watch before any network request
        // or child spawn. The guard cancels and aborts itself on every early
        // return; a successful connection transfers the task into `Self`.
        let authority_watch = config.reviewed_plugin.clone().map(|source| {
            PendingAuthorityWatch::start(
                source,
                cancel_token.clone(),
                Arc::clone(&authority_revocation_reason),
            )
        });
        let http_client = if config.url.is_some() {
            Some(
                prepare_mcp_http_client(
                    &name,
                    &config,
                    global_timeouts,
                    network_policy,
                    &cancel_token,
                    connect_timeout_secs,
                )
                .await?,
            )
        } else {
            None
        };
        let transport: Box<dyn McpTransport> = if backend == McpBackend::Host {
            Box::new(
                crate::extension_host::mcp::SdkTransport::connect_with_http(
                    &name,
                    &config,
                    cancel_token.clone(),
                    Duration::from_secs(connect_timeout_secs),
                    http_client,
                )
                .await?,
            )
        } else if let Some(url) = &config.url {
            let client = http_client.expect("prepared HTTP authority");
            if is_legacy_sse_transport(&config) {
                Box::new(
                    SseTransport::connect(
                        client,
                        url.clone(),
                        cancel_token.clone(),
                        Duration::from_secs(connect_timeout_secs),
                    )
                    .await?,
                )
            } else {
                let mut http = HttpTransport::new(
                    client,
                    url.clone(),
                    cancel_token.clone(),
                    Duration::from_secs(connect_timeout_secs),
                );
                // Best-effort session preflight for servers that require
                // a session ID on every POST including `initialize`
                // (e.g. Hindsight, #1629). Failures are non-fatal — the
                // `initialize` POST will proceed and may capture a session
                // ID from the response instead.
                if let Err(e) = http.try_establish_session().await {
                    tracing::debug!(
                        target: "mcp",
                        server = %name,
                        error = %e,
                        "session-establishment GET skipped; proceeding with POST initialize"
                    );
                }
                Box::new(http)
            }
        } else if let Some(command) = &config.command {
            Box::new(StdioTransport::spawn(
                &name,
                command,
                &config,
                cancel_token.clone(),
            )?)
        } else {
            anyhow::bail!("MCP server '{name}' config must have either 'command' or 'url'");
        };
        // Revalidate after transport construction as well: remote setup may
        // await DNS/TLS/SSE preflight, and a concurrent process can revoke the
        // receipt during that interval. Initialization and catalog discovery
        // never start under a stale generation.
        if let Some(source) = config.reviewed_plugin.as_ref() {
            source.validate_before_use(&name, "initialize")?;
        }
        let authority_watch = authority_watch.map(PendingAuthorityWatch::disarm);

        let mut conn = Self {
            name: name.clone(),
            transport,
            tools: Vec::new(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
            request_id: AtomicU64::new(1),
            state: ConnectionState::Connecting,
            config,
            server_capabilities: None,
            instructions: None,
            discovery_timeout: Duration::from_secs(connect_timeout_secs),
            read_timeout_secs,
            cancel_token,
            authority_revocation_reason,
            authority_watch,
            catalog_generation: 0,
            decision_key: None,
        };

        // The built-in Computer Use plugin accepts consent and script calls
        // only with a decision attested by its host. Its keys travel as the
        // first message on the plugin's own stdin, never in its environment.
        if backend == McpBackend::Rust
            && conn.config.url.is_none()
            && conn.config.command.is_some()
            && conn
                .config
                .reviewed_plugin
                .as_ref()
                .is_some_and(|source| source.plugin_name() == COMPUTER_USE_PLUGIN_NAME)
        {
            let decision_key = random_key()?;
            let ledger_key = computer_use_ledger_key().await;
            conn.send(serde_json::json!({
                "jsonrpc": "2.0",
                "method": COMPUTER_USE_HOST_KEYS_METHOD,
                "params": {
                    "decision_key": hex_encode(&decision_key),
                    "ledger_key": ledger_key,
                }
            }))
            .await?;
            conn.decision_key = Some(decision_key);
        }

        // Initialize with timeout
        tokio::time::timeout(Duration::from_secs(connect_timeout_secs), conn.initialize())
            .await
            .with_context(|| format!("MCP server '{name}' initialization timed out"))??;

        conn.discover_all()
            .await
            .with_context(|| format!("MCP server '{name}' discovery failed"))?;

        conn.state = ConnectionState::Ready;
        Ok(conn)
    }

    /// Send initialize request and wait for response
    async fn initialize(&mut self) -> Result<()> {
        let init_id = self.next_id();
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "id": &init_id,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "clientInfo": {
                    "name": "codewhale-tui",
                    "version": env!("CARGO_PKG_VERSION")
                },
                // Client capabilities name what the *client* offers the server
                // (roots, sampling, elicitation). `tools`/`resources`/`prompts`
                // are server capabilities; declaring them here is off-spec and
                // strict servers reject the handshake with -32602. We offer
                // none of the client features yet, so the object is empty.
                "capabilities": {}
            }
        }))
        .await?;

        let response = self.recv(init_id).await?;
        if let Some(error) = response.get("error") {
            // A JSON-RPC error on `initialize` is the server refusing the
            // handshake, not a transport fault: name the server and what was
            // launched, and carry the child's last stderr line, which is
            // usually the real reason (an MCP proxy that cannot reach its
            // upstream answers -32602 and explains itself only on stderr).
            let stderr = self
                .transport
                .last_stderr_line()
                .await
                .map(|line| format!("; server stderr: {line}"))
                .unwrap_or_default();
            // Classified from the raw error and stderr *before* any
            // suppression, so a reviewed plugin's AWS server (the aws-core
            // plugin ships one) still learns its login expired. Only our own
            // fixed hint text leaves this branch, never the server's words.
            let raw = format!("{error}{stderr}");
            let hint = if mcp_error_is_aws_login(&raw, mcp_server_oauth_capable(&self.config)) {
                format!("; {}", aws_login_hint(&self.config, &self.name, &raw))
            } else {
                String::new()
            };
            if self.config.reviewed_plugin.is_some() {
                anyhow::bail!(
                    "Reviewed plugin MCP server returned an error in 'initialize' (server details suppressed to protect environment-backed credentials){hint}"
                );
            }
            let launched = match (&self.config.command, &self.config.url) {
                (Some(command), _) => format!("command `{}`", mcp_display_target("stdio", command)),
                (None, Some(_)) => "HTTP endpoint".to_string(),
                (None, None) => "server".to_string(),
            };
            anyhow::bail!(
                "MCP server '{}' rejected initialize ({launched}): {error}{stderr}{hint}",
                self.name
            );
        }
        let result = response_result(
            &response,
            "initialize",
            self.config.reviewed_plugin.is_some(),
        )?;
        // Per spec, a server that cannot speak the advertised revision answers
        // with one it does support. Accept any dated revision we still
        // implement; anything else ends the handshake.
        let negotiated = result
            .and_then(|result| result.get("protocolVersion"))
            .and_then(|version| version.as_str())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "MCP server '{}' initialize result omitted protocolVersion",
                    self.name
                )
            })?;
        anyhow::ensure!(
            MCP_CLIENT_ACCEPTED_PROTOCOL_VERSIONS.contains(&negotiated),
            "MCP server '{}' negotiated unsupported protocol version '{negotiated}' (supported: {})",
            self.name,
            MCP_CLIENT_ACCEPTED_PROTOCOL_VERSIONS.join(", ")
        );
        self.transport.set_protocol_version(negotiated);
        self.server_capabilities = McpServerCapabilities::from_initialize_response(&response);
        self.instructions = codewhale_mcp::sanitize_server_instructions(
            &self.name,
            result.and_then(|result| result.get("instructions")),
        );

        // Send initialized notification (no id, no response expected)
        self.send(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        }))
        .await?;

        Ok(())
    }

    /// Discover and admit one complete tools/resources/prompts generation.
    async fn discover_all(&mut self) -> Result<()> {
        let capabilities = self.server_capabilities;
        let server = self.name.clone();
        let discovery_timeout = self.discovery_timeout;
        let mut budget = McpCatalogBudget::new();
        let mut tools = None;
        let mut resources = None;
        let mut resource_templates = None;
        let mut prompts = None;

        // Missing initialize metadata is treated as a legacy/unknown server:
        // retain tool discovery and bounded best-effort probes for compatibility.
        // When capabilities are advertised, do not call methods the server says
        // it does not implement (notably JetBrains tools-only MCP servers).
        if capabilities.is_none_or(|capabilities| capabilities.tools) {
            tools = Some(
                tokio::time::timeout(discovery_timeout, self.discover_tools(&mut budget))
                    .await
                    .with_context(|| {
                        format!(
                            "MCP server '{}' tool discovery timed out after {:?}",
                            server, discovery_timeout
                        )
                    })??,
            );
        }

        // Keep all three optional calls within one discovery-timeout budget in
        // the worst case while also respecting a tighter transport read timeout.
        let optional_timeout =
            (discovery_timeout / 3).min(Duration::from_secs(self.read_timeout_secs));
        if capabilities.is_none_or(|capabilities| capabilities.resources) {
            resources = run_optional_discovery(
                &server,
                "resources/list",
                optional_timeout,
                self.discover_resources(&mut budget),
            )
            .await;
            resource_templates = run_optional_discovery(
                &server,
                "resources/templates/list",
                optional_timeout,
                self.discover_resource_templates(&mut budget),
            )
            .await;
        }
        if capabilities.is_none_or(|capabilities| capabilities.prompts) {
            prompts = run_optional_discovery(
                &server,
                "prompts/list",
                optional_timeout,
                self.discover_prompts(&mut budget),
            )
            .await;
        }
        // Ordinary optional-method errors remain best effort. Admit only the
        // freshly staged families; do not mix them with an old generation.
        // A budget/cursor refusal preserves the entire previous catalog.
        budget.ensure_available()?;
        self.tools = tools.unwrap_or_default();
        self.resources = resources.unwrap_or_default();
        self.resource_templates = resource_templates.unwrap_or_default();
        self.prompts = prompts.unwrap_or_default();
        Ok(())
    }

    /// Discover available tools from the MCP server
    async fn discover_tools(&mut self, budget: &mut McpCatalogBudget) -> Result<Vec<McpTool>> {
        let mut cursor: Option<String> = None;
        let mut discovered = Vec::new();
        loop {
            budget.ensure_available()?;
            let list_id = self.next_id();
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": &list_id,
                "method": "tools/list",
                "params": params
            }))
            .await?;

            let response = self.recv(list_id).await?;
            let Some(result) = response_result(
                &response,
                "tools/list",
                self.config.reviewed_plugin.is_some(),
            )?
            else {
                break;
            };

            let items = result
                .get("tools")
                .and_then(|tools| tools.as_array())
                .map_or(0, Vec::len);
            cursor = budget.observe_page("tools/list", result, items)?;
            if let Some(arr) = result.get("tools").and_then(|t| t.as_array()) {
                for item in arr {
                    match serde_json::from_value::<McpTool>(item.clone()) {
                        Ok(tool) => discovered.push(tool),
                        Err(err) => {
                            // Skip individual malformed entries instead of
                            // dropping the whole page (#1410). The old
                            // `unwrap_or_default()` would silently throw
                            // away every tool when one was misshapen.
                            tracing::debug!(target: "mcp", ?err, "skipping malformed tool item");
                        }
                    }
                }
            }

            if cursor.is_none() {
                break;
            }
        }
        // Sort by tool name so the order the model sees doesn't depend on
        // server-side pagination ordering — keeps the prompt prefix stable
        // for cache-hit purposes (#1319).
        discovered.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(discovered)
    }

    /// Discover available resources from the MCP server
    async fn discover_resources(
        &mut self,
        budget: &mut McpCatalogBudget,
    ) -> Result<Vec<McpResource>> {
        let mut cursor: Option<String> = None;
        let mut discovered = Vec::new();
        loop {
            budget.ensure_available()?;
            let list_id = self.next_id();
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": &list_id,
                "method": "resources/list",
                "params": params
            }))
            .await?;

            let response = self.recv(list_id).await?;
            let Some(result) = response_result(
                &response,
                "resources/list",
                self.config.reviewed_plugin.is_some(),
            )?
            else {
                break;
            };

            let items = result
                .get("resources")
                .and_then(|resources| resources.as_array())
                .map_or(0, Vec::len);
            cursor = budget.observe_page("resources/list", result, items)?;
            if let Some(arr) = result.get("resources").and_then(|r| r.as_array()) {
                for item in arr {
                    match serde_json::from_value::<McpResource>(item.clone()) {
                        Ok(resource) => discovered.push(resource),
                        Err(err) => {
                            tracing::debug!(target: "mcp", ?err, "skipping malformed resource item");
                        }
                    }
                }
            }

            if cursor.is_none() {
                break;
            }
        }
        Ok(discovered)
    }

    /// Discover available resource templates from the MCP server
    async fn discover_resource_templates(
        &mut self,
        budget: &mut McpCatalogBudget,
    ) -> Result<Vec<McpResourceTemplate>> {
        let mut cursor: Option<String> = None;
        let mut discovered = Vec::new();
        loop {
            budget.ensure_available()?;
            let list_id = self.next_id();
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": &list_id,
                "method": "resources/templates/list",
                "params": params
            }))
            .await?;

            let response = self.recv(list_id).await?;
            let Some(result) = response_result(
                &response,
                "resources/templates/list",
                self.config.reviewed_plugin.is_some(),
            )?
            else {
                break;
            };

            let templates = result
                .get("resourceTemplates")
                .or_else(|| result.get("templates"))
                .or_else(|| result.get("resource_templates"));
            let items = templates
                .and_then(|templates| templates.as_array())
                .map_or(0, Vec::len);
            cursor = budget.observe_page("resources/templates/list", result, items)?;
            if let Some(arr) = templates.and_then(|t| t.as_array()) {
                for item in arr {
                    match serde_json::from_value::<McpResourceTemplate>(item.clone()) {
                        Ok(tmpl) => discovered.push(tmpl),
                        Err(err) => {
                            tracing::debug!(target: "mcp", ?err, "skipping malformed resource_template item");
                        }
                    }
                }
            }

            if cursor.is_none() {
                break;
            }
        }
        Ok(discovered)
    }

    /// Discover available prompts from the MCP server
    async fn discover_prompts(&mut self, budget: &mut McpCatalogBudget) -> Result<Vec<McpPrompt>> {
        let mut cursor: Option<String> = None;
        let mut discovered = Vec::new();
        loop {
            budget.ensure_available()?;
            let list_id = self.next_id();
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            self.send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": &list_id,
                "method": "prompts/list",
                "params": params
            }))
            .await?;

            let response = self.recv(list_id).await?;
            let Some(result) = response_result(
                &response,
                "prompts/list",
                self.config.reviewed_plugin.is_some(),
            )?
            else {
                break;
            };

            let items = result
                .get("prompts")
                .and_then(|prompts| prompts.as_array())
                .map_or(0, Vec::len);
            cursor = budget.observe_page("prompts/list", result, items)?;
            if let Some(arr) = result.get("prompts").and_then(|p| p.as_array()) {
                for item in arr {
                    match serde_json::from_value::<McpPrompt>(item.clone()) {
                        Ok(prompt) => discovered.push(prompt),
                        Err(err) => {
                            tracing::debug!(target: "mcp", ?err, "skipping malformed prompt item");
                        }
                    }
                }
            }

            if cursor.is_none() {
                break;
            }
        }
        Ok(discovered)
    }

    /// Call a tool on this MCP server
    #[cfg(test)]
    pub async fn call_tool(
        &mut self,
        tool_name: &str,
        arguments: serde_json::Value,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        self.call_tool_decided(tool_name, arguments, timeout_secs, None)
            .await
    }

    /// Call a tool, attaching an attested person's decision when there is
    /// one and this server shares a decision key with the host.
    async fn call_tool_decided(
        &mut self,
        tool_name: &str,
        arguments: serde_json::Value,
        timeout_secs: u64,
        decision: Option<&crate::core::engine::HumanDecision>,
    ) -> Result<serde_json::Value> {
        let mut params = serde_json::json!({
            "name": tool_name,
            "arguments": arguments
        });
        if decision.is_some()
            && let Some(key) = self.decision_key.as_ref()
        {
            params["_meta"] = serde_json::json!({
                COMPUTER_USE_DECISION_META: attest_decision(key, tool_name, &params["arguments"])?,
            });
        }
        self.call_method_decided("tools/call", params, timeout_secs, decision)
            .await
    }

    /// Read a resource from this MCP server
    pub async fn read_resource(
        &mut self,
        uri: &str,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        self.call_method(
            "resources/read",
            serde_json::json!({
                "uri": uri
            }),
            timeout_secs,
        )
        .await
    }

    /// Get a prompt from this MCP server
    pub async fn get_prompt(
        &mut self,
        prompt_name: &str,
        arguments: serde_json::Value,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        self.call_method(
            "prompts/get",
            serde_json::json!({
                "name": prompt_name,
                "arguments": arguments
            }),
            timeout_secs,
        )
        .await
    }

    /// Generic method to call an MCP method
    async fn call_method(
        &mut self,
        method: &str,
        params: serde_json::Value,
        timeout_secs: u64,
    ) -> Result<serde_json::Value> {
        self.call_method_decided(method, params, timeout_secs, None)
            .await
    }
    async fn call_method_decided(
        &mut self,
        method: &str,
        params: serde_json::Value,
        timeout_secs: u64,
        decision: Option<&crate::core::engine::HumanDecision>,
    ) -> Result<serde_json::Value> {
        let cancellation = self
            .config
            .reviewed_plugin
            .as_ref()
            .and_then(|source| source.native_mcp.as_ref())
            .cloned();
        let Some(cancellation) = cancellation else {
            return self
                .call_method_decided_inner(method, params, timeout_secs, decision)
                .await;
        };
        let outcome = tokio::select! {
            biased;
            _=cancellation.withdrawn()=>None,
            result=self.call_method_decided_inner(method,params,timeout_secs,decision)=>Some(result),
        };
        match outcome {
            Some(result)=>result,
            None=>self.finish_guarded_error(anyhow::anyhow!("Native MCP operation cancelled because its definition was withdrawn; the operation is not replayed")).await,
        }
    }
    async fn call_method_decided_inner(
        &mut self,
        method: &str,
        params: serde_json::Value,
        timeout_secs: u64,
        decision: Option<&crate::core::engine::HumanDecision>,
    ) -> Result<serde_json::Value> {
        if self.state != ConnectionState::Ready {
            anyhow::bail!(
                "Failed to call MCP method '{}': connection '{}' is not ready",
                method,
                self.name
            );
        }
        if let Some(source) = self.config.reviewed_plugin.as_ref() {
            source.validate_before_use(&self.name, method)?;
        }

        let call_id = self.next_id();
        // One deadline bounds the whole request. Streamable HTTP reads the
        // reply inside the POST, so a budget on the receive alone would leave
        // that transport to its client-wide ceiling (the larger of the read
        // and execute knobs) instead of this request's own budget.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
        let expired_error = anyhow::anyhow!(
            "MCP method '{}' on server '{}' timed out after {}s",
            method,
            self.name,
            timeout_secs
        );
        match tokio::time::timeout_at(
            deadline,
            self.send_decided(
                serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": &call_id,
                    "method": method,
                    "params": params
                }),
                decision,
                Duration::from_secs(timeout_secs),
            ),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return self.finish_guarded_error(error).await,
            Err(_) => {
                // A send abandoned mid-write can leave a partial frame on a
                // stream transport, so the frame boundary is unknown: rebuild
                // the connection rather than reuse it.
                self.state = ConnectionState::Disconnected;
                return self.finish_guarded_error(expired_error).await;
            }
        }

        // The request's own budget is its only receive deadline. A per-frame
        // read-knob wait here either undercut it — a server is silent for the
        // whole execution of a tool call, so the knob fired first, marked the
        // connection Disconnected, and capped a raised `execute_timeout` — or
        // tied with it, leaving the connection's fate to timer order. On
        // expiry the request is abandoned and the connection kept: a late
        // reply carries the abandoned id and the next receive skips it.
        //
        // Known limitation: the server is never told a request was abandoned
        // (no `notifications/cancelled`), whether by this budget or by the
        // caller dropping the call (turn cancellation). A server that handles
        // requests one at a time answers the next call only after finishing
        // the abandoned one, so that call can wait up to its own budget —
        // 1800s for `tools/call` by default. Cancelling `cancel_token` instead
        // marks the connection dead, so the pool rebuilds it (a new child for
        // stdio) before the next call.
        let response = match tokio::time::timeout_at(deadline, self.recv_reply(call_id, None))
            .await
            .with_context(|| {
                format!(
                    "MCP method '{}' on server '{}' timed out after {}s",
                    method, self.name, timeout_secs
                )
            }) {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => return self.finish_guarded_error(error).await,
            Err(error) => return self.finish_guarded_error(error).await,
        };

        if let Some(source) = self.config.reviewed_plugin.as_ref() {
            source.validate_before_use(&self.name, method)?;
        }
        if let Some(error) = response.get("error") {
            if self.config.reviewed_plugin.is_some() {
                // Preserve only the fixed recovery class, just as initialize
                // does; raw server details may contain plugin credentials.
                let raw = error.to_string();
                let hint = if mcp_error_is_aws_login(&raw, mcp_server_oauth_capable(&self.config)) {
                    format!("; {}", aws_login_hint(&self.config, &self.name, &raw))
                } else {
                    String::new()
                };
                anyhow::bail!(
                    "Reviewed plugin MCP server returned an error in '{method}' (server details suppressed to protect environment-backed credentials){hint}"
                );
            }
            return Err(anyhow::anyhow!(
                "MCP error in '{}': {}",
                method,
                serde_json::to_string_pretty(error)?
            ));
        }

        // JSON-RPC requires exactly one of `result` / `error`. Treating a
        // response carrying neither as an empty success handed the model a
        // `null` tool result that is indistinguishable from a tool that
        // genuinely returned nothing. An explicit `"result": null` is still a
        // valid empty success and passes through unchanged.
        response.get("result").cloned().with_context(|| {
            format!(
                "MCP response from server '{}' for '{method}' contained neither a result nor an error",
                self.name
            )
        })
    }

    /// Get discovered tools
    pub fn tools(&self) -> &[McpTool] {
        &self.tools
    }

    /// Get discovered resources
    pub fn resources(&self) -> &[McpResource] {
        &self.resources
    }

    /// Get discovered resource templates
    pub fn resource_templates(&self) -> &[McpResourceTemplate] {
        &self.resource_templates
    }

    /// Get discovered prompts
    pub fn prompts(&self) -> &[McpPrompt] {
        &self.prompts
    }

    /// Get server name
    #[allow(dead_code)] // Public API for MCP consumers
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Ready to dispatch: the transport is live **and** the plugin bundle
    /// backing it still carries the authority it was reviewed with.
    pub fn is_ready(&self) -> bool {
        self.is_transport_ready() && self.catalog_authorized()
    }

    /// Liveness only — no authority check.
    ///
    /// The Ready flag alone can't see a stdio child that exited between
    /// calls; the probe closes that gap so the pool rebuilds the connection
    /// instead of handing a dead transport back (#6187).
    ///
    /// Only for callers that have just run `validate_before_use` on this same
    /// source, where `is_ready`'s authority half would re-walk and re-hash the
    /// plugin bundle it already verified one statement earlier (#6209). Every
    /// other caller must use `is_ready`: dropping the authority half without
    /// a preceding check silently dispatches to a revoked or altered bundle.
    pub(crate) fn is_transport_ready(&self) -> bool {
        self.state == ConnectionState::Ready && !self.transport.probe_dead()
    }

    /// Usage guidance the server supplied at `initialize`, sanitized and
    /// capped (see [`codewhale_mcp::sanitize_server_instructions`]).
    pub fn instructions(&self) -> Option<&str> {
        self.instructions.as_deref()
    }

    /// Get server config
    pub fn config(&self) -> &McpServerConfig {
        &self.config
    }

    /// Get connection state
    #[allow(dead_code)] // Public API for MCP consumers
    pub fn state(&self) -> ConnectionState {
        self.state
    }

    fn next_id(&self) -> String {
        self.request_id.fetch_add(1, Ordering::SeqCst).to_string()
    }

    async fn send(&mut self, msg: serde_json::Value) -> Result<()> {
        self.send_decided(msg, None, self.discovery_timeout).await
    }
    async fn send_decided(
        &mut self,
        msg: serde_json::Value,
        decision: Option<&crate::core::engine::HumanDecision>,
        timeout: Duration,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(&msg).context("Failed to serialize MCP JSON-RPC message")?;
        let cancel_token = self.cancel_token.clone();
        let name = self.name.clone();
        let result = tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                Err(anyhow::anyhow!("MCP connection '{name}' was cancelled"))
            }
            result = self.transport.send_decided(bytes, decision, timeout) => result,
        };
        if result.is_err() {
            // A dead write side is as fatal as a dead read side: the pool
            // reuses any connection whose `is_ready()` is true, so leaving
            // this one in `Ready` would hand the same broken transport back
            // on every later call instead of rebuilding it.
            self.state = ConnectionState::Disconnected;
        }
        result
    }

    /// Handshake and discovery receive: each frame wait is bounded by the read
    /// knob, and a server that stays silent past it is treated as dead.
    async fn recv(&mut self, expected_id: String) -> Result<serde_json::Value> {
        self.recv_reply(expected_id, Some(self.read_timeout_secs))
            .await
    }

    /// The next transport frame, unless the connection is cancelled first.
    async fn next_frame(&mut self) -> Result<Vec<u8>> {
        tokio::select! {
            biased;
            _ = self.cancel_token.cancelled() => {
                anyhow::bail!("MCP connection '{}' was cancelled", self.name)
            }
            result = self.transport.recv() => result,
        }
    }

    /// Receive the reply to `expected_id`, skipping notifications and replies
    /// to other (abandoned) requests. `frame_timeout_secs` bounds each frame
    /// wait and treats its expiry as a dead connection; `None` leaves the
    /// whole wait to the caller's own request budget.
    async fn recv_reply(
        &mut self,
        expected_id: String,
        frame_timeout_secs: Option<u64>,
    ) -> Result<serde_json::Value> {
        loop {
            let frame = match frame_timeout_secs {
                Some(secs) => {
                    match tokio::time::timeout(Duration::from_secs(secs), self.next_frame()).await {
                        Ok(frame) => frame,
                        Err(_) => {
                            self.state = ConnectionState::Disconnected;
                            anyhow::bail!(
                                "Timed out waiting for MCP JSON-RPC response from server '{}' after {}s",
                                self.name,
                                secs
                            );
                        }
                    }
                }
                None => self.next_frame().await,
            };
            let bytes = frame.inspect_err(|_e| {
                self.state = ConnectionState::Disconnected;
            })?;
            let value: serde_json::Value = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(err) => {
                    self.state = ConnectionState::Disconnected;
                    let preview = if self.config.reviewed_plugin.is_some() {
                        "<server details suppressed for reviewed plugin>".to_string()
                    } else {
                        invalid_json_preview(&bytes)
                    };
                    return Err(err).with_context(|| {
                        format!(
                            "Invalid MCP JSON-RPC message from server '{}': {}",
                            self.name, preview
                        )
                    });
                }
            };

            // Check if this is a response with the expected id. We emit
            // string IDs because some MCP gateways reject numeric JSON-RPC
            // IDs, but accept numeric echoes for compatibility with older
            // servers and tests.
            if response_id_matches(value.get("id"), &expected_id) {
                // Marks the connection stale so it is rebuilt, but this is a
                // reply to the request, so it never qualifies for a replay.
                // An expired AWS *SSO session* is the server's upstream
                // credential, not our MCP session: rebuilding the connection
                // cannot renew it, and the stale-session wording would hide
                // the one fact the user can act on.
                if let Some(error) = value.get("error")
                    && is_mcp_stale_session_body(&error.to_string())
                    && !error_text_looks_aws_credentials_expired(&error.to_string())
                {
                    anyhow::bail!("MCP session expired: {error}");
                }
                return Ok(value);
            }
            // Skip notifications (no id) and responses with different ids
        }
    }

    /// Gracefully close the connection
    #[allow(dead_code)] // Public API for MCP consumers
    pub fn close(&mut self) {
        self.cancel_token.cancel();
        self.state = ConnectionState::Disconnected;
    }

    fn catalog_authorized(&self) -> bool {
        self.config
            .reviewed_plugin
            .as_ref()
            .is_none_or(ReviewedPluginMcpSource::catalog_is_current)
    }

    async fn finish_guarded_error<T>(&mut self, error: anyhow::Error) -> Result<T> {
        let reason = self
            .authority_revocation_reason
            .lock()
            .ok()
            .and_then(|reason| reason.clone());
        if let Some(reason) = reason {
            self.transport.shutdown().await;
            self.state = ConnectionState::Disconnected;
            anyhow::bail!(
                "MCP operation on plugin server '{}' was cancelled after authority changed: {reason}",
                self.name
            );
        }
        Err(error)
    }
}

/// Resolve the operator's proxy route for this exact request using the same
/// matcher as reqwest. A NO_PROXY match returns None: direct requests must keep
/// their public DNS validation and pins. Model and reviewed-plugin requests
/// return before even reading proxy credentials.
fn configured_mcp_proxy<F>(
    url: &reqwest::Url,
    disallow_ambient_proxy: bool,
    mut read_environment: F,
) -> Result<Option<reqwest::Proxy>>
where
    F: FnMut(&str) -> std::result::Result<String, std::env::VarError>,
{
    if disallow_ambient_proxy {
        return Ok(None);
    }
    let proxy_url = read_environment("HTTPS_PROXY")
        .or_else(|_| read_environment("https_proxy"))
        .or_else(|_| read_environment("HTTP_PROXY"))
        .or_else(|_| read_environment("http_proxy"))
        .ok()
        .filter(|value| !value.trim().is_empty());
    let Some(proxy_url) = proxy_url else {
        return Ok(None);
    };
    // Normalize userinfo and Unicode with the URL parser before passing the
    // URL to reqwest's own underlying matcher. Keep its missing-scheme support.
    let normalized = reqwest::Url::parse(&proxy_url)
        .ok()
        .filter(|url| url.has_host())
        .or_else(|| reqwest::Url::parse(&format!("http://{proxy_url}")).ok());
    let Some(normalized) = normalized else {
        tracing::warn!(target: "mcp", proxy = %redact_proxy_userinfo(&proxy_url), "ignoring malformed HTTP(S)_PROXY URL");
        return Ok(None);
    };
    let no_proxy = read_environment("NO_PROXY")
        .or_else(|_| read_environment("no_proxy"))
        .unwrap_or_default();
    let matcher = hyper_util::client::proxy::matcher::Matcher::builder()
        .all(normalized.as_str())
        .no(no_proxy)
        .build();
    let destination: oauth2::http::Uri = url.as_str().parse()?;
    let Some(route) = matcher.intercept(&destination) else {
        return Ok(None);
    };
    // Build the actual proxy from the matched route itself so the decision
    // that grants delegated DNS authority cannot diverge from the transport.
    let mut proxy = reqwest::Proxy::all(route.uri().to_string())?;
    if let Some(auth) = route.basic_auth() {
        proxy = proxy.custom_http_auth(auth.clone());
    }
    if let Some((user, password)) = route.raw_auth() {
        proxy = proxy.basic_auth(user, password);
    }
    Ok(Some(proxy))
}

impl Drop for McpConnection {
    fn drop(&mut self) {
        self.cancel_token.cancel();
        if let Some(watch) = self.authority_watch.take() {
            watch.abort();
        }
    }
}

// === McpPool - Connection Pool Management ===

#[derive(Debug, Clone)]
struct McpToolRoute {
    server_name: String,
    tool_name: String,
    catalog_generation: u64,
    plugin_authority: Option<crate::plugins::types::PluginAuthority>,
}

/// Model-facing name suffix of the synthetic self-serve OAuth login tool
/// (`mcp_<server>_authenticate`). Registered by [`McpPool::to_api_tools`] for
/// servers whose last connect failed auth-required; executed by
/// [`McpPool::call_tool`] through the same flow `/mcp login` uses.
pub(crate) const AUTHENTICATE_TOOL_NAME: &str = "authenticate";

/// Result of [`McpPool::begin_authenticate_tool`]: either the shared token
/// store already holds a usable credential (a login completed elsewhere since
/// the catalog was built) or a browser login has been started and must be
/// finished outside the pool lock.
pub(crate) enum AuthenticateToolStart {
    AlreadyAuthorized,
    Login(Box<oauth::McpOAuthToolLogin>),
}

/// How the login phase of the synthetic authenticate tool concluded, fed to
/// [`McpPool::finish_authenticate_tool`].
pub(crate) enum AuthenticateToolOutcome {
    AlreadyAuthorized,
    Authenticated { authorization_url: String },
}

/// Execute the synthetic `mcp_<server>_authenticate` tool against a shared
/// pool without holding the pool lock during the browser wait: lock to start
/// the flow, release, wait for the loopback callback, then lock again to
/// reconnect. `on_authorization_url` fires as soon as the URL exists — before
/// the wait — so the runtime can show it to the user while the call blocks;
/// the model only sees the URL in the result, after the flow has already
/// finished, so this hook is the user's real path to the sign-in page when
/// the browser did not open.
pub(crate) async fn authenticate_tool_via_pool(
    pool: &Arc<tokio::sync::Mutex<McpPool>>,
    server_name: &str,
    on_authorization_url: impl FnOnce(&str),
) -> Result<serde_json::Value> {
    let start = pool
        .lock()
        .await
        .begin_authenticate_tool(server_name)
        .await?;
    let outcome = match start {
        AuthenticateToolStart::AlreadyAuthorized => AuthenticateToolOutcome::AlreadyAuthorized,
        AuthenticateToolStart::Login(login) => {
            let authorization_url = login.authorization_url().to_string();
            on_authorization_url(&authorization_url);
            login.finish().await?;
            AuthenticateToolOutcome::Authenticated { authorization_url }
        }
    };
    pool.lock()
        .await
        .finish_authenticate_tool(server_name, outcome)
        .await
}

/// Pool of MCP connections for reuse
/// Protocol owner selected only by user configuration. Rust is the default;
/// Host explicitly selects the SDK and never falls back to Rust on refusal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpBackend {
    #[default]
    Rust,
    Host,
}
impl McpBackend {
    pub(crate) fn from_config(config: &crate::config::Config) -> Self {
        config
            .extension_host
            .as_ref()
            .map_or(Self::Rust, |host| host.mcp_backend)
    }
}

pub struct McpPool {
    backend: McpBackend,
    /// Immutable operator ceiling; source reloads and shared child pools cannot relax it.
    disallowed_tools: Vec<String>,
    connections: HashMap<String, McpConnection>,
    config: McpConfig,
    network_policy: Option<NetworkPolicyDecider>,
    /// Source paths the config was loaded from. Empty for pools constructed
    /// directly via `new` (tests, ad-hoc snapshots). Workspace-aware pools
    /// track both global and project-level MCP config paths so lazy reload sees
    /// either file appear or change.
    config_sources: Vec<PathBuf>,
    workspace: Option<PathBuf>,
    plugin_registry: Option<Arc<crate::plugins::PluginRegistry>>,
    /// 64-bit content hash of the active config (`hash_mcp_config`). Compared
    /// against the freshly-loaded config after an mtime change to skip
    /// reloading when the file was merely touched.
    config_hash: u64,
    /// Monotonic identity for the exact config/plugin catalog generation that
    /// advertised a callable MCP item. Resolution captures this value and the
    /// call boundary rejects any intervening lazy reload or dynamic mutation.
    catalog_generation: AtomicU64,
    /// Most recently observed mtime for `config_sources`.
    last_mtimes: Vec<Option<std::time::SystemTime>>,
    native_mcp_epoch: u64,
    /// Dynamically added MCP servers (from tool calls at runtime).
    /// These are not persisted to disk and live for the process lifetime.
    pub(crate) dynamic_servers: Arc<RwLock<HashMap<String, McpServerConfig>>>,
    /// Servers whose most recent connect attempt failed auth-required (401 /
    /// OAuth not logged in). Each gets a synthetic `mcp_<server>_authenticate`
    /// tool in the model catalog so the model can self-serve the OAuth login
    /// instead of dead-ending on the error item. BTreeSet keeps catalog
    /// construction deterministic.
    needs_auth_servers: BTreeSet<String>,
    /// Configured OAuth callback overrides, so the synthetic self-serve
    /// login tool honors the same pre-registered redirect URI `/mcp login`
    /// uses (`mcp_oauth_callback_port` / `mcp_oauth_callback_url`).
    oauth_callback_port: Option<u16>,
    oauth_callback_url: Option<String>,
    /// Bumped on every `needs_auth_servers` mutation. The engine reads it
    /// around a tool call so a live 401 that flips the auth surface can
    /// flag `mcp_catalog_changed` on the failed result and the turn loop
    /// replaces the pool's catalog slice before the next model request.
    needs_auth_generation: u64,
    /// Per-server cooldown after a failed connect, keyed by server name.
    ///
    /// The turn loop rebuilds the tool catalog on every user message, and
    /// that used to re-attempt every server that was not ready — so twenty
    /// configured servers with four dead ones paid four connect timeouts
    /// before the first token, every single turn, forever. A failure now
    /// buys a growing cooldown; the recorded diagnosis is replayed while it
    /// holds, so a skipped server still reads as failing and never as
    /// healthy. Explicit intent (`retry_connection`, `get_or_connect`, a
    /// config reload) ignores the cooldown.
    connect_backoff: HashMap<String, ConnectBackoff>,
    /// Servers the supervisor last saw dead. Death is reported once, on the
    /// transition, so status surfaces flip exactly when liveness does instead
    /// of re-emitting every sweep (#6187).
    supervised_dead: HashSet<String>,
    /// Servers the supervisor stopped auto-reconnecting after
    /// [`SUPERVISOR_PARK_AFTER_CONSECUTIVE_FAILURES`] consecutive failures.
    /// A stored-ready connection or an explicit `/mcp retry` clears the park.
    supervised_parked: HashSet<String>,
    /// Servers with a spawned connect in flight right now. `connect_all`,
    /// the session boot pass, and explicit tool-selection connects all mark
    /// names here and clear them on resolution, so status surfaces never
    /// have to infer "connecting" from "enabled but not connected yet"
    /// (#6033): under lazy boot an unconnected server is one nobody has
    /// asked for, not one mid-handshake.
    connecting: HashSet<String>,
}

/// One server's cooldown: when to try again, and what to say until then.
struct ConnectBackoff {
    consecutive_failures: u32,
    retry_after: std::time::Instant,
    last_error: String,
}

/// One supervised reconnect candidate: the name, the config to redial, and
/// whether this sweep newly observed the death.
pub(crate) struct SupervisionDue {
    pub name: String,
    pub config: McpServerConfig,
    pub fresh_death: bool,
}

/// One supervisor sweep's plan: candidates to redial plus the transitions
/// the plan phase already knows (recoveries and newly parked servers).
pub(crate) struct SupervisionPlan {
    pub backend: McpBackend,
    pub due: Vec<SupervisionDue>,
    pub recovered: Vec<String>,
    pub parked: Vec<String>,
    pub timeouts: McpTimeouts,
    pub network_policy: Option<NetworkPolicyDecider>,
    pub catalog_generation: u64,
}

/// One supervisor sweep's transitions. Death, recovery, failed attempts, and
/// parking are reported on transition only, so the engine emits a snapshot
/// update exactly when something changed (#6187).
#[derive(Debug, Default)]
pub(crate) struct McpSupervisorUpdate {
    /// Newly observed dead, with the reconnect failure that confirmed it.
    pub died: Vec<(String, String)>,
    /// Reconnect attempt failed for an already-dead server, with last error.
    pub failed: Vec<(String, String)>,
    /// Dead last sweep, alive now.
    pub recovered: Vec<String>,
    /// Newly parked after repeated failures; explicit `/mcp retry` resumes.
    pub parked: Vec<String>,
}

impl McpSupervisorUpdate {
    pub(crate) fn is_empty(&self) -> bool {
        self.died.is_empty()
            && self.failed.is_empty()
            && self.recovered.is_empty()
            && self.parked.is_empty()
    }

    fn merge(&mut self, other: McpSupervisorUpdate) {
        self.died.extend(other.died);
        self.failed.extend(other.failed);
        self.recovered.extend(other.recovered);
        self.parked.extend(other.parked);
    }
}

/// Cooldown after `failures` consecutive failed connects.
///
/// Doubling from 30s to a 10-minute ceiling: long enough that a wall of dead
/// servers costs nothing per turn, short enough that a server coming back
/// (a laptop rejoining a network, a local server restarted) is picked up
/// within one coffee break without the user touching anything.
fn connect_backoff_delay(failures: u32) -> std::time::Duration {
    const BASE: std::time::Duration = std::time::Duration::from_secs(30);
    const CAP: std::time::Duration = std::time::Duration::from_secs(600);
    BASE.saturating_mul(1u32 << failures.saturating_sub(1).min(5))
        .min(CAP)
}

type McpPendingConnect = (String, McpServerConfig);
type McpConnectError = (String, anyhow::Error);

/// The connected-app server named by an `mcp_<server>_<tool>` tool name.
/// Presentation only: server names may themselves hold `_`, so this is never
/// a policy input.
#[must_use]
pub fn connected_app_server(tool_name: &str) -> Option<&str> {
    let rest = tool_name.strip_prefix("mcp_")?;
    match rest.split_once('_') {
        Some((server, _)) if !server.is_empty() => Some(server),
        _ if !rest.is_empty() => Some(rest),
        _ => None,
    }
}

/// Whether an explicit tool selection (`tools_always_load`, a turn's
/// `allowed_tools`) covers `server`: either an exact `mcp_<server>_<tool>`
/// name or an `mcp_<prefix>*` glob whose prefix reaches the server name.
/// One definition shared by the lazy boot pass and the per-turn
/// explicit-connect wait so both agree on what a selection starts (#6033).
pub(crate) fn tool_selection_covers_server(requested: &[String], server: &str) -> bool {
    let prefix = format!("mcp_{}_", server.to_ascii_lowercase());
    requested.iter().any(|name| {
        name.starts_with(&prefix)
            || name
                .strip_suffix('*')
                .is_some_and(|rule| prefix.starts_with(rule))
    })
}

impl McpPool {
    /// Create a new pool with the given configuration
    pub fn new(config: McpConfig) -> Self {
        let config_hash = hash_mcp_config(&config);
        Self {
            backend: McpBackend::Rust,
            connections: HashMap::new(),
            disallowed_tools: Vec::new(),
            config,
            network_policy: None,
            oauth_callback_port: None,
            oauth_callback_url: None,
            config_sources: Vec::new(),
            workspace: None,
            plugin_registry: None,
            config_hash,
            catalog_generation: AtomicU64::new(1),
            connect_backoff: HashMap::new(),
            supervised_dead: HashSet::new(),
            supervised_parked: HashSet::new(),
            connecting: HashSet::new(),
            last_mtimes: Vec::new(),
            native_mcp_epoch: crate::extension_host::native_mcp::epoch(),
            dynamic_servers: Arc::new(RwLock::new(HashMap::new())),
            needs_auth_servers: BTreeSet::new(),
            needs_auth_generation: 0,
        }
    }

    pub fn with_backend(mut self, backend: McpBackend) -> Self {
        self.backend = backend;
        self
    }
    pub(crate) fn backend(&self) -> McpBackend {
        self.backend
    }

    /// Create a pool from a configuration file path.
    #[cfg(test)]
    pub fn from_config_path(path: &std::path::Path) -> Result<Self> {
        let config = load_config(path)?;
        let mut pool = Self::new(config);
        pool.config_sources = vec![path.to_path_buf()];
        pool.last_mtimes = vec![mcp_config_mtime(path)];
        Ok(pool)
    }

    /// Create a pool from global MCP config plus workspace-local
    /// `.codewhale/mcp.json`. Project servers override same-name global
    /// servers and default stdio `cwd` to the workspace root.
    #[cfg(test)]
    pub fn from_config_path_with_workspace(
        path: &std::path::Path,
        workspace: &Path,
    ) -> Result<Self> {
        let plugins = Arc::new(crate::plugins::PluginRegistry::empty(workspace));
        Self::from_config_path_with_workspace_and_plugins(path, workspace, plugins)
    }

    pub fn from_config_path_with_workspace_and_plugins(
        path: &std::path::Path,
        workspace: &Path,
        plugins: Arc<crate::plugins::PluginRegistry>,
    ) -> Result<Self> {
        if plugins.workspace() != workspace {
            anyhow::bail!("plugin registry workspace does not match MCP pool workspace");
        }
        let config = load_config_with_workspace_and_plugins(path, workspace, plugins.as_ref())?;
        let workspace = checked_workspace_path(workspace)?;
        let mut pool = Self::new(config);
        pool.config_sources = vec![
            path.to_path_buf(),
            checked_workspace_mcp_config_path(&workspace)?,
        ];
        pool.config_sources
            .extend(crate::config::workspace_trust_config_candidate_paths());
        pool.last_mtimes = pool
            .config_sources
            .iter()
            .map(|source| mcp_config_mtime(source))
            .collect();
        pool.workspace = Some(workspace);
        pool.plugin_registry = Some(plugins);
        Ok(pool)
    }

    /// Construct a source-aware empty pool after the initial config load
    /// failed. Keeping the source paths means a later edit or explicit
    /// `/mcp reload` can recover in-process instead of pinning the session to
    /// an ad-hoc pool that has no files to re-read.
    pub(crate) fn empty_with_workspace_config_sources(
        path: &std::path::Path,
        workspace: &Path,
        plugins: Arc<crate::plugins::PluginRegistry>,
    ) -> Result<Self> {
        validate_mcp_config_path(path)?;
        if plugins.workspace() != workspace {
            anyhow::bail!("plugin registry workspace does not match MCP pool workspace");
        }
        let workspace = checked_workspace_path(workspace)?;
        let mut pool = Self::new(McpConfig::default());
        pool.config_sources = vec![
            path.to_path_buf(),
            checked_workspace_mcp_config_path(&workspace)?,
        ];
        pool.config_sources
            .extend(crate::config::workspace_trust_config_candidate_paths());
        pool.last_mtimes = pool
            .config_sources
            .iter()
            .map(|source| mcp_config_mtime(source))
            .collect();
        pool.workspace = Some(workspace);
        pool.plugin_registry = Some(plugins);
        Ok(pool)
    }

    /// Install the session ceiling before any connection or model catalog is exposed.
    pub(crate) fn with_disallowed_tools(mut self, rules: Vec<String>) -> Self {
        self.disallowed_tools.extend(rules);
        self
    }

    /// Only a prefix covering the entire namespace suppresses a server. An
    /// individual tool denial must preserve its siblings and resource access.
    pub(crate) fn server_denied_by(rules: &[String], server: &str) -> bool {
        let namespace = format!("mcp_{server}_").to_ascii_lowercase();
        rules.iter().any(|rule| {
            rule.to_ascii_lowercase()
                .strip_suffix('*')
                .is_some_and(|prefix| namespace.starts_with(prefix))
        })
    }

    fn server_allowed(&self, server: &str) -> bool {
        !Self::server_denied_by(&self.disallowed_tools, server)
    }

    pub(crate) fn tool_allowed(&self, name: &str) -> bool {
        !crate::core::engine::tool_catalog::tool_matches_any_rule(&self.disallowed_tools, name)
    }

    fn require_server(&self, server: &str) -> Result<()> {
        anyhow::ensure!(
            self.server_allowed(server),
            "Failed to find MCP server: {server}"
        );
        Ok(())
    }

    pub(crate) fn authorize_call(
        rules: &[String],
        name: &str,
        input: &serde_json::Value,
    ) -> Result<()> {
        anyhow::ensure!(
            !crate::core::engine::tool_catalog::tool_matches_any_rule(rules, name),
            "Unknown MCP tool name: {name}"
        );
        if matches!(
            name,
            "list_mcp_resources"
                | "list_mcp_resource_templates"
                | "mcp_read_resource"
                | "read_mcp_resource"
                | "mcp_get_prompt"
        ) && let Some(server) = input.get("server").and_then(serde_json::Value::as_str)
        {
            anyhow::ensure!(
                !Self::server_denied_by(rules, server),
                "Failed to find MCP server: {server}"
            );
        }
        Ok(())
    }

    /// Attach a per-domain network policy (#135). When set, HTTP/SSE
    /// transports are gated through it; STDIO transports are unaffected.
    pub fn with_network_policy(mut self, policy: NetworkPolicyDecider) -> Self {
        self.network_policy = Some(policy);
        self
    }

    /// Configure the OAuth callback overrides (`mcp_oauth_callback_port` /
    /// `mcp_oauth_callback_url`) for installations whose OAuth client has a
    /// pre-registered redirect URI. The synthetic self-serve login tool must
    /// use the same overrides as `/mcp login`, or the provider rejects its
    /// ephemeral loopback redirect.
    pub fn with_oauth_callback(mut self, port: Option<u16>, url: Option<String>) -> Self {
        self.oauth_callback_port = port;
        self.oauth_callback_url = url;
        self
    }

    pub(crate) fn connect_timeouts(&self) -> McpTimeouts {
        self.config.timeouts
    }

    pub(crate) fn cloned_network_policy(&self) -> Option<NetworkPolicyDecider> {
        self.network_policy.clone()
    }

    pub(crate) fn current_catalog_generation(&self) -> u64 {
        self.catalog_generation.load(Ordering::SeqCst)
    }

    fn drop_connection(&mut self, server_name: &str, reason: &str) {
        if self.connections.remove(server_name).is_some() {
            tracing::debug!(
                target: "mcp",
                server = %server_name,
                reason = %reason,
                "dropped MCP connection"
            );
        }
    }

    fn drop_all_connections(&mut self, reason: &str) {
        // Auth state is only known from a live connect attempt; once every
        // connection is dropped (config reload, source switch, shutdown) the
        // next attempt re-derives it. A reload is explicit intent, so every
        // cooldown lifts with it.
        self.connect_backoff.clear();
        self.needs_auth_servers.clear();
        self.needs_auth_generation = self.needs_auth_generation.wrapping_add(1);
        if self.connections.is_empty() {
            return;
        }
        let count = self.connections.len();
        tracing::debug!(
            target: "mcp",
            count,
            reason = %reason,
            "dropping MCP connections"
        );
        self.connections.clear();
    }

    /// If the source config file's mtime has changed since the last check,
    /// re-read it and (only when the content hash also changed) drop all
    /// existing connections so the next `get_or_connect` reattaches under
    /// the new config. No-op when the pool was constructed via [`McpPool::new`]
    /// (no source path), when stat fails, or when the file content is
    /// byte-identical to what we last loaded. Returns `Ok(true)` if any
    /// connections were dropped, `Ok(false)` otherwise.
    ///
    /// This is the lazy half of the auto-reload story for #1267: instead of a
    /// long-lived file watcher, the next tool invocation pays a single `stat`
    /// call (and only re-reads the file when the mtime moved). On networked
    /// or remote filesystems where mtime granularity is poor, the hash
    /// compare keeps us from churning connections on every check.
    fn reload_from_config_sources(&mut self, force: bool) -> Result<bool> {
        if self.config_sources.is_empty() {
            if force {
                anyhow::bail!("MCP pool has no configuration source to reload");
            }
            return Ok(false);
        }
        let current_mtimes: Vec<_> = self
            .config_sources
            .iter()
            .map(|path| mcp_config_mtime(path))
            .collect();
        let native_epoch = crate::extension_host::native_mcp::epoch();
        let native_changed =
            self.plugin_registry.is_some() && native_epoch != self.native_mcp_epoch;
        if !force && !native_changed && current_mtimes == self.last_mtimes {
            return Ok(false);
        }
        // An mtime moved, or the user explicitly requested a reload: re-read
        // the complete global + workspace + plugin-backed config.
        let primary = self
            .config_sources
            .first()
            .context("MCP config source list unexpectedly empty")?;
        let new_config = if let Some(workspace) = self.workspace.as_deref() {
            match self.plugin_registry.as_deref() {
                Some(plugins) => {
                    load_config_with_workspace_and_plugins(primary, workspace, plugins)?
                }
                None => load_config_with_workspace(primary, workspace)?,
            }
        } else {
            load_config(primary)?
        };
        let new_hash = hash_mcp_config(&new_config);
        // Always advance mtimes so a touched-but-unchanged file doesn't
        // make us re-read on every subsequent call.
        self.last_mtimes = current_mtimes;
        self.native_mcp_epoch = native_epoch;
        if !force && new_hash == self.config_hash {
            return Ok(false);
        }
        // A real content change, or an explicit reload, invalidates every
        // advertised route and live transport. The latter matters when OAuth
        // credentials changed without changing the config bytes.
        self.drop_all_connections(if force {
            "explicit config reload"
        } else {
            "config reload"
        });
        self.config = new_config;
        self.config_hash = new_hash;
        self.catalog_generation.fetch_add(1, Ordering::SeqCst);
        Ok(true)
    }

    pub async fn reload_if_config_changed(&mut self) -> Result<bool> {
        self.reload_from_config_sources(false)
    }

    /// Force a source re-read and drop every live connection so the next
    /// connect pass reattaches under the current configuration and
    /// credentials — without waiting for any handshake. An explicit reload is
    /// intent, so cooldowns lift and even a byte-identical config re-dials.
    ///
    /// An unreadable or malformed source returns `Err` **before** anything is
    /// dropped: a failed reload leaves the live tool pool intact. Dynamic
    /// in-memory servers remain registered because this mutates the existing
    /// pool rather than replacing it.
    pub(crate) fn force_reload_config_sources(&mut self) -> Result<()> {
        self.reload_from_config_sources(true).map(|_| ())
    }

    /// Install a replacement global config source transactionally, preserving
    /// this shared pool (and its dynamic runtime servers) for parent and
    /// sub-agent holders. A malformed replacement leaves the current config,
    /// connections, and source paths unchanged. On success every live
    /// connection is dropped; the caller reattaches through its own connect
    /// pass so no pool lock is held across a handshake.
    pub(crate) fn switch_workspace_config_source(
        &mut self,
        path: &Path,
        workspace: &Path,
        plugins: Arc<crate::plugins::PluginRegistry>,
    ) -> Result<()> {
        validate_mcp_config_path(path)?;
        if plugins.workspace() != workspace {
            anyhow::bail!("plugin registry workspace does not match MCP pool workspace");
        }
        let workspace = checked_workspace_path(workspace)?;
        let new_config =
            load_config_with_workspace_and_plugins(path, &workspace, plugins.as_ref())?;
        let mut new_sources = vec![
            path.to_path_buf(),
            checked_workspace_mcp_config_path(&workspace)?,
        ];
        new_sources.extend(crate::config::workspace_trust_config_candidate_paths());
        let new_mtimes = new_sources
            .iter()
            .map(|source| mcp_config_mtime(source))
            .collect();

        self.drop_all_connections("config source switch");
        self.config_hash = hash_mcp_config(&new_config);
        self.config = new_config;
        self.config_sources = new_sources;
        self.last_mtimes = new_mtimes;
        self.workspace = Some(workspace);
        self.plugin_registry = Some(plugins);
        self.catalog_generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// Refresh one retained caller snapshot through the same transactional
    /// source loader. A different attachment must fork; it cannot retarget a
    /// parent's shared pool. No transport, credential or catalog owner changes.
    pub(crate) fn bind_caller_plugins(
        &mut self,
        plugins: Arc<crate::plugins::PluginRegistry>,
    ) -> Result<()> {
        let next = plugins.caller_selection();
        let previous = self
            .plugin_registry
            .as_deref()
            .and_then(crate::plugins::PluginRegistry::caller_selection);
        if next == previous {
            return Ok(());
        }
        anyhow::ensure!(
            previous.is_none() || next.is_some(),
            "MCP caller selection was omitted"
        );
        if let (Some(previous), Some(next)) = (previous, next) {
            anyhow::ensure!(
                previous.attachment_id == next.attachment_id,
                "another MCP caller must use its selected pool fork"
            );
        }
        crate::extension_host::validate_caller_plugins(Some(&plugins))
            .map_err(anyhow::Error::msg)?;
        if let Some(path) = self.config_sources.first().cloned() {
            let workspace = plugins.workspace().to_path_buf();
            self.switch_workspace_config_source(&path, &workspace, plugins)?;
        } else {
            let mut config = self.config.clone();
            config
                .servers
                .retain(|_, server| server.reviewed_plugin.is_none());
            let config = merge_plugin_mcp_servers(config, &plugins)?;
            self.drop_all_connections("caller selection changed");
            self.config_hash = hash_mcp_config(&config);
            self.config = config;
            self.workspace = Some(plugins.workspace().to_path_buf());
            self.plugin_registry = Some(plugins);
            self.catalog_generation.fetch_add(1, Ordering::SeqCst);
        }
        self.native_mcp_epoch = crate::extension_host::native_mcp::epoch();
        Ok(())
    }

    /// A selected child uses this same Rust pool factory with its own caller
    /// receipt. Dynamic operator servers keep their single shared registry.
    pub(crate) fn fork_for_plugins(
        &self,
        plugins: Arc<crate::plugins::PluginRegistry>,
    ) -> Result<Self> {
        let workspace = plugins.workspace().to_path_buf();
        let config = match self.config_sources.first() {
            Some(primary) => {
                load_config_with_workspace_and_plugins(primary, &workspace, plugins.as_ref())?
            }
            None => {
                let mut config = self.config.clone();
                config
                    .servers
                    .retain(|_, server| server.reviewed_plugin.is_none());
                merge_plugin_mcp_servers(config, plugins.as_ref())?
            }
        };
        let mut pool = Self::new(config)
            .with_backend(self.backend)
            .with_disallowed_tools(self.disallowed_tools.clone());
        pool.network_policy = self.network_policy.clone();
        pool.oauth_callback_port = self.oauth_callback_port;
        pool.oauth_callback_url = self.oauth_callback_url.clone();
        pool.dynamic_servers = Arc::clone(&self.dynamic_servers);
        pool.config_sources = self.config_sources.clone();
        if pool.config_sources.len() > 1 {
            pool.config_sources[1] = checked_workspace_mcp_config_path(&workspace)?;
        }
        pool.last_mtimes = pool
            .config_sources
            .iter()
            .map(|path| mcp_config_mtime(path))
            .collect();
        pool.workspace = Some(workspace);
        pool.plugin_registry = Some(plugins);
        Ok(pool)
    }
    pub(crate) fn validate_native_caller(
        &self,
        plugins: Option<&crate::plugins::PluginRegistry>,
    ) -> Result<()> {
        if self.config.servers.values().any(|server| {
            server
                .reviewed_plugin
                .as_ref()
                .is_some_and(|source| source.native_mcp.is_some())
        }) {
            let selection = plugins.and_then(crate::plugins::PluginRegistry::caller_selection);
            anyhow::ensure!(
                selection
                    == self
                        .plugin_registry
                        .as_deref()
                        .and_then(crate::plugins::PluginRegistry::caller_selection)
                    && self
                        .config
                        .servers
                        .values()
                        .filter_map(|server| server.reviewed_plugin.as_ref()?.native_mcp.as_ref())
                        .all(|receipt| Some(receipt.selection()) == selection),
                "Native MCP pool belongs to another caller composition"
            );
            crate::extension_host::validate_caller_plugins(plugins).map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }
    /// Get or create a connection to a server
    pub async fn get_or_connect(&mut self, server_name: &str) -> Result<&mut McpConnection> {
        // Lazy auto-reload (#1267 part 2): cheap mtime-then-hash check before
        // each connection lookup. Transient FS errors are logged but not
        // propagated so a brief hiccup can't take down the whole tool dispatch.
        if let Err(e) = self.reload_if_config_changed().await {
            tracing::warn!("MCP config reload check failed: {e:#}");
        }

        self.require_server(server_name)?;
        let plugin_source = self
            .connections
            .get(server_name)
            .and_then(|connection| connection.config().reviewed_plugin.clone())
            .or_else(|| {
                self.config
                    .servers
                    .get(server_name)
                    .and_then(|config| config.reviewed_plugin.clone())
            });
        if let Some(source) = plugin_source
            && let Err(error) = source.validate_before_use(server_name, "use")
        {
            self.drop_connection(server_name, "plugin authority revoked or changed");
            return Err(error);
        }

        // Authority was just validated above for this same source; checking
        // it again here would re-hash the bundle within one dispatch (#6209).
        let is_ready = self
            .connections
            .get(server_name)
            .map(McpConnection::is_transport_ready)
            .unwrap_or(false);
        if is_ready {
            return self
                .connections
                .get_mut(server_name)
                .ok_or_else(|| anyhow::anyhow!("MCP connection disappeared for {server_name}"));
        }

        // Take (don't drop) the stale connection: if the reconnect attempt
        // below fails, the previous connection is restored so its last-good
        // tool catalog stays model-visible during the outage instead of
        // disappearing with a dropped transport (#6187).
        let previous_connection = self.connections.remove(server_name);
        if previous_connection.is_some() {
            tracing::debug!(
                target: "mcp",
                server = %server_name,
                reason = "reconnect",
                "detached MCP connection for reconnect"
            );
        }

        // Check static config first, then dynamic servers
        let server_config = self
            .config
            .servers
            .get(server_name)
            .cloned()
            .or_else(|| self.dynamic_servers.read().get(server_name).cloned())
            .ok_or_else(|| anyhow::anyhow!("Failed to find MCP server: {server_name}"))?;

        if !server_config.is_enabled() {
            anyhow::bail!("Failed to connect MCP server '{server_name}': server is disabled");
        }

        let mut connection = match McpConnection::connect_with_backend(
            server_name.to_string(),
            server_config,
            &self.config.timeouts,
            self.network_policy.as_ref(),
            self.backend,
        )
        .await
        {
            Ok(connection) => connection,
            Err(error) => {
                self.note_connect_failure(server_name, &error);
                if let Some(previous) = previous_connection {
                    tracing::debug!(
                        target: "mcp",
                        server = %server_name,
                        "reconnect failed; restored the previous MCP connection and its last-good catalog"
                    );
                    self.connections.insert(server_name.to_string(), previous);
                }
                return Err(error);
            }
        };
        connection.catalog_generation = self.catalog_generation.load(Ordering::SeqCst);

        self.store_ready_connection(server_name.to_string(), connection)?;
        self.connections
            .get_mut(server_name)
            .ok_or_else(|| anyhow::anyhow!("Failed to store MCP connection for {server_name}"))
    }

    /// Retry exactly one server against the configuration already owned by
    /// this pool.
    ///
    /// Unlike normal lazy tool dispatch, an explicit row retry must not notice
    /// a concurrent config mtime and invalidate healthy siblings. Config edits
    /// remain owned by the explicit reload path; this operation only replaces
    /// the named transport.
    pub async fn retry_connection(&mut self, server_name: &str) -> Result<&mut McpConnection> {
        self.require_server(server_name)?;
        // A person asked for this one by name. Clear the cooldown so the
        // attempt happens now and, if it fails again, the ladder restarts
        // from the short end rather than from wherever it had climbed to.
        // Explicit intent restarts supervision: the cooldown, the dead mark,
        // and any park all clear, so the supervisor resumes watching whatever
        // this retry stores — or stays quiet while the server is connectionless.
        self.connect_backoff.remove(server_name);
        self.supervised_dead.remove(server_name);
        self.supervised_parked.remove(server_name);
        let plugin_source = self
            .connections
            .get(server_name)
            .and_then(|connection| connection.config().reviewed_plugin.clone())
            .or_else(|| {
                self.config
                    .servers
                    .get(server_name)
                    .and_then(|config| config.reviewed_plugin.clone())
            });
        if let Some(source) = plugin_source
            && let Err(error) = source.validate_before_use(server_name, "use")
        {
            self.drop_connection(server_name, "plugin authority revoked or changed");
            return Err(error);
        }

        self.drop_connection(server_name, "retry");

        let server_config = self
            .config
            .servers
            .get(server_name)
            .cloned()
            .or_else(|| self.dynamic_servers.read().get(server_name).cloned())
            .ok_or_else(|| anyhow::anyhow!("Failed to find MCP server: {server_name}"))?;

        if !server_config.is_enabled() {
            anyhow::bail!("Failed to connect MCP server '{server_name}': server is disabled");
        }

        let mut connection = match McpConnection::connect_with_backend(
            server_name.to_string(),
            server_config,
            &self.config.timeouts,
            self.network_policy.as_ref(),
            self.backend,
        )
        .await
        {
            Ok(connection) => connection,
            Err(error) => {
                self.note_connect_failure(server_name, &error);
                return Err(error);
            }
        };
        connection.catalog_generation = self.current_catalog_generation();
        self.store_ready_connection(server_name.to_string(), connection)?;
        self.connections
            .get_mut(server_name)
            .ok_or_else(|| anyhow::anyhow!("Failed to store MCP connection for {server_name}"))
    }

    pub(crate) fn store_ready_connection(
        &mut self,
        name: String,
        connection: McpConnection,
    ) -> Result<()> {
        self.require_server(&name)?;
        anyhow::ensure!(
            connection.catalog_generation == self.current_catalog_generation(),
            "MCP configuration changed while connecting {name}; retry against the current config"
        );
        if let Some(source) = connection.config().reviewed_plugin.as_ref() {
            source.validate_before_use(&name, "use")?;
        }
        // A successful connect settles the auth question for this server,
        // and the cooldown with it — plus any supervisor dead mark or park,
        // since a stored-ready connection is alive by construction.
        self.connecting.remove(&name);
        self.connect_backoff.remove(&name);
        self.supervised_dead.remove(&name);
        self.supervised_parked.remove(&name);
        if self.needs_auth_servers.remove(&name) {
            self.needs_auth_generation = self.needs_auth_generation.wrapping_add(1);
        }
        self.connections.insert(name, connection);
        Ok(())
    }

    /// Record a connect failure's auth classification. When the failure looks
    /// like a missing/expired OAuth login, the next model catalog offers the
    /// synthetic `mcp_<server>_authenticate` tool so the model can self-serve
    /// the login instead of dead-ending on the error item. A non-auth
    /// failure replaces the verdict — the state is "the most recent connect
    /// failed auth-required", not "some connect once did".
    pub(crate) fn note_connect_failure(&mut self, name: &str, error: &anyhow::Error) {
        self.connecting.remove(name);
        if !self.server_allowed(name) {
            return;
        }
        let entry = self
            .connect_backoff
            .entry(name.to_string())
            .or_insert(ConnectBackoff {
                consecutive_failures: 0,
                retry_after: std::time::Instant::now(),
                last_error: String::new(),
            });
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        entry.retry_after =
            std::time::Instant::now() + connect_backoff_delay(entry.consecutive_failures);
        entry.last_error = format_mcp_error_for_display(error);
        let changed = if self.error_needs_oauth_login(name, error) {
            self.needs_auth_servers.insert(name.to_string())
        } else {
            self.needs_auth_servers.remove(name)
        };
        if changed {
            self.needs_auth_generation = self.needs_auth_generation.wrapping_add(1);
        }
    }

    /// Whether `error` from `name` is an OAuth-style auth-required failure.
    /// An expired AWS login on a server that is not OAuth-capable often says
    /// `401`/`Unauthorized` too, but `/mcp login` and the synthetic
    /// authenticate tool cannot renew it: it is excluded here so the
    /// needs-auth set (and every surface derived from it) never misroutes it.
    fn error_needs_oauth_login(&self, name: &str, error: &anyhow::Error) -> bool {
        if !oauth::error_looks_auth_required(error) {
            return false;
        }
        let oauth_capable = self
            .server_config(name)
            .is_some_and(|config| mcp_server_oauth_capable(&config));
        !mcp_error_is_aws_login(&format!("{error:#}"), oauth_capable)
    }

    /// Current needs-auth surface generation. Compare across a tool call to
    /// learn whether the call flipped a server into or out of the
    /// `◆ auth required` state (a live 401, or a login that landed).
    #[must_use]
    pub fn needs_auth_generation(&self) -> u64 {
        self.needs_auth_generation
    }

    /// Whether the server's most recent connect attempt failed auth-required
    /// (the typed `◆ auth required` state). Cleared by a successful connect
    /// and by any full connection drop (reload, source switch, shutdown).
    #[must_use]
    pub fn server_needs_auth(&self, name: &str) -> bool {
        self.server_allowed(name) && self.needs_auth_servers.contains(name)
    }

    /// The needs-auth server that owns a model tool name (`mcp_<server>_…`),
    /// if any: the server the model is trying to reach with a real tool name
    /// from a catalog built before its login lapsed. Longest configured name
    /// wins so `mcp_a_b_tool` routes to server `a_b` over `a`.
    fn needs_auth_server_for_tool_name(&self, prefixed_name: &str) -> Option<String> {
        let rest = prefixed_name.strip_prefix("mcp_")?;
        self.needs_auth_servers
            .iter()
            .filter(|server| {
                self.server_allowed(server)
                    && self.tool_allowed(prefixed_name)
                    && rest
                        .strip_prefix(server.as_str())
                        .is_some_and(|suffix| suffix.starts_with('_'))
            })
            .max_by_key(|server| server.len())
            .cloned()
    }

    /// Peak concurrent spawn+handshake attempts. Uncapped, a config full of
    /// `npx` servers would start one node runtime per server at the same
    /// instant — a memory spike on low-end machines the sequential loop never
    /// produced. Eight keeps wall-clock wins (the connect timeout dominates)
    /// while bounding peak memory.
    const CONNECT_CONCURRENCY: usize = 8;

    /// Consecutive failed reconnects after which the supervisor parks a
    /// server instead of redialing it. The cooldown ladder already spaces
    /// attempts, but a server that never answers (wrong binary, dead port)
    /// should not burn a spawn+handshake every sweep forever. A stored-ready
    /// connection or an explicit `/mcp retry` clears the park — and every
    /// success resets the count, so an occasionally-crashing server keeps
    /// recovering instead of parking.
    const SUPERVISOR_PARK_AFTER_CONSECUTIVE_FAILURES: u32 = 5;

    /// Supervisor sweep cadence. Death is noticed within one tick; an idle
    /// tick costs one pool lock plus a `try_wait` per stdio child.
    const SUPERVISOR_TICK: std::time::Duration = std::time::Duration::from_secs(5);

    /// One supervisor sweep's reconnect candidates, computed under a brief
    /// pool lock. Handshakes run outside the lock via
    /// [`Self::spawn_pending_connects`], so a wedged server never blocks a
    /// live turn's pool access while it burns its connect timeout.
    pub(crate) fn plan_supervision(&mut self) -> SupervisionPlan {
        let dynamic = self.dynamic_servers.read();
        let candidates: Vec<(String, McpServerConfig)> = self
            .config
            .servers
            .iter()
            .filter(|(name, server)| server.is_enabled() && self.server_allowed(name))
            .map(|(name, server)| (name.clone(), server.clone()))
            .chain(
                dynamic
                    .iter()
                    .filter(|(_, server)| server.is_enabled())
                    .map(|(name, server)| (name.clone(), server.clone())),
            )
            .collect();
        drop(dynamic);
        let watched: HashSet<String> = candidates.iter().map(|(name, _)| name.clone()).collect();
        // Silent prune: manual retries drop connections the supervisor never
        // re-spawns (on-demand reconnect owns connectionless servers), and
        // removed/disabled servers leave supervision without an event.
        self.supervised_dead
            .retain(|name| watched.contains(name) && self.connections.contains_key(name));
        self.supervised_parked.retain(|name| watched.contains(name));
        let mut due = Vec::new();
        let mut recovered = Vec::new();
        let mut parked = Vec::new();
        let now = std::time::Instant::now();
        for (name, config) in candidates {
            let Some(connection) = self.connections.get(&name) else {
                continue;
            };
            if connection.is_transport_ready() {
                if self.supervised_dead.remove(&name) {
                    self.supervised_parked.remove(&name);
                    recovered.push(name);
                }
                continue;
            }
            // A login-pending server cannot be fixed by redialing; the auth
            // surface owns it. It stays out of the dead set so recovery via
            // login reports nothing stale.
            if self.needs_auth_servers.contains(&name) {
                continue;
            }
            let fresh_death = self.supervised_dead.insert(name.clone());
            if self.connecting.contains(&name) {
                continue;
            }
            if let Some(backoff) = self.connect_backoff.get(&name) {
                if backoff.consecutive_failures >= Self::SUPERVISOR_PARK_AFTER_CONSECUTIVE_FAILURES
                {
                    if self.supervised_parked.insert(name.clone()) {
                        parked.push(name);
                    }
                    continue;
                }
                if now < backoff.retry_after {
                    continue;
                }
            }
            due.push(SupervisionDue {
                name,
                config,
                fresh_death,
            });
        }
        SupervisionPlan {
            backend: self.backend,
            due,
            recovered,
            parked,
            timeouts: self.config.timeouts,
            network_policy: self.network_policy.clone(),
            catalog_generation: self.catalog_generation.load(Ordering::SeqCst),
        }
    }

    /// Resolve one supervised reconnect attempt. Success stores the live
    /// connection (which clears the backoff, the dead mark, and any park);
    /// failure records the backoff and reports the death or the repeated
    /// failure with the diagnosis, parking on the threshold crossing.
    pub(crate) fn resolve_supervision_attempt(
        &mut self,
        name: &str,
        fresh_death: bool,
        result: Result<McpConnection, anyhow::Error>,
    ) -> McpSupervisorUpdate {
        let mut update = McpSupervisorUpdate::default();
        let stored =
            result.and_then(|connection| self.store_ready_connection(name.to_string(), connection));
        match stored {
            Ok(()) => {
                if !fresh_death {
                    update.recovered.push(name.to_string());
                }
            }
            Err(error) => {
                self.note_connect_failure(name, &error);
                let last_error = self
                    .connect_backoff
                    .get(name)
                    .map(|backoff| backoff.last_error.clone())
                    .unwrap_or_else(|| format!("{error:#}"));
                if fresh_death {
                    update.died.push((name.to_string(), last_error));
                } else {
                    update.failed.push((name.to_string(), last_error));
                }
                if self.connect_backoff.get(name).is_some_and(|backoff| {
                    backoff.consecutive_failures >= Self::SUPERVISOR_PARK_AFTER_CONSECUTIVE_FAILURES
                }) && self.supervised_parked.insert(name.to_string())
                {
                    update.parked.push(name.to_string());
                }
            }
        }
        update
    }

    /// Watch every live connection and reconnect the dead ones. Exits when
    /// the pool is dropped (the engine holds the only strong reference) or
    /// the engine stops listening. Reports transitions only, so the engine
    /// emits a snapshot update exactly when something changed (#6187).
    pub(crate) async fn supervise_pool(
        pool: std::sync::Weak<tokio::sync::Mutex<McpPool>>,
        tx: tokio::sync::mpsc::Sender<McpSupervisorUpdate>,
    ) {
        loop {
            tokio::time::sleep(Self::SUPERVISOR_TICK).await;
            let Some(pool) = pool.upgrade() else { break };
            let plan = pool.lock().await.plan_supervision();
            if plan.due.is_empty() && plan.recovered.is_empty() && plan.parked.is_empty() {
                continue;
            }
            let mut connects = Self::spawn_pending_connects(
                plan.due
                    .iter()
                    .map(|due| (due.name.clone(), due.config.clone()))
                    .collect(),
                plan.timeouts,
                plan.network_policy.clone(),
                plan.catalog_generation,
                plan.backend,
            );
            let mut update = McpSupervisorUpdate {
                recovered: plan.recovered,
                parked: plan.parked,
                ..Default::default()
            };
            let fresh_by_name: HashMap<String, bool> = plan
                .due
                .into_iter()
                .map(|due| (due.name, due.fresh_death))
                .collect();
            while let Some(joined) = connects.join_next().await {
                let (name, result) = joined
                    .unwrap_or_else(|error| ("connection task".to_string(), Err(error.into())));
                let fresh_death = fresh_by_name.get(&name).copied().unwrap_or(false);
                let resolution =
                    pool.lock()
                        .await
                        .resolve_supervision_attempt(&name, fresh_death, result);
                update.merge(resolution);
            }
            if !update.is_empty() && tx.send(update).await.is_err() {
                break;
            }
        }
    }

    /// Collect the configured servers a connect pass should start. `only`
    /// scopes the pass to the given names; `None` connects every enabled,
    /// allowed server (`connect_all`). Dynamic runtime servers stay
    /// registered and connect via [`Self::get_or_connect`]; connect passes
    /// have never spawned them. Every emitted name is marked
    /// [`Self::connecting`] until its spawn resolves.
    pub(crate) fn collect_pending_connects(
        &mut self,
        only: Option<&HashSet<String>>,
    ) -> (Vec<McpPendingConnect>, Vec<McpConnectError>) {
        let names: Vec<String> = self
            .config
            .servers
            .iter()
            .filter(|(name, server)| server.is_enabled() && self.server_allowed(name))
            .filter(|(name, _)| only.is_none_or(|set| set.contains(*name)))
            .map(|(name, _)| name.clone())
            .collect();
        let mut pending = Vec::new();
        let mut errors = Vec::new();
        for name in names {
            let Some(server_config) = self.config.servers.get(&name).cloned() else {
                continue;
            };

            let plugin_source = self
                .connections
                .get(&name)
                .and_then(|connection| connection.config().reviewed_plugin.clone())
                .or_else(|| server_config.reviewed_plugin.clone());
            if let Some(source) = plugin_source
                && let Err(error) = source.validate_before_use(&name, "use")
            {
                self.drop_connection(&name, "plugin authority revoked or changed");
                errors.push((name, error));
                continue;
            }

            // Authority validated immediately above for this same source.
            if self
                .connections
                .get(&name)
                .is_some_and(McpConnection::is_transport_ready)
            {
                continue;
            }
            // Inside its cooldown a failed server costs nothing and still
            // tells the truth: the recorded diagnosis is replayed so the row
            // keeps reading `error`, rather than going quiet and looking
            // healthy because nobody asked.
            if let Some(backoff) = self.connect_backoff.get(&name)
                && std::time::Instant::now() < backoff.retry_after
            {
                errors.push((name, anyhow::anyhow!(backoff.last_error.clone())));
                continue;
            }
            if self.connecting.contains(&name) {
                // An earlier pass spawned this connect and it has not
                // resolved; a second pass must not spawn a duplicate.
                continue;
            }
            self.drop_connection(&name, "reconnect");
            self.connecting.insert(name.clone());
            pending.push((name, server_config));
        }
        (pending, errors)
    }

    /// Start connects for servers an explicit tool selection named. Unlike a
    /// boot pass the selection is the intent — cooldowns do not apply — but
    /// servers already ready or already in flight are left alone, and plugin
    /// authority is re-validated exactly as in
    /// [`Self::collect_pending_connects`]. Covers dynamic servers too: a
    /// selection can name one.
    pub(crate) fn take_pending_connects_for(
        &mut self,
        names: &[String],
    ) -> (Vec<McpPendingConnect>, Vec<McpConnectError>) {
        let mut pending = Vec::new();
        let mut errors = Vec::new();
        for name in names {
            let Some(server_config) = self.server_config(name) else {
                continue;
            };
            if !server_config.is_enabled() || !self.server_allowed(name) {
                continue;
            }
            let plugin_source = self
                .connections
                .get(name)
                .and_then(|connection| connection.config().reviewed_plugin.clone())
                .or_else(|| server_config.reviewed_plugin.clone());
            if let Some(source) = plugin_source
                && let Err(error) = source.validate_before_use(name, "use")
            {
                self.drop_connection(name, "plugin authority revoked or changed");
                errors.push((name.clone(), error));
                continue;
            }
            // Authority validated immediately above for this same source.
            if self
                .connections
                .get(name)
                .is_some_and(McpConnection::is_transport_ready)
                || !self.connecting.insert(name.clone())
            {
                continue;
            }
            self.drop_connection(name, "reconnect");
            pending.push((name.clone(), server_config));
        }
        (pending, errors)
    }

    /// Forget in-flight marks for connects whose spawns were aborted before
    /// resolution (boot-pass abort on config change, deadline expiry).
    pub(crate) fn cancel_connecting(&mut self, names: &HashSet<String>) {
        self.connecting.retain(|name| !names.contains(name));
    }

    /// Servers with a connect in flight right now — the one honest answer to
    /// "which servers are connecting" (#6033).
    pub(crate) fn connecting_servers(&self) -> Vec<String> {
        self.connecting.iter().cloned().collect()
    }

    /// Enabled, allowed configured servers the boot pass starts at session
    /// start: every user-configured one except those marked `lazy = true`.
    /// Plugin-contributed servers and lazy ones connect on demand unless
    /// `required` or covered by the session's explicit tool selections.
    pub(crate) fn eager_boot_server_names(&self, requested: &[String]) -> HashSet<String> {
        self.config
            .servers
            .iter()
            .filter(|(name, server)| server.is_enabled() && self.server_allowed(name))
            .filter(|(name, server)| {
                server.required
                    || (!server.lazy && server.reviewed_plugin.is_none())
                    || tool_selection_covers_server(requested, name)
            })
            .map(|(name, _)| name.clone())
            .collect()
    }

    /// Configured servers a turn may name to the model, sorted: enabled,
    /// allowed and `permitted`, the same fail-closed gates
    /// [`Self::configured_servers_for_search`] applies before a handshake.
    /// Names only — nothing is connected and no schema is read.
    pub(crate) fn discoverable_configured_servers(
        &self,
        permitted: impl Fn(&str) -> bool,
    ) -> Vec<String> {
        let mut names: Vec<String> = self
            .config
            .servers
            .iter()
            .filter(|(name, config)| {
                config.is_enabled() && self.server_allowed(name) && permitted(name)
            })
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }

    /// Match discovery intent against actual configured server identities only.
    /// No guessed tool/schema is inserted; the caller still obtains tools/list.
    pub(crate) fn configured_servers_for_search(
        &self,
        query: &str,
        match_kind: &str,
        permitted: impl Fn(&str) -> bool,
    ) -> Result<Vec<String>> {
        const MAX_DISCOVERY_SERVERS: usize = 8;
        let regex = match match_kind {
            "regex" => Some(crate::regex_cache::compile_user_regex(query)?),
            "bm25" => None,
            _ => anyhow::bail!("Unsupported tool search match algorithm"),
        };
        let query = query.trim().to_ascii_lowercase();
        let mut names = Vec::new();
        for (name, config) in &self.config.servers {
            if !config.is_enabled() || !self.server_allowed(name) || !permitted(name) {
                continue;
            }
            let name_lower = name.to_ascii_lowercase();
            let prefix = format!("mcp_{name_lower}_");
            let matches = if let Some(regex) = &regex {
                // A general regex such as .* is not MCP launch intent. Match
                // only a named server, or an explicit MCP namespace search.
                (query.contains("mcp_")
                    && (regex.is_match(&prefix)
                        || query.trim_start_matches('^').starts_with(&prefix)))
                    || query == name_lower
            } else {
                query.split_whitespace().any(|term| {
                    term == "mcp"
                        || term == name_lower
                        || term.starts_with(&prefix)
                        || (term.len() >= 3 && name_lower.contains(term))
                })
            };
            if matches {
                names.push(name.clone());
            }
        }
        names.sort();
        names.truncate(MAX_DISCOVERY_SERVERS);
        Ok(names)
    }

    /// Enabled, allowed servers — configured or dynamic — covered by an
    /// explicit tool selection (`mcp_<server>_*` names or `mcp_<prefix>*`
    /// globs). These are the names a turn is allowed to start on demand.
    pub(crate) fn explicitly_selected_server_names(&self, requested: &[String]) -> Vec<String> {
        let dynamic = self.dynamic_servers.read();
        self.config
            .servers
            .iter()
            .chain(dynamic.iter())
            .filter(|(name, server)| server.is_enabled() && self.server_allowed(name))
            .filter(|(name, _)| tool_selection_covers_server(requested, name))
            .map(|(name, _)| name.clone())
            .collect()
    }

    pub(crate) fn push_required_server_errors(&self, errors: &mut Vec<McpConnectError>) {
        for (name, server_cfg) in &self.config.servers {
            // Only stand in for a missing diagnosis. When the connect attempt
            // above already reported why this server failed, appending a
            // second, contentless entry for the same name buries it: callers
            // fold these pairs into a `HashMap<name, message>`, so the later
            // generic string silently replaced the real cause.
            if self.server_allowed(name)
                && server_cfg.required
                && server_cfg.is_enabled()
                && !self
                    .connections
                    .get(name)
                    .is_some_and(McpConnection::is_ready)
                && !errors.iter().any(|(failed, _)| failed == name)
            {
                errors.push((
                    name.clone(),
                    anyhow::anyhow!("required MCP server failed to initialize"),
                ));
            }
        }
    }

    /// Handshake the pending servers concurrently without holding the pool
    /// lock. Callers insert results under a short lock so a live turn can
    /// snapshot ready tools while optional servers are still connecting.
    pub(crate) fn spawn_pending_connects(
        pending: Vec<McpPendingConnect>,
        timeouts: McpTimeouts,
        network_policy: Option<NetworkPolicyDecider>,
        catalog_generation: u64,
        backend: McpBackend,
    ) -> tokio::task::JoinSet<(String, Result<McpConnection, anyhow::Error>)> {
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(Self::CONNECT_CONCURRENCY));
        let mut joins: tokio::task::JoinSet<(String, Result<McpConnection, anyhow::Error>)> =
            tokio::task::JoinSet::new();
        for (name, config) in pending {
            let permit = semaphore.clone();
            let network_policy = network_policy.clone();
            joins.spawn(async move {
                let connection = std::panic::AssertUnwindSafe(async {
                    let _permit = permit.acquire_owned().await;
                    McpConnection::connect_with_backend(
                        name.clone(),
                        config,
                        &timeouts,
                        network_policy.as_ref(),
                        backend,
                    )
                    .await
                    .map(|mut connection| {
                        connection.catalog_generation = catalog_generation;
                        connection
                    })
                })
                .catch_unwind()
                .await
                .unwrap_or_else(|_| Err(anyhow::anyhow!("MCP connection task panicked")));
                (name, connection)
            });
        }

        joins
    }

    /// Connect to all enabled servers, returning errors for failed connections.
    ///
    /// Servers connect **concurrently** (bounded by [`Self::CONNECT_CONCURRENCY`]).
    /// This used to be a sequential loop over `get_or_connect`, so every
    /// server paid the slowest server's spawn+handshake from its own budget:
    /// with the default 10s connect timeout, N servers meant a worst case of
    /// N×10s before the pool was usable. Each connection still gets its own
    /// configured connect timeout; one wedged server can no longer serialize
    /// the rest.
    ///
    /// Semantics preserved from the sequential loop: only configured servers
    /// are connected (dynamic runtime entries stay registered), the config is
    /// reloaded before the name snapshot (so a server added mid-session
    /// connects on this call, not the next), plugin-authority revocation
    /// drops the connection instead of silently reconnecting, and the
    /// required-server sweep reports at most one error per name. Config edits
    /// that land while the batch is in flight are reconciled by one retry
    /// pass: a content change drops every connection the previous pass
    /// inserted.
    pub async fn connect_all(&mut self) -> Vec<(String, anyhow::Error)> {
        let mut errors = Vec::new();
        // Reload before taking the configured-name snapshot. Previously the
        // first call after adding a server captured the old names, then only
        // noticed the config change inside `get_or_connect`, delaying the new
        // server until a second turn.
        if let Err(err) = self.reload_if_config_changed().await {
            errors.push(("configuration".to_string(), err));
            return errors;
        }

        // Drop needs-auth markers for servers that are no longer connectable
        // (removed from config or disabled); the loop below re-marks any
        // server whose fresh connect attempt still fails auth-required.
        {
            let dynamic = self.dynamic_servers.read();
            let before = self.needs_auth_servers.len();
            self.needs_auth_servers.retain(|name| {
                self.config
                    .servers
                    .get(name)
                    .is_some_and(|server| server.is_enabled())
                    || dynamic.get(name).is_some_and(|server| server.is_enabled())
            });
            if self.needs_auth_servers.len() != before {
                self.needs_auth_generation = self.needs_auth_generation.wrapping_add(1);
            }
        }

        for _pass in 0..2 {
            let (pending, auth_errors) = self.collect_pending_connects(None);
            errors.extend(auth_errors);
            if pending.is_empty() {
                break;
            }

            let mut connects = Self::spawn_pending_connects(
                pending,
                self.config.timeouts,
                self.network_policy.clone(),
                self.catalog_generation.load(Ordering::SeqCst),
                self.backend,
            );
            while let Some(joined) = connects.join_next().await {
                let (name, result) = joined
                    .unwrap_or_else(|error| ("connection task".to_string(), Err(error.into())));
                let result = result
                    .and_then(|connection| self.store_ready_connection(name.clone(), connection));
                if let Err(error) = result {
                    self.note_connect_failure(&name, &error);
                    errors.push((name, error));
                }
            }

            // Reconcile a config edit that landed mid-batch: a content
            // change dropped every connection this pass inserted, so run one
            // more pass against the new config and drop the stale pass's
            // errors with it.
            match self.reload_if_config_changed().await {
                Ok(true) => {
                    errors.clear();
                    continue;
                }
                Ok(false) => break,
                Err(error) => {
                    errors.push(("configuration".to_string(), error));
                    break;
                }
            }
        }

        self.push_required_server_errors(&mut errors);
        errors
    }

    /// The single definition of an MCP tool's model-facing name.
    ///
    /// [`Self::all_tools`] (which builds the model catalog) and
    /// [`Self::resolved_tool_servers`] (which tells tool inspection which
    /// server owns a name) both call this, so a human-facing server
    /// attribution can never drift from the name the model actually received.
    #[must_use]
    pub fn mcp_model_tool_name(server: &str, tool: &str) -> String {
        format!("mcp_{server}_{tool}")
    }

    /// Map an exact `mcp_<server>_authenticate` model tool name to its server
    /// when the synthetic self-serve OAuth login tool should answer for it:
    /// the server's last connect failed auth-required, or the name matches a
    /// configured OAuth-capable server whose catalog entry is stale (a login
    /// completed since the catalog was built). Never fires when a ready
    /// connection advertises a real tool under the same model name — the
    /// server's own `authenticate` tool always wins.
    pub(crate) fn authenticate_tool_target(&self, prefixed_name: &str) -> Option<String> {
        if !self.tool_allowed(prefixed_name) {
            return None;
        }
        let target = self
            .needs_auth_servers
            .iter()
            .find(|server| {
                Self::mcp_model_tool_name(server, AUTHENTICATE_TOOL_NAME) == prefixed_name
            })
            .cloned()
            .or_else(|| {
                let dynamic = self.dynamic_servers.read();
                self.config
                    .servers
                    .iter()
                    .chain(dynamic.iter())
                    .find(|(name, _)| {
                        Self::mcp_model_tool_name(name, AUTHENTICATE_TOOL_NAME) == prefixed_name
                    })
                    .map(|(name, _)| name.clone())
            })?;
        let capable = {
            let dynamic = self.dynamic_servers.read();
            self.config
                .servers
                .get(&target)
                .or_else(|| dynamic.get(&target))
                .is_some_and(oauth::server_supports_oauth_login)
        };
        if !capable || !self.server_allowed(&target) {
            return None;
        }
        if self.parse_prefixed_name(prefixed_name).is_ok() {
            return None;
        }
        Some(target)
    }

    /// The configured server by name, static config first, then the
    /// session's dynamically added servers.
    fn server_config(&self, server_name: &str) -> Option<McpServerConfig> {
        self.config
            .servers
            .get(server_name)
            .cloned()
            .or_else(|| self.dynamic_servers.read().get(server_name).cloned())
    }

    /// Phase one of the synthetic `mcp_<server>_authenticate` tool: decide
    /// whether a browser login is needed and, if so, start it. Only touches
    /// config, the connection map, and the token store — it returns as soon
    /// as the authorization URL exists, so a caller holding the pool lock can
    /// release it before the (up to five minute) browser wait in
    /// [`oauth::McpOAuthToolLogin::finish`]. Holding the lock across that wait
    /// would freeze every other MCP call, the `/mcp` manager, and the
    /// Extensions view for the whole sign-in.
    pub(crate) async fn begin_authenticate_tool(
        &self,
        server_name: &str,
    ) -> Result<AuthenticateToolStart> {
        self.require_server(server_name)?;
        Self::authorize_call(
            &self.disallowed_tools,
            &Self::mcp_model_tool_name(server_name, AUTHENTICATE_TOOL_NAME),
            &serde_json::json!({}),
        )?;
        let server = self
            .server_config(server_name)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{server_name}' is no longer configured"))?;
        if !server.is_enabled() {
            anyhow::bail!("MCP server '{server_name}' is disabled");
        }

        // Already-authorized branch: a login that completed since the catalog
        // was built (e.g. `codewhale mcp login` in another window) must not
        // restart the browser flow — adopt the stored tokens by reconnecting.
        let ready = self
            .connections
            .get(server_name)
            .is_some_and(McpConnection::is_ready);
        if ready || oauth::has_usable_stored_tokens(server_name, &server) {
            return Ok(AuthenticateToolStart::AlreadyAuthorized);
        }
        let login = oauth::begin_oauth_login_for_server_tool(
            server_name,
            &server,
            None,
            self.oauth_callback_port,
            self.oauth_callback_url.as_deref(),
            self.network_policy.as_ref(),
        )
        .await?;
        Ok(AuthenticateToolStart::Login(Box::new(login)))
    }

    /// Phase two of the synthetic authenticate tool: reconnect the server so
    /// its real tools resolve in this session, and describe the outcome for
    /// the model. `get_or_connect` drops any non-ready connection itself, so
    /// both the fresh-login and already-authorized outcomes fall through to
    /// it. Errors are never swallowed — a reconnect that still fails
    /// auth-required re-marks the server (via `get_or_connect`) and surfaces
    /// truthfully for the model to relay.
    pub(crate) async fn finish_authenticate_tool(
        &mut self,
        server_name: &str,
        outcome: AuthenticateToolOutcome,
    ) -> Result<serde_json::Value> {
        self.require_server(server_name)?;
        let rules = self.disallowed_tools.clone();
        match self.get_or_connect(server_name).await {
            Ok(conn) => {
                let tools: Vec<String> = conn
                    .tools()
                    .iter()
                    .filter(|tool| conn.config().is_tool_enabled(&tool.name))
                    .map(|tool| Self::mcp_model_tool_name(server_name, &tool.name))
                    .filter(|name| {
                        !crate::core::engine::tool_catalog::tool_matches_any_rule(&rules, name)
                    })
                    .collect();
                let (status, detail) = match &outcome {
                    AuthenticateToolOutcome::Authenticated { .. } => (
                        "authenticated",
                        "authenticated successfully and is now connected",
                    ),
                    AuthenticateToolOutcome::AlreadyAuthorized => (
                        "already_authorized",
                        "already had valid OAuth credentials and is now connected",
                    ),
                };
                let mut result = serde_json::json!({
                    "status": status,
                    "server": server_name,
                    "tools": tools,
                    "message": format!(
                        "MCP server '{server_name}' {detail}. Its real MCP tools (listed in 'tools') replaced the synthetic authenticate tool and are callable from the next model request in this session."
                    ),
                });
                if let AuthenticateToolOutcome::Authenticated { authorization_url } = outcome {
                    result["authorization_url"] = serde_json::Value::String(authorization_url);
                }
                Ok(result)
            }
            Err(error) => {
                // A stored token the server just rejected must not feed the
                // loop again: `begin_authenticate_tool` short-circuits to
                // AlreadyAuthorized whenever usable-looking tokens exist, so
                // keeping them means the model re-calls the synthetic tool
                // forever while every reconnect fails auth-required. Drop
                // the durable copy so the next begin starts a real login.
                if matches!(outcome, AuthenticateToolOutcome::AlreadyAuthorized)
                    && oauth::error_looks_auth_required(&error)
                    && let Some(server) = self.server_config(server_name)
                {
                    match oauth::delete_oauth_tokens_for_server(server_name, &server) {
                        Ok(true) => tracing::info!(
                            target: "mcp",
                            server = %server_name,
                            "rejected stored OAuth token removed after failed AlreadyAuthorized reconnect"
                        ),
                        Ok(false) => {}
                        Err(delete_err) => tracing::warn!(
                            target: "mcp",
                            server = %server_name,
                            error = %delete_err,
                            "could not remove rejected stored OAuth token"
                        ),
                    }
                }
                Err(error).with_context(|| {
                    format!(
                        "MCP server '{server_name}' completed OAuth login but the reconnect failed"
                    )
                })
            }
        }
    }

    /// Execute the synthetic `mcp_<server>_authenticate` tool in place,
    /// holding `&mut self` (and therefore any enclosing pool lock) for the
    /// whole flow. Engine tool execution uses
    /// [`authenticate_tool_via_pool`] instead, which releases the shared
    /// pool between the two phases.
    async fn run_authenticate_tool(&mut self, server_name: &str) -> Result<serde_json::Value> {
        let outcome = match self.begin_authenticate_tool(server_name).await? {
            AuthenticateToolStart::AlreadyAuthorized => AuthenticateToolOutcome::AlreadyAuthorized,
            AuthenticateToolStart::Login(login) => {
                let authorization_url = login.authorization_url().to_string();
                login.finish().await?;
                AuthenticateToolOutcome::Authenticated { authorization_url }
            }
        };
        self.finish_authenticate_tool(server_name, outcome).await
    }

    /// The model-facing recovery for a server in the `◆ auth required`
    /// state: the one call that recovers it. Names the synthetic
    /// `mcp_<server>_authenticate` tool when the server is OAuth-servable, and
    /// otherwise the credential source (plugin environment header, manual
    /// bearer) the server is allowed to authenticate with.
    fn auth_required_hint(&self, server_name: &str) -> String {
        let tool_name = Self::mcp_model_tool_name(server_name, AUTHENTICATE_TOOL_NAME);
        if self.authenticate_tool_target(&tool_name).is_some() {
            return format!(
                "MCP server '{server_name}' requires OAuth login (◆ auth required); call the `{tool_name}` tool to authenticate, or run `/mcp login {server_name}`"
            );
        }
        let recovery = match self.server_config(server_name) {
            Some(server) => oauth::auth_required_recovery_hint(server_name, &server),
            None => oauth::auth_required_login_hint(server_name),
        };
        format!("MCP server '{server_name}' requires authentication (◆ auth required); {recovery}")
    }

    /// Route an auth-required failure from a live request into the same
    /// typed state a failed connect produces: drop the connection (its
    /// credential is no longer accepted), mark the server needs-auth so the
    /// next catalog offers the synthetic login tool, and name the recovery on
    /// the error. Any other error passes through untouched.
    fn note_live_call_failure(&mut self, server_name: &str, error: anyhow::Error) -> anyhow::Error {
        if !self.error_needs_oauth_login(server_name, &error) {
            // An expired AWS login on a live call names its own recovery
            // instead of passing through as a bare 401.
            let text = format!("{error:#}");
            if let Some(config) = self.server_config(server_name)
                && mcp_error_is_aws_login(&text, mcp_server_oauth_capable(&config))
            {
                let hint = aws_login_hint(&config, server_name, &text);
                self.drop_connection(server_name, "AWS credentials expired on live call");
                self.note_connect_failure(server_name, &error);
                return error.context(hint);
            }
            return error;
        }
        self.drop_connection(server_name, "auth required on live call");
        self.note_connect_failure(server_name, &error);
        error.context(self.auth_required_hint(server_name))
    }

    /// Fold `(server, tool)` pairs into `model name -> owning server`.
    ///
    /// Mirrors [`Self::all_tools`]' ambiguity rule: when two servers produce
    /// the same model name, the name is dropped entirely rather than
    /// attributed to an arbitrary winner. Callers then report it as unknown.
    #[must_use]
    pub fn resolve_tool_server_map<'a>(
        pairs: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> std::collections::BTreeMap<String, String> {
        let mut resolved: std::collections::BTreeMap<String, Option<String>> =
            std::collections::BTreeMap::new();
        for (server, tool) in pairs {
            match resolved.entry(Self::mcp_model_tool_name(server, tool)) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(Some(server.to_string()));
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.insert(None);
                }
            }
        }
        resolved
            .into_iter()
            .filter_map(|(name, server)| server.map(|server| (name, server)))
            .collect()
    }

    /// Model tool name -> owning server name, for the tools this pool actually
    /// resolved. Names the pool did not resolve are simply absent, so callers
    /// report them as unknown instead of parsing `mcp_{server}_{tool}` (a
    /// server name may itself contain `_`, so that split is a guess).
    ///
    /// Read-only projection used by tool inspection; it never connects or
    /// executes.
    #[must_use]
    pub fn resolved_tool_servers(&self) -> std::collections::BTreeMap<String, String> {
        Self::resolve_tool_server_map(self.connections.iter().flat_map(|(server, conn)| {
            let authorized = self.server_allowed(server) && conn.catalog_authorized();
            conn.tools().iter().filter_map(move |tool| {
                (authorized
                    && conn.config().is_tool_enabled(&tool.name)
                    && self.tool_allowed(&Self::mcp_model_tool_name(server, &tool.name)))
                .then_some((server.as_str(), tool.name.as_str()))
            })
        }))
    }

    /// Guidance from connected servers that may be put in front of the model,
    /// as `(server, instructions)` sorted by server name.
    ///
    /// A server qualifies only when it is ready, allowed and still authorized
    /// (the same gates as [`Self::resolved_tool_servers`]), supplied non-empty
    /// instructions, and owns at least one enabled tool for which
    /// `model_visible` holds — the caller passes the turn's final catalog, so
    /// a server whose tools are all denied by the permission posture
    /// contributes nothing.
    #[must_use]
    pub fn model_server_instructions(
        &self,
        model_visible: impl Fn(&str) -> bool,
    ) -> Vec<(String, String)> {
        let servers: BTreeSet<String> = self
            .resolved_tool_servers()
            .into_iter()
            .filter(|(tool, _)| model_visible(tool))
            .map(|(_, server)| server)
            .collect();
        servers
            .into_iter()
            .filter_map(|server| {
                let conn = self.connections.get(&server)?;
                if conn.state() != ConnectionState::Ready {
                    return None;
                }
                let text = conn.instructions()?.to_string();
                Some((server, text))
            })
            .collect()
    }

    /// Insert a ready, idle connection with the given tools and guidance, for
    /// tests outside this module that need a pool without spawning a server.
    #[cfg(test)]
    pub(crate) fn insert_test_connection(
        &mut self,
        server: &str,
        tools: &[&str],
        instructions: Option<&str>,
    ) {
        struct IdleTransport;
        #[async_trait::async_trait]
        impl McpTransport for IdleTransport {
            async fn send(&mut self, _msg: Vec<u8>) -> Result<()> {
                Ok(())
            }
            async fn recv(&mut self) -> Result<Vec<u8>> {
                anyhow::bail!("idle test transport has no responses")
            }
        }
        let config: McpServerConfig = serde_json::from_value(serde_json::json!({
            "command": "codewhale-test-idle-mcp"
        }))
        .expect("minimal server config");
        let conn = McpConnection {
            name: server.to_string(),
            transport: Box::new(IdleTransport),
            tools: tools
                .iter()
                .map(|name| McpTool {
                    name: (*name).to_string(),
                    description: None,
                    input_schema: serde_json::json!({"type": "object"}),
                    annotations: None,
                })
                .collect(),
            resources: Vec::new(),
            resource_templates: Vec::new(),
            prompts: Vec::new(),
            request_id: AtomicU64::new(1),
            state: ConnectionState::Ready,
            config,
            server_capabilities: None,
            instructions: instructions.map(str::to_string),
            discovery_timeout: Duration::from_secs(1),
            read_timeout_secs: 1,
            cancel_token: tokio_util::sync::CancellationToken::new(),
            authority_revocation_reason: Arc::new(std::sync::Mutex::new(None)),
            authority_watch: None,
            catalog_generation: 0,
            decision_key: None,
        };
        self.connections.insert(server.to_string(), conn);
    }

    /// Get all discovered tools with server-prefixed names
    pub fn all_tools(&self) -> Vec<(String, &McpTool)> {
        let mut by_name: std::collections::BTreeMap<String, Option<&McpTool>> =
            std::collections::BTreeMap::new();
        for (server, conn) in &self.connections {
            if !self.server_allowed(server) || !conn.catalog_authorized() {
                continue;
            }
            for tool in conn.tools() {
                if !conn.config().is_tool_enabled(&tool.name) {
                    continue;
                }
                let name = Self::mcp_model_tool_name(server, &tool.name);
                if !self.tool_allowed(&name) {
                    continue;
                }
                match by_name.entry(name.clone()) {
                    std::collections::btree_map::Entry::Vacant(entry) => {
                        entry.insert(Some(tool));
                    }
                    std::collections::btree_map::Entry::Occupied(mut entry) => {
                        tracing::warn!(
                            target: "mcp",
                            model_tool = %name,
                            "hiding ambiguous MCP model tool name"
                        );
                        entry.insert(None);
                    }
                }
            }
        }
        by_name
            .into_iter()
            .filter_map(|(name, tool)| tool.map(|tool| (name, tool)))
            .collect()
    }

    /// Get all discovered resources with server-prefixed names
    pub fn all_resources(&self) -> Vec<(String, &McpResource)> {
        let mut resources = Vec::new();
        for (server, conn) in &self.connections {
            if !self.server_allowed(server) || !conn.catalog_authorized() {
                continue;
            }
            for resource in conn.resources() {
                // Format: mcp_{server}_{resource_name}
                // Note: resource names might contain spaces, we should probably slugify them
                let safe_name = resource.name.replace(' ', "_").to_lowercase();
                resources.push((format!("mcp_{server}_{safe_name}"), resource));
            }
        }
        resources
    }

    /// Get all discovered resource templates with server-prefixed names
    #[allow(dead_code)] // Public API for MCP resource discovery
    pub fn all_resource_templates(&self) -> Vec<(String, &McpResourceTemplate)> {
        let mut templates = Vec::new();
        for (server, conn) in &self.connections {
            if !self.server_allowed(server) || !conn.catalog_authorized() {
                continue;
            }
            for template in conn.resource_templates() {
                let safe_name = template.name.replace(' ', "_").to_lowercase();
                templates.push((format!("mcp_{server}_{safe_name}"), template));
            }
        }
        templates
    }

    async fn list_resources(&mut self, server: Option<String>) -> Result<Vec<serde_json::Value>> {
        if let Some(server_name) = server {
            let conn = self.get_or_connect(&server_name).await?;
            let resources = conn
                .resources()
                .iter()
                .map(|resource| {
                    serde_json::json!({
                        "server": server_name.clone(),
                        "uri": resource.uri,
                        "name": resource.name,
                        "description": resource.description,
                        "mime_type": resource.mime_type,
                    })
                })
                .collect();
            return Ok(resources);
        }

        let mut items = Vec::new();
        let errors = self.connect_all().await;
        for (server, err) in errors {
            tracing::warn!("Failed to connect MCP server '{server}' for resources: {err:#}");
            if let Some(item) = self.mcp_recovery_error_item(&server, &err) {
                items.push(item);
            }
        }
        for (server, conn) in &self.connections {
            if !self.server_allowed(server) || !conn.catalog_authorized() {
                continue;
            }
            for resource in conn.resources() {
                items.push(serde_json::json!({
                    "server": server,
                    "uri": resource.uri,
                    "name": resource.name,
                    "description": resource.description,
                    "mime_type": resource.mime_type,
                }));
            }
        }
        Ok(items)
    }

    async fn list_resource_templates(
        &mut self,
        server: Option<String>,
    ) -> Result<Vec<serde_json::Value>> {
        if let Some(server_name) = server {
            let conn = self.get_or_connect(&server_name).await?;
            let templates = conn
                .resource_templates()
                .iter()
                .map(|template| {
                    serde_json::json!({
                        "server": server_name.clone(),
                        "uri_template": template.uri_template,
                        "name": template.name,
                        "description": template.description,
                        "mime_type": template.mime_type,
                    })
                })
                .collect();
            return Ok(templates);
        }

        let mut items = Vec::new();
        let errors = self.connect_all().await;
        for (server, err) in errors {
            tracing::warn!(
                "Failed to connect MCP server '{server}' for resource templates: {err:#}"
            );
            if let Some(item) = self.mcp_recovery_error_item(&server, &err) {
                items.push(item);
            }
        }
        for (server, conn) in &self.connections {
            if !self.server_allowed(server) || !conn.catalog_authorized() {
                continue;
            }
            for template in conn.resource_templates() {
                items.push(serde_json::json!({
                    "server": server,
                    "uri_template": template.uri_template,
                    "name": template.name,
                    "description": template.description,
                    "mime_type": template.mime_type,
                }));
            }
        }
        Ok(items)
    }

    /// Project recoverable connection failures for every resource listing.
    /// AWS CLI credentials cannot be renewed by the synthetic OAuth tool.
    fn mcp_recovery_error_item(
        &self,
        server: &str,
        error: &anyhow::Error,
    ) -> Option<serde_json::Value> {
        let text = format!("{error:#}");
        if let Some(config) = self.server_config(server)
            && mcp_error_is_aws_login(&text, mcp_server_oauth_capable(&config))
        {
            return Some(serde_json::json!({
                "error": "aws_login_required",
                "server": server,
                "message": aws_login_hint(&config, server, &text),
            }));
        }
        self.error_needs_oauth_login(server, error)
            .then(|| self.mcp_auth_required_error_item(server))
    }

    /// Listing-time error item for a needs-auth server. Carries the same
    /// recovery the tool-call path names (the synthetic authenticate tool
    /// when OAuth-servable) so a resource or prompt listing never dead-ends
    /// on a login the model could have self-served.
    fn mcp_auth_required_error_item(&self, server: &str) -> serde_json::Value {
        let mut item = serde_json::json!({
            "error": "authentication_required",
            "server": server,
            "message": self.auth_required_hint(server),
        });
        let tool_name = Self::mcp_model_tool_name(server, AUTHENTICATE_TOOL_NAME);
        if self.authenticate_tool_target(&tool_name).is_some() {
            item["authenticate_tool"] = serde_json::Value::String(tool_name);
        }
        item
    }

    /// Get all discovered prompts with server-prefixed names
    pub fn all_prompts(&self) -> Vec<(String, &McpPrompt)> {
        let mut prompts = Vec::new();
        for (server, conn) in &self.connections {
            if !self.server_allowed(server) || !conn.catalog_authorized() {
                continue;
            }
            for prompt in conn.prompts() {
                // Format: mcp_{server}_{prompt}
                prompts.push((format!("mcp_{}_{}", server, prompt.name), prompt));
            }
        }
        prompts
    }

    /// Read a resource from a specific server
    pub async fn read_resource(
        &mut self,
        server_name: &str,
        uri: &str,
    ) -> Result<serde_json::Value> {
        let global_timeouts = self.config.timeouts;
        let conn = self.get_or_connect(server_name).await?;
        let advertised_literal = conn.resources().iter().any(|resource| resource.uri == uri);
        let advertised_template = conn
            .resource_templates()
            .iter()
            .any(|template| resource_uri_matches_template(uri, &template.uri_template));
        if !advertised_literal && !advertised_template {
            anyhow::bail!("MCP resource URI '{uri}' was not advertised by server '{server_name}'");
        }
        let timeout = conn.config().effective_read_timeout(&global_timeouts);
        conn.read_resource(uri, timeout)
            .await
            .map_err(|error| self.note_live_call_failure(server_name, error))
    }

    /// Get a prompt from a specific server
    pub async fn get_prompt(
        &mut self,
        server_name: &str,
        prompt_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        let global_timeouts = self.config.timeouts;
        let conn = self.get_or_connect(server_name).await?;
        if !conn
            .prompts()
            .iter()
            .any(|prompt| prompt.name == prompt_name)
        {
            anyhow::bail!(
                "MCP prompt '{prompt_name}' was not advertised by server '{server_name}'"
            );
        }
        let timeout = conn.config().effective_execute_timeout(&global_timeouts);
        conn.get_prompt(prompt_name, arguments, timeout)
            .await
            .map_err(|error| self.note_live_call_failure(server_name, error))
    }

    /// Parse a prefixed name into (server_name, tool_name)
    pub(crate) fn parse_prefixed_name(&self, prefixed_name: &str) -> Result<(String, String)> {
        Self::authorize_call(
            &self.disallowed_tools,
            prefixed_name,
            &serde_json::json!({}),
        )?;
        let Some(rest) = prefixed_name.strip_prefix("mcp_") else {
            anyhow::bail!("Invalid MCP tool name: {prefixed_name}");
        };

        let mut matched: Option<(String, String)> = None;
        for (server, connection) in &self.connections {
            if !self.server_allowed(server) || !connection.catalog_authorized() {
                continue;
            }
            for tool in connection.tools() {
                if !connection.config().is_tool_enabled(&tool.name)
                    || format!("{server}_{}", tool.name) != rest
                {
                    continue;
                }
                if matched.is_some() {
                    anyhow::bail!(
                        "Ambiguous MCP tool name '{prefixed_name}' matches more than one server/tool authority"
                    );
                }
                matched = Some((server.clone(), tool.name.clone()));
            }
        }
        if let Some(matched) = matched {
            return Ok(matched);
        }

        Err(anyhow::anyhow!("Unknown MCP tool name: {prefixed_name}"))
    }

    /// Resolve an MCP tool through an exact advertised catalog. A configured
    /// but lazy server may be connected and asked for `tools/list`; the
    /// requested suffix is never treated as authority on its own.
    async fn resolve_advertised_tool(&mut self, prefixed_name: &str) -> Result<McpToolRoute> {
        Self::authorize_call(
            &self.disallowed_tools,
            prefixed_name,
            &serde_json::json!({}),
        )?;
        if let Ok((server_name, tool_name)) = self.parse_prefixed_name(prefixed_name) {
            return self.capture_tool_route(server_name, tool_name);
        }
        let Some(rest) = prefixed_name.strip_prefix("mcp_") else {
            anyhow::bail!("Invalid MCP tool name: {prefixed_name}");
        };
        let mut candidates = {
            let dynamic = self.dynamic_servers.read();
            self.config
                .servers
                .iter()
                .filter_map(|(name, config)| {
                    (config.is_enabled()
                        && self.server_allowed(name)
                        && rest
                            .strip_prefix(name)
                            .is_some_and(|suffix| suffix.starts_with('_')))
                    .then_some(name.clone())
                })
                .chain(dynamic.iter().filter_map(|(name, config)| {
                    (config.is_enabled()
                        && self.server_allowed(name)
                        && rest
                            .strip_prefix(name)
                            .is_some_and(|suffix| suffix.starts_with('_')))
                    .then_some(name.clone())
                }))
                .collect::<Vec<_>>()
        };
        candidates.sort();
        candidates.dedup();
        for server in candidates {
            // Connecting and catalog discovery are the only lazy side effects.
            // A guessed method is never sent to the transport.
            let _ = self.get_or_connect(&server).await?;
        }
        let (server_name, tool_name) = self.parse_prefixed_name(prefixed_name)?;
        self.capture_tool_route(server_name, tool_name)
    }

    fn capture_tool_route(&self, server_name: String, tool_name: String) -> Result<McpToolRoute> {
        let connection = self
            .connections
            .get(&server_name)
            .context("advertised MCP connection disappeared during resolution")?;
        let plugin_authority = connection
            .config()
            .reviewed_plugin
            .as_ref()
            .map(|source| source.authority.clone());
        Ok(McpToolRoute {
            server_name,
            tool_name,
            catalog_generation: connection.catalog_generation,
            plugin_authority,
        })
    }

    /// Every model-facing tool name the runtime MCP pool can own right now:
    /// the current `to_api_tools` output plus the synthetic
    /// `mcp_<server>_authenticate` name for every enabled OAuth-servable
    /// server, whether or not it is currently needs-auth. The turn loop
    /// uses this universe to REPLACE the pool's slice of the tool catalog
    /// instead of additively merging it — the synthetic entry must leave
    /// after a login, and dead real tools must leave after a live 401.
    /// The model-visible tool-name universe for an already-built catalog.
    ///
    /// Takes the catalog rather than rebuilding it: `to_api_tools` re-verifies
    /// every reviewed plugin bundle, so calling both meant hashing each bundle
    /// twice per turn to produce two views of one thing — and the two could
    /// disagree if authority drifted between them (#6209).
    pub fn model_tool_names(
        &self,
        api_tools: &[codewhale_models::Tool],
    ) -> std::collections::HashSet<String> {
        let mut names: std::collections::HashSet<String> =
            api_tools.iter().map(|tool| tool.name.clone()).collect();
        let dynamic = self.dynamic_servers.read();
        for (server, config) in self.config.servers.iter().chain(dynamic.iter()) {
            if self.server_allowed(server)
                && config.is_enabled()
                && oauth::server_supports_oauth_login(config)
                && self.tool_allowed(&Self::mcp_model_tool_name(server, AUTHENTICATE_TOOL_NAME))
            {
                names.insert(Self::mcp_model_tool_name(server, AUTHENTICATE_TOOL_NAME));
            }
        }
        names
    }

    /// Record the approval hints for this catalog's tools (CW-11). Every
    /// server this pool lists is rewritten, so a tool whose server lost its
    /// plugin review, or dropped a hint, loses the relaxation with it.
    fn record_tool_approval_hints(&self) {
        let mut hints = MCP_TOOL_APPROVAL_HINTS.write();
        for (server, conn) in &self.connections {
            let authorized = self.server_allowed(server) && conn.catalog_authorized();
            let reviewed_plugin = conn.config().reviewed_plugin.is_some();
            for tool in conn.tools() {
                let name = Self::mcp_model_tool_name(server, &tool.name);
                match approval_hint_for(tool, reviewed_plugin).filter(|_| authorized) {
                    Some(hint) => {
                        hints.insert(name, hint);
                    }
                    None => {
                        hints.remove(&name);
                    }
                }
            }
        }
    }

    /// Convert discovered tools to API Tool format
    pub fn to_api_tools(&self) -> Vec<codewhale_models::Tool> {
        self.record_tool_approval_hints();
        let mut api_tools = Vec::new();
        // Add regular tools
        for (name, tool) in self.all_tools() {
            api_tools.push(codewhale_models::Tool {
                tool_type: None,
                name,
                description: tool.description.clone().unwrap_or_default(),
                input_schema: tool.input_schema.clone(),
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
        }

        // A server whose last connect failed auth-required has no real tools
        // to advertise. In their place offer exactly one synthetic
        // `mcp_<server>_authenticate` tool so the model can self-serve the
        // OAuth login instead of dead-ending on the listing's error item.
        // Never shadow a real tool that owns the same model name.
        {
            let dynamic = self.dynamic_servers.read();
            for server in &self.needs_auth_servers {
                let Some(config) = self
                    .config
                    .servers
                    .get(server)
                    .or_else(|| dynamic.get(server))
                else {
                    continue;
                };
                if !self.server_allowed(server)
                    || !config.is_enabled()
                    || !oauth::server_supports_oauth_login(config)
                {
                    continue;
                }
                let name = Self::mcp_model_tool_name(server, AUTHENTICATE_TOOL_NAME);
                if !self.tool_allowed(&name) {
                    continue;
                }
                if api_tools.iter().any(|tool| tool.name == name) {
                    continue;
                }
                api_tools.push(codewhale_models::Tool {
                    tool_type: None,
                    name,
                    description: oauth::authenticate_tool_description(server),
                    input_schema: serde_json::json!({
                        "type": "object",
                        "properties": {}
                    }),
                    allowed_callers: Some(vec!["direct".to_string()]),
                    defer_loading: Some(false),
                    input_examples: None,
                    strict: None,
                    cache_control: None,
                });
            }
        }

        // Only advertise each resource-listing meta-tool when the servers actually
        // expose the corresponding kind. Previously both were injected whenever any
        // MCP server was configured, so tools-only servers left the model with
        // meta-tools that can only ever return empty results — a wasted tool slot
        // and prompt tokens. Gate each on its own non-empty collection, mirroring
        // the `mcp_read_resource` guard below (`!resources.is_empty()`).
        if !self.all_resources().is_empty() {
            api_tools.push(codewhale_models::Tool {
                tool_type: None,
                name: "list_mcp_resources".to_string(),
                description: "List available MCP resources across servers (optionally filtered by server).".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "Optional MCP server name to filter by" }
                    }
                }),
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
        }
        if !self.all_resource_templates().is_empty() {
            api_tools.push(codewhale_models::Tool {
                tool_type: None,
                name: "list_mcp_resource_templates".to_string(),
                description: "List available MCP resource templates across servers (optionally filtered by server).".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "Optional MCP server name to filter by" }
                    }
                }),
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
        }

        // Add resource reading tools if resources exist
        let resources = self.all_resources();
        if !resources.is_empty() {
            api_tools.push(codewhale_models::Tool {
                tool_type: None,
                name: "mcp_read_resource".to_string(),
                description: "Read a resource from an MCP server using its URI".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "The name of the MCP server" },
                        "uri": { "type": "string", "description": "The URI of the resource to read" }
                    },
                    "required": ["server", "uri"]
                }),
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
            api_tools.push(codewhale_models::Tool {
                tool_type: None,
                name: "read_mcp_resource".to_string(),
                description: "Alias for mcp_read_resource.".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "The name of the MCP server" },
                        "uri": { "type": "string", "description": "The URI of the resource to read" }
                    },
                    "required": ["server", "uri"]
                }),
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
        }

        // Add prompt getting tools if prompts exist
        let prompts = self.all_prompts();
        if !prompts.is_empty() {
            api_tools.push(codewhale_models::Tool {
                tool_type: None,
                name: "mcp_get_prompt".to_string(),
                description: "Get a prompt from an MCP server".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "server": { "type": "string", "description": "The name of the MCP server" },
                        "name": { "type": "string", "description": "The name of the prompt" },
                        "arguments": {
                            "type": "object",
                            "description": "Optional arguments for the prompt",
                            "additionalProperties": { "type": "string" }
                        }
                    },
                    "required": ["server", "name"]
                }),
                allowed_callers: Some(vec!["direct".to_string()]),
                defer_loading: Some(false),
                input_examples: None,
                strict: None,
                cache_control: None,
            });
        }

        // Sort by name for prefix-cache stability — the tool block sent to
        // the model needs to be deterministic across runs (#1319).
        api_tools.retain(|tool| self.tool_allowed(&tool.name));
        api_tools.sort_by(|a, b| a.name.cmp(&b.name));
        api_tools
    }

    /// Apply a child's narrower ceiling without changing the shared pool.
    pub(crate) async fn call_tool_with_disallowed(
        &mut self,
        name: &str,
        input: serde_json::Value,
        rules: &[String],
        decision: Option<&crate::core::engine::HumanDecision>,
    ) -> Result<serde_json::Value> {
        Self::authorize_call(&self.disallowed_tools, name, &input)?;
        Self::authorize_call(rules, name, &input)?;
        if !rules.is_empty()
            && rules != self.disallowed_tools.as_slice()
            && matches!(name, "list_mcp_resources" | "list_mcp_resource_templates")
            && input
                .get("server")
                .and_then(serde_json::Value::as_str)
                .is_none()
        {
            self.reload_if_config_changed().await?;
            let servers = self.enabled_server_names();
            let mut items = Vec::new();
            for server in servers {
                if Self::server_denied_by(rules, &server) {
                    continue;
                }
                let result = if name == "list_mcp_resources" {
                    self.list_resources(Some(server.clone())).await
                } else {
                    self.list_resource_templates(Some(server.clone())).await
                };
                match result {
                    Ok(mut resources) => items.append(&mut resources),
                    Err(error) => {
                        let Some(mut item) = self.mcp_recovery_error_item(&server, &error) else {
                            tracing::warn!("MCP resource discovery failed: {error:#}");
                            continue;
                        };
                        let auth_name = Self::mcp_model_tool_name(&server, AUTHENTICATE_TOOL_NAME);
                        if item.get("authenticate_tool").is_some()
                            && crate::core::engine::tool_catalog::tool_matches_any_rule(
                                rules, &auth_name,
                            )
                        {
                            item.as_object_mut()
                                .expect("error item object")
                                .remove("authenticate_tool");
                            item["message"] =
                                serde_json::json!("MCP server requires authentication");
                        }
                        items.push(item);
                    }
                }
            }
            let field = if name == "list_mcp_resources" {
                "resources"
            } else {
                "templates"
            };
            return Ok(serde_json::json!({ field: items }));
        }
        let synthetic_auth = self.authenticate_tool_target(name).is_some();
        let mut result = self.call_tool_with_decision(name, input, decision).await?;
        if synthetic_auth {
            Self::filter_authenticate_result(&mut result, rules);
        }
        Ok(result)
    }

    pub(crate) fn filter_authenticate_result(result: &mut serde_json::Value, rules: &[String]) {
        if let Some(tools) = result
            .get_mut("tools")
            .and_then(serde_json::Value::as_array_mut)
        {
            tools.retain(|name| {
                name.as_str().is_some_and(|name| {
                    !crate::core::engine::tool_catalog::tool_matches_any_rule(rules, name)
                })
            });
        }
    }

    /// Call a tool by its prefixed name (mcp_{server}_{tool})
    #[cfg(test)]
    pub async fn call_tool(
        &mut self,
        prefixed_name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value> {
        self.call_tool_with_decision(prefixed_name, arguments, None)
            .await
    }

    /// Call a tool, carrying a person's card decision for this exact call.
    pub(crate) async fn call_tool_with_decision(
        &mut self,
        prefixed_name: &str,
        arguments: serde_json::Value,
        decision: Option<&crate::core::engine::HumanDecision>,
    ) -> Result<serde_json::Value> {
        Self::authorize_call(&self.disallowed_tools, prefixed_name, &arguments)?;
        if decision.is_some_and(|decision| !decision.authorizes(prefixed_name, &arguments)) {
            anyhow::bail!("Human approval does not authorize this exact MCP call");
        }
        if prefixed_name == "list_mcp_resources" {
            let server = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let resources = self.list_resources(server).await?;
            return Ok(serde_json::json!({ "resources": resources }));
        }

        if prefixed_name == "list_mcp_resource_templates" {
            let server = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let templates = self.list_resource_templates(server).await?;
            return Ok(serde_json::json!({ "templates": templates }));
        }

        if prefixed_name == "mcp_read_resource" {
            let server_name = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .context("Missing 'server' argument")?;
            let uri = arguments
                .get("uri")
                .and_then(|v| v.as_str())
                .context("Missing 'uri' argument")?;
            return self.read_resource(server_name, uri).await;
        }

        if prefixed_name == "read_mcp_resource" {
            let server_name = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .context("Missing 'server' argument")?;
            let uri = arguments
                .get("uri")
                .and_then(|v| v.as_str())
                .context("Missing 'uri' argument")?;
            return self.read_resource(server_name, uri).await;
        }

        if prefixed_name == "mcp_get_prompt" {
            let server_name = arguments
                .get("server")
                .and_then(|v| v.as_str())
                .context("Missing 'server' argument")?;
            let name = arguments
                .get("name")
                .and_then(|v| v.as_str())
                .context("Missing 'name' argument")?;
            let args = arguments
                .get("arguments")
                .cloned()
                .unwrap_or(serde_json::json!({}));
            return self.get_prompt(server_name, name, args).await;
        }

        // Synthetic self-serve OAuth login: `mcp_<server>_authenticate` runs
        // the same flow `/mcp login` uses and reconnects the server on
        // success, so the real tools resolve in this session.
        if let Some(server_name) = self.authenticate_tool_target(prefixed_name) {
            return self.run_authenticate_tool(&server_name).await;
        }

        let route = match self.resolve_advertised_tool(prefixed_name).await {
            Ok(route) => route,
            Err(error) => {
                // A real tool name reached a server whose login has lapsed
                // (stale catalog, or a mid-session 401). Name the one call
                // that recovers it instead of leaving a dead error.
                if let Some(server) = self.needs_auth_server_for_tool_name(prefixed_name) {
                    return Err(error.context(self.auth_required_hint(&server)));
                }
                return Err(error);
            }
        };
        let server_name = route.server_name.clone();
        let tool_name = route.tool_name.clone();
        // Copy the global timeouts to avoid borrow conflict
        let global_timeouts = self.config.timeouts;
        let conn = self.get_or_connect(&server_name).await?;
        if conn.catalog_generation != route.catalog_generation {
            anyhow::bail!("MCP catalog changed after tool resolution; retry the call");
        }
        if conn
            .config()
            .reviewed_plugin
            .as_ref()
            .map(|source| &source.authority)
            != route.plugin_authority.as_ref()
            || !conn.config().is_tool_enabled(&tool_name)
            || !conn.tools().iter().any(|tool| tool.name == tool_name)
        {
            anyhow::bail!("MCP tool '{tool_name}' is disabled for server '{server_name}'");
        }
        let timeout = conn.config().effective_execute_timeout(&global_timeouts);
        let result = match conn
            .call_tool_decided(&tool_name, arguments.clone(), timeout, decision)
            .await
        {
            Ok(result) => Ok(result),
            // A rejected credential is not a stale session: reconnecting
            // replays the same rejection, so it takes the auth-required
            // path below instead of the transparent retry. Only a typed
            // transport-level refusal of the session id proves the server
            // never ran the call, so only that class is replayed.
            Err(err)
                if is_mcp_session_rejected_error(&err)
                    && !oauth::error_looks_auth_required(&err) =>
            {
                tracing::debug!(
                    target: "mcp",
                    server = server_name,
                    tool = tool_name,
                    error = %err,
                    "retrying MCP tool call after stale session"
                );
                self.drop_connection(&server_name, "stale session retry");
                // No `?` here: a reconnect that fails auth-required must
                // still reach the classification below.
                match self.get_or_connect(&server_name).await {
                    Ok(conn) => {
                        if conn.catalog_generation != route.catalog_generation
                            || conn
                                .config()
                                .reviewed_plugin
                                .as_ref()
                                .map(|source| &source.authority)
                                != route.plugin_authority.as_ref()
                            || !conn.config().is_tool_enabled(&tool_name)
                            || !conn.tools().iter().any(|tool| tool.name == tool_name)
                        {
                            Err(anyhow::anyhow!(
                                "MCP tool '{tool_name}' is disabled for server '{server_name}'"
                            ))
                        } else {
                            let timeout = conn.config().effective_execute_timeout(&global_timeouts);
                            conn.call_tool_decided(&tool_name, arguments, timeout, decision)
                                .await
                        }
                    }
                    // A reconnect that fails must not swallow the call error
                    // that triggered it: report both, original first.
                    Err(reconnect_err) => Err(anyhow::anyhow!(
                        "{err:#}; reconnect failed: {reconnect_err:#}"
                    )),
                }
            }
            // The transport died after the request was written, or the
            // server answered this request id with a session error: either
            // way it may already have run the tool, so replaying it could
            // repeat a side effect. Rebuild the connection for the next call
            // and let the caller decide whether to repeat this one.
            Err(err)
                if is_mcp_connection_lost_error(&err)
                    && !oauth::error_looks_auth_required(&err) =>
            {
                tracing::debug!(
                    target: "mcp",
                    server = server_name,
                    tool = tool_name,
                    error = %err,
                    "MCP connection lost during tool call; not retrying"
                );
                self.drop_connection(&server_name, "connection lost during tool call");
                Err(err.context(format!(
                    "MCP server '{server_name}' connection closed during tool call \
                     '{tool_name}'; outcome unknown, not retried"
                )))
            }
            Err(err) => Err(err),
        };
        // A credential the server stopped accepting mid-session (revoked or
        // rotated elsewhere, refresh rejected) is the same `◆ auth required`
        // class as a failed connect: land it in the same typed state so the
        // next catalog offers the login tool instead of a dead error.
        result.map_err(|err| self.note_live_call_failure(&server_name, err))
    }

    /// Get list of configured server names (static + dynamic)
    #[allow(dead_code)] // Public API for MCP consumers
    pub fn server_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .config
            .servers
            .keys()
            .filter(|name| self.server_allowed(name))
            .cloned()
            .collect();
        let dynamic = self.dynamic_servers.read();
        for name in dynamic.keys() {
            if self.server_allowed(name) && !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    /// Add a runtime server configuration (in-memory only, not persisted).
    ///
    /// This is used for dynamically started MCP servers from chat context.
    /// Stored in `dynamic_servers` so it doesn't interfere with file-based config reload.
    ///
    /// Returns `Err` if a server with the same name already exists as a static config
    /// or a dynamic config. The caller should surface the error to the LLM/user.
    pub fn add_runtime_server_config(
        &self,
        name: String,
        config: McpServerConfig,
    ) -> Result<(), String> {
        self.require_server(&name)
            .map_err(|error| error.to_string())?;
        if self.config.servers.contains_key(&name) {
            return Err(format!(
                "MCP server '{}' already exists in the config file. \
                 Reconnect with start_mcp_server using only its exact name (omit server), \
                 or run /mcp retry with that name. This preserves its stored credentials.",
                name
            ));
        }
        let mut dynamic = self.dynamic_servers.write();
        if dynamic.contains_key(&name) {
            return Err(format!(
                "MCP server '{}' was already started earlier in this session. \
                 Reconnect with start_mcp_server using only its exact name (omit server).",
                name
            ));
        }
        let mut config = config;
        config.runtime_added = true;
        dynamic.insert(name, config);
        self.catalog_generation.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    /// Remove an in-memory runtime server after a failed start attempt.
    /// This makes dynamic registration transactional: callers may retry the
    /// same deterministic name after correcting an argument or install issue.
    pub fn remove_runtime_server_config(&mut self, name: &str) {
        self.drop_connection(name, "runtime server start rolled back");
        if self.dynamic_servers.write().remove(name).is_some() {
            self.catalog_generation.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Get list of connected server names
    pub fn connected_servers(&self) -> Vec<&str> {
        self.connections
            .iter()
            .filter(|(name, c)| self.server_allowed(name) && c.is_ready())
            .map(|(n, _)| n.as_str())
            .collect()
    }

    /// Names of every *enabled* server this pool would connect on the next
    /// turn (static config + dynamic runtime entries).
    ///
    /// Read-only: unlike [`Self::connect_all`], it neither reloads the config
    /// sources nor starts a process. `/preview-request` uses it, together
    /// with [`Self::connected_servers`] and
    /// [`Self::config_sources_unchanged`], to decide whether the currently
    /// connected tool set is *exactly* what the next turn would send — and to
    /// report the tool surface as unavailable when it is not (#1004).
    pub fn enabled_server_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .config
            .servers
            .iter()
            .filter(|(name, server)| server.is_enabled() && self.server_allowed(name))
            .map(|(name, _)| name.clone())
            .collect();
        let dynamic = self.dynamic_servers.read();
        for (name, server) in dynamic.iter() {
            if self.server_allowed(name) && server.is_enabled() && !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    }

    /// Compare against the freshly authorized merged configuration without
    /// reloading or disconnecting any sibling transport.
    pub(crate) fn config_matches(&self, config: &McpConfig) -> bool {
        hash_mcp_config(config) == self.config_hash
    }

    /// Whether every configured MCP source still has the mtime this pool last
    /// read, i.e. whether `connect_all` would find anything new.
    ///
    /// Stats files; never reads, parses, reloads, or drops a connection. A
    /// pool with no configured source is trivially unchanged.
    pub fn config_sources_unchanged(&self) -> bool {
        if self.config_sources.is_empty() {
            return true;
        }
        let current: Vec<_> = self
            .config_sources
            .iter()
            .map(|path| mcp_config_mtime(path))
            .collect();
        current == self.last_mtimes
    }

    /// Graceful shutdown of every connection in the pool: send SIGTERM to
    /// each stdio child and give them a short grace period before drop
    /// fires SIGKILL. Whalescale#420.
    ///
    /// Call from the TUI exit path *before* dropping the pool to give
    /// MCP servers a chance to flush state. The fallback Drop on
    /// `StdioTransport` still sends SIGTERM if this never runs, so even
    /// abnormal exits avoid leaking PIDs without a signal.
    pub async fn shutdown_all(&mut self) {
        let names: Vec<String> = self.connections.keys().cloned().collect();
        for name in names {
            if let Some(conn) = self.connections.get_mut(&name) {
                conn.transport.shutdown().await;
            }
        }
        self.connections.clear();
    }

    /// Check if a tool name is an MCP tool
    pub fn is_mcp_tool(name: &str) -> bool {
        name.starts_with("mcp_")
            || matches!(
                name,
                "list_mcp_resources" | "list_mcp_resource_templates" | "read_mcp_resource"
            )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpWriteStatus {
    Created,
    Overwritten,
    SkippedExists,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpDiscoveredItem {
    pub name: String,
    pub model_name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerSnapshot {
    pub name: String,
    pub enabled: bool,
    pub required: bool,
    pub transport: String,
    pub command_or_url: String,
    pub connect_timeout: u64,
    pub execute_timeout: u64,
    pub read_timeout: u64,
    pub connected: bool,
    pub error: Option<String>,
    /// Typed `◆ auth required` state: the server's most recent connect
    /// attempt failed because a login is missing, expired, or revoked
    /// (401 / OAuth not logged in / `invalid_grant`). Derived from the pool's
    /// needs-auth set — the same source that decides whether the model gets
    /// the synthetic `mcp_<server>_authenticate` tool — so every surface
    /// (session boot row, `/mcp` manager, Extensions, model catalog) agrees.
    pub auth_required: bool,
    pub capability_metadata: McpServerCapabilityMetadata,
    pub tools: Vec<McpDiscoveredItem>,
    pub resources: Vec<McpDiscoveredItem>,
    pub prompts: Vec<McpDiscoveredItem>,
}

impl McpServerSnapshot {
    /// Recovery for this observed server. The typed auth-required state wins
    /// over error-text sniffing so a needs-auth server always routes to
    /// `/mcp login <name>`.
    #[must_use]
    pub fn recovery_kind(&self, oauth_capable: bool) -> Option<McpRecoveryKind> {
        if self.enabled && !self.connected && self.auth_required {
            return Some(McpRecoveryKind::Reauth);
        }
        mcp_recovery_kind(
            self.enabled,
            self.started(),
            self.connected,
            self.error.as_deref(),
            oauth_capable,
        )
    }

    /// Whether this session ever attempted the server. Boot is lazy (#6033):
    /// a configured server nobody asked for has no connection, no recorded
    /// failure, and no observed capabilities — it was never started, so its
    /// recovery is `connect`, not `reconnect`, and an OAuth-capable one is
    /// not yet known to need a login.
    #[must_use]
    pub fn started(&self) -> bool {
        self.connected
            || self.auth_required
            || self.error.is_some()
            || !matches!(
                self.capability_metadata,
                McpServerCapabilityMetadata::NotObserved
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpManagerSnapshot {
    pub config_path: std::path::PathBuf,
    pub config_exists: bool,
    pub reload_required: bool,
    pub servers: Vec<McpServerSnapshot>,
}

/// First-class recovery for a configured MCP server. Commands named here exist:
/// `/mcp login`, `/mcp reload`, `/mcp validate`, `/mcp enable`. There is no
/// `/mcp auth`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpRecoveryKind {
    Enable,
    Connect,
    Reconnect,
    Reauth,
    Diagnose,
    /// The server's AWS credentials (an SSO session or a temporary token)
    /// expired. The fix is outside Codewhale — `aws sso login` in a terminal
    /// (see [`aws_login_hint`]) — and `/mcp login` cannot help: it is an
    /// OAuth flow for HTTP servers, and a stdio AWS proxy is not one. The row
    /// action is the single-server retry to run once the login is done.
    AwsLogin,
}

impl McpRecoveryKind {
    #[must_use]
    pub fn slash_command(self, name: &str) -> String {
        match self {
            Self::Enable => format!("/mcp enable {name}"),
            // Reconnect one server, not all of them. A row that reads
            // `[reconnect] aws` and then reloads all 23 configured servers is
            // not the action it advertised: it takes ~40 s, it disturbs every
            // healthy connection, and the row the user aimed at is still
            // pending when the list comes back. `/mcp retry <name>` reaches
            // `retry_mcp_server`, which reconnects exactly that server.
            Self::Connect | Self::Reconnect | Self::AwsLogin if mcp_name_is_command_safe(name) => {
                format!("/mcp retry {name}")
            }
            // A name the command line cannot carry safely still gets the
            // blunt instrument rather than a quoted-argument hazard.
            Self::Connect | Self::Reconnect | Self::AwsLogin => "/mcp reload".to_string(),
            Self::Reauth => format!("/mcp login {name}"),
            Self::Diagnose if mcp_name_is_command_safe(name) => format!("/mcp validate {name}"),
            Self::Diagnose => "/mcp validate".to_string(),
        }
    }

    #[must_use]
    pub fn label_key(self) -> codewhale_localization::MessageId {
        match self {
            Self::Enable => codewhale_localization::MessageId::ExtensionsActionEnable,
            Self::Connect => codewhale_localization::MessageId::ExtensionsActionConnect,
            Self::Reconnect => codewhale_localization::MessageId::ExtensionsActionReconnect,
            Self::Reauth => codewhale_localization::MessageId::ExtensionsActionReauth,
            // The row action is `/mcp retry <name>` in place, not a login
            // flow, so it is labelled for what it runs. The why (run
            // `aws login` first) is carried in the row detail, which the
            // snapshot guarantees names the command (see
            // `snapshot_from_config`).
            Self::AwsLogin => codewhale_localization::MessageId::ExtensionsActionReconnect,
            Self::Diagnose => codewhale_localization::MessageId::ExtensionsActionDiagnose,
        }
    }
}

#[must_use]
pub fn mcp_name_is_command_safe(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Display target for an MCP server row: stdio servers show the command
/// name only — no `./…` relative-path prefix, no directories, no args —
/// while URL servers keep their full URL, which is the identity. The
/// snapshot's `command_or_url` keeps full fidelity for the engine and the
/// wire; this is presentation only.
#[must_use]
pub fn mcp_display_target(transport: &str, command_or_url: &str) -> String {
    if transport != "stdio" {
        return command_or_url.to_string();
    }
    let command = command_or_url
        .split_whitespace()
        .next()
        .unwrap_or(command_or_url);
    let name = command.rsplit(['/', '\\']).next().unwrap_or(command);
    if name.is_empty() {
        command_or_url.to_string()
    } else {
        name.to_string()
    }
}

#[must_use]
pub fn mcp_server_oauth_capable(config: &McpServerConfig) -> bool {
    // Use the login path's own authority, including discovery-only HTTP
    // servers, manual Authorization and the reviewed-plugin restriction.
    oauth::server_supports_oauth_login(config)
}

#[must_use]
pub fn mcp_recovery_kind(
    enabled: bool,
    inspected: bool,
    connected: bool,
    error: Option<&str>,
    oauth_capable: bool,
) -> Option<McpRecoveryKind> {
    if !enabled {
        return Some(McpRecoveryKind::Enable);
    }
    if let Some(error) = error {
        // Before the OAuth classifier: an expired AWS token often surfaces as
        // `UnauthorizedException`/`401`, which would otherwise route to
        // `/mcp login` — an OAuth flow that cannot renew an AWS session.
        // Gated on the server not being OAuth-capable: an OAuth server
        // fronted by corporate SSO saying "SSO token expired" needs its own
        // login, not `aws sso login`.
        if mcp_error_is_aws_login(error, oauth_capable) {
            return Some(McpRecoveryKind::AwsLogin);
        }
        if oauth::error_text_looks_auth_required(error) {
            return Some(McpRecoveryKind::Reauth);
        }
        return Some(McpRecoveryKind::Diagnose);
    }
    if !inspected {
        return Some(McpRecoveryKind::Connect);
    }
    if connected {
        // A server that is enabled, inspected, connected and erroring on
        // nothing needs no recovery. It used to be labelled `diagnose`, so
        // every healthy row advertised a repair it did not need — founder
        // live-test: "even the ones that are connected say diagnose lol".
        return None;
    }
    if oauth_capable {
        return Some(McpRecoveryKind::Reauth);
    }
    Some(McpRecoveryKind::Reconnect)
}

/// Whether an MCP server's error (usually its forwarded stderr line) says the
/// AWS credentials it launched with have expired. The founder's `aws` server
/// failed `initialize` with -32602 whose only real cause was an expired SSO
/// login; the generic "diagnose" gave no way forward. Every pattern is
/// anchored to AWS CLI / SDK wording so an unrelated "token expired" from an
/// OAuth server still reaches the OAuth classifier.
#[must_use]
pub fn error_text_looks_aws_credentials_expired(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    let expired = text.contains("expired") || text.contains("invalid");
    // botocore / AWS CLI v2 SSO wording ("The SSO session associated with
    // this profile has expired or is otherwise invalid", "Error loading SSO
    // Token: Token for … does not exist").
    let sso = ["sso session", "sso token", "sso login", "sso oidc"]
        .iter()
        .any(|phrase| text.contains(phrase));
    let mentions_aws = text
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|word| word == "aws");
    (sso && (expired || text.contains("does not exist")))
        // AWS CLI 2.32+ `aws login` sessions: "LoginRefreshRequired: Please
        // reauthenticate using aws login" (the founder's -32602 case).
        || text.contains("loginrefreshrequired")
        // Our own hint, so a reviewed plugin's suppressed error (which keeps
        // only this fixed text) still classifies on every later surface.
        || text.contains("aws credentials expired:")
        || text.contains("reauthenticate using aws login")
        // STS / service error codes for expired temporary credentials.
        || text.contains("expiredtoken")
        || text.contains("security token included in the request is expired")
        || text.contains("tokenrefreshrequired")
        || (mentions_aws && text.contains("token") && text.contains("expired"))
}

/// Whether an MCP error means "renew the AWS login in a terminal". Only a
/// server that is not OAuth-capable qualifies: an OAuth server's expired
/// token is renewed by `/mcp login`, whatever words its SSO front uses.
#[must_use]
pub fn mcp_error_is_aws_login(text: &str, oauth_capable: bool) -> bool {
    !oauth_capable && error_text_looks_aws_credentials_expired(text)
}

/// The terminal command that renews an expired AWS login for `config`, with
/// the follow-up retry. `error_text` picks the command: a CLI 2.32+
/// `aws login` session (`LoginRefreshRequired`) renews with `aws login`,
/// everything else with `aws sso login`. The profile comes from the server's
/// own `--profile` argument, else its `AWS_PROFILE` env entry (not for a
/// reviewed plugin, whose env stays out of every surface); with neither the
/// plain command uses the default profile, which is what the server does.
#[must_use]
pub fn aws_login_hint(config: &McpServerConfig, server: &str, error_text: &str) -> String {
    let profile = config
        .args
        .iter()
        .position(|arg| arg == "--profile")
        .and_then(|index| config.args.get(index + 1))
        .map(String::as_str)
        .or_else(|| {
            config
                .args
                .iter()
                .find_map(|arg| arg.strip_prefix("--profile="))
        })
        .or_else(|| {
            config
                .reviewed_plugin
                .is_none()
                .then(|| config.env.get("AWS_PROFILE").map(String::as_str))
                .flatten()
        })
        .filter(|profile| !profile.starts_with('-') && mcp_name_is_command_safe(profile));
    let lower = error_text.to_ascii_lowercase();
    let command = if lower.contains("loginrefreshrequired")
        || lower.contains("using aws login")
        || lower.contains("run `aws login")
    {
        "aws login"
    } else {
        "aws sso login"
    };
    let login = match profile {
        Some(profile) => format!("{command} --profile {profile}"),
        None => command.to_string(),
    };
    let retry = McpRecoveryKind::AwsLogin.slash_command(server);
    format!("AWS credentials expired: run `{login}` in a terminal, then `{retry}`")
}

pub fn load_config(path: &Path) -> Result<McpConfig> {
    validate_mcp_config_path(path)?;
    let Some(contents) = read_mcp_config_file(path)? else {
        return Ok(McpConfig::default());
    };
    serde_json::from_str(&contents).map_err(|_| {
        anyhow::anyhow!(
            "Failed to parse MCP config {}; file contents were omitted",
            codewhale_config::quote_os_path(path)
        )
    })
}

/// Maximum bytes read from an MCP config file. Configs are kilobytes.
const MAX_MCP_CONFIG_BYTES: u64 = 1024 * 1024;

fn read_mcp_config_file(path: &Path) -> Result<Option<String>> {
    read_bounded_mcp_config_file(path, MAX_MCP_CONFIG_BYTES)
}

/// [`read_mcp_config_file`] with a caller-chosen size bound, for foreign files
/// such as `~/.claude.json` that carry far more than an MCP server map.
fn read_bounded_mcp_config_file(path: &Path, max_bytes: u64) -> Result<Option<String>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("Failed to inspect MCP config {}", path.display()));
        }
    };
    let file_type = metadata.file_type();
    if file_type.is_symlink() || !file_type.is_file() {
        anyhow::bail!("MCP config path must be a regular file: {}", path.display());
    }

    let file = open_mcp_config_file(path)
        .with_context(|| format!("Failed to read MCP config {}", path.display()))?;
    let mut contents = String::new();
    file.take(max_bytes + 1)
        .read_to_string(&mut contents)
        .with_context(|| format!("Failed to read MCP config {}", path.display()))?;
    if contents.len() as u64 > max_bytes {
        anyhow::bail!(
            "MCP config {} exceeds the {} MiB limit",
            path.display(),
            max_bytes / (1024 * 1024)
        );
    }
    Ok(Some(contents))
}

#[cfg(unix)]
fn open_mcp_config_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_mcp_config_file(path: &Path) -> std::io::Result<fs::File> {
    fs::File::open(path)
}

pub fn workspace_mcp_config_path(workspace: &Path) -> PathBuf {
    normalize_workspace_path(workspace)
        .join(".codewhale")
        .join("mcp.json")
}

/// Which configuration file declares an MCP server.
///
/// The rows in `/mcp` are the union of the user's global file, the trusted
/// workspace's own file, and every installed plugin's contribution. Until
/// this existed the panel offered `e` and `d` on all three and then wrote to
/// the global file regardless, so toggling or removing a project server
/// failed with "MCP server '<name>' not found" — the row looked mutable and
/// was not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpServerScope {
    /// The user's own config file, shared by every workspace.
    Global,
    /// `<workspace>/.codewhale/mcp.json`, honoured only once the workspace
    /// is trusted in user-owned config (#417).
    Project(PathBuf),
    /// Contributed by an installed plugin bundle. It lives in no config
    /// file, so it is switched off by disabling the plugin that owns it.
    Plugin,
}

impl McpServerScope {
    /// The file a mutation for this server must write, or `None` when the
    /// server has no config file of its own.
    #[must_use]
    pub fn config_path(&self, global_path: &Path) -> Option<PathBuf> {
        match self {
            Self::Global => Some(global_path.to_path_buf()),
            Self::Project(path) => Some(path.clone()),
            Self::Plugin => None,
        }
    }
}

/// Resolve the file that declares `name`.
///
/// Project entries are checked first because
/// [`load_config_with_workspace_and_plugins`] lets them override a
/// same-named global server, so the project file is the one a mutation has
/// to edit for the change to be observable.
/// Every server name declared by the trusted workspace's own config file.
///
/// Resolved once per panel snapshot so the row rendering can label scope
/// without a file read per row.
#[must_use]
pub fn project_server_names(global_path: &Path, workspace: &Path) -> BTreeSet<String> {
    let Ok(workspace) = checked_workspace_path(workspace) else {
        return BTreeSet::new();
    };
    if !workspace_allows_project_mcp_config(&workspace) {
        return BTreeSet::new();
    }
    let Ok(project_path) = checked_workspace_mcp_config_path(&workspace) else {
        return BTreeSet::new();
    };
    if !project_path.exists() || paths_refer_to_same_config(global_path, &project_path) {
        return BTreeSet::new();
    }
    load_config(&project_path)
        .map(|config| config.servers.into_keys().collect())
        .unwrap_or_default()
}

#[must_use]
pub fn resolve_server_scope(global_path: &Path, workspace: &Path, name: &str) -> McpServerScope {
    if let Ok(workspace) = checked_workspace_path(workspace)
        && workspace_allows_project_mcp_config(&workspace)
        && let Ok(project_path) = checked_workspace_mcp_config_path(&workspace)
        && project_path.exists()
        && !paths_refer_to_same_config(global_path, &project_path)
        && load_config(&project_path).is_ok_and(|config| config.servers.contains_key(name))
    {
        return McpServerScope::Project(project_path);
    }
    if load_config(global_path).is_ok_and(|config| config.servers.contains_key(name)) {
        return McpServerScope::Global;
    }
    McpServerScope::Plugin
}

/// Plugin name of the built-in Computer Use bundle.
pub(crate) const COMPUTER_USE_PLUGIN_NAME: &str = "computer-use";

/// First message the host sends the built-in Computer Use plugin: its
/// per-connection decision key and the persisted-ledger key.
pub(crate) const COMPUTER_USE_HOST_KEYS_METHOD: &str = "codewhale/host_keys";

/// `_meta` key carrying an attested person's decision on a `tools/call`.
pub(crate) const COMPUTER_USE_DECISION_META: &str = "codewhale/user_decision";

/// Secret-store slot of the key that signs remembered Computer Use grants.
const COMPUTER_USE_LEDGER_KEY_SLOT: &str = "codewhale_cu_ledger_key";

pub(crate) fn random_key() -> Result<[u8; 32]> {
    use ring::rand::SecureRandom as _;
    let mut key = [0_u8; 32];
    ring::rand::SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| {
            anyhow::anyhow!("System randomness is unavailable for Computer Use approval")
        })?;
    Ok(key)
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

/// `{nonce, args_json, mac}` for one call: `mac` is HMAC-SHA256 over
/// `tool \0 args_json \0 nonce`, and the plugin checks that `args_json`
/// parses to the arguments it received.
pub(crate) fn attest_decision(
    key: &[u8; 32],
    tool_name: &str,
    arguments: &serde_json::Value,
) -> Result<serde_json::Value> {
    let nonce = hex_encode(&random_key()?[..16]);
    let args_json = serde_json::to_string(arguments)?;
    let message = [
        tool_name.as_bytes(),
        b"\0",
        args_json.as_bytes(),
        b"\0",
        nonce.as_bytes(),
    ]
    .concat();
    let tag = ring::hmac::sign(
        &ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key),
        &message,
    );
    Ok(serde_json::json!({
        "nonce": nonce,
        "args_json": args_json,
        "mac": hex_encode(tag.as_ref()),
    }))
}

/// The key that signs remembered Computer Use grants, created once in the
/// secret store. `None` when the store is unavailable: remembered grants
/// then do not survive the session.
pub(crate) async fn computer_use_ledger_key() -> Option<String> {
    if cfg!(test) {
        return None;
    }
    tokio::task::spawn_blocking(|| {
        let secrets = codewhale_secrets::Secrets::auto_detect();
        // Concurrent connection starts must use the same persisted key;
        // reading and replacing it share the secret store's entry authority.
        secrets
            .with_entry_transaction(COMPUTER_USE_LEDGER_KEY_SLOT, |stored| {
                if let Some(key) = stored.as_ref()
                    && key.len() == 64
                    && key.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Ok(Some(key.clone()));
                }
                let Ok(bytes) = random_key() else {
                    return Ok(None);
                };
                let key = hex_encode(&bytes);
                *stored = Some(key.clone());
                Ok(Some(key))
            })
            .ok()
            .flatten()
    })
    .await
    .ok()
    .flatten()
}

/// User-configured servers that launch the same Computer Use plugin as the
/// enabled built-in `computer-use` bundle, with the argument that gave each
/// one away. Two copies advertise every Computer Use schema twice (about
/// 2.5k tokens on every request) and keep two consent ledgers. Diagnostic
/// only: callers warn and never remove the user's entry.
pub(crate) fn duplicate_computer_use_servers(config: &McpConfig) -> Vec<(String, String)> {
    let builtin_enabled = config.servers.values().any(|server| {
        server.is_enabled()
            && server
                .reviewed_plugin
                .as_ref()
                .is_some_and(|source| source.plugin_name() == COMPUTER_USE_PLUGIN_NAME)
    });
    if !builtin_enabled {
        return Vec::new();
    }
    let mut duplicates = config
        .servers
        .iter()
        .filter(|(_, server)| server.reviewed_plugin.is_none() && server.is_enabled())
        .filter_map(|(name, server)| {
            launches_computer_use_plugin(server).map(|arg| (name.clone(), arg))
        })
        .collect::<Vec<_>>();
    duplicates.sort();
    duplicates
}

/// The argument naming a Computer Use `mcp/server.mjs`, when this server
/// runs one: the `plugin.json` next to its `mcp/` directory says
/// `"name": "computer-use"`, or — when no manifest is readable — the path
/// has the bundle's shape (`.../computer-use/.../mcp/server.mjs`, any case
/// or separator).
fn launches_computer_use_plugin(server: &McpServerConfig) -> Option<String> {
    server.command.as_ref()?;
    server.args.iter().find_map(|arg| {
        let normalized = arg.replace('\\', "/");
        if !normalized.ends_with("mcp/server.mjs") {
            return None;
        }
        let script = Path::new(arg);
        let script = if script.is_relative() {
            server
                .cwd
                .as_ref()
                .map_or_else(|| script.to_path_buf(), |cwd| cwd.join(script))
        } else {
            script.to_path_buf()
        };
        let root = script.parent().and_then(Path::parent);
        if let Some(root) = root
            && let Ok(text) = std::fs::read_to_string(root.join("plugin.json"))
        {
            let name = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|manifest| manifest.get("name")?.as_str().map(str::to_string));
            return (name.as_deref() == Some(COMPUTER_USE_PLUGIN_NAME)).then(|| arg.clone());
        }
        let squashed = normalized.to_ascii_lowercase().replace([' ', '-', '_'], "");
        squashed.contains("computeruse").then(|| arg.clone())
    })
}

pub fn load_config_with_workspace(global_path: &Path, workspace: &Path) -> Result<McpConfig> {
    let plugins = crate::plugins::PluginRegistry::empty(workspace);
    load_config_with_workspace_and_plugins(global_path, workspace, &plugins)
}

pub fn load_config_with_workspace_and_plugins(
    global_path: &Path,
    workspace: &Path,
    plugins: &crate::plugins::PluginRegistry,
) -> Result<McpConfig> {
    let mut merged = load_config(global_path)?;
    let workspace = checked_workspace_path(workspace)?;
    let project_path = checked_workspace_mcp_config_path(&workspace)?;
    if !project_path.exists() || paths_refer_to_same_config(global_path, &project_path) {
        return merge_plugin_mcp_servers(merged, plugins);
    }
    // Workspace-local MCP can spawn stdio servers, so it is only honored after
    // the user has trusted this workspace in user-owned config. Do not accept
    // project-local legacy trust markers here: a repository could carry those
    // files itself and silently reintroduce the project-scope `mcp_config_path`
    // risk denied in #417.
    if !workspace_allows_project_mcp_config(&workspace) {
        return merge_plugin_mcp_servers(merged, plugins);
    }

    let mut project = load_config(&project_path)?;
    for server in project.servers.values_mut() {
        if server.command.is_some() && server.url.is_none() {
            server.cwd = Some(resolve_project_mcp_cwd(&workspace, server.cwd.as_deref())?);
        }
    }
    merged.servers.extend(project.servers);

    merge_plugin_mcp_servers(merged, plugins)
}

fn merge_plugin_mcp_servers(
    config: McpConfig,
    registry: &crate::plugins::PluginRegistry,
) -> Result<McpConfig> {
    let Some(state_path) = registry.state_path().map(Path::to_path_buf) else {
        return Ok(config);
    };
    let plugins = registry
        .active_plugins()
        .into_iter()
        .filter_map(|plugin| {
            plugin
                .authority(state_path.clone(), registry.workspace().to_path_buf())
                .map(|authority| (plugin.name().to_string(), plugin.clone(), authority))
        })
        .collect::<Vec<_>>();

    let host_environment = registry.host_environment().ok_or_else(|| {
        anyhow::anyhow!("active plugin registry is missing its pre-dotenv environment snapshot")
    })?;
    let mut config = merge_plugin_mcp_servers_from_plugins_with_environment(
        config,
        plugins,
        Arc::clone(&host_environment),
    )?;
    for (name, mut server, authority, receipt) in
        crate::extension_host::native_mcp::for_plugins(registry).map_err(anyhow::Error::msg)?
    {
        let qualified = qualified_plugin_server_name(&authority.plugin_name, &name);
        anyhow::ensure!(
            !config.servers.contains_key(&qualified),
            "Native MCP namespace collides with an existing reviewed or explicit server"
        );
        if server.command.is_some() {
            let root = authority
                .staged_manifest
                .parent()
                .context("Native stage has no root")?;
            server.cwd = Some(resolve_plugin_mcp_cwd(root, server.cwd.as_deref())?);
            freeze_plugin_stdio_paths(&mut server, root)?;
        }
        let mut source = ReviewedPluginMcpSource::from_authority(
            authority,
            server.url.as_deref(),
            Arc::clone(&host_environment),
        )?;
        source.native_mcp = Some(receipt);
        server.reviewed_plugin = Some(source);
        config.servers.insert(qualified, server);
    }
    Ok(config)
}

fn merge_plugin_mcp_servers_from_plugins_with_environment(
    mut config: McpConfig,
    plugins: impl IntoIterator<
        Item = (
            String,
            crate::plugins::types::LoadedPlugin,
            crate::plugins::types::PluginAuthority,
        ),
    >,
    host_environment: Arc<crate::plugins::HostEnvironment>,
) -> Result<McpConfig> {
    for (plugin_name, plugin, authority) in plugins {
        // Adapter-level denial keeps headless paths fail-closed even if a
        // future caller accidentally passes the full inventory instead of the
        // registry's active-only view.
        if !plugin.active() {
            continue;
        }
        if let Some(mcp_servers) = &plugin.manifest.mcp_servers {
            let mut mcp_servers = mcp_servers.iter().collect::<Vec<_>>();
            mcp_servers.sort_by_key(|(name, _)| *name);
            for (server_name, server_config) in mcp_servers {
                let required_capability = if server_config.command.is_some()
                    && server_config.url.is_none()
                {
                    crate::plugins::activation::PluginActivationCapability::McpStdio
                } else if server_config.url.is_some() && server_config.command.is_none() {
                    crate::plugins::activation::PluginActivationCapability::McpRemote
                } else {
                    tracing::warn!(
                        target: "mcp",
                        plugin = %plugin_name,
                        server = %server_name,
                        "plugin MCP server is neither a reviewed stdio nor remote transport; denying it"
                    );
                    continue;
                };
                if !plugin.component_active(required_capability)
                    || crate::plugins::registry::verify_plugin_component_authority(
                        &authority,
                        required_capability,
                    )
                    .is_err()
                {
                    tracing::warn!(
                        target: "mcp",
                        plugin = %plugin_name,
                        server = %server_name,
                        capability = required_capability.as_str(),
                        "plugin bundle changed after review or this transport is inactive; denying the MCP server until reload and re-review"
                    );
                    continue;
                }
                let qualified_name = qualified_plugin_server_name(&plugin_name, server_name);
                if config.servers.contains_key(&qualified_name) {
                    tracing::warn!(
                        target: "mcp",
                        plugin = %plugin_name,
                        server = %server_name,
                        qualified_name = %qualified_name,
                        "explicit MCP configuration keeps precedence over a colliding plugin server"
                    );
                    continue;
                }
                let mut server_config = server_config.clone();

                if server_config.command.is_some() && server_config.url.is_none() {
                    let staged_root = plugin
                        .staged_root
                        .as_deref()
                        .context("active plugin is missing its runtime snapshot")?;
                    server_config.cwd = Some(resolve_plugin_mcp_cwd(
                        staged_root,
                        server_config.cwd.as_deref(),
                    )?);
                    freeze_plugin_stdio_paths(&mut server_config, staged_root)?;
                }
                server_config.reviewed_plugin = Some(ReviewedPluginMcpSource::from_authority(
                    authority.clone(),
                    server_config.url.as_deref(),
                    Arc::clone(&host_environment),
                )?);

                config.servers.insert(qualified_name, server_config);
            }
        }
    }

    Ok(config)
}

#[cfg(test)]
fn merge_plugin_mcp_servers_from_plugins(
    config: McpConfig,
    plugins: impl IntoIterator<
        Item = (
            String,
            crate::plugins::types::LoadedPlugin,
            crate::plugins::types::PluginAuthority,
        ),
    >,
) -> Result<McpConfig> {
    merge_plugin_mcp_servers_from_plugins_with_environment(
        config,
        plugins,
        Arc::new(crate::plugins::HostEnvironment::capture()),
    )
}

/// Split a qualified plugin server key back into `(plugin, server)`.
///
/// The length prefix written by [`qualified_plugin_server_name`] exists so
/// this split is unambiguous even when a plugin or server name contains `-`.
/// Display surfaces use it to show `codewhale-account-plugins/codewhale-plugins`
/// instead of the wire key `plugin-25-codewhale-account-plugins-codewhale-plugins`,
/// which reads as noise in a list and tells a person nothing.
#[must_use]
pub fn split_qualified_plugin_server_name(qualified: &str) -> Option<(&str, &str)> {
    let rest = qualified.strip_prefix("plugin-")?;
    let (len, rest) = rest.split_once('-')?;
    let len: usize = len.parse().ok()?;
    if !rest.is_char_boundary(len) {
        return None;
    }
    let (plugin, server) = rest.split_at(len);
    Some((plugin, server.strip_prefix('-')?))
}

pub(crate) fn qualified_plugin_server_name(plugin_name: &str, server_name: &str) -> String {
    format!(
        "plugin-{}-{}-{}",
        plugin_name.len(),
        plugin_name,
        server_name
    )
}

fn freeze_plugin_stdio_paths(config: &mut McpServerConfig, staged_root: &Path) -> Result<()> {
    if let Some(command) = config.command.as_mut()
        && (command.contains('/') || command.contains('\\'))
    {
        let frozen = resolve_plugin_mcp_cwd(staged_root, Some(Path::new(command)))?;
        *command = frozen.display().to_string();
    }
    let runtime_cwd = config.cwd.as_deref().unwrap_or(staged_root).to_path_buf();
    for argument in &mut config.args {
        if argument.starts_with('-') || Path::new(argument).is_absolute() {
            continue;
        }
        let candidate = normalize_path_components(&runtime_cwd.join(argument.as_str()));
        if candidate.exists() {
            let frozen = candidate
                .canonicalize()
                .context("failed to freeze reviewed plugin MCP argument path")?;
            if !frozen.starts_with(staged_root) {
                anyhow::bail!("reviewed plugin MCP argument path escaped its staged root");
            }
            *argument = frozen.display().to_string();
        }
    }
    Ok(())
}

fn resolve_plugin_mcp_cwd(plugin_path: &Path, cwd: Option<&Path>) -> Result<PathBuf> {
    let cwd = match cwd {
        Some(cwd) if cwd.is_relative() => normalize_path_components(&plugin_path.join(cwd)),
        Some(cwd) => normalize_path_components(cwd),
        None => plugin_path.to_path_buf(),
    };
    let resolved = cwd
        .canonicalize()
        .unwrap_or_else(|_| normalize_path_components(&cwd));
    if !resolved.starts_with(plugin_path) {
        anyhow::bail!("reviewed plugin MCP path escaped its staged root");
    }
    Ok(resolved)
}

fn workspace_allows_project_mcp_config(workspace: &Path) -> bool {
    crate::config::is_workspace_trusted(workspace)
}

fn checked_workspace_mcp_config_path(workspace: &Path) -> Result<PathBuf> {
    Ok(checked_workspace_path(workspace)?
        .join(".codewhale")
        .join("mcp.json"))
}

fn checked_workspace_path(workspace: &Path) -> Result<PathBuf> {
    if workspace.as_os_str().is_empty() {
        anyhow::bail!("workspace path cannot be empty");
    }
    if workspace
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        anyhow::bail!("workspace path cannot contain '..' components");
    }
    let absolute = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve current directory for workspace")?
            .join(workspace)
    };
    match absolute.canonicalize() {
        Ok(path) => Ok(path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            Ok(normalize_path_components(&absolute))
        }
        Err(err) => {
            Err(err).with_context(|| format!("failed to resolve workspace {}", workspace.display()))
        }
    }
}

fn normalize_workspace_path(workspace: &Path) -> PathBuf {
    if let Ok(canonical) = workspace.canonicalize() {
        return canonical;
    }
    let absolute = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(workspace)
    };
    normalize_path_components(&absolute)
}

fn resolve_project_mcp_cwd(workspace: &Path, cwd: Option<&Path>) -> Result<PathBuf> {
    let cwd = match cwd {
        Some(cwd) if cwd.is_relative() => normalize_path_components(&workspace.join(cwd)),
        Some(cwd) => normalize_path_components(cwd),
        None => workspace.to_path_buf(),
    };
    let resolved = cwd
        .canonicalize()
        .unwrap_or_else(|_| normalize_path_components(&cwd));
    if !resolved.starts_with(workspace) {
        anyhow::bail!(
            "Project MCP server cwd must stay within workspace: {}",
            resolved.display()
        );
    }
    Ok(resolved)
}

/// Drop the Win32 verbatim prefix before handing a path to an external child
/// (Node MCP peers, etc.). `Path::canonicalize` returns `\\?\C:\...`; Node's
/// ESM loader and several Windows tools refuse or mis-handle that spelling for
/// ordinary paths under MAX_PATH. Containment checks keep the prefixed form;
/// only argv and current_dir are rewritten. Same rule as runtime_api job cwd
/// and Git for Windows.
#[cfg(any(windows, test))]
fn plain_windows_child_path(path: &str) -> String {
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    match path.strip_prefix(r"\\?\") {
        Some(rest)
            if rest.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && rest.as_bytes().get(1) == Some(&b':') =>
        {
            rest.to_string()
        }
        _ => path.to_string(),
    }
}

#[cfg(windows)]
pub(crate) fn strip_windows_verbatim_for_child(
    path: impl AsRef<std::ffi::OsStr>,
) -> std::ffi::OsString {
    let raw = path.as_ref().to_string_lossy();
    std::ffi::OsString::from(plain_windows_child_path(&raw))
}

#[cfg(not(windows))]
pub(crate) fn strip_windows_verbatim_for_child(
    path: impl AsRef<std::ffi::OsStr>,
) -> std::ffi::OsString {
    path.as_ref().to_os_string()
}

fn normalize_path_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        normalized
    }
}

fn paths_refer_to_same_config(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => normalize_workspace_path(left) == normalize_workspace_path(right),
    }
}

/// Rebuild a JSON value with every object's keys in sorted order.
///
/// [`McpConfig`] is full of `HashMap`s (`servers`, and per-server `env`,
/// `headers`, `env_headers`), and `serde_json` is built here with
/// `preserve_order`, so a serialization inherits whatever order the source
/// `HashMap` happened to iterate in. Two `HashMap`s built separately in one
/// process do *not* share an iteration order — `RandomState` re-seeds per map
/// — so the raw serialization of two structurally identical configs differs.
/// Sorting first makes the byte form depend only on content. The value tree
/// here is the fixed `McpConfig` shape, so the recursion depth is bounded by
/// that struct, not by untrusted input.
fn canonicalize_json_keys(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> = map.into_iter().collect();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonicalize_json_keys(value)))
                    .collect(),
            )
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(canonicalize_json_keys).collect())
        }
        other => other,
    }
}

/// 64-bit content hash of an [`McpConfig`]. Used by [`McpPool`] to decide
/// whether a freshly-read config differs from the one currently driving the
/// live connections. Hashing the JSON serialization avoids forcing every
/// nested config type to derive `Hash` (the timeouts struct, network policy
/// stubs, etc.); the serialization is key-sorted first so the hash depends on
/// content alone and not on per-`HashMap` iteration order. The hash is stable
/// within a process for structurally identical configs, and across runs of the
/// same Rust toolchain for byte-identical input.
fn hash_mcp_config(config: &McpConfig) -> u64 {
    use std::hash::{Hash, Hasher};
    let canonical = serde_json::to_value(config)
        .map(canonicalize_json_keys)
        .unwrap_or(serde_json::Value::Null);
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    let mut native = config
        .servers
        .iter()
        .filter_map(|(name, server)| {
            Some((
                name,
                server
                    .reviewed_plugin
                    .as_ref()?
                    .native_mcp
                    .as_ref()?
                    .catalog_identity(),
            ))
        })
        .collect::<Vec<_>>();
    native.sort_by_key(|(name, _)| *name);
    native.hash(&mut hasher);
    hasher.finish()
}

/// Best-effort fetch of the MCP config file's last-modified time. Returns
/// `None` when the file is missing, when stat fails, when the platform
/// doesn't expose mtime, or when the path fails the same allow-list check
/// that MCP configuration reads and mutations apply. The lazy-reload check in
/// `McpPool::get_or_connect` treats `None` as "skip the check this turn",
/// so a rejected path simply degrades to "no auto-reload" rather than an
/// error path. Callers already validate via `validate_mcp_config_path` at
/// construction time; the redundant validation here keeps this helper
/// safe-by-construction for any future caller and ties the validation to
/// the call site rather than relying on cross-function reasoning.
fn mcp_config_mtime(path: &Path) -> Option<std::time::SystemTime> {
    validate_mcp_config_path(path).ok()?;
    fs::metadata(path).ok()?.modified().ok()
}

/// A stale caller must reload instead of overwriting another process's edit.
#[derive(Debug)]
pub struct McpRevisionConflict;
impl std::fmt::Display for McpRevisionConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MCP configuration changed; reload it before saving")
    }
}
impl std::error::Error for McpRevisionConflict {}

fn config_revision(raw: Option<&str>) -> String {
    raw.map_or_else(
        || "mcp-v1-absent".to_owned(),
        |raw| format!("mcp-v1-{}", crate::hashing::sha256_hex(raw.as_bytes())),
    )
}

pub fn read_config_revision(path: &Path) -> Result<String> {
    validate_mcp_config_path(path)?;
    let raw = read_mcp_config_file(path)?;
    Ok(config_revision(raw.as_deref()))
}

/// Apply only changed known fields to the original JSON. Unknown fields in
/// unrelated objects and in edited server entries remain operator-owned.
fn apply_json_delta(
    raw: &mut serde_json::Value,
    before: &serde_json::Value,
    after: &serde_json::Value,
) {
    if before == after {
        return;
    }
    if let (Some(raw), Some(before), Some(after)) =
        (raw.as_object_mut(), before.as_object(), after.as_object())
    {
        for key in before.keys().chain(after.keys()).collect::<BTreeSet<_>>() {
            match (before.get(key), after.get(key)) {
                (Some(old), Some(new)) if old != new => {
                    apply_json_delta(raw.entry(key.clone()).or_insert(old.clone()), old, new);
                }
                (None, Some(new)) => {
                    raw.insert(key.clone(), new.clone());
                }
                (Some(_), None) => {
                    raw.remove(key);
                }
                _ => {}
            }
        }
    } else {
        *raw = after.clone();
    }
}

/// Every managed MCP writer rereads under the same OS-process lock. This is
/// a delta operation, not a save of a previously loaded typed snapshot.
pub fn mutate_config<T>(
    path: &Path,
    expected_revision: Option<&str>,
    mutate: impl FnOnce(&mut McpConfig) -> Result<T>,
) -> Result<(T, String)> {
    validate_mcp_config_path(path)?;
    reject_linked_workspace_state_path(path)?;
    codewhale_config::with_config_write_lock(path, |path| {
        let original = read_mcp_config_file(path)?;
        let revision = config_revision(original.as_deref());
        if expected_revision.is_some_and(|expected| expected != revision) {
            return Err(McpRevisionConflict.into());
        }
        let mut raw: serde_json::Value = match original.as_deref() {
            Some(raw) => serde_json::from_str(raw).map_err(|_| {
                anyhow::anyhow!("Failed to parse MCP config; file contents were omitted")
            })?,
            None => serde_json::json!({}),
        };
        anyhow::ensure!(raw.is_object(), "MCP config must be an object");
        let mut config: McpConfig = serde_json::from_value(raw.clone())
            .map_err(|_| anyhow::anyhow!("Invalid MCP config; file contents were omitted"))?;
        let before = serde_json::to_value(&config)?;
        let result = mutate(&mut config)?;
        let after = serde_json::to_value(&config)?;
        if before == after {
            return Ok((result, revision));
        }
        // Preserve legacy spelling while applying the canonical typed delta.
        let legacy = raw.get("mcpServers").is_some();
        if legacy {
            let object = raw
                .as_object_mut()
                .context("MCP config must be an object")?;
            let servers = object.remove("mcpServers").expect("checked above");
            object.insert("servers".into(), servers);
        }
        apply_json_delta(&mut raw, &before, &after);
        if legacy {
            let object = raw
                .as_object_mut()
                .context("MCP config must be an object")?;
            if let Some(servers) = object.remove("servers") {
                object.insert("mcpServers".into(), servers);
            }
        }
        let rendered = serde_json::to_string_pretty(&raw)?;
        if rendered.len() as u64 > MAX_MCP_CONFIG_BYTES {
            anyhow::bail!("MCP config exceeds the 1 MiB limit");
        }
        write_atomic(path, rendered.as_bytes())
            .with_context(|| format!("Failed to write MCP config {}", path.display()))?;
        Ok((result, config_revision(Some(&rendered))))
    })
}

fn mcp_template_json() -> Result<String> {
    let mut cfg = McpConfig::default();
    cfg.servers.insert(
        "example".to_string(),
        McpServerConfig {
            command: Some("node".to_string()),
            args: vec!["./path/to/your-mcp-server.js".to_string()],
            env: HashMap::new(),
            cwd: None,
            url: None,
            transport: None,
            connect_timeout: None,
            execute_timeout: None,
            read_timeout: None,
            disabled: true,
            enabled: true,
            required: false,
            lazy: false,
            enabled_tools: Vec::new(),
            disabled_tools: Vec::new(),
            headers: HashMap::new(),
            env_headers: HashMap::new(),
            bearer_token_env_var: None,
            scopes: Vec::new(),
            oauth: None,
            oauth_resource: None,
            reviewed_plugin: None,
            runtime_added: false,
            allow_private_network: false,
        },
    );
    serde_json::to_string_pretty(&cfg).context("Failed to render MCP template JSON")
}

pub fn init_config(path: &Path, force: bool) -> Result<McpWriteStatus> {
    validate_mcp_config_path(path)?;
    reject_linked_workspace_state_path(path)?;
    codewhale_config::with_config_write_lock(path, |path| {
        let original = read_mcp_config_file(path)?;
        if let Some(raw) = original.as_deref() {
            let _: McpConfig = serde_json::from_str(raw)
                .map_err(|_| anyhow::anyhow!("Invalid MCP config; file contents were omitted"))?;
            if !force {
                return Ok(McpWriteStatus::SkippedExists);
            }
        }
        let template = mcp_template_json()?;
        write_atomic(path, template.as_bytes())?;
        Ok(if original.is_some() {
            McpWriteStatus::Overwritten
        } else {
            McpWriteStatus::Created
        })
    })
}

pub fn add_server_config(
    path: &Path,
    name: String,
    command: Option<String>,
    url: Option<String>,
    args: Vec<String>,
    transport: Option<String>,
) -> Result<()> {
    if command.is_none() && url.is_none() {
        anyhow::bail!("Provide either a command or URL for MCP server '{name}'.");
    }
    validate_mcp_transport(transport.as_deref())?;
    mutate_config(path, None, |cfg| {
        cfg.servers.insert(
            name,
            McpServerConfig {
                command,
                args,
                env: HashMap::new(),
                cwd: None,
                url,
                transport,
                connect_timeout: None,
                execute_timeout: None,
                read_timeout: None,
                disabled: false,
                enabled: true,
                required: false,
                lazy: false,
                enabled_tools: Vec::new(),
                disabled_tools: Vec::new(),
                headers: HashMap::new(),
                env_headers: HashMap::new(),
                bearer_token_env_var: None,
                scopes: Vec::new(),
                oauth: None,
                oauth_resource: None,
                reviewed_plugin: None,
                runtime_added: false,
                allow_private_network: false,
            },
        );
        Ok(())
    })
    .map(|_| ())
}

pub fn remove_server_config(path: &Path, name: &str) -> Result<()> {
    mutate_config(path, None, |cfg| {
        if cfg.servers.remove(name).is_none() {
            anyhow::bail!("MCP server '{name}' not found");
        }
        Ok(())
    })
    .map(|_| ())
}

pub fn set_server_enabled(path: &Path, name: &str, enabled: bool) -> Result<()> {
    mutate_config(path, None, |cfg| {
        let server = cfg
            .servers
            .get_mut(name)
            .ok_or_else(|| anyhow::anyhow!("MCP server '{name}' not found"))?;
        server.enabled = enabled;
        server.disabled = !enabled;
        Ok(())
    })
    .map(|_| ())
}

#[cfg(test)]
pub fn manager_snapshot_from_config(
    path: &Path,
    reload_required: bool,
) -> Result<McpManagerSnapshot> {
    let cfg = load_config(path)?;
    Ok(snapshot_from_config(
        path,
        path.exists(),
        reload_required,
        &cfg,
        None,
    ))
}

#[cfg(test)]
pub fn manager_snapshot_from_config_with_workspace(
    path: &Path,
    workspace: &Path,
    reload_required: bool,
) -> Result<McpManagerSnapshot> {
    let plugins = crate::plugins::PluginRegistry::empty(workspace);
    manager_snapshot_from_config_with_workspace_and_plugins(
        path,
        workspace,
        reload_required,
        &plugins,
    )
}

pub fn manager_snapshot_from_config_with_workspace_and_plugins(
    path: &Path,
    workspace: &Path,
    reload_required: bool,
    plugins: &crate::plugins::PluginRegistry,
) -> Result<McpManagerSnapshot> {
    let cfg = load_config_with_workspace_and_plugins(path, workspace, plugins)?;
    Ok(snapshot_from_config(
        path,
        path.exists(),
        reload_required,
        &cfg,
        None,
    ))
}

#[cfg(test)]
pub async fn discover_manager_snapshot(
    path: &Path,
    network_policy: Option<NetworkPolicyDecider>,
    reload_required: bool,
) -> Result<McpManagerSnapshot> {
    let cfg = load_config(path)?;
    let mut pool = McpPool::new(cfg.clone());
    if let Some(policy) = network_policy {
        pool = pool.with_network_policy(policy);
    }
    let errors = pool
        .connect_all()
        .await
        .into_iter()
        .map(|(name, err)| (name, format_mcp_error_for_display(&err)))
        .collect::<HashMap<_, _>>();
    Ok(snapshot_from_config(
        path,
        path.exists(),
        reload_required,
        &cfg,
        Some((&pool, &errors)),
    ))
}

pub async fn discover_manager_snapshot_with_workspace_and_plugins(
    path: &Path,
    workspace: &Path,
    network_policy: Option<NetworkPolicyDecider>,
    reload_required: bool,
    plugins: Arc<crate::plugins::PluginRegistry>,
    backend: McpBackend,
) -> Result<McpManagerSnapshot> {
    let cfg = load_config_with_workspace_and_plugins(path, workspace, plugins.as_ref())?;
    let mut pool = McpPool::new(cfg.clone()).with_backend(backend);
    pool.workspace = Some(checked_workspace_path(workspace)?);
    pool.plugin_registry = Some(plugins);
    if let Some(policy) = network_policy {
        pool = pool.with_network_policy(policy);
    }
    let errors = pool
        .connect_all()
        .await
        .into_iter()
        .map(|(name, err)| (name, format_mcp_error_for_display(&err)))
        .collect::<HashMap<_, _>>();
    Ok(snapshot_from_config(
        path,
        path.exists(),
        reload_required,
        &cfg,
        Some((&pool, &errors)),
    ))
}

pub(crate) fn format_mcp_error_for_display(error: &anyhow::Error) -> String {
    codewhale_config::persistence::redact_secrets(&format!("{error:#}"))
}

impl McpPool {
    /// Snapshot the live pool rather than starting a second discovery pool.
    /// This keeps the manager, hotbar, and next model turn aligned on one
    /// exact config/catalog generation.
    pub(crate) fn manager_snapshot(
        &self,
        path: &Path,
        reload_required: bool,
        errors: &HashMap<String, String>,
    ) -> McpManagerSnapshot {
        snapshot_from_config(
            path,
            path.exists(),
            reload_required,
            &self.config,
            Some((self, errors)),
        )
    }
}

fn snapshot_from_config(
    path: &Path,
    config_exists: bool,
    reload_required: bool,
    cfg: &McpConfig,
    discovery: Option<(&McpPool, &HashMap<String, String>)>,
) -> McpManagerSnapshot {
    let mut servers = cfg
        .servers
        .iter()
        .filter(|(name, _)| discovery.is_none_or(|(pool, _)| pool.server_allowed(name)))
        .map(|(name, server)| {
            let transport = if server.url.is_some() {
                if is_legacy_sse_transport(server) {
                    "sse"
                } else {
                    "http/sse"
                }
            } else {
                "stdio"
            };
            let command_or_url = server.url.clone().unwrap_or_else(|| {
                let mut command = server
                    .command
                    .clone()
                    .unwrap_or_else(|| "(missing)".to_string());
                if !server.args.is_empty() {
                    command.push(' ');
                    command.push_str(&server.args.join(" "));
                }
                command
            });
            let mut snapshot = McpServerSnapshot {
                name: name.clone(),
                enabled: server.is_enabled(),
                required: server.required,
                transport: transport.to_string(),
                command_or_url,
                connect_timeout: server.effective_connect_timeout(&cfg.timeouts),
                execute_timeout: server.effective_execute_timeout(&cfg.timeouts),
                read_timeout: server.effective_read_timeout(&cfg.timeouts),
                connected: false,
                error: if server.is_enabled() {
                    None
                } else {
                    Some("disabled".to_string())
                },
                auth_required: false,
                capability_metadata: McpServerCapabilityMetadata::NotObserved,
                tools: Vec::new(),
                resources: Vec::new(),
                prompts: Vec::new(),
            };

            if let Some((pool, errors)) = discovery {
                if let Some(error) = pool
                    .connect_backoff
                    .get(name)
                    .map(|backoff| &backoff.last_error)
                    .or_else(|| errors.get(name))
                {
                    snapshot.error = Some(error.clone());
                }
                // The pool's needs-auth set is the authority; the error text
                // fallback keeps a boot-time error map (held by the engine
                // after the pool's live state was rebuilt) on the same
                // classification instead of downgrading to a plain failure.
                let oauth_capable = mcp_server_oauth_capable(server);
                let aws_login = snapshot
                    .error
                    .as_deref()
                    .is_some_and(|error| mcp_error_is_aws_login(error, oauth_capable));
                // An expired AWS login is never `◆ auth required`: the
                // needs-auth set already excludes it, and the text fallback
                // must too, or a `401`-worded AWS error reaches `/mcp login`.
                snapshot.auth_required = server.is_enabled()
                    && (pool.server_needs_auth(name)
                        || (!aws_login
                            && snapshot
                                .error
                                .as_deref()
                                .is_some_and(oauth::error_text_looks_auth_required)));
                // The row's retry is only useful once the user has run the
                // AWS login. Errors from paths other than `initialize` (child
                // exit, EOF, tools/list) carry no hint, so name it here or
                // the retry fails the same way and says nothing.
                if aws_login
                    && let Some(error) = snapshot.error.as_mut()
                    && !error.contains("AWS credentials expired:")
                {
                    let hint = aws_login_hint(server, name, error);
                    error.push_str("; ");
                    error.push_str(&hint);
                }
                if let Some(conn) = pool.connections.get(name) {
                    snapshot.connected = conn.is_ready();
                    snapshot.capability_metadata = conn.server_capabilities.map_or(
                        McpServerCapabilityMetadata::LegacyFallback,
                        McpServerCapabilityMetadata::Advertised,
                    );
                    if snapshot.connected {
                        // A count of connected servers and nothing else. The
                        // name, the command or URL, and the error string are
                        // user-chosen and routinely name internal infra.
                        codewhale_telemetry::session_counters()
                            .bump(codewhale_telemetry::Counter::McpServerConnected);
                    }
                    snapshot.tools = conn
                        .tools()
                        .iter()
                        .filter(|tool| {
                            conn.config().is_tool_enabled(&tool.name)
                                && pool
                                    .tool_allowed(&McpPool::mcp_model_tool_name(name, &tool.name))
                        })
                        .map(|tool| McpDiscoveredItem {
                            name: tool.name.clone(),
                            model_name: format!("mcp_{}_{}", name, tool.name),
                            description: tool.description.clone(),
                        })
                        .collect();
                    snapshot.resources =
                        conn.resources()
                            .iter()
                            .map(|resource| McpDiscoveredItem {
                                name: resource.name.clone(),
                                model_name: format!(
                                    "mcp_{}_{}",
                                    name,
                                    resource.name.replace(' ', "_").to_lowercase()
                                ),
                                description: resource.description.clone(),
                            })
                            .chain(conn.resource_templates().iter().map(|template| {
                                McpDiscoveredItem {
                                    name: template.name.clone(),
                                    model_name: format!(
                                        "mcp_{}_{}",
                                        name,
                                        template.name.replace(' ', "_").to_lowercase()
                                    ),
                                    description: template.description.clone(),
                                }
                            }))
                            .collect();
                    snapshot.prompts = conn
                        .prompts()
                        .iter()
                        .map(|prompt| McpDiscoveredItem {
                            name: prompt.name.clone(),
                            model_name: format!("mcp_{}_{}", name, prompt.name),
                            description: prompt.description.clone(),
                        })
                        .collect();
                }
            }

            snapshot
        })
        .collect::<Vec<_>>();
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    McpManagerSnapshot {
        config_path: path.to_path_buf(),
        config_exists,
        reload_required,
        servers,
    }
}

#[cfg(test)]
mod windows_child_path_tests {
    use super::plain_windows_child_path;

    #[test]
    fn plain_windows_child_path_drops_only_the_verbatim_prefix() {
        assert_eq!(
            plain_windows_child_path(r"\\?\C:\ws\peer.mjs"),
            r"C:\ws\peer.mjs"
        );
        assert_eq!(
            plain_windows_child_path(r"\\?\UNC\host\share\peer.mjs"),
            r"\\host\share\peer.mjs"
        );
        for unchanged in [
            r"C:\ws\peer.mjs",
            r"\\host\share",
            r"\\?\Volume{0}\ws",
            "/tmp/peer.mjs",
            "peer.mjs",
        ] {
            assert_eq!(plain_windows_child_path(unchanged), unchanged);
        }
    }
}

#[cfg(test)]
mod qualified_plugin_server_name_tests {
    use super::{qualified_plugin_server_name, split_qualified_plugin_server_name};

    /// The wire key round-trips even when both halves contain `-`, which is
    /// what the length prefix is for. The Extensions panel relies on this to
    /// show `plugin/server` instead of `plugin-25-plugin-server`.
    #[test]
    fn a_qualified_plugin_server_name_round_trips_through_its_split() {
        for (plugin, server) in [
            ("codewhale-account-plugins", "codewhale-plugins"),
            ("kimi-datasource", "data"),
            ("a", "b"),
            ("dash-heavy-name-here", "server-with-dashes"),
        ] {
            let qualified = qualified_plugin_server_name(plugin, server);
            assert_eq!(
                split_qualified_plugin_server_name(&qualified),
                Some((plugin, server)),
                "round trip failed for {qualified}"
            );
        }
    }

    #[test]
    fn a_name_that_is_not_a_plugin_key_does_not_split() {
        for plain in [
            "aws",
            "github",
            "plugin-",
            "plugin-x-a-b",
            "plugin-99-short",
        ] {
            assert_eq!(split_qualified_plugin_server_name(plain), None, "{plain}");
        }
    }
}

// === Unit Tests ===

#[cfg(test)]
mod tests;
#[cfg(test)]
pub(crate) use tests::computer_use_test_fixture;

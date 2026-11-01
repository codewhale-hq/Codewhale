use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct McpProvenance(Arc<dyn std::any::Any + Send + Sync>);

impl McpProvenance {
    pub fn new<T: std::any::Any + Send + Sync>(value: T) -> Self {
        Self(Arc::new(value))
    }

    #[must_use]
    pub fn downcast_ref<T: std::any::Any>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl std::fmt::Debug for McpProvenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("McpProvenance(..)")
    }
}

pub fn is_relative_stdio_path_arg(value: &str) -> bool {
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
    !std::path::Path::new(value).is_absolute() && !windows_absolute
}

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
pub fn default_connect_timeout() -> u64 {
    30
}
// 30 minutes: an MCP tool call legitimately runs minutes — builds, test
// suites, scrapes, remote jobs. The old 60s default returned "timed out" to
// the model for healthy-but-slow tools, which then retried and compounded
// the cost. Per-server and global `execute_timeout` overrides still win.
// Scope note: `prompts/get` also routes through `effective_execute_timeout`,
// so it inherits this default; its server-side template work is normally
// fast, but the override knob is the intended way to keep it tight.
pub fn default_execute_timeout() -> u64 {
    1800
}
pub fn default_read_timeout() -> u64 {
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
    /// Optional exact RFC 8707 resource pin, checked against SDK discovery.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_resource: Option<String>,
    /// In-memory provenance for MCP servers contributed by a reviewed plugin
    /// bundle. This is never deserialized from or serialized into user config:
    /// only the trusted plugin merge adapter may attach it.
    #[serde(skip)]
    pub reviewed_plugin: Option<McpProvenance>,
    /// Only the runtime registration boundary can attach this provenance.
    #[serde(skip)]
    pub runtime_added: bool,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct McpServerOAuthConfig {
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_metadata_url: Option<String>,
}

const PACKAGE_RUNNER_CONNECT_TIMEOUT_SECS: u64 = 120;

fn default_enabled() -> bool {
    true
}

impl McpServerConfig {
    pub fn effective_connect_timeout(&self, global: &McpTimeouts) -> u64 {
        self.connect_timeout.unwrap_or_else(|| {
            if self.launches_package_runner() {
                global
                    .connect_timeout
                    .max(PACKAGE_RUNNER_CONNECT_TIMEOUT_SECS)
            } else {
                global.connect_timeout
            }
        })
    }

    fn launches_package_runner(&self) -> bool {
        self.url.is_none()
            && self
                .command
                .as_deref()
                .and_then(|command| std::path::Path::new(command).file_stem())
                .and_then(|stem| stem.to_str())
                .is_some_and(|stem| {
                    matches!(
                        stem.to_ascii_lowercase().as_str(),
                        "npx" | "pnpx" | "bunx" | "uvx"
                    )
                })
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

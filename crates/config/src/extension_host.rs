use serde::Deserialize;

// === Types ===

/// `[extension_host]`: settings for the experimental TypeScript extension
/// host (`[features] extension_host`). User config only: project-scope config
/// is applied by an explicit allowlist that does not include this table.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionHostConfig {
    /// Explicit MCP protocol backend. Host is the default and supports stdio,
    /// HTTP and SSE independently of optional Native extension activation.
    #[serde(default)]
    pub mcp_backend: crate::McpBackend,
    /// Which runtime runs the host: `node` (the default), `bun`, or `auto`
    /// (Bun when a supported one is found and starts, else Node). `bun` and
    /// `auto` are opt-ins: Bun is not the default until it is qualified on
    /// every platform. An explicit `bun` or `node` never falls back to the
    /// other runtime. Unset means `node`, except that a table setting only
    /// `bun` means `bun` ([`Self::effective_runtime`]).
    #[serde(default)]
    pub runtime: Option<ExtensionHostRuntime>,
    /// Path to a Node.js runtime (`^22.19 || >=24`). When set it is the only
    /// Node candidate: if it does not run or is below the floor, Node
    /// resolution fails with that reason instead of searching `PATH`.
    /// Unset, every `node` on `PATH` is tried in order, skipping any inside
    /// a `node_modules` directory or the working directory.
    #[serde(default)]
    pub node: Option<String>,
    /// Path to a Bun runtime (>= 1.4.0). When set it is the only Bun
    /// candidate, as for `node`. Unset, `bun` on `PATH` and then
    /// `$BUN_INSTALL/bin` (default `~/.bun/bin`) are tried, with the same
    /// skips.
    #[serde(default)]
    pub bun: Option<String>,
}

impl ExtensionHostConfig {
    /// `runtime` as configured, else `bun` when only a Bun path is set (the
    /// table names no other runtime), else `node`.
    #[must_use]
    pub fn effective_runtime(&self) -> ExtensionHostRuntime {
        match (self.runtime, &self.node, &self.bun) {
            (Some(runtime), _, _) => runtime,
            (None, None, Some(_)) => ExtensionHostRuntime::Bun,
            (None, _, _) => ExtensionHostRuntime::Node,
        }
    }
}

/// `[extension_host] runtime`. Node is the default; Bun (`bun`, or `auto`,
/// which prefers it) stays an opt-in until an explicit, recorded cutover.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionHostRuntime {
    Auto,
    Bun,
    #[default]
    Node,
}

impl ExtensionHostRuntime {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Bun => "bun",
            Self::Node => "node",
        }
    }
}

/// `[plugins."<name>"]`: per-plugin settings, keyed by the plugin's manifest
/// name. User config only (like `[extension_host]`): project-scope config is
/// applied by an explicit allowlist that does not include this table, so a
/// repository cannot configure the plugins it asks you to trust.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PluginSettings {
    /// `[plugins."<name>".config]`: a TOML table delivered as the `config`
    /// argument of the plugin's `apply(ctx, config)` when the extension host
    /// activates it, validated against the plugin's own `Config` schema when it
    /// declares one. A change takes effect at `/plugin reload` (the plugin is
    /// re-activated). Values are not secrets storage: they are shown to the
    /// plugin's code, and `/plugin show` lists their keys.
    #[serde(default)]
    pub config: Option<toml::Table>,
}

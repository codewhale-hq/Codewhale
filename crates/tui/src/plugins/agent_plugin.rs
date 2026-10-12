//! Agent Plugins v1.0.0 (agent-plugins.org) format support.
//!
//! The standard is a vendor-neutral packaging format: a `plugin.json` manifest
//! with a closed root (`$schema`, `name`, and the optional well-known fields;
//! everything client-specific lives under `extensions`, keyed by reverse-domain
//! namespace), a sibling `mcp.json` for MCP servers (the manifest root is
//! closed, so servers cannot live in `plugin.json`), and a `skills/<name>/
//! SKILL.md` tree that already matches Codewhale's skill layout.
//!
//! This module owns the mapping between that format and Codewhale's internal
//! [`PluginManifest`]:
//!
//! * **Consume** — [`parse_plugin_json`] + [`standard_to_manifest`] and
//!   [`parse_mcp_json`]. Unknown `extensions` namespaces are ignored, never
//!   rejected: that is what lets a Cursor- or Copilot-authored plugin load
//!   here. Codewhale-specific fields round-trip through
//!   `extensions["net.codewhale"]`.
//! * **Publish** — [`manifest_to_standard`], emitting a spec-valid
//!   `plugin.json` (+ `mcp.json` when servers exist). [`validate_plugin_json`]
//!   and [`validate_mcp_json`] re-check every emission against the standard's
//!   shape before it is written, so upstream drift fails here rather than
//!   shipping.
//! * **Names** — the standard's name rule
//!   (`^(?!.*(?:--|\.\.))[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$`, 1–64 chars) is
//!   [`is_standard_plugin_name`]; [`slugify_plugin_name`] maps a legacy name
//!   onto it. A slugified name that collides with an existing plugin is an
//!   error, never a silent rename.
//!
//! Auto-migration of on-disk `plugin.toml` files is deliberately **not**
//! implemented here; this module only converts representations.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::manifest::{
    CURRENT_SCHEMA_VERSION, MAX_PLUGIN_NAME_CHARS, PluginCapabilities, PluginManifest, PluginMeta,
    PluginPathSpec, PluginWhen,
};
use crate::mcp::{McpServerConfig, McpServerOAuthConfig};

/// Manifest file names, in discovery preference order. `plugin.json` is the
/// native Agent Plugins format; `plugin.toml` is the legacy Codewhale format
/// and stays readable.
pub const PLUGIN_JSON_NAME: &str = "plugin.json";
pub const PLUGIN_TOML_NAME: &str = "plugin.toml";
/// Kimi Code's native plugin manifest. Codewhale consumes the compatible
/// Skills and MCP subset directly so official Kimi bundles do not need to be
/// rewritten or copied by hand before installation.
pub const KIMI_PLUGIN_JSON_NAME: &str = "kimi.plugin.json";
/// Sibling file carrying MCP server definitions for a `plugin.json` bundle.
pub const MCP_JSON_NAME: &str = "mcp.json";

/// `$schema` values emitted for the v1.0.0 documents.
pub const PLUGIN_SCHEMA_URL: &str = "https://agent-plugins.org/schemas/plugin.json";
pub const MCP_SCHEMA_URL: &str = "https://agent-plugins.org/schemas/mcp.json";

/// Codewhale's reverse-domain extension namespace.
pub const CODEWHALE_NAMESPACE: &str = "net.codewhale";

/// `env` keys a plugin may never define: the host runtime owns these.
const RESERVED_MCP_ENV_NAMES: [&str; 2] = ["PLUGIN_ROOT", "PLUGIN_DATA"];

/// The standard's name rule: lowercase ASCII letters/digits with internal
/// single hyphens or dots, starting and ending alphanumeric, never containing
/// `--` or `..`, 1–64 chars. Equivalent to
/// `^(?!.*(?:--|\.\.))[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$`.
#[must_use]
pub fn is_standard_plugin_name(name: &str) -> bool {
    let count = name.chars().count();
    if count == 0 || count > MAX_PLUGIN_NAME_CHARS {
        return false;
    }
    if name.contains("--") || name.contains("..") {
        return false;
    }
    let alphanumeric = |ch: char| ch.is_ascii_lowercase() || ch.is_ascii_digit();
    name.chars().next().is_some_and(alphanumeric)
        && name.chars().next_back().is_some_and(alphanumeric)
        && name
            .chars()
            .all(|ch| alphanumeric(ch) || ch == '-' || ch == '.')
}

/// Map an arbitrary legacy plugin name onto the standard's name rule.
///
/// Letters are ASCII-lowercased; every maximal run of non-conforming
/// characters collapses to one separator (`.` when the run is dots only, `-`
/// otherwise), leading/trailing separators are dropped, and the result is
/// truncated to 64 chars. Errors when nothing usable remains — slugification
/// never invents a name.
pub fn slugify_plugin_name(name: &str) -> Result<String, String> {
    let mut slug = String::new();
    let mut pending_separator: Option<char> = None;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if let Some(separator) = pending_separator.take()
                && !slug.is_empty()
            {
                slug.push(separator);
            }
            slug.push(ch.to_ascii_lowercase());
        } else {
            let separator = if ch == '.' { '.' } else { '-' };
            pending_separator = Some(match (pending_separator, separator) {
                (Some('.'), '.') => '.',
                (None, sep) => sep,
                // Any run touching a hyphen (or mixing separators) is a hyphen.
                _ => '-',
            });
        }
    }
    while slug.chars().count() > MAX_PLUGIN_NAME_CHARS {
        slug.pop();
    }
    while slug.ends_with(['-', '.']) {
        slug.pop();
    }
    if slug.is_empty() {
        return Err(format!(
            "plugin name `{name}` cannot be slugified: it contains no ASCII letters or digits"
        ));
    }
    debug_assert!(is_standard_plugin_name(&slug));
    Ok(slug)
}

/// Locate the manifest of a bundle directory: `plugin.json` when present,
/// otherwise a legacy `plugin.toml`. A bundle shipping both (dual-publish for
/// older clients) is read through its `plugin.json`.
#[must_use]
pub fn resolve_manifest_path(root: &Path) -> Option<PathBuf> {
    let json = root.join(PLUGIN_JSON_NAME);
    if json.is_file() {
        return Some(json);
    }
    let kimi_json = root.join(KIMI_PLUGIN_JSON_NAME);
    if kimi_json.is_file() {
        return Some(kimi_json);
    }
    let toml = root.join(PLUGIN_TOML_NAME);
    if toml.is_file() {
        return Some(toml);
    }
    let claude = root.join(".claude-plugin").join(PLUGIN_JSON_NAME);
    claude.is_file().then_some(claude)
}

/// The bundle root is outside Claude's manifest-only metadata directory.
pub(crate) fn plugin_root_for_manifest(path: &Path) -> Option<&Path> {
    let parent = path.parent()?;
    if path.file_name()?.to_str()? == PLUGIN_JSON_NAME
        && parent
            .file_name()
            .is_some_and(|name| name == ".claude-plugin")
    {
        parent.parent()
    } else {
        Some(parent)
    }
}

/// Claude's skills/commands/agents/MCP subset uses the existing declarative
/// adapters. Unsupported components fail validation; no hooks are silently lost.
pub(crate) fn parse_claude_plugin_json(
    text: &str,
    root: &Path,
    mcp_bytes: Option<&[u8]>,
) -> Result<PluginManifest, String> {
    let mut value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| format!("invalid Claude plugin.json: {error}"))?;
    let object = value
        .as_object_mut()
        .ok_or("Claude plugin.json must be an object")?;
    for component in ["hooks/hooks.json", ".lsp.json"] {
        if root.join(component).exists() {
            return Err(format!(
                "Claude component {component} is not supported by this importer; no partial plugin will be installed"
            ));
        }
    }

    // Reuse the compatible, closed metadata/component schema. Claude's
    // default component directories are additive to explicit paths.
    for component in ["skills", "commands", "agents"] {
        if root.join(component).is_dir() {
            let mut paths = match object.remove(component) {
                Some(value) => {
                    let spec = serde_json::from_value::<KimiPathSpec>(value)
                        .map_err(|error| format!("invalid Claude {component} paths: {error}"))?
                        .into_plugin_path_spec()?;
                    spec.path.into_iter().chain(spec.paths).collect::<Vec<_>>()
                }
                None => Vec::new(),
            };
            if !paths.iter().any(|path| {
                path.strip_prefix("./")
                    .unwrap_or(path)
                    .trim_end_matches('/')
                    == component
            }) {
                paths.push(component.to_string());
            }
            object.insert(component.to_string(), serde_json::json!(paths));
        }
    }
    let mut servers = match object.remove("mcpServers") {
        Some(serde_json::Value::Object(map)) => map,
        Some(_) => return Err("Claude mcpServers must be an inline server object; custom MCP file paths are not supported".to_string()),
        None => serde_json::Map::new(),
    };
    if let Some(bytes) = mcp_bytes {
        let mut file: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| format!("invalid .mcp.json: {error}"))?;
        let map = file.as_object_mut().ok_or(".mcp.json must be an object")?;
        let entries = if let Some(wrapped) = map.remove("mcpServers") {
            map.remove("$schema");
            if !map.is_empty() {
                return Err("unsupported .mcp.json wrapper fields".to_string());
            }
            wrapped
                .as_object()
                .cloned()
                .ok_or(".mcp.json mcpServers must be an object")?
        } else {
            std::mem::take(map)
        };
        for (name, server) in entries {
            if servers.insert(name.clone(), server).is_some() {
                return Err(format!(
                    "MCP server {name} is declared in both plugin.json and .mcp.json"
                ));
            }
        }
    }
    for (name, server) in &mut servers {
        if server.get("type").and_then(serde_json::Value::as_str) == Some("http") {
            server["type"] = serde_json::json!("streamable-http");
        }
        if server.to_string().contains("${CLAUDE_PLUGIN_ROOT}") {
            return Err(format!(
                "MCP server {name} uses unsupported CLAUDE_PLUGIN_ROOT expansion; use bundle-relative command paths"
            ));
        }
    }
    object.insert("mcpServers".to_string(), serde_json::Value::Object(servers));
    let mut manifest = parse_kimi_plugin_json(&value.to_string(), root)
        .map_err(|error| error.replace("kimi.plugin.json", "Claude plugin.json"))?;
    // Official Claude bundles express credential sources as header templates.
    // Translate only exact environment references into the existing reviewed
    // credential contract. Never expand credentials while importing a bundle.
    if let Some(servers) = &mut manifest.mcp_servers {
        for (name, server) in servers {
            for (header, value) in std::mem::take(&mut server.headers) {
                let authorization = header.eq_ignore_ascii_case("authorization");
                if server
                    .env_headers
                    .keys()
                    .any(|key| key.eq_ignore_ascii_case(&header))
                    || (authorization && server.bearer_token_env_var.is_some())
                {
                    return Err(format!(
                        "MCP server {name} has duplicate credential sources for header {header}"
                    ));
                }
                if authorization
                    && let Some(source) = value
                        .strip_prefix("Bearer ")
                        .and_then(super::manifest::exact_environment_placeholder)
                {
                    server.bearer_token_env_var = Some(source.to_string());
                } else if let Some(source) = super::manifest::exact_environment_placeholder(&value)
                {
                    server.env_headers.insert(header, source.to_string());
                } else {
                    return Err(format!(
                        "MCP server {name} header {header} must use an exact environment reference; literal or compound header values are not supported"
                    ));
                }
            }
        }
    }
    Ok(manifest)
}

// ─────────────────────────────────────────────────────────────────────────────
// Kimi Code compatibility
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct KimiPluginManifest {
    #[serde(rename = "$schema", default)]
    schema: Option<String>,
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    author: Option<KimiAuthor>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    repository: Option<String>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    skills: Option<KimiPathSpec>,
    #[serde(default)]
    commands: Option<KimiPathSpec>,
    #[serde(default)]
    agents: Option<KimiPathSpec>,
    #[serde(rename = "mcpServers", default)]
    mcp_servers: BTreeMap<String, KimiMcpServer>,
    #[serde(rename = "interface", default)]
    interface_metadata: Option<KimiInterface>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum KimiAuthor {
    Text(String),
    Detailed(StandardAuthor),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum KimiPathSpec {
    One(String),
    Many(Vec<String>),
}

impl KimiPathSpec {
    fn into_plugin_path_spec(self) -> Result<PluginPathSpec, String> {
        match self {
            Self::One(path) => Ok(PluginPathSpec {
                path: Some(path),
                paths: Vec::new(),
            }),
            Self::Many(paths) if paths.is_empty() => {
                Err("Kimi plugin component path list must not be empty".to_string())
            }
            Self::Many(paths) => Ok(PluginPathSpec { path: None, paths }),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct KimiInterface {
    #[serde(rename = "displayName", default)]
    display_name: Option<String>,
    #[serde(rename = "shortDescription", default)]
    _short_description: Option<String>,
    #[serde(rename = "longDescription", default)]
    _long_description: Option<String>,
    #[serde(rename = "developerName", default)]
    _developer_name: Option<String>,
    #[serde(rename = "websiteURL", default)]
    _website_url: Option<String>,
    #[serde(rename = "iconUrl", default)]
    _icon_url: Option<String>,
    #[serde(rename = "category", default)]
    _category: Option<String>,
    #[serde(rename = "hostKind", default)]
    host_kind: Option<String>,
    #[serde(default)]
    platforms: Vec<String>,
    #[serde(rename = "mcpOverrides", default)]
    mcp_overrides: BTreeMap<String, KimiMcpInterfaceOverride>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct KimiMcpInterfaceOverride {
    #[serde(rename = "displayName", default)]
    _display_name: Option<String>,
    #[serde(rename = "iconUrl", default)]
    _icon_url: Option<String>,
}

/// Kimi's MCP shape is the standard MCP server entry plus a top-level
/// `enabledTools` allow-list. Keeping this as a separate closed type prevents
/// Kimi-only fields from weakening the Agent Plugins `mcp.json` parser.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct KimiMcpServer {
    #[serde(rename = "type", default)]
    transport: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    extensions: BTreeMap<String, serde_json::Value>,
    #[serde(rename = "enabledTools", default)]
    enabled_tools: Vec<String>,
}

impl KimiMcpServer {
    fn into_config(self, id: &str) -> Result<McpServerConfig, String> {
        let enabled_tools = self.enabled_tools;
        let standard = StandardMcpServer {
            transport: self.transport,
            command: self.command,
            args: self.args,
            env: self.env,
            cwd: self.cwd,
            url: self.url,
            headers: self.headers,
            extensions: self.extensions,
        };
        let mut config = standard_server_to_config(id, standard)?;
        if !enabled_tools.is_empty() {
            if !config.enabled_tools.is_empty() && config.enabled_tools != enabled_tools {
                return Err(format!(
                    "kimi.plugin.json MCP server `{id}` declares conflicting enabledTools filters"
                ));
            }
            config.enabled_tools = enabled_tools;
        }
        Ok(config)
    }
}

/// Parse the Skills/MCP-compatible subset of a Kimi Code plugin manifest.
///
/// The input shape is deliberately closed. Kimi capabilities Codewhale does
/// not yet implement (for example lifecycle hooks or prompt injection) fail
/// validation instead of being silently dropped and then presented as fully
/// working. In addition to the Skills-only official bundles, this accepts the
/// known Kimi-managed CU shape: display/platform interface metadata and MCP
/// `enabledTools`. External applications, daemons, binaries, and permissions
/// are prerequisites outside this parser and are never implied to exist.
pub fn parse_kimi_plugin_json(text: &str, root: &Path) -> Result<PluginManifest, String> {
    let kimi: KimiPluginManifest = serde_json::from_str(text)
        .map_err(|error| format!("failed to parse kimi.plugin.json: {error}"))?;
    if let Some(schema) = kimi.schema.as_deref()
        && schema.trim().is_empty()
    {
        return Err("kimi.plugin.json `$schema` must be a non-empty string".to_string());
    }
    if !is_kimi_plugin_name(&kimi.name) {
        return Err(format!(
            "kimi.plugin.json name `{}` is invalid (1-{MAX_PLUGIN_NAME_CHARS} lowercase ASCII letters, digits, hyphens, or underscores; must start with a letter or digit)",
            kimi.name
        ));
    }

    let skills = match kimi.skills {
        Some(paths) => Some(paths.into_plugin_path_spec()?),
        None if root.join("SKILL.md").is_file() => Some(PluginPathSpec {
            path: Some(".".to_string()),
            paths: Vec::new(),
        }),
        None => None,
    };
    let commands = kimi
        .commands
        .map(KimiPathSpec::into_plugin_path_spec)
        .transpose()?;
    let agents = kimi
        .agents
        .map(KimiPathSpec::into_plugin_path_spec)
        .transpose()?;

    let mut mcp_servers = HashMap::with_capacity(kimi.mcp_servers.len());
    for (id, server) in kimi.mcp_servers {
        mcp_servers.insert(id.clone(), server.into_config(&id)?);
    }
    let mut network_hosts = mcp_servers
        .values()
        .filter_map(|server| server.url.as_deref())
        .filter_map(|url| reqwest::Url::parse(url).ok())
        .filter_map(|url| url.host_str().map(str::to_ascii_lowercase))
        .collect::<Vec<_>>();
    network_hosts.sort();
    network_hosts.dedup();

    let author = match kimi.author {
        Some(KimiAuthor::Text(author)) => Some(author),
        Some(KimiAuthor::Detailed(author)) => Some(compose_author(author)?),
        None => None,
    };
    let (display_name, when) = match kimi.interface_metadata {
        Some(interface) => {
            if let Some(host_kind) = interface.host_kind.as_deref()
                && !host_kind.eq_ignore_ascii_case("local")
            {
                return Err(format!(
                    "kimi.plugin.json interface hostKind `{host_kind}` is unsupported; only `local` is understood"
                ));
            }
            if interface
                .mcp_overrides
                .keys()
                .any(|key| key.trim().is_empty())
            {
                return Err(
                    "kimi.plugin.json interface mcpOverrides keys must not be empty".to_string(),
                );
            }
            let mut platforms = Vec::with_capacity(interface.platforms.len());
            let mut seen = BTreeSet::new();
            const SUPPORTED_PLATFORMS: &[&str] = &[
                "windows", "linux", "macos", "freebsd", "openbsd", "netbsd", "android", "ios",
            ];
            for platform in interface.platforms {
                let platform = platform.trim().to_ascii_lowercase();
                if !SUPPORTED_PLATFORMS.contains(&platform.as_str()) {
                    return Err(format!(
                        "kimi.plugin.json interface has unsupported platform `{platform}`"
                    ));
                }
                if !seen.insert(platform.clone()) {
                    return Err(format!(
                        "kimi.plugin.json interface repeats platform `{platform}`"
                    ));
                }
                platforms.push(platform);
            }
            let when = (!platforms.is_empty()).then_some(PluginWhen {
                os: Some(platforms),
                binaries: None,
            });
            (interface.display_name, when)
        }
        None => (None, None),
    };

    Ok(PluginManifest {
        schema_version: CURRENT_SCHEMA_VERSION,
        plugin: PluginMeta {
            name: kimi.name,
            description: kimi.description,
            version: kimi.version.unwrap_or_else(|| "0.0.0".to_string()),
            author,
            homepage: kimi.homepage,
            repository: kimi.repository,
            license: kimi.license,
            keywords: kimi.keywords,
            display_name,
            icon: None,
        },
        skills,
        commands,
        agents,
        hooks: None,
        lsp: None,
        native: None,
        providers: BTreeMap::new(),
        mcp_servers: (!mcp_servers.is_empty()).then_some(mcp_servers),
        capabilities: PluginCapabilities {
            network_hosts,
            ..PluginCapabilities::default()
        },
        when,
    })
}

#[must_use]
pub fn is_kimi_plugin_name(name: &str) -> bool {
    let count = name.chars().count();
    count > 0
        && count <= MAX_PLUGIN_NAME_CHARS
        && name
            .chars()
            .next()
            .is_some_and(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
        && name
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

// ─────────────────────────────────────────────────────────────────────────────
// Standard document shapes
// ─────────────────────────────────────────────────────────────────────────────

/// `plugin.json` per Agent Plugins v1.0.0. The root is closed
/// (`additionalProperties: false`); client-specific data belongs under
/// `extensions`, keyed by reverse-domain namespace.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StandardPluginManifest {
    #[serde(rename = "$schema")]
    pub schema: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<StandardAuthor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StandardAuthor {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Codewhale's own `extensions["net.codewhale"]` payload: every manifest
/// concept the standard does not define. Unknown keys inside our own namespace
/// are rejected (`deny_unknown_fields`) — the ignore-unknowns rule applies to
/// *other* vendors' namespaces, not to a silently dropped Codewhale field.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodewhalePluginExtension {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<PluginPathSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<PluginPathSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<PluginPathSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<PluginPathSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lsp: Option<PluginPathSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<PluginPathSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, super::providers::PluginProviderDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<PluginCapabilities>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<PluginWhen>,
}

impl CodewhalePluginExtension {
    #[must_use]
    fn is_empty(&self) -> bool {
        self.display_name.is_none()
            && self.icon.is_none()
            && self.skills.is_none()
            && self.commands.is_none()
            && self.agents.is_none()
            && self.hooks.is_none()
            && self.lsp.is_none()
            && self.native.is_none()
            && self.providers.is_empty()
            && self.capabilities.is_none()
            && self.when.is_none()
    }
}

/// `mcp.json` per Agent Plugins v1.0.0: `$schema` plus `mcpServers` keyed by
/// server id.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StandardMcpFile {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    #[serde(rename = "mcpServers")]
    pub servers: BTreeMap<String, StandardMcpServer>,
}

/// One `mcp.json` server entry. Transports: `stdio` (command/args/env/cwd),
/// `streamable-http` (url/headers), and `sse` (url/headers). `type` may be
/// omitted and is then inferred from `command` vs `url`. Codewhale-only
/// server options ride in `extensions["net.codewhale"]`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StandardMcpServer {
    #[serde(rename = "type", default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

/// Codewhale-only MCP server options, carried per-server under
/// `extensions["net.codewhale"]` in `mcp.json` so the standard fields round-
/// trip losslessly with Codewhale's richer review model (timeouts, tool
/// filters, env-backed credentials, enablement).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CodewhaleMcpExtension {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_headers: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearer_token_env_var: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execute_timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_timeout: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<McpServerOAuthConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_resource: Option<String>,
}

impl CodewhaleMcpExtension {
    #[must_use]
    fn is_empty(&self) -> bool {
        self.env_headers.is_empty()
            && self.bearer_token_env_var.is_none()
            && self.connect_timeout.is_none()
            && self.execute_timeout.is_none()
            && self.read_timeout.is_none()
            && self.enabled.is_none()
            && self.disabled.is_none()
            && self.required.is_none()
            && self.enabled_tools.is_empty()
            && self.disabled_tools.is_empty()
            && self.scopes.is_empty()
            && self.oauth.is_none()
            && self.oauth_resource.is_none()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Consume: standard documents -> internal manifest
// ─────────────────────────────────────────────────────────────────────────────

/// Parse a `plugin.json` document. Unknown root keys are rejected (the
/// standard's root is closed); unknown `extensions` namespaces parse fine and
/// are dropped by [`standard_to_manifest`].
pub fn parse_plugin_json(text: &str) -> Result<StandardPluginManifest, String> {
    let manifest: StandardPluginManifest = serde_json::from_str(text)
        .map_err(|error| format!("failed to parse plugin.json: {error}"))?;
    if manifest.schema.trim().is_empty() {
        return Err("plugin.json `$schema` must be a non-empty string".to_string());
    }
    if !is_standard_plugin_name(&manifest.name) {
        return Err(format!(
            "plugin.json name `{}` is invalid under the Agent Plugins standard (1-{MAX_PLUGIN_NAME_CHARS} lowercase ASCII letters, digits, or internal single `-`/`.`; never `--` or `..`)",
            manifest.name
        ));
    }
    Ok(manifest)
}

/// Parse an `mcp.json` document into internal server configs, enforcing the
/// per-transport shapes and the reserved `env` names.
pub fn parse_mcp_json(text: &str) -> Result<HashMap<String, McpServerConfig>, String> {
    let file: StandardMcpFile =
        serde_json::from_str(text).map_err(|error| format!("failed to parse mcp.json: {error}"))?;
    let mut servers = HashMap::with_capacity(file.servers.len());
    for (id, server) in file.servers {
        servers.insert(id.clone(), standard_server_to_config(&id, server)?);
    }
    Ok(servers)
}

fn standard_server_to_config(
    id: &str,
    server: StandardMcpServer,
) -> Result<McpServerConfig, String> {
    let extension = codewhale_mcp_extension(&server.extensions)?;
    let transport = resolve_transport(id, &server)?;
    let (command, url, sse) = match transport {
        StandardTransport::Stdio => {
            let command = server.command.clone().ok_or_else(|| {
                format!("mcp.json server `{id}` declares stdio transport without a command")
            })?;
            if server.url.is_some() || !server.headers.is_empty() {
                return Err(format!(
                    "mcp.json stdio server `{id}` may not declare url or headers"
                ));
            }
            for key in server.env.keys() {
                if RESERVED_MCP_ENV_NAMES.contains(&key.as_str()) {
                    return Err(format!(
                        "mcp.json server `{id}` env may not define reserved name `{key}`"
                    ));
                }
            }
            (Some(command), None, false)
        }
        StandardTransport::StreamableHttp | StandardTransport::Sse => {
            let url = server.url.clone().ok_or_else(|| {
                format!("mcp.json server `{id}` declares an HTTP transport without a url")
            })?;
            if server.command.is_some()
                || !server.args.is_empty()
                || !server.env.is_empty()
                || server.cwd.is_some()
            {
                return Err(format!(
                    "mcp.json HTTP server `{id}` may not declare command, args, env, or cwd"
                ));
            }
            (None, Some(url), transport == StandardTransport::Sse)
        }
    };
    Ok(McpServerConfig {
        command,
        args: server.args,
        env: server.env.into_iter().collect(),
        cwd: server.cwd.map(PathBuf::from),
        url,
        transport: sse.then(|| "sse".to_string()),
        connect_timeout: extension.connect_timeout,
        execute_timeout: extension.execute_timeout,
        read_timeout: extension.read_timeout,
        disabled: extension.disabled.unwrap_or(false),
        enabled: extension.enabled.unwrap_or(true),
        required: extension.required.unwrap_or(false),
        lazy: false,
        enabled_tools: extension.enabled_tools,
        disabled_tools: extension.disabled_tools,
        headers: server.headers.into_iter().collect(),
        env_headers: extension.env_headers.into_iter().collect(),
        bearer_token_env_var: extension.bearer_token_env_var,
        scopes: extension.scopes,
        oauth: extension.oauth,
        oauth_resource: extension.oauth_resource,
        reviewed_plugin: None,
        runtime_added: false,
        allow_private_network: false,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StandardTransport {
    Stdio,
    StreamableHttp,
    Sse,
}

fn resolve_transport(id: &str, server: &StandardMcpServer) -> Result<StandardTransport, String> {
    match server.transport.as_deref() {
        Some(declared) => match declared.to_ascii_lowercase().as_str() {
            "stdio" => Ok(StandardTransport::Stdio),
            "streamable-http" => Ok(StandardTransport::StreamableHttp),
            "sse" => Ok(StandardTransport::Sse),
            _ => Err(format!(
                "mcp.json server `{id}` has unknown type `{declared}`; expected `stdio`, `streamable-http`, or `sse`"
            )),
        },
        None => match (server.command.is_some(), server.url.is_some()) {
            (true, false) => Ok(StandardTransport::Stdio),
            (false, true) => Ok(StandardTransport::StreamableHttp),
            _ => Err(format!(
                "mcp.json server `{id}` must declare exactly one of command or url, or set an explicit type"
            )),
        },
    }
}

fn codewhale_mcp_extension(
    extensions: &BTreeMap<String, serde_json::Value>,
) -> Result<CodewhaleMcpExtension, String> {
    match extensions.get(CODEWHALE_NAMESPACE) {
        Some(value) => serde_json::from_value(value.clone()).map_err(|error| {
            format!("mcp.json server extensions[\"{CODEWHALE_NAMESPACE}\"] is invalid: {error}")
        }),
        None => Ok(CodewhaleMcpExtension::default()),
    }
}

/// Convert a parsed `plugin.json` (+ its sibling `mcp.json` servers, when the
/// file exists) into the internal manifest. `root` is the bundle directory:
/// the standard fixes the skills layout at `skills/`, so a bundle with no
/// Codewhale extension gets the default skills spec exactly when that
/// directory exists.
pub fn standard_to_manifest(
    standard: StandardPluginManifest,
    mcp_servers: Option<HashMap<String, McpServerConfig>>,
    root: &Path,
) -> Result<PluginManifest, String> {
    let extension = match standard.extensions.get(CODEWHALE_NAMESPACE) {
        Some(value) => {
            serde_json::from_value::<CodewhalePluginExtension>(value.clone()).map_err(|error| {
                format!("plugin.json extensions[\"{CODEWHALE_NAMESPACE}\"] is invalid: {error}")
            })?
        }
        None => CodewhalePluginExtension::default(),
    };
    let skills = extension.skills.or_else(|| {
        root.join("skills").is_dir().then(|| PluginPathSpec {
            path: Some("skills".to_string()),
            paths: Vec::new(),
        })
    });
    Ok(PluginManifest {
        schema_version: CURRENT_SCHEMA_VERSION,
        plugin: PluginMeta {
            name: standard.name,
            description: standard.description,
            version: standard.version.unwrap_or_else(|| "0.0.0".to_string()),
            author: standard.author.map(compose_author).transpose()?,
            homepage: standard.homepage,
            repository: standard.repository,
            license: standard.license,
            keywords: standard.keywords,
            display_name: extension.display_name,
            icon: extension.icon,
        },
        skills,
        commands: extension.commands,
        agents: extension.agents,
        hooks: extension.hooks,
        lsp: extension.lsp,
        native: extension.native,
        providers: extension.providers,
        mcp_servers,
        capabilities: extension.capabilities.unwrap_or_default(),
        when: extension.when,
    })
}

/// `author: {name, email, url}` flattens onto the internal free-form author
/// string (`name <email> (url)`). The inverse mapping keeps the whole string
/// in `author.name`, so a Codewhale-origin author round-trips verbatim.
fn compose_author(author: StandardAuthor) -> Result<String, String> {
    let name = author.name.trim();
    if name.is_empty() {
        return Err("plugin.json author.name must be a non-empty string".to_string());
    }
    let mut composed = name.to_string();
    if let Some(email) = author
        .email
        .as_deref()
        .filter(|email| !email.trim().is_empty())
    {
        composed.push_str(" <");
        composed.push_str(email.trim());
        composed.push('>');
    }
    if let Some(url) = author.url.as_deref().filter(|url| !url.trim().is_empty()) {
        composed.push_str(" (");
        composed.push_str(url.trim());
        composed.push(')');
    }
    Ok(composed)
}

// ─────────────────────────────────────────────────────────────────────────────
// Publish: internal manifest -> standard documents
// ─────────────────────────────────────────────────────────────────────────────

/// The spec-valid documents for one plugin, ready to serialize and write.
#[derive(Debug, Clone)]
pub struct StandardEmission {
    pub plugin_json: StandardPluginManifest,
    /// Present exactly when the plugin declares MCP servers.
    pub mcp_json: Option<StandardMcpFile>,
    /// The conforming name the bundle is published under.
    pub exported_name: String,
    /// The original name, preserved when publishing required slugification (or
    /// when an imported bundle already carried one).
    pub display_name: Option<String>,
}

/// Convert an internal manifest into spec-valid documents.
///
/// A name that is invalid under the standard is slugified and the original is
/// preserved as the display name. `existing_names` must hold every other
/// plugin name in scope: a slugified name colliding with an existing plugin is
/// an error, never a silent rename.
pub fn manifest_to_standard(
    manifest: &PluginManifest,
    existing_names: &BTreeSet<String>,
) -> Result<StandardEmission, String> {
    let original = manifest.plugin.name.as_str();
    let (name, renamed) = if is_standard_plugin_name(original) {
        (original.to_string(), false)
    } else {
        (slugify_plugin_name(original)?, true)
    };
    if renamed && existing_names.contains(&name) {
        return Err(format!(
            "slugifying plugin `{original}` yields `{name}`, which collides with an existing plugin; rename one of them first"
        ));
    }
    let display_name = if renamed {
        Some(original.to_string())
    } else {
        manifest.plugin.display_name.clone()
    };

    let extension = CodewhalePluginExtension {
        display_name: display_name.clone(),
        icon: manifest.plugin.icon.clone(),
        skills: manifest
            .skills
            .clone()
            .filter(|spec| !is_default_skills_spec(spec)),
        commands: manifest.commands.clone(),
        agents: manifest.agents.clone(),
        hooks: manifest.hooks.clone(),
        lsp: manifest.lsp.clone(),
        native: manifest.native.clone(),
        providers: manifest.providers.clone(),
        capabilities: (!capabilities_are_default(&manifest.capabilities))
            .then(|| manifest.capabilities.clone()),
        when: manifest.when.clone(),
    };

    let plugin_json = StandardPluginManifest {
        schema: PLUGIN_SCHEMA_URL.to_string(),
        name: name.clone(),
        version: Some(manifest.plugin.version.clone()).filter(|version| !version.trim().is_empty()),
        description: manifest.plugin.description.clone(),
        author: manifest.plugin.author.clone().map(|author| StandardAuthor {
            name: author,
            email: None,
            url: None,
        }),
        homepage: manifest.plugin.homepage.clone(),
        repository: manifest.plugin.repository.clone(),
        license: manifest.plugin.license.clone(),
        keywords: manifest.plugin.keywords.clone(),
        extensions: if extension.is_empty() {
            BTreeMap::new()
        } else {
            let value = serde_json::to_value(&extension)
                .map_err(|error| format!("failed to encode Codewhale extension: {error}"))?;
            BTreeMap::from([(CODEWHALE_NAMESPACE.to_string(), value)])
        },
    };

    let mcp_json = match &manifest.mcp_servers {
        Some(servers) if !servers.is_empty() => {
            let mut standard_servers = BTreeMap::new();
            for (id, config) in servers {
                standard_servers.insert(id.clone(), config_to_standard_server(id, config)?);
            }
            Some(StandardMcpFile {
                schema: Some(MCP_SCHEMA_URL.to_string()),
                servers: standard_servers,
            })
        }
        _ => None,
    };

    Ok(StandardEmission {
        plugin_json,
        mcp_json,
        exported_name: name,
        display_name,
    })
}

/// The standard fixes the skills tree at `skills/`; this spec (or its absence)
/// needs no extension entry.
#[must_use]
pub fn is_default_skills_spec(spec: &PluginPathSpec) -> bool {
    let mut declared = spec.path.iter().chain(spec.paths.iter());
    declared.next().is_some_and(|path| path == "skills") && declared.next().is_none()
}

fn capabilities_are_default(capabilities: &PluginCapabilities) -> bool {
    capabilities.filesystem_roots.is_empty()
        && capabilities.network_hosts.is_empty()
        && !capabilities.lifecycle_mutation
}

fn config_to_standard_server(
    id: &str,
    config: &McpServerConfig,
) -> Result<StandardMcpServer, String> {
    let extension = CodewhaleMcpExtension {
        env_headers: config
            .env_headers
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        bearer_token_env_var: config.bearer_token_env_var.clone(),
        connect_timeout: config.connect_timeout,
        execute_timeout: config.execute_timeout,
        read_timeout: config.read_timeout,
        enabled: (!config.enabled).then_some(false),
        disabled: config.disabled.then_some(true),
        required: config.required.then_some(true),
        enabled_tools: config.enabled_tools.clone(),
        disabled_tools: config.disabled_tools.clone(),
        scopes: config.scopes.clone(),
        oauth: config.oauth.clone(),
        oauth_resource: config.oauth_resource.clone(),
    };
    let extensions = if extension.is_empty() {
        BTreeMap::new()
    } else {
        let value = serde_json::to_value(&extension)
            .map_err(|error| format!("failed to encode Codewhale MCP extension: {error}"))?;
        BTreeMap::from([(CODEWHALE_NAMESPACE.to_string(), value)])
    };
    match (config.command.as_deref(), config.url.as_deref()) {
        (Some(command), None) => {
            for key in config.env.keys() {
                if RESERVED_MCP_ENV_NAMES.contains(&key.as_str()) {
                    return Err(format!(
                        "MCP server `{id}` env may not define reserved name `{key}`"
                    ));
                }
            }
            Ok(StandardMcpServer {
                transport: Some("stdio".to_string()),
                command: Some(command.to_string()),
                args: config.args.clone(),
                env: config
                    .env
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                cwd: config
                    .cwd
                    .as_ref()
                    .map(|cwd| cwd.to_string_lossy().into_owned()),
                url: None,
                headers: BTreeMap::new(),
                extensions,
            })
        }
        (None, Some(url)) => {
            let sse = config
                .transport
                .as_deref()
                .is_some_and(|transport| transport.eq_ignore_ascii_case("sse"));
            Ok(StandardMcpServer {
                transport: Some(if sse { "sse" } else { "streamable-http" }.to_string()),
                command: None,
                args: Vec::new(),
                env: BTreeMap::new(),
                cwd: None,
                url: Some(url.to_string()),
                headers: config
                    .headers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
                extensions,
            })
        }
        _ => Err(format!(
            "MCP server `{id}` must declare exactly one of command or url to export"
        )),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Emission conformance checks
// ─────────────────────────────────────────────────────────────────────────────
//
// Every document Codewhale writes is re-validated against the standard's shape
// before it hits disk; the same functions back the schema-conformance tests.

/// Validate a serialized `plugin.json` value against the standard's shape:
/// closed root, required `$schema`/`name`, typed optional fields, and the name
/// rule.
pub fn validate_plugin_json(value: &serde_json::Value) -> Result<(), String> {
    const ROOT_KEYS: [&str; 10] = [
        "$schema",
        "name",
        "version",
        "description",
        "author",
        "homepage",
        "repository",
        "license",
        "keywords",
        "extensions",
    ];
    let object = value
        .as_object()
        .ok_or_else(|| "plugin.json must be a JSON object".to_string())?;
    for key in object.keys() {
        if !ROOT_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "plugin.json root key `{key}` is not in the closed schema"
            ));
        }
    }
    let schema = required_string(object, "$schema")?;
    if schema.trim().is_empty() {
        return Err("plugin.json `$schema` must be a non-empty string".to_string());
    }
    let name = required_string(object, "name")?;
    if !is_standard_plugin_name(name) {
        return Err(format!(
            "plugin.json name `{name}` violates the standard's name rule"
        ));
    }
    for key in [
        "version",
        "description",
        "homepage",
        "repository",
        "license",
    ] {
        optional_string(object, key)?;
    }
    if let Some(author) = object.get("author") {
        let author = author
            .as_object()
            .ok_or_else(|| "plugin.json author must be an object".to_string())?;
        for key in author.keys() {
            if !["name", "email", "url"].contains(&key.as_str()) {
                return Err(format!(
                    "plugin.json author key `{key}` is not in the schema"
                ));
            }
        }
        required_string(author, "name")?;
        optional_string(author, "email")?;
        optional_string(author, "url")?;
    }
    if let Some(keywords) = object.get("keywords") {
        let keywords = keywords
            .as_array()
            .ok_or_else(|| "plugin.json keywords must be an array of strings".to_string())?;
        if keywords.iter().any(|keyword| !keyword.is_string()) {
            return Err("plugin.json keywords must be an array of strings".to_string());
        }
    }
    if let Some(extensions) = object.get("extensions")
        && !extensions.is_object()
    {
        return Err("plugin.json extensions must be an object keyed by namespace".to_string());
    }
    Ok(())
}

/// Validate a serialized `mcp.json` value against the standard's shape:
/// `$schema` + `mcpServers`, the per-transport field sets, and the reserved
/// `env` names.
pub fn validate_mcp_json(value: &serde_json::Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "mcp.json must be a JSON object".to_string())?;
    for key in object.keys() {
        if !["$schema", "mcpServers"].contains(&key.as_str()) {
            return Err(format!(
                "mcp.json root key `{key}` is not in the closed schema"
            ));
        }
    }
    if let Some(schema) = object.get("$schema")
        && !schema.is_string()
    {
        return Err("mcp.json `$schema` must be a string".to_string());
    }
    let servers = object
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "mcp.json must carry an `mcpServers` object".to_string())?;
    for (id, server) in servers {
        validate_mcp_server_json(id, server)?;
    }
    Ok(())
}

fn validate_mcp_server_json(id: &str, server: &serde_json::Value) -> Result<(), String> {
    const SERVER_KEYS: [&str; 8] = [
        "type",
        "command",
        "args",
        "env",
        "cwd",
        "url",
        "headers",
        "extensions",
    ];
    let object = server
        .as_object()
        .ok_or_else(|| format!("mcp.json server `{id}` must be an object"))?;
    for key in object.keys() {
        if !SERVER_KEYS.contains(&key.as_str()) {
            return Err(format!(
                "mcp.json server `{id}` key `{key}` is not in the closed schema"
            ));
        }
    }
    let transport = match object.get("type") {
        Some(value) => {
            let declared = value
                .as_str()
                .ok_or_else(|| format!("mcp.json server `{id}` type must be a string"))?;
            if !["stdio", "streamable-http", "sse"].contains(&declared) {
                return Err(format!(
                    "mcp.json server `{id}` has unknown type `{declared}`"
                ));
            }
            Some(declared)
        }
        None => None,
    };
    for key in ["command", "cwd", "url"] {
        optional_string(object, key).map_err(|error| format!("mcp.json server `{id}`: {error}"))?;
    }
    if let Some(values) = object.get("args") {
        let values = values
            .as_array()
            .ok_or_else(|| format!("mcp.json server `{id}` args must be an array of strings"))?;
        if values.iter().any(|value| !value.is_string()) {
            return Err(format!(
                "mcp.json server `{id}` args must be an array of strings"
            ));
        }
    }
    for key in ["env", "headers"] {
        if let Some(values) = object.get(key) {
            let values = values
                .as_object()
                .ok_or_else(|| format!("mcp.json server `{id}` {key} must be a string map"))?;
            if values.values().any(|value| !value.is_string()) {
                return Err(format!("mcp.json server `{id}` {key} must be a string map"));
            }
        }
    }
    if let Some(env) = object.get("env").and_then(serde_json::Value::as_object) {
        for key in env.keys() {
            if RESERVED_MCP_ENV_NAMES.contains(&key.as_str()) {
                return Err(format!(
                    "mcp.json server `{id}` env may not define reserved name `{key}`"
                ));
            }
        }
    }
    if let Some(extensions) = object.get("extensions")
        && !extensions.is_object()
    {
        return Err(format!(
            "mcp.json server `{id}` extensions must be an object"
        ));
    }
    let has_command = object.contains_key("command");
    let has_url = object.contains_key("url");
    let stdio_fields_empty = !has_command
        && !object.contains_key("args")
        && !object.contains_key("env")
        && !object.contains_key("cwd");
    let inferred = match transport {
        Some(declared) => declared,
        None if has_command && !has_url => "stdio",
        None if has_url && !has_command => "streamable-http",
        None => {
            return Err(format!(
                "mcp.json server `{id}` must declare exactly one of command or url, or set an explicit type"
            ));
        }
    };
    match inferred {
        "stdio" => {
            if !has_command {
                return Err(format!(
                    "mcp.json stdio server `{id}` must declare a command"
                ));
            }
            if has_url || object.contains_key("headers") {
                return Err(format!(
                    "mcp.json stdio server `{id}` may not declare url or headers"
                ));
            }
        }
        _ => {
            if !has_url {
                return Err(format!("mcp.json HTTP server `{id}` must declare a url"));
            }
            if !stdio_fields_empty {
                return Err(format!(
                    "mcp.json HTTP server `{id}` may not declare command, args, env, or cwd"
                ));
            }
        }
    }
    Ok(())
}

fn required_string<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("plugin.json requires a `{key}` string"))
}

fn optional_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<(), String> {
    match object.get(key) {
        Some(value) if !value.is_string() => Err(format!("`{key}` must be a string")),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full-featured legacy manifest: every Codewhale-specific field the
    /// standard does not define, plus both MCP transports.
    const ROUND_TRIP_TOML: &str = r#"schema_version = 1

[plugin]
name = "round-trip"
version = "1.2.3"
description = "Round-trip fixture"
author = "Jane Doe"
homepage = "https://example.com/round-trip"
repository = "https://github.com/example/round-trip"
license = "MIT"
keywords = ["one", "two"]

[skills]
path = "skills"

[commands]
path = "commands"

[capabilities]
filesystem_roots = ["src"]
network_hosts = ["example.com"]

[when]
os = ["macos", "linux"]

[mcp_servers.local]
command = "run.sh"
args = ["--port", "8080"]
env = { API_KEY = "${ROUND_TRIP_API_KEY}" }
cwd = "bin"

[mcp_servers.remote]
url = "https://example.com/mcp"
transport = "sse"
connect_timeout = 30
required = true
enabled_tools = ["search"]
env_headers = { Authorization = "ROUND_TRIP_REMOTE_TOKEN" }
"#;

    #[test]
    fn standard_name_rule_matches_the_spec_regex() {
        let long = "a".repeat(MAX_PLUGIN_NAME_CHARS);
        for valid in [
            "a",
            "0",
            "ab",
            "a-b",
            "a.b",
            "a-b.c-d",
            "a.-b",
            "acme.tools",
            long.as_str(),
        ] {
            assert!(is_standard_plugin_name(valid), "{valid}");
        }
        let too_long = "a".repeat(MAX_PLUGIN_NAME_CHARS + 1);
        for invalid in [
            "",
            "A",
            "a_b",
            "a b",
            "-a",
            "a-",
            ".a",
            "a.",
            "a--b",
            "a..b",
            "café",
            too_long.as_str(),
        ] {
            assert!(!is_standard_plugin_name(invalid), "{invalid}");
        }
    }

    #[test]
    fn slugify_maps_legacy_names_onto_the_standard_rule() {
        let cases = [
            ("round-trip", "round-trip"),
            ("acme.tools", "acme.tools"),
            ("My_Plugin Name", "my-plugin-name"),
            ("Foo--Bar", "foo-bar"),
            ("a..b", "a.b"),
            ("--weird--", "weird"),
            ("UPPER_SNAKE Case.v2", "upper-snake-case.v2"),
            ("  padded  ", "padded"),
            ("dots...here", "dots.here"),
            ("mixed._-seps", "mixed-seps"),
        ];
        for (input, expected) in cases {
            let slug = slugify_plugin_name(input).unwrap();
            assert_eq!(slug, expected, "input: {input}");
            assert!(is_standard_plugin_name(&slug), "slug for {input}");
        }

        let long = format!("{}-suffix", "a".repeat(70));
        let slug = slugify_plugin_name(&long).unwrap();
        assert!(slug.chars().count() <= MAX_PLUGIN_NAME_CHARS);
        assert!(is_standard_plugin_name(&slug));
        let boundary = format!("{}-{}", "a".repeat(63), "b".repeat(10));
        assert_eq!(slugify_plugin_name(&boundary).unwrap(), "a".repeat(63));

        for unusable in ["___", "...", "", "插件"] {
            assert!(slugify_plugin_name(unusable).is_err(), "{unusable}");
        }
    }

    #[test]
    fn toml_to_standard_to_internal_round_trip_is_stable() {
        let internal: PluginManifest = toml::from_str(ROUND_TRIP_TOML).unwrap();
        let emission = manifest_to_standard(&internal, &BTreeSet::new()).unwrap();
        assert_eq!(emission.exported_name, "round-trip");
        assert_eq!(emission.display_name, None);

        // Every emitted document conforms to the standard's shape.
        let plugin_value = serde_json::to_value(&emission.plugin_json).unwrap();
        validate_plugin_json(&plugin_value).unwrap();
        let mcp_value = serde_json::to_value(emission.mcp_json.as_ref().unwrap()).unwrap();
        validate_mcp_json(&mcp_value).unwrap();

        // The consume side derives the default skills spec from the standard
        // skills/ directory on disk.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("skills")).unwrap();

        let plugin_text = serde_json::to_string_pretty(&plugin_value).unwrap();
        let standard = parse_plugin_json(&plugin_text).unwrap();
        let mcp_text = serde_json::to_string_pretty(&mcp_value).unwrap();
        let servers = parse_mcp_json(&mcp_text).unwrap();
        let restored = standard_to_manifest(standard, Some(servers), tmp.path()).unwrap();

        assert_eq!(
            serde_json::to_value(&internal).unwrap(),
            serde_json::to_value(&restored).unwrap(),
            "plugin.toml -> internal -> plugin.json + mcp.json -> internal drifted"
        );
    }

    #[test]
    fn emitted_documents_carry_only_standard_root_keys() {
        let internal: PluginManifest = toml::from_str(ROUND_TRIP_TOML).unwrap();
        let emission = manifest_to_standard(&internal, &BTreeSet::new()).unwrap();
        let plugin_value = serde_json::to_value(&emission.plugin_json).unwrap();
        // mcp_servers never leak into the closed plugin.json root; Codewhale
        // data is namespaced.
        assert!(plugin_value.get("mcp_servers").is_none());
        assert!(plugin_value.get("mcpServers").is_none());
        assert!(
            plugin_value["extensions"]
                .get(CODEWHALE_NAMESPACE)
                .is_some()
        );
        let mcp_value = serde_json::to_value(emission.mcp_json.as_ref().unwrap()).unwrap();
        assert_eq!(mcp_value["$schema"], serde_json::json!(MCP_SCHEMA_URL));
        assert_eq!(mcp_value["mcpServers"]["local"]["type"], "stdio");
        assert_eq!(mcp_value["mcpServers"]["remote"]["type"], "sse");
        assert_eq!(
            mcp_value["mcpServers"]["remote"]["extensions"][CODEWHALE_NAMESPACE]["connect_timeout"],
            serde_json::json!(30)
        );
    }

    #[test]
    fn slugified_export_preserves_display_name_and_collision_is_an_error() {
        let toml = "schema_version = 1\n[plugin]\nname = \"Foo--Bar\"\nversion = \"1.0.0\"\n";
        let internal: PluginManifest = toml::from_str(toml).unwrap();

        let emission = manifest_to_standard(&internal, &BTreeSet::new()).unwrap();
        assert_eq!(emission.exported_name, "foo-bar");
        assert_eq!(emission.display_name.as_deref(), Some("Foo--Bar"));
        let extension = emission.plugin_json.extensions[CODEWHALE_NAMESPACE].clone();
        assert_eq!(extension["display_name"], serde_json::json!("Foo--Bar"));
        validate_plugin_json(&serde_json::to_value(&emission.plugin_json).unwrap()).unwrap();

        let colliding = BTreeSet::from(["foo-bar".to_string()]);
        let error = manifest_to_standard(&internal, &colliding).unwrap_err();
        assert!(error.contains("collides"), "{error}");
    }

    #[test]
    fn unknown_extensions_namespaces_are_ignored_not_rejected() {
        let text = r#"{
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "acme.tools",
            "version": "2.0.0",
            "author": {"name": "Acme", "email": "plugins@acme.example", "url": "https://acme.example"},
            "extensions": {
                "com.example.client": {"anything": [1, 2, 3], "nested": {"x": true}},
                "net.codewhale": {"when": {"os": ["macos"]}}
            }
        }"#;
        let standard = parse_plugin_json(text).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let manifest = standard_to_manifest(standard, None, tmp.path()).unwrap();
        assert_eq!(manifest.plugin.name, "acme.tools");
        assert_eq!(
            manifest.plugin.author.as_deref(),
            Some("Acme <plugins@acme.example> (https://acme.example)")
        );
        assert_eq!(
            manifest.when.and_then(|when| when.os),
            Some(vec!["macos".to_string()])
        );
        assert!(
            manifest.skills.is_none(),
            "no skills/ directory means no skills spec"
        );

        // Unknown keys inside our own namespace are rejected.
        let bad_extension = r#"{
            "$schema": "https://agent-plugins.org/schemas/plugin.json",
            "name": "acme.tools",
            "extensions": {"net.codewhale": {"surprise": true}}
        }"#;
        let standard = parse_plugin_json(bad_extension).unwrap();
        assert!(standard_to_manifest(standard, None, tmp.path()).is_err());

        // Unknown root keys are rejected: the standard's root is closed.
        let bad_root = r#"{"$schema": "x", "name": "acme.tools", "mcpServers": {}}"#;
        assert!(parse_plugin_json(bad_root).is_err());
    }

    #[test]
    fn plugin_json_requires_schema_and_a_standard_name() {
        let missing_schema = r#"{"name": "ok"}"#;
        assert!(parse_plugin_json(missing_schema).is_err());
        let bad_name = r#"{"$schema": "x", "name": "Not--Valid"}"#;
        assert!(parse_plugin_json(bad_name).is_err());
    }

    #[test]
    fn mcp_json_transports_and_reserved_env_names() {
        let reserved =
            r#"{"mcpServers": {"x": {"command": "run", "env": {"PLUGIN_ROOT": "/tmp"}}}}"#;
        let error = parse_mcp_json(reserved).unwrap_err();
        assert!(error.contains("PLUGIN_ROOT"), "{error}");

        let inferred = r#"{"mcpServers": {
            "local": {"command": "run.sh", "args": ["-y"]},
            "remote": {"url": "https://example.com/mcp"}
        }}"#;
        let servers = parse_mcp_json(inferred).unwrap();
        assert_eq!(servers["local"].command.as_deref(), Some("run.sh"));
        assert!(servers["local"].transport.is_none());
        assert!(servers["remote"].transport.is_none());

        let sse = r#"{"mcpServers": {"r": {"type": "sse", "url": "https://example.com/sse"}}}"#;
        assert_eq!(
            parse_mcp_json(sse).unwrap()["r"].transport.as_deref(),
            Some("sse")
        );
        for bad in [
            r#"{"mcpServers": {"r": {"type": "websocket", "url": "https://example.com/ws"}}}"#,
            r#"{"mcpServers": {"r": {"command": "x", "url": "https://example.com"}}}"#,
            r#"{"mcpServers": {"r": {"type": "stdio", "command": "x", "url": "https://example.com"}}}"#,
            r#"{"mcpServers": {"r": {"url": "https://example.com", "args": ["x"]}}}"#,
            r#"{"mcpServers": {"r": {"type": "sse"}}}"#,
        ] {
            assert!(parse_mcp_json(bad).is_err(), "{bad}");
        }

        let extended = r#"{"mcpServers": {"r": {"url": "https://example.com/mcp",
            "extensions": {"net.codewhale": {"connect_timeout": 30, "required": true,
                "env_headers": {"Authorization": "TOKEN_ENV"}}}}}}"#;
        let servers = parse_mcp_json(extended).unwrap();
        assert_eq!(servers["r"].connect_timeout, Some(30));
        assert!(servers["r"].required);
        assert_eq!(servers["r"].env_headers["Authorization"], "TOKEN_ENV");
    }

    #[test]
    fn emission_validators_reject_nonconforming_documents() {
        let extra_root_key = serde_json::json!({"$schema": "x", "name": "ok", "surprise": 1});
        assert!(validate_plugin_json(&extra_root_key).is_err());
        let bad_name = serde_json::json!({"$schema": "x", "name": "Not--Valid"});
        assert!(validate_plugin_json(&bad_name).is_err());
        let nameless_author =
            serde_json::json!({"$schema": "x", "name": "ok", "author": {"email": "a@b.c"}});
        assert!(validate_plugin_json(&nameless_author).is_err());

        let reserved_env = serde_json::json!({"mcpServers": {"x": {"command": "run", "env": {"PLUGIN_DATA": "y"}}}});
        assert!(validate_mcp_json(&reserved_env).is_err());
        let extra_root = serde_json::json!({"mcpServers": {}, "extra": true});
        assert!(validate_mcp_json(&extra_root).is_err());
        let cross_transport = serde_json::json!({"mcpServers": {"x": {"type": "sse", "url": "https://e.c", "cwd": "bin"}}});
        assert!(validate_mcp_json(&cross_transport).is_err());

        let conforming = serde_json::json!({
            "$schema": MCP_SCHEMA_URL,
            "mcpServers": {"x": {"type": "stdio", "command": "run", "env": {"A": "${B}"}}}
        });
        validate_mcp_json(&conforming).unwrap();
    }
}

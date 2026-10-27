//! MCP Registry sync tool.
//!
//! `registry_sync` fetches the MCP Registry index, filters stdio servers,
//! caches locally, and returns a summary. The snapshot is reused while
//! fresh (`INCREMENTAL_INTERVAL_SECS`); refresh is incremental via
//! `updated_since`, with a full pagination only when the snapshot is
//! missing or older than `FULL_RESYNC_INTERVAL_SECS`. Downloads run in the
//! background; the cache file is replaced atomically and doubles as the
//! launch-metadata store for `start_registry_mcp_server` (a failed sync
//! leaves the previous snapshot untouched).
//!
//! Upstream contract (MCP Registry, preview — breaking changes possible):
//!   * List operation `GET /v0.1/servers` (cursor / limit / search / version
//!     / include_deleted params):
//!     <https://registry.modelcontextprotocol.io/docs#/operations/list-servers-v0.1>
//!     OpenAPI source: <https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/api/openapi.yaml>
//!   * Aggregator integration guide (pagination format, server status
//!     lifecycle):
//!     <https://github.com/modelcontextprotocol/registry/blob/main/docs/modelcontextprotocol-io/registry-aggregators.mdx>

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, MutexGuard};

use crate::mcp::McpPool;
use crate::tools::spec::{
    ApprovalRequirement, ToolCapability, ToolContext, ToolError, ToolResult, ToolSpec,
};
use codewhale_util::utils::write_atomic;

// === Registry API response types ===

#[derive(Deserialize)]
struct RegistryResponse {
    servers: Vec<RegistryServerEntry>,
    metadata: Option<RegistryMetadata>,
}

#[derive(Deserialize)]
struct RegistryServerEntry {
    server: RegistryServer,
    // Registry-managed metadata. Carries the lifecycle `status` under the
    // official extension key (see `RegistryOfficialMeta`); the
    // publisher-provided subkey is deliberately not declared.
    #[serde(rename = "_meta", default)]
    meta: Option<RegistryResponseMeta>,
}

impl RegistryServerEntry {
    /// Lifecycle status reported by the official registry extension.
    /// `"active"` (or an absent extension) keeps the entry; `"deprecated"`
    /// and `"deleted"` retire it — the aggregator guide recommends dropping
    /// `deleted` entries (moderation takedowns: spam/malware/illegal) from
    /// downstream indexes, and we treat `deprecated` the same so the model
    /// is only offered servers the publisher still stands behind.
    /// <https://github.com/modelcontextprotocol/registry/blob/main/docs/modelcontextprotocol-io/registry-aggregators.mdx>
    fn lifecycle_status(&self) -> Option<&str> {
        self.meta
            .as_ref()
            .and_then(|m| m.official.as_ref())
            .and_then(|o| o.status.as_deref())
    }
}

#[derive(Deserialize)]
struct RegistryResponseMeta {
    #[serde(rename = "io.modelcontextprotocol.registry/official", default)]
    official: Option<RegistryOfficialMeta>,
}

/// `status` is required upstream (enum `active | deprecated | deleted`);
/// kept Optional here so a missing extension never fails a page parse.
#[derive(Deserialize)]
struct RegistryOfficialMeta {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Deserialize)]
struct RegistryServer {
    name: String,
    description: String,
    // `title`, `version`, `repository` are deliberately not declared —
    // the cache no longer carries them (see `McpRegistryServerEntry`)
    // and serde silently drops any extra fields, so we don't pay to
    // validate or store data we'd immediately throw away. All three are
    // optional in the upstream 2025-12 schema.
    #[serde(default)]
    packages: Option<Vec<RegistryPackage>>,
}

#[derive(Deserialize)]
struct RegistryPackage {
    #[serde(rename = "registryType")]
    registry_type: String,
    identifier: String,
    // The upstream OCI entries (e.g. docker.io/foo/bar:1.2.3) omit the
    // top-level `version` field because the tag is the version. Mirror that
    // — Optional, with a fallback that parses the trailing `:tag` from the
    // identifier when missing.
    #[serde(default)]
    version: Option<String>,
    // The upstream 2025-12 schema dropped `runtimeHint` for nearly every
    // entry (35/36 in the first page omit it; the runner is implied by
    // `registryType`). Keep it Optional and fall back to a registry-type
    // table when absent.
    #[serde(rename = "runtimeHint", default)]
    runtime_hint: Option<String>,
    // The upstream schema now models `transport` as an object:
    // `{"type": "stdio"}`. Older docs showed a bare string. Accept both
    // so a future flip-back doesn't break us.
    #[serde(deserialize_with = "deserialize_transport", default)]
    transport: Option<String>,
    #[serde(default)]
    #[serde(rename = "packageArguments")]
    package_arguments: Vec<RegistryArg>,
    #[serde(
        rename = "runtimeArguments",
        deserialize_with = "deserialize_runtime_arguments",
        default
    )]
    runtime_arguments: Vec<String>,
    /// Registry-provided environment requirements are intentionally kept
    /// transient. Runtime-discovered servers have no configuration channel
    /// for secrets/API keys, so any package declaring environment variables
    /// is ineligible and never reaches the on-disk cache.
    #[serde(rename = "environmentVariables", default)]
    environment_variables: Value,
}

impl RegistryPackage {
    fn declares_environment_variables(&self) -> bool {
        match &self.environment_variables {
            Value::Null => false,
            Value::Array(values) => !values.is_empty(),
            Value::Object(values) => !values.is_empty(),
            // Fail closed if a future Registry schema uses an unexpected shape.
            _ => true,
        }
    }
}

/// Deserialize `transport` as either a bare string (`"stdio"`) or an object
/// (`{"type": "stdio"}`). The MCP Registry 2025-12 schema ships the object
/// shape; older/draft docs showed the bare string. We accept both.
fn deserialize_transport<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrString {
        Bare(String),
        Wrapped {
            #[serde(rename = "type")]
            r#type: String,
        },
    }

    let opt: Option<OneOrString> = Option::deserialize(deserializer)?;
    Ok(opt.map(|v| match v {
        OneOrString::Bare(s) => s,
        OneOrString::Wrapped { r#type } => r#type,
    }))
}

/// Deserialize `runtimeArguments` as either `Vec<String>` (old schema)
/// or `Vec<{value, name, default, type, ...}>` (2025-12 schema). In the
/// object case we derive a string value from the available fields:
/// - Named args (`type: "named"`): `"{name} {default}"` or just `name`
/// - Positional args (`type: "positional"`): `default`
/// - Legacy objects with `value`: use `value` directly
fn deserialize_runtime_arguments<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrArg {
        Bare(String),
        Wrapped {
            #[serde(default)]
            value: Option<String>,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            default: Option<String>,
        },
    }

    let raw: Vec<StringOrArg> = Vec::deserialize(deserializer)?;
    let mut args = Vec::new();
    for arg in raw {
        match arg {
            StringOrArg::Bare(value) => args.push(value),
            StringOrArg::Wrapped {
                value: Some(value), ..
            } => args.push(value),
            StringOrArg::Wrapped { name, default, .. } => {
                if let Some(name) = name {
                    args.push(name);
                }
                if let Some(default) = default {
                    args.push(default);
                }
            }
        }
    }
    Ok(args)
}

/// Derive a runtime hint from `registryType` when the upstream omits one.
/// Kept small on purpose: only the runtimes we know how to launch.
fn default_runtime_hint(registry_type: &str) -> Option<&'static str> {
    match registry_type {
        "npm" => Some("npx"),
        "pypi" => Some("uvx"),
        _ => None,
    }
}

#[derive(Deserialize)]
struct RegistryArg {
    // Positional arguments ship without a name — only `value` and
    // `type`. Named arguments (`{"name": "--foo", "value": "bar", ...}`)
    // carry it. Accept both: when missing, downstream code uses `value`
    // as the arg name.
    #[serde(default)]
    name: Option<String>,
    description: Option<String>,
    // Upstream allows omitting `isRequired`; default false per spec.
    #[serde(rename = "isRequired", default)]
    is_required: bool,
    // Upstream `type` discriminator (`"positional"` / `"named"`). Drives
    // cmd-format decisions downstream. Renamed because `type` is a
    // reserved word in Rust.
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    value: Option<String>,
    default: Option<String>,
    // `format` dropped — never read by any consumer.
}

// === Cached index types ===

#[derive(Deserialize)]
struct RegistryMetadata {
    #[serde(rename = "nextCursor")]
    next_cursor: Option<String>,
}

// === Cached index types ===
//
// The cache file (`~/.codewhale/mcp-index.json`) is the on-disk source of
// truth for Registry-discovered local MCP launch metadata.

/// Bumped whenever the cache shape changes. Lets the loader detect an old
/// cache file and trigger a full resync instead of failing to deserialize.
pub const MCP_REGISTRY_CACHE_VERSION: u32 = 6;

#[derive(Serialize, Deserialize, Clone)]
pub struct McpRegistryIndex {
    pub version: u32,
    pub count: usize,
    pub servers: Vec<McpRegistryServerEntry>,
    /// RFC3339 timestamp of the last successful sync. Absent or older
    /// than `INCREMENTAL_INTERVAL_SECS` triggers the next refresh;
    /// older than `FULL_RESYNC_INTERVAL_SECS` makes it a full resync.
    #[serde(default)]
    pub synced_at: Option<DateTime<Utc>>,
}

/// One Registry catalog entry exposed to the model for contextual selection.
#[derive(Serialize, Deserialize, Clone)]
pub struct DigestEntry {
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_args: Vec<McpRegistryArgEntry>,
}

/// One cached server entry used by discovery and structured startup.
///
/// Everything else that the upstream Registry ships (`title`, repository,
/// `packages[]`, optional named args,
/// runtime_arguments at package level) is dropped. Fixed positional
/// `packageArguments` (e.g. an `mcp` subcommand) are folded into
/// `run_command` at render time rather than kept as fields.
#[derive(Serialize, Deserialize, Clone)]
pub struct McpRegistryServerEntry {
    pub name: String,
    pub description: String,
    pub launch: McpLaunchSpec,
}

/// Host-owned launch data for one zero-environment stdio server.
#[derive(Serialize, Deserialize, Clone)]
pub struct McpLaunchSpec {
    /// Template for the run command. The literal substring `<ARGS>` is
    /// replaced by host-rendered structured argument values.
    pub run_command: String,
    pub required_args: Vec<McpRegistryArgEntry>,
}

/// One CLI argument required at install time. `is_required` was dropped
/// because the cache only stores required args (others are filtered out
/// during sync). `kind` carries the upstream `type` discriminator
/// (`"positional"` vs `"named"`) so the cmd builder can decide whether
/// to emit `--name value` or just `value`.
#[derive(Serialize, Deserialize, Clone)]
pub struct McpRegistryArgEntry {
    pub name: String,
    pub kind: Option<String>,
    pub description: Option<String>,
    pub default: Option<String>,
}

// === Tool implementation ===

pub struct McpSyncRegistry {
    cache_path_override: Option<PathBuf>,
}

impl McpSyncRegistry {
    /// Default instance; resolves the cache under `dirs::home_dir()`.
    pub fn new() -> Self {
        Self {
            cache_path_override: None,
        }
    }

    /// Test hook: pin the cache file to an explicit path. `dirs::home_dir()`
    /// resolves the OS profile directory on Windows via SHGetKnownFolderPath,
    /// which no environment variable can redirect, so tests that need a
    /// hermetic cache inject the path directly on every platform.
    #[cfg(test)]
    pub fn with_cache_path(path: PathBuf) -> Self {
        Self {
            cache_path_override: Some(path),
        }
    }

    fn cache_path(&self) -> Result<PathBuf, ToolError> {
        match &self.cache_path_override {
            Some(path) => Ok(path.clone()),
            None => dirs::home_dir()
                .ok_or_else(|| ToolError::execution_failed("Cannot determine home directory"))
                .map(|h| h.join(".codewhale").join("mcp-index.json")),
        }
    }
}

const REGISTRY_API: &str = "https://registry.modelcontextprotocol.io/v0.1/servers";
const PER_PAGE: usize = 100;
const REQUEST_TIMEOUT_SECS: u64 = 30;
/// Inter-page delay: the upstream stalls under request bursts.
const PAGE_PACING_MS: u64 = 500;
/// Freshness window: within it `registry_sync` serves the cache with zero
/// network requests; past it a background refresh starts.
const INCREMENTAL_INTERVAL_SECS: i64 = 24 * 60 * 60;
/// Age at which the background refresh falls back to a full pagination,
/// reconciling servers that vanished from the listing entirely.
const FULL_RESYNC_INTERVAL_SECS: i64 = 30 * 24 * 60 * 60;
/// Identifies the client (RFC 9110); matches the crate-wide convention in
/// `web/fetch.rs`. HTTP hygiene, not a fix for upstream stalls.
const USER_AGENT: &str = concat!(
    "Mozilla/5.0 (compatible; codewhale/",
    env!("CARGO_PKG_VERSION"),
    "; +https://github.com/codewhale-hq/CodeWhale)"
);
/// Bounded retries for one sync. The on-disk cache only changes at the
/// final atomic replace, so a failed fetch (HTTP/parse error) never
/// mutates state and retrying is side-effect free.
const MAX_SYNC_ATTEMPTS: usize = 3;
/// Connect budget for Registry-launched servers. The 10s global default is
/// meant for pre-installed servers; Registry packages are typically fetched
/// on first launch via npx/uvx, which routinely exceeds it.
const REGISTRY_CONNECT_TIMEOUT_SECS: u64 = 60;

fn cache_path() -> Result<PathBuf, ToolError> {
    dirs::home_dir()
        .ok_or_else(|| ToolError::execution_failed("Cannot determine home directory"))
        .map(|h| h.join(".codewhale").join("mcp-index.json"))
}

fn read_cache(path: &Path) -> Option<McpRegistryIndex> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// True when the snapshot is fresh enough to serve without a network
/// round-trip. Version mismatch or missing timestamp count as stale.
fn cache_is_fresh(cache: &McpRegistryIndex, now: DateTime<Utc>) -> bool {
    if cache.version != MCP_REGISTRY_CACHE_VERSION {
        return false;
    }
    cache.synced_at.is_some_and(|synced| {
        now.signed_duration_since(synced).num_seconds() < INCREMENTAL_INTERVAL_SECS
    })
}

/// Convert one fetched listing into launchable cache entries. Servers the
/// upstream marks `deleted`/`deprecated` are dropped (the aggregator guide
/// recommends removing `deleted` entries — moderation takedowns — from
/// downstream indexes), as is anything the structured launcher cannot run.
/// The cache is a full snapshot every sync, so this filtering is the whole
/// story: retired servers simply never enter the fresh index.
/// Status lives in registry-managed `_meta`
/// (`ServerResponse._meta["io.modelcontextprotocol.registry/official"]
/// .status`, enum `active | deprecated | deleted`).
/// <https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/api/openapi.yaml>
fn viable_entries(entries: Vec<RegistryServerEntry>) -> Vec<McpRegistryServerEntry> {
    entries
        .into_iter()
        .filter(|entry| {
            !matches!(
                entry.lifecycle_status(),
                Some("deleted") | Some("deprecated")
            )
        })
        .filter_map(|entry| server_to_entry(entry.server))
        .collect()
}

/// Render the run command template. Positional `packageArguments` with a
/// fixed literal (or defaulted) value are part of the invocation itself —
/// e.g. the `mcp` subcommand in `npx -y agentic-mermaid@0.1.2 mcp` — so
/// they are folded into the command right after the package spec.
/// `<ARGS>` is the splice point for everything user-supplied: positional
/// placeholders plus each `required_args` entry, rendered as named or
/// positional arguments by the structured Registry launcher.
fn build_run_command(
    runtime_hint: &str,
    identifier: &str,
    version: &str,
    runtime_arguments: &[String],
    package_arguments: &[RegistryArg],
) -> String {
    let runtime = runtime_hint;
    let mut normalized_runtime_arguments = runtime_arguments.to_vec();
    if runtime_hint == "npx"
        && !normalized_runtime_arguments
            .iter()
            .any(|argument| matches!(argument.as_str(), "-y" | "--yes"))
    {
        normalized_runtime_arguments.insert(0, "-y".to_string());
    }
    let mid = normalized_runtime_arguments
        .iter()
        .map(|argument| shell_words::quote(argument))
        .collect::<Vec<_>>()
        .join(" ");
    let mid_with_space = if mid.is_empty() {
        String::new()
    } else {
        format!("{mid} ")
    };
    let (sep, tail) = match runtime_hint {
        "npx" => ("@", version.to_string()),
        "uvx" => ("==", version.to_string()),
        _ => return String::new(),
    };
    // Upstream positional packageArguments ship without a `name`; named
    // args always carry one. A nameless arg with a literal `value` (or a
    // `default`) is a fixed token of the invocation, not user input —
    // dropping it renders a command that cannot start the server (the
    // agentic-mermaid `mcp` subcommand bug).
    let fixed: Vec<String> = package_arguments
        .iter()
        .filter(|a| !a.is_required)
        .filter(|a| a.name.is_none())
        .filter_map(|a| a.value.as_deref().or(a.default.as_deref()))
        .map(|value| shell_words::quote(value).into_owned())
        .collect();
    let fixed_str = if fixed.is_empty() {
        String::new()
    } else {
        format!(" {}", fixed.join(" "))
    };
    let package_spec = format!("{identifier}{sep}{tail}");
    let package = shell_words::quote(&package_spec);
    format!("{runtime} {mid_with_space}{package}{fixed_str} <ARGS>")
}

fn build_launch_spec(
    runtime_hint: &str,
    identifier: &str,
    version: &str,
    pkg: &RegistryPackage,
) -> McpLaunchSpec {
    McpLaunchSpec {
        run_command: build_run_command(
            runtime_hint,
            identifier,
            version,
            &pkg.runtime_arguments,
            &pkg.package_arguments,
        ),
        required_args: pkg
            .package_arguments
            .iter()
            .filter(|a| a.is_required)
            .enumerate()
            .map(|(index, a)| McpRegistryArgEntry {
                // Positional args omit `name` upstream; fall back to the
                // value so the cache still carries something the cmd
                // builder can render.
                name: a
                    .name
                    .clone()
                    .or_else(|| a.value.clone())
                    .unwrap_or_else(|| format!("arg_{}", index + 1)),
                kind: a.kind.clone(),
                description: a.description.clone(),
                default: a.default.clone(),
            })
            .collect(),
    }
}

fn server_to_entry(server: RegistryServer) -> Option<McpRegistryServerEntry> {
    // We only need the FIRST viable stdio package per server for
    // launch metadata; everything beyond it would just duplicate
    // info. Filter to stdio, resolve runtime_hint + version, and stop
    // at the first hit.
    let first_pkg = server
        .packages
        .unwrap_or_default()
        .into_iter()
        .filter(|p| p.transport.as_deref() == Some("stdio"))
        .filter(|p| !p.declares_environment_variables())
        // The automatic launcher currently has deterministic install/run
        // semantics for package-manager-backed npm and PyPI entries only.
        .filter(|p| matches!(p.registry_type.as_str(), "npm" | "pypi"))
        .find_map(|p| {
            let expected_hint = default_runtime_hint(&p.registry_type)?;
            let hint = p
                .runtime_hint
                .clone()
                .unwrap_or_else(|| expected_hint.to_string());
            if hint != expected_hint {
                return None;
            }
            let version = p.version.clone()?;
            Some((p, hint, version))
        });

    let (pkg, hint, version) = first_pkg?;

    Some(McpRegistryServerEntry {
        name: server.name,
        description: server.description,
        launch: build_launch_spec(&hint, &pkg.identifier, &version, &pkg),
    })
}

/// Prompt attached to every `registry_sync` result.
///
/// A scored list is not a verdict. This used to read "you must call
/// start_registry_mcp_server ... before using shell commands, local programs,
/// custom code, or a manual implementation", which turned any near-miss row
/// into an obligation: a turn that only had to write an HTML file and read a
/// fixture matched a browser-automation server and spent itself on starting
/// that server. The result now describes the list and leaves the choice with
/// the model, while keeping the one instruction the host actually depends on —
/// start an entry through `start_registry_mcp_server`, never by running its
/// package command through the shell.
const REGISTRY_FIRST_PROMPT: &str = concat!(
    "These are the top scored matches for your query from the local Registry ",
    "snapshot; the full catalog stays on the host. A match is only worth ",
    "starting when it covers a capability you do not already have — if an ",
    "available tool, a project script, or a few lines of local code already ",
    "does the job, use that and ignore these results. To start one, call ",
    "start_registry_mcp_server with its exact name; never install or run its ",
    "package command through the shell. If nothing here covers the missing ",
    "capability, refine the query once, then continue with local tools.",
);

/// Host-side cap on model-visible Registry matches. The complete catalog
/// stays on disk; only this many matched entries ever reach the model.
const MAX_REGISTRY_MATCHES: usize = 8;

#[derive(Serialize)]
struct RegistryCatalogResult {
    instruction: &'static str,
    /// Total entries in the on-disk catalog (reported, never shipped).
    total: usize,
    query: String,
    servers: Vec<DigestEntry>,
}

fn catalog_from_cache(cache: &McpRegistryIndex, query: &str) -> RegistryCatalogResult {
    let servers = search_registry_entries(&cache.servers, query, MAX_REGISTRY_MATCHES);
    RegistryCatalogResult {
        instruction: REGISTRY_FIRST_PROMPT,
        total: cache.servers.len(),
        query: query.to_string(),
        servers,
    }
}

/// Deterministic host-side scoring: name hits outrank description hits,
/// exact name match outranks substring, ties break alphabetically. The model
/// never sees the un-matched remainder of the catalog.
fn search_registry_entries(
    entries: &[McpRegistryServerEntry],
    query: &str,
    limit: usize,
) -> Vec<DigestEntry> {
    let terms = query
        .split_whitespace()
        .map(str::to_ascii_lowercase)
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    let mut scored = entries
        .iter()
        .map(|server| {
            let name = server.name.to_ascii_lowercase();
            let description = server.description.to_ascii_lowercase();
            let mut score = 0u32;
            for term in &terms {
                if name == *term {
                    score = score.saturating_add(100);
                } else if name.contains(term) {
                    score = score.saturating_add(50);
                }
                if description.contains(term) {
                    score = score.saturating_add(10);
                }
            }
            (score, server)
        })
        .filter(|(score, _)| *score > 0)
        .collect::<Vec<_>>();
    scored.sort_by(|(a_score, a), (b_score, b)| {
        b_score.cmp(a_score).then_with(|| a.name.cmp(&b.name))
    });
    scored
        .into_iter()
        .take(limit)
        .map(|(_, server)| DigestEntry {
            name: server.name.clone(),
            description: server.description.clone(),
            required_args: server.launch.required_args.clone(),
        })
        .collect()
}

/// Always fast: serve the local snapshot and start a background download
/// only when it is missing or stale.
async fn load_registry_catalog(
    path: &Path,
    query: &str,
) -> Result<RegistryCatalogResult, ToolError> {
    let existing = read_cache(path);
    let fresh = existing
        .as_ref()
        .is_some_and(|cache| cache_is_fresh(cache, Utc::now()));
    if !fresh {
        spawn_background_sync(path);
    }
    Ok(catalog_for_snapshot(existing, query))
}

/// Decide what `registry_sync` returns: the scored top matches for the
/// query from the cached entries (they pin their own package versions, so
/// snapshot age is irrelevant), or an empty match set when no snapshot
/// exists yet.
fn catalog_for_snapshot(existing: Option<McpRegistryIndex>, query: &str) -> RegistryCatalogResult {
    match existing {
        Some(cache) => catalog_from_cache(&cache, query),
        // No snapshot yet: an empty match set has no candidates to act on;
        // a background sync has already been queued.
        None => RegistryCatalogResult {
            instruction: "",
            total: 0,
            query: query.to_string(),
            servers: Vec::new(),
        },
    }
}

/// Process-wide lock so at most one background sync runs at a time;
/// released when the sync settles, so the next call can retry.
fn try_acquire_sync_permit() -> Option<MutexGuard<'static, ()>> {
    static SYNC_GUARD: OnceLock<AsyncMutex<()>> = OnceLock::new();
    SYNC_GUARD
        .get_or_init(|| AsyncMutex::new(()))
        .try_lock()
        .ok()
}

/// Start the download in the background. Returns false when one is
/// already running.
fn spawn_background_sync(path: &Path) -> bool {
    let Some(permit) = try_acquire_sync_permit() else {
        return false;
    };
    let path = path.to_path_buf();
    tokio::spawn(async move {
        if let Err(error) = sync_once(&path).await {
            tracing::warn!("background Registry sync failed: {error}");
        }
        drop(permit);
    });
    true
}

/// Paginate the listing; with `updated_since`, only servers updated after
/// it. The filter MUST be repeated on every page — the cursor alone does
/// not carry it (verified against the live API). Never writes anything.
async fn fetch_registry_entries(
    client: &reqwest::Client,
    updated_since: Option<DateTime<Utc>>,
) -> Result<Vec<RegistryServerEntry>, ToolError> {
    let mut all_entries: Vec<RegistryServerEntry> = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut url = format!("{REGISTRY_API}?version=latest&limit={PER_PAGE}");
        if let Some(since) = updated_since {
            url.push_str(&format!(
                "&updated_since={}",
                urlencoding::encode(&since.to_rfc3339())
            ));
        }
        if let Some(ref c) = cursor {
            url.push_str(&format!("&cursor={}", urlencoding::encode(c)));
        }
        let resp = client
            .get(&url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| ToolError::execution_failed(format!("Registry API: {e}")))?;
        let text = resp
            .text()
            .await
            .map_err(|e| ToolError::execution_failed(format!("Registry body: {e}")))?;
        let body: RegistryResponse = serde_json::from_str(&text)
            .map_err(|e| ToolError::execution_failed(format!("Registry JSON parse: {e}")))?;
        all_entries.extend(body.servers);
        cursor = body.metadata.and_then(|m| m.next_cursor);
        if cursor.is_none() {
            break;
        }
        // Pace the next request so a burst of page fetches does not stall
        // the upstream (and, with many clients, each other).
        tokio::time::sleep(std::time::Duration::from_millis(PAGE_PACING_MS)).await;
    }
    Ok(all_entries)
}

/// Refresh strategy: full listing, or `updated_since` delta from the
/// snapshot's last sync (full when the snapshot is missing, legacy, or
/// older than `FULL_RESYNC_INTERVAL_SECS`).
#[derive(Debug, PartialEq)]
enum SyncStrategy {
    Full,
    Incremental { since: DateTime<Utc> },
}

fn sync_strategy(cache: Option<&McpRegistryIndex>, now: DateTime<Utc>) -> SyncStrategy {
    match cache {
        None => SyncStrategy::Full,
        Some(cache) if cache.version != MCP_REGISTRY_CACHE_VERSION => SyncStrategy::Full,
        Some(cache) => match cache.synced_at {
            None => SyncStrategy::Full,
            Some(synced)
                if now.signed_duration_since(synced).num_seconds() >= FULL_RESYNC_INTERVAL_SECS =>
            {
                SyncStrategy::Full
            }
            Some(synced) => SyncStrategy::Incremental { since: synced },
        },
    }
}

/// Merge an incremental listing into the snapshot (in memory). Delta
/// entries replace by name; retired or no-longer-launchable servers are
/// dropped; everything else keeps its cached copy.
fn merge_incremental_entries(
    base: &[McpRegistryServerEntry],
    entries: Vec<RegistryServerEntry>,
) -> Vec<McpRegistryServerEntry> {
    let mut merged: HashMap<String, McpRegistryServerEntry> = base
        .iter()
        .map(|entry| (entry.name.clone(), entry.clone()))
        .collect();
    for entry in entries {
        let name = entry.server.name.clone();
        if matches!(
            entry.lifecycle_status(),
            Some("deleted") | Some("deprecated")
        ) {
            merged.remove(&name);
            continue;
        }
        match server_to_entry(entry.server) {
            Some(updated) => {
                merged.insert(name, updated);
            }
            None => {
                merged.remove(&name);
            }
        }
    }
    merged.into_values().collect()
}

/// Fetch the index (full or incremental), assemble the next snapshot in
/// memory, and atomically replace the cache file. Retried on failure;
/// the old snapshot survives any failed sync.
async fn sync_once(path: &Path) -> Result<(), ToolError> {
    let client = codewhale_release::tls::reqwest_client_builder()
        .user_agent(USER_AGENT)
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .build()
        .map_err(|e| ToolError::execution_failed(format!("HTTP client: {e}")))?;

    let now = Utc::now();
    let cached = read_cache(path);
    let strategy = sync_strategy(cached.as_ref(), now);
    let updated_since = match strategy {
        SyncStrategy::Full => None,
        SyncStrategy::Incremental { since } => Some(since),
    };

    let mut last_error: Option<ToolError> = None;
    let mut servers: Option<Vec<McpRegistryServerEntry>> = None;
    for _attempt in 0..MAX_SYNC_ATTEMPTS {
        match fetch_registry_entries(&client, updated_since).await {
            Ok(entries) => {
                servers = Some(match strategy {
                    SyncStrategy::Full => viable_entries(entries),
                    SyncStrategy::Incremental { .. } => merge_incremental_entries(
                        &cached.expect("incremental needs a cache").servers,
                        entries,
                    ),
                });
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let servers = servers.ok_or_else(|| {
        last_error.unwrap_or_else(|| ToolError::execution_failed("Registry sync failed"))
    })?;
    let index = McpRegistryIndex {
        version: MCP_REGISTRY_CACHE_VERSION,
        count: servers.len(),
        servers,
        synced_at: Some(now),
    };
    // Blocking filesystem work (including `write_atomic`'s publish-retry)
    // runs on the blocking pool — tool handlers execute on the Tokio
    // runtime (blocking-call convention, #6149).
    let json_str = serde_json::to_string_pretty(&index)
        .map_err(|e| ToolError::execution_failed(format!("Serialize: {e}")))?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::execution_failed(format!("Create cache dir: {e}")))?;
        }
        write_atomic(&path, json_str.as_bytes())
            .map_err(|e| ToolError::execution_failed(format!("Write cache: {e}")))
    })
    .await
    .map_err(|e| ToolError::execution_failed(format!("Write cache task: {e}")))??;
    Ok(())
}

#[async_trait::async_trait]
impl ToolSpec for McpSyncRegistry {
    fn name(&self) -> &str {
        "registry_sync"
    }

    fn description(&self) -> &str {
        "Search installable local MCP servers for a capability this session \
         does not already have, and return at most eight scored matches; the \
         full Registry index stays host-side. Describe the missing capability \
         in the query. The index contains only stdio packages that declare no \
         environment variables or API keys. Use this when an available tool, a \
         project script, or ordinary local code cannot do the job — not before \
         ordinary work. To use a match, call start_registry_mcp_server with its \
         exact name; do not run its package command through the shell."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Required. The specialized capability to \
                     search for, e.g. 'convert PDF to markdown' or 'postgres \
                     database access'. Matched server names and descriptions \
                     are scored host-side; at most eight matches return."
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Auto
    }

    fn supports_parallel(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .ok_or_else(|| {
                ToolError::invalid_input(
                    "registry_sync requires a non-empty 'query' describing the \
                     specialized capability to search for.",
                )
            })?;
        let path = self.cache_path()?;
        let result = load_registry_catalog(&path, query).await?;
        let json = serde_json::to_string(&result)
            .map_err(|e| ToolError::execution_failed(format!("Serialize: {e}")))?;
        Ok(ToolResult::success(json))
    }
}

/// Start one zero-environment stdio server selected from the Registry cache.
/// The model supplies a Registry identity and structured CLI values; the host
/// owns command construction, so no arbitrary shell command or environment
/// channel is exposed by the discovery flow.
pub struct StartRegistryMcpServer {
    pool: Arc<AsyncMutex<McpPool>>,
}

impl StartRegistryMcpServer {
    pub fn new(pool: Arc<AsyncMutex<McpPool>>) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ToolSpec for StartRegistryMcpServer {
    fn name(&self) -> &str {
        "start_registry_mcp_server"
    }

    fn description(&self) -> &str {
        "Install and start a local stdio MCP server previously returned by \
         registry_sync. Only Registry packages that declare no environment \
         variables are eligible. Pass the exact registry_name and, when the \
         discovery result lists required_args, provide their values in the \
         structured arguments object. The connected server's complete tool \
         schemas become callable in the same turn."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "registry_name": {
                    "type": "string",
                    "description": "Exact server name returned by registry_sync"
                },
                "arguments": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Values keyed by required_args[].name; omit when none are required"
                }
            },
            "required": ["registry_name"]
        })
    }

    fn capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Network, ToolCapability::ExecutesCode]
    }

    fn approval_requirement(&self) -> ApprovalRequirement {
        ApprovalRequirement::Required
    }

    async fn execute(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult, ToolError> {
        let registry_name = input
            .get("registry_name")
            .and_then(Value::as_str)
            .ok_or_else(|| ToolError::invalid_input("missing required field: registry_name"))?;
        let supplied: HashMap<String, String> = match input.get("arguments") {
            Some(value) => serde_json::from_value(value.clone())
                .map_err(|error| ToolError::invalid_input(format!("invalid arguments: {error}")))?,
            None => HashMap::new(),
        };

        let path = cache_path()?;
        let cache = read_cache(&path).ok_or_else(|| {
            ToolError::execution_failed(format!(
                "no current Registry cache at {}; run registry_sync first",
                path.display()
            ))
        })?;
        if cache.version != MCP_REGISTRY_CACHE_VERSION {
            return Err(ToolError::execution_failed(format!(
                "cache at {} is from an older schema version; run registry_sync first",
                path.display()
            )));
        }
        let entry = cache
            .servers
            .iter()
            .find(|server| server.name == registry_name)
            .ok_or_else(|| ToolError::invalid_input("registry_name is not present in the cache"))?;

        let expected: HashSet<&str> = entry
            .launch
            .required_args
            .iter()
            .map(|arg| arg.name.as_str())
            .collect();
        if let Some(unknown) = supplied
            .keys()
            .find(|name| !expected.contains(name.as_str()))
        {
            return Err(ToolError::invalid_input(format!(
                "unknown argument '{unknown}' for {registry_name}"
            )));
        }

        let mut rendered_args = Vec::new();
        for argument in &entry.launch.required_args {
            let value = supplied
                .get(&argument.name)
                .cloned()
                .or_else(|| argument.default.clone())
                .ok_or_else(|| {
                    ToolError::invalid_input(format!(
                        "missing required argument '{}' for {registry_name}",
                        argument.name
                    ))
                })?;
            if matches!(argument.kind.as_deref(), Some("named")) && !argument.name.is_empty() {
                rendered_args.push(shell_words::quote(&argument.name).into_owned());
            }
            rendered_args.push(shell_words::quote(&value).into_owned());
        }

        let command = entry
            .launch
            .run_command
            .replace("<ARGS>", &rendered_args.join(" "));
        let delegated = json!({
            "server": command.trim(),
            "name": registry_name,
            // Registry packages cold-start through npx/uvx downloads; the
            // 10s default connect budget is routinely exceeded on first
            // launch. This override is host-supplied only — it is not part
            // of the model-facing schema of either tool.
            "connect_timeout": REGISTRY_CONNECT_TIMEOUT_SECS,
        });
        crate::tools::runtime_mcp::StartRuntimeMcpServer::new(Arc::clone(&self.pool))
            .execute(delegated, ctx)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_no_packages_filtered() {
        let server = RegistryServer {
            name: "test".into(),
            description: "desc".into(),
            packages: None,
        };
        assert!(server_to_entry(server).is_none());
    }

    #[test]
    fn server_remote_only_filtered() {
        let server = RegistryServer {
            name: "test".into(),
            description: "desc".into(),
            packages: Some(vec![RegistryPackage {
                registry_type: "npm".into(),
                identifier: "@test/pkg".into(),
                version: Some("1.0.0".into()),
                runtime_hint: Some("npx".into()),
                transport: Some("streamable-http".into()),
                package_arguments: vec![],
                runtime_arguments: vec![],
                environment_variables: Value::Null,
            }]),
        };
        assert!(server_to_entry(server).is_none());
    }

    #[test]
    fn server_stdio_kept() {
        let server = RegistryServer {
            name: "test".into(),
            description: "desc".into(),
            packages: Some(vec![RegistryPackage {
                registry_type: "npm".into(),
                identifier: "@test/pkg".into(),
                version: Some("1.0.0".into()),
                runtime_hint: Some("npx".into()),
                transport: Some("stdio".into()),
                package_arguments: vec![],
                runtime_arguments: vec!["-y".into()],
                environment_variables: Value::Null,
            }]),
        };
        let entry = server_to_entry(server).unwrap();
        assert_eq!(entry.launch.run_command, "npx -y @test/pkg@1.0.0 <ARGS>");
    }

    #[test]
    fn server_declaring_environment_variables_is_filtered() {
        let server = RegistryServer {
            name: "needs-secret".into(),
            description: "requires an API key".into(),
            packages: Some(vec![RegistryPackage {
                registry_type: "npm".into(),
                identifier: "@test/secret-server".into(),
                version: Some("1.0.0".into()),
                runtime_hint: Some("npx".into()),
                transport: Some("stdio".into()),
                package_arguments: vec![],
                runtime_arguments: vec!["-y".into()],
                environment_variables: json!([{ "name": "API_KEY", "isRequired": true }]),
            }]),
        };
        assert!(server_to_entry(server).is_none());
    }

    #[test]
    fn fixed_positional_package_arguments_render_into_run_command() {
        // Regression for the agentic-mermaid launch failure: upstream
        // declares `packageArguments: [{"value": "mcp", "type":
        // "positional"}]` (no `isRequired`), and dropping that token
        // produced `npx -y agentic-mermaid@0.1.2 <ARGS>` — which starts
        // the package's default entrypoint, not the MCP server.
        let server = RegistryServer {
            name: "io.github.adewale/agentic-mermaid".into(),
            description: "Render Mermaid diagrams through MCP.".into(),
            packages: Some(vec![RegistryPackage {
                registry_type: "npm".into(),
                identifier: "agentic-mermaid".into(),
                version: Some("0.1.2".into()),
                runtime_hint: Some("npx".into()),
                transport: Some("stdio".into()),
                package_arguments: vec![RegistryArg {
                    name: None,
                    description: None,
                    is_required: false,
                    kind: Some("positional".into()),
                    value: Some("mcp".into()),
                    default: None,
                }],
                runtime_arguments: vec!["-y".into()],
                environment_variables: Value::Null,
            }]),
        };
        let entry = server_to_entry(server).unwrap();
        assert_eq!(
            entry.launch.run_command,
            "npx -y agentic-mermaid@0.1.2 mcp <ARGS>"
        );
        assert!(entry.launch.required_args.is_empty());
    }

    #[test]
    fn placeholder_positional_package_argument_stays_in_required_args() {
        // The flip side: a positional arg with neither `value` nor
        // `default` is user input (e.g. an allowed directory), not a
        // fixed token — it must NOT leak into the rendered command.
        let server = RegistryServer {
            name: "test".into(),
            description: "desc".into(),
            packages: Some(vec![RegistryPackage {
                registry_type: "npm".into(),
                identifier: "@test/fs".into(),
                version: Some("1.0.0".into()),
                runtime_hint: Some("npx".into()),
                transport: Some("stdio".into()),
                package_arguments: vec![RegistryArg {
                    name: None,
                    description: Some("Directory to expose".into()),
                    is_required: true,
                    kind: Some("positional".into()),
                    value: None,
                    default: None,
                }],
                runtime_arguments: vec!["-y".into()],
                environment_variables: Value::Null,
            }]),
        };
        let entry = server_to_entry(server).unwrap();
        assert_eq!(entry.launch.run_command, "npx -y @test/fs@1.0.0 <ARGS>");
        assert_eq!(entry.launch.required_args.len(), 1);
    }

    #[test]
    fn fixed_argument_with_spaces_preserves_one_process_argument() {
        let command = build_run_command(
            "npx",
            "@test/fs",
            "1.0.0",
            &[],
            &[RegistryArg {
                name: None,
                description: None,
                is_required: false,
                kind: Some("positional".into()),
                value: Some("/tmp/a folder".into()),
                default: None,
            }],
        );
        let parsed = shell_words::split(command.replace("<ARGS>", "").trim()).unwrap();
        assert_eq!(parsed.last().map(String::as_str), Some("/tmp/a folder"));
    }

    #[test]
    fn server_without_explicit_stdio_transport_is_filtered() {
        let server = RegistryServer {
            name: "test".into(),
            description: "desc".into(),
            packages: Some(vec![RegistryPackage {
                registry_type: "npm".into(),
                identifier: "@test/pkg".into(),
                version: Some("1.0.0".into()),
                runtime_hint: Some("npx".into()),
                transport: None,
                package_arguments: vec![],
                runtime_arguments: vec![],
                environment_variables: Value::Null,
            }]),
        };
        assert!(server_to_entry(server).is_none());
    }

    #[test]
    fn unsupported_registry_runtime_is_not_advertised() {
        let server = RegistryServer {
            name: "container-only".into(),
            description: "OCI stdio server".into(),
            packages: Some(vec![RegistryPackage {
                registry_type: "oci".into(),
                identifier: "docker.io/example/server:1.0.0".into(),
                version: None,
                runtime_hint: Some("docker".into()),
                transport: Some("stdio".into()),
                package_arguments: vec![],
                runtime_arguments: vec![],
                environment_variables: Value::Null,
            }]),
        };
        assert!(server_to_entry(server).is_none());
    }

    #[test]
    fn registry_type_and_runtime_must_match() {
        let server = RegistryServer {
            name: "mismatched".into(),
            description: "invalid npm runner".into(),
            packages: Some(vec![RegistryPackage {
                registry_type: "npm".into(),
                identifier: "example".into(),
                version: Some("1.0.0".into()),
                runtime_hint: Some("uvx".into()),
                transport: Some("stdio".into()),
                package_arguments: vec![],
                runtime_arguments: vec![],
                environment_variables: Value::Null,
            }]),
        };
        assert!(server_to_entry(server).is_none());
    }

    /// End-to-end smoke test: cold-start `McpSyncRegistry::execute()`
    /// against the live Registry, wait for the background download to
    /// land, then verify the cache file and the next payload.
    ///
    /// Ignored: needs network + minutes of wall clock (page pacing, flaky
    /// upstream). Run manually with:
    ///   cargo test -p codewhale-tui --bin codewhale-tui --locked \
    ///     execute_writes_cache_file_and_returns_summary -- --ignored --nocapture
    ///
    /// The cache path is injected into a tempdir so the real cache is
    /// untouched (works on every platform).
    #[tokio::test]
    #[ignore = "requires network access to the public MCP Registry; \
                run with `cargo test -- --ignored`"]
    async fn execute_writes_cache_file_and_returns_summary() {
        use crate::tools::spec::ToolContext;

        let tmp = tempfile::tempdir().expect("tempdir");
        // Persist the tempdir so we can inspect the cache file after the
        // test returns. `TempDir::keep` (newer API) disables Drop's
        // cleanup so the directory leaks — acceptable for a manual-run
        // integration smoke test that intentionally outlives its scope.
        let tmp_path = tmp.keep();
        let cache_path = tmp_path.join(".codewhale").join("mcp-index.json");

        let ctx = ToolContext::new(tmp_path.clone());

        let input = json!({ "query": "filesystem file access" });

        let result = McpSyncRegistry::with_cache_path(cache_path.clone())
            .execute(input, &ctx)
            .await
            .expect("execute() should not error against the live Registry");

        assert!(
            result.success,
            "execute returned non-success: content={}",
            result.content
        );
        // Cold start: empty catalog returned immediately, download in the
        // background.
        let first_payload: serde_json::Value =
            serde_json::from_str(&result.content).expect("result content must parse");
        assert_eq!(first_payload["total"], 0, "no cache ⇒ empty catalog");

        // Poll for the background download to land, then assert on the
        // final snapshot (page pacing + upstream stalls take minutes).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        let cache = loop {
            if let Ok(raw) = std::fs::read_to_string(&cache_path)
                && let Ok(cache) = serde_json::from_str::<McpRegistryIndex>(&raw)
            {
                break cache;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "cache file did not appear at {:?} within 600s",
                cache_path
            );
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        };

        // The Registry has hundreds of stdio servers as of 2026; expect at
        // least 1 from a single page. If this fails the upstream either
        // removed all stdio entries or our filter is wrong.
        assert!(
            cache.count > 0,
            "expected at least 1 stdio server, got count={}",
            cache.count
        );
        assert_eq!(
            cache.servers.len(),
            cache.count,
            "cache.count must equal cache.servers.len()"
        );
        for entry in &cache.servers {
            assert!(!entry.name.is_empty(), "server entry has empty name");
            assert!(
                entry.launch.run_command.ends_with("<ARGS>"),
                "kept entry {} run_command should end with <ARGS>; got: {}",
                entry.name,
                entry.launch.run_command
            );
        }

        // After the background download lands, the next call serves the
        // fresh snapshot and its payload must match the cache file.
        let result = McpSyncRegistry::with_cache_path(cache_path.clone())
            .execute(json!({ "query": "filesystem file access" }), &ctx)
            .await
            .expect("execute() should succeed once the cache is fresh");
        let payload: serde_json::Value =
            serde_json::from_str(&result.content).expect("result content must parse");
        assert_eq!(payload["total"].as_u64(), Some(cache.count as u64));
        let matches = payload["servers"].as_array().map(Vec::len);
        assert!(
            matches.is_some_and(|len| len <= 8),
            "model-visible matches must stay bounded: {matches:?}"
        );
        assert!(
            matches.is_some_and(|len| len >= 1),
            "the filesystem fixture must match the filesystem query"
        );
    }

    fn make_test_cache() -> McpRegistryIndex {
        let server = McpRegistryServerEntry {
            name: "io.modelcontextprotocol/filesystem".into(),
            description: "Read/write local files with sandboxed paths".into(),
            launch: McpLaunchSpec {
                run_command: "npx -y @modelcontextprotocol/server-filesystem@1.0.0 <ARGS>".into(),
                required_args: vec![],
            },
        };
        McpRegistryIndex {
            version: MCP_REGISTRY_CACHE_VERSION,
            count: 1,
            servers: vec![server],
            synced_at: Some(Utc::now()),
        }
    }

    #[test]
    fn catalog_search_reports_total_and_returns_only_matches() {
        let cache = make_test_cache();
        let catalog = catalog_from_cache(&cache, "filesystem");
        assert_eq!(catalog.total, 1);
        assert_eq!(catalog.servers.len(), 1);
        assert_eq!(
            catalog.servers[0].name,
            "io.modelcontextprotocol/filesystem"
        );
        assert_eq!(
            catalog.servers[0].description,
            "Read/write local files with sandboxed paths"
        );
        // A query that does not match still reports the total, but returns
        // an empty match set — never the full catalog.
        let miss = catalog_from_cache(&cache, "database");
        assert_eq!(miss.total, 1);
        assert!(miss.servers.is_empty());
    }

    /// A scored row must not read as an obligation. The prompt this result
    /// carries used to say the model "must call start_registry_mcp_server ...
    /// before using shell commands, local programs, custom code, or a manual
    /// implementation", which is how a turn that only needed to write an HTML
    /// file and read a fixture ended up trying to start a browser server.
    #[test]
    fn result_prompt_does_not_oblige_starting_a_match() {
        let catalog = catalog_from_cache(&make_test_cache(), "filesystem");
        let instruction = catalog.instruction;

        assert!(!instruction.contains("must call"));
        assert!(!instruction.contains("custom code"));
        assert!(!instruction.contains("manual implementation"));

        // What the result is still responsible for saying: the matches are
        // only worth starting for a capability the session lacks, and a
        // package is started through the approved host path, never the shell.
        assert!(instruction.contains("a capability you do not already have"));
        assert!(instruction.contains("start_registry_mcp_server with its exact name"));
        assert!(instruction.contains("never install or run its package command through the shell"));
    }

    /// Parse one `ServerResponse` JSON object the way the upstream list
    /// endpoint ships it. Lifecycle status travels in registry-managed
    /// `_meta` (`io.modelcontextprotocol.registry/official`), as a
    /// SIBLING of `server` — not inside the server body — and this
    /// helper keeps the tests honest about that path.
    /// <https://github.com/modelcontextprotocol/registry/blob/main/docs/reference/api/openapi.yaml>
    fn parse_server_entry(server_json: Value, status: Option<&str>) -> RegistryServerEntry {
        let mut response = json!({ "server": server_json });
        if let Some(status) = status {
            response["_meta"] =
                json!({ "io.modelcontextprotocol.registry/official": { "status": status } });
        }
        serde_json::from_value(response).expect("ServerResponse must deserialize")
    }

    #[test]
    fn viable_entries_keeps_only_active_launchable_servers() {
        let launchable = |name: &str, status: Option<&str>| {
            parse_server_entry(
                json!({
                    "name": name,
                    "description": "d",
                    "packages": [{
                        "registryType": "npm",
                        "identifier": "@test/pkg",
                        "version": "1.0.0",
                        "runtimeHint": "npx",
                        "transport": "stdio",
                        "packageArguments": [],
                        "runtimeArguments": [],
                        "environmentVariables": null
                    }]
                }),
                status,
            )
        };
        let entries = vec![
            launchable("a/active", Some("active")),
            launchable("b/deprecated", Some("deprecated")),
            launchable("c/deleted", Some("deleted")),
            // No `_meta` at all: treated as active.
            launchable("d/no-meta", None),
            // Missing a launchable package: dropped by the launcher filter.
            parse_server_entry(json!({ "name": "e/no-pkg", "description": "d" }), None),
        ];

        let viable = viable_entries(entries);

        let names: Vec<&str> = viable.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["a/active", "d/no-meta"]);
    }

    /// Minimal cache entry — only `name`/`description`/launch matter to the
    /// catalog conversion tests.
    fn cache_entry(name: &str, description: &str) -> McpRegistryServerEntry {
        McpRegistryServerEntry {
            name: name.into(),
            description: description.into(),
            launch: McpLaunchSpec {
                run_command: "npx pkg@1.0.0 <ARGS>".into(),
                required_args: vec![],
            },
        }
    }

    #[test]
    fn catalog_search_caps_model_visible_matches() {
        let servers = (0..12)
            .map(|index| cache_entry(&format!("example/file-{index}"), "file server"))
            .collect::<Vec<_>>();
        let cache = McpRegistryIndex {
            version: MCP_REGISTRY_CACHE_VERSION,
            count: servers.len(),
            servers,
            synced_at: Some(Utc::now()),
        };
        let result = catalog_from_cache(&cache, "file");
        assert_eq!(result.total, 12);
        assert!(
            result.servers.len() <= MAX_REGISTRY_MATCHES,
            "model-visible matches stay bounded"
        );
    }

    #[test]
    fn cache_is_fresh_honors_ttl_version_and_missing_timestamp() {
        let now = Utc::now();
        let base = make_test_cache();

        // Within the window: fresh.
        let mut fresh = base.clone();
        fresh.synced_at = Some(now - chrono::Duration::minutes(30));
        assert!(cache_is_fresh(&fresh, now));

        // At/over the incremental window boundary: stale.
        let mut expired = base.clone();
        expired.synced_at = Some(now - chrono::Duration::seconds(INCREMENTAL_INTERVAL_SECS + 1));
        assert!(!cache_is_fresh(&expired, now));

        // Legacy cache without a timestamp: stale (triggers one resync).
        let mut legacy = base.clone();
        legacy.synced_at = None;
        assert!(!cache_is_fresh(&legacy, now));

        // Schema version mismatch: always stale.
        let mut old_schema = base.clone();
        old_schema.version = MCP_REGISTRY_CACHE_VERSION - 1;
        old_schema.synced_at = Some(now);
        assert!(!cache_is_fresh(&old_schema, now));

        // Clock skew: a future-stamped cache stays fresh.
        let mut future = base.clone();
        future.synced_at = Some(now + chrono::Duration::hours(2));
        assert!(cache_is_fresh(&future, now));
    }

    #[test]
    fn catalog_for_snapshot_returns_empty_catalog_when_no_cache() {
        let result = catalog_for_snapshot(None, "anything");
        assert_eq!(result.total, 0);
        assert!(result.servers.is_empty());
    }

    #[test]
    fn catalog_for_snapshot_serves_cached_entries_without_flags() {
        let result = catalog_for_snapshot(Some(make_test_cache()), "filesystem");
        assert_eq!(result.total, 1);
        assert_eq!(result.servers.len(), 1);
    }

    #[test]
    fn catalog_for_snapshot_serves_old_snapshot_as_is() {
        // Snapshot age is invisible to the model: every entry pins its own
        // package version, so even a month-old snapshot is served as-is.
        let mut cache = make_test_cache();
        cache.synced_at = Some(Utc::now() - chrono::Duration::days(31));
        let result = catalog_for_snapshot(Some(cache), "filesystem");
        assert_eq!(result.total, 1);
        assert_eq!(result.servers.len(), 1);
    }

    #[test]
    fn sync_strategy_decides_full_vs_incremental() {
        let now = Utc::now();

        // No cache at all: full.
        assert_eq!(sync_strategy(None, now), SyncStrategy::Full);

        // Legacy schema version: full.
        let mut old = make_test_cache();
        old.version = MCP_REGISTRY_CACHE_VERSION - 1;
        assert_eq!(sync_strategy(Some(&old), now), SyncStrategy::Full);

        // Missing timestamp: full.
        let mut no_ts = make_test_cache();
        no_ts.synced_at = None;
        assert_eq!(sync_strategy(Some(&no_ts), now), SyncStrategy::Full);

        // Older than the full-resync window: full (reconciliation — an
        // incremental delta cannot observe vanished servers).
        let mut ancient = make_test_cache();
        ancient.synced_at = Some(now - chrono::Duration::days(31));
        assert_eq!(sync_strategy(Some(&ancient), now), SyncStrategy::Full);

        // Past the incremental window but inside the full-resync window:
        // incremental from the last sync.
        let mut stale = make_test_cache();
        stale.synced_at = Some(now - chrono::Duration::days(2));
        assert_eq!(
            sync_strategy(Some(&stale), now),
            SyncStrategy::Incremental {
                since: stale.synced_at.expect("set above")
            }
        );
    }

    /// One launchable delta entry (active stdio npm package); `status`
    /// None means no `_meta` (treated as active).
    fn delta_entry(name: &str, status: Option<&str>) -> RegistryServerEntry {
        parse_server_entry(
            json!({
                "name": name,
                "description": "d",
                "packages": [{
                    "registryType": "npm",
                    "identifier": "@test/pkg",
                    "version": "2.0.0",
                    "runtimeHint": "npx",
                    "transport": "stdio",
                    "packageArguments": [],
                    "runtimeArguments": [],
                    "environmentVariables": null
                }]
            }),
            status,
        )
    }

    #[test]
    fn merge_incremental_entries_updates_inserts_and_removes() {
        let base = vec![
            cache_entry("a/unchanged", "unchanged"),
            cache_entry("b/updated", "old description"),
            cache_entry("c/deleted", "will be removed"),
            cache_entry("d/deprecated", "will be removed"),
            cache_entry("e/unlaunchable", "will be removed"),
        ];
        let entries = vec![
            // Replace the cached copy by name.
            delta_entry("b/updated", Some("active")),
            // Insert a brand-new server.
            delta_entry("f/new", Some("active")),
            // Retired servers are dropped from the snapshot.
            delta_entry("c/deleted", Some("deleted")),
            delta_entry("d/deprecated", Some("deprecated")),
            // Present but no longer launchable (npm package whose runner
            // no longer matches): the stale cached copy is dropped too.
            parse_server_entry(
                json!({
                    "name": "e/unlaunchable",
                    "description": "d",
                    "packages": [{
                        "registryType": "npm",
                        "identifier": "@test/pkg",
                        "version": "2.0.0",
                        "runtimeHint": "uvx",
                        "transport": "stdio",
                        "packageArguments": [],
                        "runtimeArguments": [],
                        "environmentVariables": null
                    }]
                }),
                Some("active"),
            ),
        ];

        let merged = merge_incremental_entries(&base, entries);
        let by_name: HashMap<&str, &McpRegistryServerEntry> = merged
            .iter()
            .map(|entry| (entry.name.as_str(), entry))
            .collect();

        assert_eq!(merged.len(), 3);
        assert!(
            by_name.contains_key("a/unchanged"),
            "entries absent from the delta must keep their cached copy"
        );
        assert!(
            by_name.contains_key("f/new"),
            "a new server in the delta must be inserted"
        );
        assert_eq!(
            by_name["b/updated"].description, "d",
            "an updated entry must replace the cached copy"
        );
        assert!(!by_name.contains_key("c/deleted"));
        assert!(!by_name.contains_key("d/deprecated"));
        assert!(
            !by_name.contains_key("e/unlaunchable"),
            "an entry that lost its launchable package must be dropped"
        );
    }

    /// The sync permit is exclusive while held (no duplicate background
    /// downloads) and reusable after release (failed syncs get retried).
    /// Direct acceptance: a 4,786-entry fixture must never expose more
    /// than eight model-visible matches, and the serialized payload stays
    /// bounded regardless of catalog size.
    #[test]
    fn huge_catalog_search_stays_bounded_at_eight_matches() {
        let entries = (0..4_786)
            .map(|index| McpRegistryServerEntry {
                name: format!("com.example/server-{index}"),
                description: format!("Specialized capability server number {index} for conversion"),
                launch: McpLaunchSpec {
                    run_command: "npx -y com.example/server <ARGS>".into(),
                    required_args: vec![],
                },
            })
            .collect::<Vec<_>>();

        let matches = search_registry_entries(&entries, "conversion server", MAX_REGISTRY_MATCHES);
        assert_eq!(matches.len(), MAX_REGISTRY_MATCHES, "cap at eight matches");

        let payload = serde_json::to_string(&matches).expect("serialize matches");
        assert!(
            payload.len() < 16_000,
            "bounded payload, got {} bytes",
            payload.len()
        );

        // A query that matches nothing returns an empty set — never the
        // whole catalog.
        let none = search_registry_entries(&entries, "nothing matches this", MAX_REGISTRY_MATCHES);
        assert!(none.is_empty());
    }

    #[test]
    fn search_ranks_exact_name_over_substring_and_ties_break_alphabetically() {
        let entries = [
            entry("b/convert", "converts documents"),
            entry("a/convert-pro", "converts documents better"),
            entry(
                "zzz-unrelated",
                "but mentions convert deep in a long description",
            ),
        ];
        let matches = search_registry_entries(&entries, "convert", MAX_REGISTRY_MATCHES);
        let names = matches.iter().map(|m| m.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, vec!["a/convert-pro", "b/convert", "zzz-unrelated"]);
    }

    #[test]
    fn empty_query_matches_nothing_instead_of_everything() {
        let entries = [entry("a/server", "does things")];
        assert!(search_registry_entries(&entries, "   ", MAX_REGISTRY_MATCHES).is_empty());
    }

    fn entry(name: &str, description: &str) -> McpRegistryServerEntry {
        McpRegistryServerEntry {
            name: name.into(),
            description: description.into(),
            launch: McpLaunchSpec {
                run_command: "npx -y pkg <ARGS>".into(),
                required_args: vec![],
            },
        }
    }

    #[tokio::test]
    async fn background_sync_permit_is_exclusive_until_released() {
        let first = try_acquire_sync_permit();
        assert!(first.is_some(), "first acquisition must succeed");
        assert!(
            try_acquire_sync_permit().is_none(),
            "second acquisition must fail while the first is held"
        );
        drop(first);
        let again = try_acquire_sync_permit();
        assert!(again.is_some(), "permit must be reusable after release");
        drop(again);
    }

    /// With a fresh snapshot on disk, `registry_sync` must serve it
    /// without touching the network: the fixture is a single synthetic
    /// server, so any live sync would return a different catalog and fail
    /// the assertion. The cache path is injected explicitly because
    /// `dirs::home_dir()` resolves the OS profile directory on Windows via
    /// SHGetKnownFolderPath — no environment variable can redirect it.
    #[tokio::test]
    async fn fresh_cache_serves_catalog_without_network() {
        use crate::tools::spec::ToolContext;

        let tmp = tempfile::tempdir().expect("tempdir");
        let cache_dir = tmp.path().join(".codewhale");
        std::fs::create_dir_all(&cache_dir).expect("create cache dir");
        let cache_file = cache_dir.join("mcp-index.json");
        let index = make_test_cache();
        std::fs::write(
            &cache_file,
            serde_json::to_string_pretty(&index).expect("serialize fixture"),
        )
        .expect("write fixture cache");

        let ctx = ToolContext::new(tmp.path().to_path_buf());
        let result = McpSyncRegistry::with_cache_path(cache_file)
            .execute(json!({ "query": "filesystem" }), &ctx)
            .await
            .expect("fresh cache must serve without network");

        let payload: serde_json::Value =
            serde_json::from_str(&result.content).expect("result content must parse");
        assert_eq!(payload["total"], 1, "fixture catalog is reported in full");
        assert_eq!(
            payload["servers"][0]["name"], "io.modelcontextprotocol/filesystem",
            "cache-first path must return the cached entry, not a live sync"
        );
    }

    /// Manual smoke run: cold-start against the live Registry, wait for
    /// the background download, then print the final payload to stdout.
    /// Run with:
    ///   cargo test -p codewhale-tui --bin codewhale-tui --locked \
    ///     execute_and_print_catalog_for_manual_inspection -- --ignored --nocapture
    #[tokio::test]
    #[ignore = "manual smoke run; prints the tool result to stdout \
                (requires network access to registry.modelcontextprotocol.io)"]
    // The module tree denies `print_stderr` (scroll-demon guard, #1085) so
    // TUI runtime code can never leak into ratatui's buffer. This test is
    // the deliberate exception: it only runs manually (`--ignored`) and its
    // entire purpose is printing the payload for operator inspection.
    #[allow(clippy::print_stderr)]
    async fn execute_and_print_catalog_for_manual_inspection() {
        use crate::tools::spec::ToolContext;

        let tmp = tempfile::tempdir().expect("tempdir");
        // Persist the tempdir past the test so the cache file survives and
        // can be inspected from the shell after the test returns.
        let tmp_path = tmp.keep();
        let cache_path = tmp_path.join(".codewhale").join("mcp-index.json");

        let ctx = ToolContext::new(tmp_path.clone());

        let input = json!({ "query": "filesystem file access" });

        let result = McpSyncRegistry::with_cache_path(cache_path.clone())
            .execute(input, &ctx)
            .await
            .expect("execute() should not error against the live Registry");

        // The cold-start call returns immediately (empty catalog +
        // background download). Wait for the download to land, then call
        // again and print the final payload the model would receive.
        eprintln!("cold-start payload (background download flagged):");
        eprintln!("{}", result.content);
        eprintln!("waiting for the background download to land...");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
        while !cache_path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "cache file did not appear at {:?} within 600s",
                cache_path
            );
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }

        let result = McpSyncRegistry::with_cache_path(cache_path.clone())
            .execute(json!({}), &ctx)
            .await
            .expect("execute() should succeed once the cache is fresh");

        eprintln!("\n=== registry_sync output ===");
        eprintln!("tool:               registry_sync");
        eprintln!(
            "status:             {}",
            if result.success { "ok" } else { "fail" }
        );
        eprintln!("cache_path:         {}", cache_path.display());
        eprintln!("cache_exists:       {}", cache_path.exists());
        if cache_path.exists() {
            match std::fs::metadata(&cache_path) {
                Ok(meta) => eprintln!("cache_size_bytes:   {}", meta.len()),
                Err(e) => eprintln!("cache_stat_error:   {e}"),
            }
        }
        eprintln!("--- catalog payload (what the model sees) ---");
        eprintln!("{}", result.content);
        eprintln!("=== end ===\n");
    }
}

//! Plugin bundle and marketplace management over the Runtime API.
//!
//! `GET /v1/apps/plugins` and `GET /v1/apps/plugins/{selector}` expose the
//! same registry the TUI reads, with the same honest state vocabulary
//! (`active`, `enabled-untrusted`, `unstaged`, …) and the same capability
//! inventory a terminal review shows. Mutations run through the exact
//! reviewed paths the TUI uses — `plugins::mutation::execute` for
//! install/update/uninstall (installs always land disabled and untrusted)
//! and the registry's hash-bound receipt flow for trust/enable/disable —
//! so a GUI client can never bypass a review the TUI would require.
//!
//! Marketplace endpoints share `plugins::marketplace::document` with the
//! `/plugin marketplace` command: local catalog documents only, tiers are
//! display-only, and installs route through the same reviewed installer.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::uri::Authority;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use codewhale_plugins::marketplace::document::{
    CatalogInstallResolution, load_catalog_document, resolve_candidate_install,
};
use codewhale_plugins::marketplace::store::MarketplaceStore;
use codewhale_plugins::mutation::{
    PluginMutationContext, PluginMutationOutcome, PluginMutationRequest,
};
use codewhale_plugins::types::{LoadedPlugin, PluginDiagnostic, PluginDiagnosticLevel};

use super::{ApiError, RuntimeApiState};

const PLUGIN_UI_MAX_BYTES: u64 = 8 * 1024 * 1024;
const PLUGIN_UI_SLOT: &str = "customize.plugins.detail";

// ---------------------------------------------------------------------------
// Response shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(super) struct PluginInventorySummary {
    pub(super) skills: usize,
    pub(super) mcp_servers: usize,
    pub(super) stdio_mcp_servers: usize,
    pub(super) remote_mcp_servers: usize,
    pub(super) commands: usize,
    pub(super) agents: usize,
    pub(super) hooks: usize,
    pub(super) lsp: usize,
    pub(super) native: usize,
    pub(super) filesystem_roots: Vec<String>,
    pub(super) network_hosts: Vec<String>,
    pub(super) lifecycle_mutation: bool,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginDiagnosticEntry {
    pub(super) level: &'static str,
    pub(super) code: String,
    pub(super) message: String,
    pub(super) path: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginSummaryEntry {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) display_name: Option<String>,
    pub(super) icon: Option<String>,
    pub(super) author: Option<String>,
    pub(super) homepage: Option<String>,
    pub(super) platforms: Vec<String>,
    pub(super) version: String,
    pub(super) description: Option<String>,
    pub(super) scope: &'static str,
    pub(super) origin: &'static str,
    pub(super) path: String,
    pub(super) state: &'static str,
    pub(super) enabled: bool,
    pub(super) trust_status: &'static str,
    pub(super) active: bool,
    pub(super) compatibility: &'static str,
    pub(super) inventory: PluginInventorySummary,
    pub(super) content_hash: String,
    pub(super) capability_hash: String,
    pub(super) state_generation: u64,
    pub(super) diagnostics: Vec<PluginDiagnosticEntry>,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginsResponse {
    pub(super) workspace: String,
    pub(super) plugins: Vec<PluginSummaryEntry>,
    pub(super) registry_diagnostics: Vec<PluginDiagnosticEntry>,
    pub(super) validation_clean: bool,
}

/// One reviewed-plugin MCP server in the trust-review payload. Environment
/// and header maps expose only key names; URLs expose only their network
/// authority. Command and argument text remain the bundle's declared launch
/// instructions for review, so bundles should pass credentials through env.
#[derive(Debug, Serialize)]
pub(super) struct PluginMcpServerReview {
    pub(super) name: String,
    pub(super) kind: &'static str,
    pub(super) command: Option<String>,
    pub(super) args: Vec<String>,
    pub(super) url: Option<String>,
    pub(super) env_keys: Vec<String>,
    pub(super) header_keys: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginSkillReview {
    pub(super) name: String,
    pub(super) description: String,
}

/// The capability review a human approves (or rejects) before trusting a
/// bundle. Structured so a GUI renders it without parsing prose.
#[derive(Debug, Serialize)]
pub(super) struct PluginReviewPayload {
    /// Confirmation token binding a trust call to this exact content and
    /// capability set (`POST .../trust {"token": ...}`).
    pub(super) token: String,
    pub(super) capabilities: Vec<&'static str>,
    pub(super) unsupported_capabilities: Vec<&'static str>,
    pub(super) filesystem_roots: Vec<String>,
    pub(super) network_hosts: Vec<String>,
    pub(super) lifecycle_mutation: bool,
    pub(super) mcp_servers: Vec<PluginMcpServerReview>,
    pub(super) skills: Vec<PluginSkillReview>,
    pub(super) commands: Vec<String>,
    pub(super) agents: Vec<String>,
    pub(super) hooks: Vec<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginDetailResponse {
    #[serde(flatten)]
    pub(super) summary: PluginSummaryEntry,
    pub(super) repository: Option<String>,
    pub(super) license: Option<String>,
    pub(super) keywords: Vec<String>,
    pub(super) staged: bool,
    pub(super) review: PluginReviewPayload,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginMutationResponse {
    pub(super) outcome: &'static str,
    pub(super) name: String,
    pub(super) path: Option<String>,
    pub(super) content_hash: Option<String>,
    pub(super) note: Option<&'static str>,
    /// Fresh post-mutation state of the affected bundle, when it still
    /// exists (uninstall removes it).
    pub(super) plugin: Option<PluginSummaryEntry>,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginActionResponse {
    pub(super) name: String,
    pub(super) action: &'static str,
    pub(super) state: &'static str,
    pub(super) note: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginUiSlotResponse {
    pub(super) slot: String,
    pub(super) base: String,
    pub(super) entry: String,
    pub(super) revision: String,
}

enum UiFileError {
    Missing,
    TooLarge,
    Invalid(String),
    Internal(String),
}

// ---------------------------------------------------------------------------
// Request shapes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(super) struct InstallPluginRequest {
    /// Install spec accepted by `PluginInstallSource::parse`: a local path
    /// (plain or `path:<dir>`), `github:owner/repo`, or an HTTPS tarball URL.
    pub(super) source: String,
    /// When present, the install is refused (and rolled back) unless the
    /// installed tree matches this reviewed content hash.
    #[serde(default)]
    pub(super) expected_content_hash: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct PluginUiQuery {
    pub(super) slot: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct DshPreviewRequest {
    /// DeepSeek Harness bundle package directory; relative paths resolve
    /// against the workspace.
    pub(super) path: String,
}

#[derive(Debug, Serialize)]
pub(super) struct DshPreviewResponse {
    /// Pass to `POST /v1/apps/plugins/install` as `source`, with
    /// `content_hash` as `expected_content_hash`, to install exactly this
    /// reviewed bundle.
    pub(super) install_source: String,
    pub(super) content_hash: String,
    pub(super) conversion: codewhale_plugins::install::dsh::DshConversion,
}

#[derive(Debug, Deserialize)]
pub(super) struct TrustPluginRequest {
    /// Review token from `GET /v1/apps/plugins/{selector}`. Required: trust
    /// is an explicit confirmation bound to both SHA-256 receipts.
    pub(super) token: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct AddMarketplaceRequest {
    pub(super) name: String,
    /// LOCAL catalog document path (kimi/claude/codex/codewhale format,
    /// auto-detected). Never fetched over the network.
    pub(super) path: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct InstallMarketplaceCandidateRequest {
    pub(super) candidate: String,
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn registry_for_state(state: &RuntimeApiState) -> Arc<codewhale_plugins::PluginRegistry> {
    state
        .plugin_discovery
        .registry_for_workspace(&state.workspace)
}

fn diagnostic_entry(diagnostic: &PluginDiagnostic) -> PluginDiagnosticEntry {
    PluginDiagnosticEntry {
        level: match diagnostic.level {
            PluginDiagnosticLevel::Warning => "warning",
            PluginDiagnosticLevel::Error => "error",
        },
        code: diagnostic.code.to_string(),
        message: diagnostic.message.clone(),
        path: diagnostic.path.as_ref().map(|p| p.display().to_string()),
    }
}

fn inventory_summary(plugin: &LoadedPlugin) -> PluginInventorySummary {
    let inventory = &plugin.inventory;
    PluginInventorySummary {
        skills: inventory.skills,
        mcp_servers: inventory.mcp_servers,
        stdio_mcp_servers: inventory.stdio_mcp_servers,
        remote_mcp_servers: inventory.remote_mcp_servers,
        commands: inventory.commands,
        agents: inventory.agents,
        hooks: inventory.hooks,
        lsp: inventory.lsp,
        native: inventory.native,
        filesystem_roots: inventory.filesystem_roots.clone(),
        network_hosts: inventory.network_hosts.clone(),
        lifecycle_mutation: inventory.lifecycle_mutation,
    }
}

fn plugin_summary(plugin: &LoadedPlugin) -> PluginSummaryEntry {
    PluginSummaryEntry {
        id: plugin.id.as_str().to_string(),
        name: plugin.name().to_string(),
        display_name: plugin.manifest.plugin.display_name.clone(),
        icon: plugin.manifest.plugin.icon.clone(),
        author: plugin.manifest.plugin.author.clone(),
        homepage: plugin.manifest.plugin.homepage.clone(),
        platforms: plugin
            .manifest
            .when
            .as_ref()
            .and_then(|when| when.os.clone())
            .unwrap_or_default(),
        version: plugin.manifest.plugin.version.clone(),
        description: plugin.manifest.plugin.description.clone(),
        scope: plugin.scope.as_str(),
        origin: plugin.origin.as_str(),
        path: plugin.canonical_root.display().to_string(),
        state: plugin.state_label(),
        enabled: plugin.enabled,
        trust_status: plugin.trust_status.as_str(),
        active: plugin.active(),
        compatibility: plugin.compatibility().as_str(),
        inventory: inventory_summary(plugin),
        content_hash: plugin.content_hash.clone(),
        capability_hash: plugin.capability_hash.clone(),
        state_generation: plugin.state_generation,
        diagnostics: plugin.diagnostics.iter().map(diagnostic_entry).collect(),
    }
}

fn mcp_server_review(
    name: &str,
    cfg: &codewhale_mcp::config::McpServerConfig,
) -> PluginMcpServerReview {
    let mut env_keys: Vec<String> = cfg.env.keys().cloned().collect();
    env_keys.sort();
    let mut header_keys: Vec<String> = cfg.headers.keys().cloned().collect();
    header_keys.sort();
    PluginMcpServerReview {
        name: name.to_string(),
        kind: if cfg.url.is_some() { "remote" } else { "stdio" },
        command: cfg.command.clone(),
        args: cfg.args.clone(),
        url: cfg
            .url
            .as_deref()
            .map(codewhale_config::structural_url_authority),
        env_keys,
        header_keys,
    }
}

fn file_stem(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn review_payload(plugin: &LoadedPlugin) -> PluginReviewPayload {
    let mut mcp_servers: Vec<_> = plugin
        .manifest
        .mcp_servers
        .as_ref()
        .map(|servers| {
            servers
                .iter()
                .map(|(name, cfg)| mcp_server_review(name, cfg))
                .collect()
        })
        .unwrap_or_default();
    mcp_servers.sort_by(|a, b| a.name.cmp(&b.name));
    let mut commands: Vec<_> = plugin
        .components
        .commands
        .iter()
        .map(|p| file_stem(p))
        .collect();
    commands.sort();
    let mut agents: Vec<_> = plugin
        .components
        .agents
        .iter()
        .map(|p| file_stem(p))
        .collect();
    agents.sort();
    let mut hooks: Vec<_> = plugin
        .components
        .hooks
        .iter()
        .map(|p| file_stem(p))
        .collect();
    hooks.sort();
    let mut skills: Vec<_> = plugin
        .skill_snapshots
        .iter()
        .map(|skill| PluginSkillReview {
            name: skill.name.clone(),
            description: skill.description.clone(),
        })
        .collect();
    skills.sort_by(|a, b| a.name.cmp(&b.name));

    PluginReviewPayload {
        token: plugin.review_token(),
        capabilities: plugin.inventory.supported_labels(),
        unsupported_capabilities: plugin.inventory.unsupported_labels(),
        filesystem_roots: plugin.inventory.filesystem_roots.clone(),
        network_hosts: plugin.inventory.network_hosts.clone(),
        lifecycle_mutation: plugin.inventory.lifecycle_mutation,
        mcp_servers,
        skills,
        commands,
        agents,
        hooks,
    }
}

fn plugin_detail(plugin: &LoadedPlugin) -> PluginDetailResponse {
    PluginDetailResponse {
        summary: plugin_summary(plugin),
        repository: plugin.manifest.plugin.repository.clone(),
        license: plugin.manifest.plugin.license.clone(),
        keywords: plugin.manifest.plugin.keywords.clone(),
        staged: plugin.staged_root.is_some(),
        review: review_payload(plugin),
    }
}

fn find_plugin(state: &RuntimeApiState, selector: &str) -> Result<LoadedPlugin, ApiError> {
    registry_for_state(state)
        .get(selector)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("plugin '{selector}' not found")))
}

fn ui_relative_path(raw: &str) -> Result<PathBuf, &'static str> {
    let raw = raw.strip_prefix("./").unwrap_or(raw);
    if raw.is_empty() {
        return Err("it is empty");
    }
    if raw.starts_with('/') || raw.contains('\\') {
        return Err("it must be a relative path without backslashes");
    }
    if raw.contains('\0') {
        return Err("it contains a NUL byte");
    }
    let mut relative = PathBuf::new();
    for component in FsPath::new(raw).components() {
        match component {
            std::path::Component::Normal(part) => relative.push(part),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => return Err("it must not contain '..'"),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                return Err("it must be relative");
            }
        }
    }
    if relative.as_os_str().is_empty() {
        return Err("it is empty");
    }
    Ok(relative)
}

fn ui_relative_display(relative: &FsPath) -> String {
    relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn ui_client_entry(manifest: &Value) -> Option<String> {
    let export = manifest.get("exports")?.get("./client")?;
    match export {
        Value::String(target) => Some(target.clone()),
        Value::Object(targets) => ["import", "default", "."]
            .iter()
            .find_map(|key| targets.get(*key).and_then(Value::as_str))
            .map(str::to_string),
        _ => None,
    }
}

fn ui_content_type(path: &FsPath) -> &'static str {
    match path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("js" | "mjs" | "cjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        _ => "application/octet-stream",
    }
}

fn ui_asset_response(bytes: Vec<u8>, content_type: &'static str) -> Response {
    let mut response = Response::new(Body::from(bytes));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

#[derive(Clone)]
struct PluginUiGrant {
    selector: String,
    authority: codewhale_config::PluginAuthority,
    state_file: Arc<std::fs::File>,
    root: PathBuf,
    root_file: Arc<std::fs::File>,
    ui_prefix: PathBuf,
    file_hashes: Arc<BTreeMap<PathBuf, String>>,
    expires: std::time::Instant,
}

#[derive(Default)]
pub(super) struct PluginUiGrants {
    entries: parking_lot::Mutex<HashMap<String, PluginUiGrant>>,
}

impl PluginUiGrants {
    fn insert(&self, grant: PluginUiGrant) -> String {
        let mut entries = self.entries.lock();
        let now = std::time::Instant::now();
        entries.retain(|_, value| value.expires > now);
        if entries.len() >= 256 {
            if let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, value)| value.expires)
                .map(|(key, _)| key.clone())
            {
                entries.remove(&oldest);
            }
        }
        loop {
            let revision = format!("\"{}\"", uuid::Uuid::new_v4());
            if !entries.contains_key(&revision) {
                entries.insert(revision.clone(), grant);
                return revision;
            }
        }
    }

    fn get(&self, revision: &str) -> Option<PluginUiGrant> {
        let mut entries = self.entries.lock();
        let now = std::time::Instant::now();
        entries.retain(|_, value| value.expires > now);
        entries.get(revision).cloned()
    }
}

fn ui_file_refusal(plugin: &LoadedPlugin, display: &str, error: UiFileError) -> ApiError {
    match error {
        UiFileError::Missing => ApiError::not_found(format!(
            "plugin '{}' has no staged file '{display}'",
            plugin.name()
        )),
        UiFileError::TooLarge => ApiError::payload_too_large(format!(
            "plugin '{}' file '{display}' exceeds the 8 MiB plugin UI limit",
            plugin.name()
        )),
        UiFileError::Invalid(reason) => ApiError::bad_request(format!(
            "plugin '{}' file '{display}' is not a readable staged file: {reason}",
            plugin.name()
        )),
        UiFileError::Internal(reason) => ApiError::internal(format!(
            "plugin '{}' file '{display}' could not be read: {reason}",
            plugin.name()
        )),
    }
}

fn ui_root_matches(grant: &PluginUiGrant) -> Result<(), UiFileError> {
    let metadata = std::fs::symlink_metadata(&grant.root)
        .map_err(|error| UiFileError::Internal(error.to_string()))?;
    if !metadata.is_dir() || codewhale_util::path_identity::metadata_is_link_or_reparse(&metadata) {
        return Err(UiFileError::Invalid("the staged root changed".into()));
    }
    let (canonical, current) = codewhale_util::workspace_dir::open_workspace_directory(&grant.root)
        .map_err(|error| UiFileError::Internal(error.to_string()))?;
    if canonical != grant.root
        || !codewhale_util::files::same_file(&grant.root_file, &current)
            .map_err(|error| UiFileError::Internal(error.to_string()))?
    {
        return Err(UiFileError::Invalid("the staged root changed".into()));
    }
    Ok(())
}

fn verified_ui_file(grant: &PluginUiGrant, relative: &FsPath) -> Result<Vec<u8>, UiFileError> {
    let expected = grant
        .file_hashes
        .get(relative)
        .ok_or(UiFileError::Missing)?;
    ui_root_matches(grant)?;
    let file = codewhale_util::fs_confined::open_read(&grant.root, &grant.root.join(relative))
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => UiFileError::Missing,
            _ => UiFileError::Invalid(error.to_string()),
        })?;
    let metadata = file
        .metadata()
        .map_err(|error| UiFileError::Internal(error.to_string()))?;
    if metadata.len() > PLUGIN_UI_MAX_BYTES {
        return Err(UiFileError::TooLarge);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(PLUGIN_UI_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| UiFileError::Internal(error.to_string()))?;
    if bytes.len() as u64 > PLUGIN_UI_MAX_BYTES {
        return Err(UiFileError::TooLarge);
    }
    let mut hash = Sha256::new();
    hash.update(b"codewhale-plugin-file-bytes-v1\0");
    hash.update(&bytes);
    let actual = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != *expected {
        return Err(UiFileError::Invalid(
            "the file no longer matches its reviewed bytes".into(),
        ));
    }
    ui_root_matches(grant)?;
    Ok(bytes)
}

fn admit_ui_grant(state: &RuntimeApiState, grant: &PluginUiGrant) -> Result<(), ApiError> {
    if grant.expires <= std::time::Instant::now() {
        return Err(ui_stale_revision());
    }
    let registry = registry_for_state(state);
    if !registry
        .get(&grant.selector)
        .is_some_and(LoadedPlugin::active)
        || registry.authority_for(&grant.selector).as_ref() != Some(&grant.authority)
    {
        return Err(ui_stale_revision());
    }
    codewhale_plugins::registry::capture_plugin_state_authority(
        &grant.authority,
        Some(&grant.state_file),
    )
    .map_err(|_| ui_stale_revision())?;
    ui_root_matches(grant).map_err(|_| ui_stale_revision())?;
    Ok(())
}

fn ui_stale_revision() -> ApiError {
    ApiError {
        status: StatusCode::PRECONDITION_FAILED,
        message: "plugin UI authorization has expired or changed; reopen the plugin".into(),
        code: None,
    }
}

fn prepare_plugin_ui(
    state: &RuntimeApiState,
    selector: &str,
) -> Result<(String, PluginUiGrant), ApiError> {
    let registry = registry_for_state(state);
    let plugin = registry
        .get(selector)
        .cloned()
        .ok_or_else(|| ApiError::not_found(format!("plugin '{selector}' not found")))?;
    if !plugin.active() {
        return Err(ApiError::bad_request(format!(
            "plugin '{}' must be active before loading its client UI",
            plugin.name()
        )));
    }
    let staged_root = plugin.staged_root.as_ref().ok_or_else(|| {
        ApiError::bad_request(format!(
            "plugin '{}' has no staged runtime snapshot",
            plugin.name()
        ))
    })?;
    let authority = registry
        .authority_for(selector)
        .ok_or_else(|| ApiError::bad_request("plugin UI has no reviewed authority"))?;
    let state_file = codewhale_plugins::registry::capture_plugin_state_authority(&authority, None)
        .map_err(|_| ui_stale_revision())?;
    codewhale_plugins::registry::verify_plugin_authority(&authority)
        .map_err(|_| ui_stale_revision())?;
    let validated =
        codewhale_plugins::manifest::PluginManifest::validate_from_path(&authority.staged_manifest)
            .map_err(|_| ui_stale_revision())?;
    if validated.content_hash != authority.content_hash
        || validated.capability_hash != authority.capability_hash
    {
        return Err(ui_stale_revision());
    }
    let metadata = std::fs::symlink_metadata(staged_root).map_err(|_| ui_stale_revision())?;
    if !metadata.is_dir() || codewhale_util::path_identity::metadata_is_link_or_reparse(&metadata) {
        return Err(ui_stale_revision());
    }
    let (root, root_file) = codewhale_util::workspace_dir::open_workspace_directory(staged_root)
        .map_err(|_| ui_stale_revision())?;
    if root != validated.canonical_root {
        return Err(ui_stale_revision());
    }
    let mut grant = PluginUiGrant {
        selector: selector.to_string(),
        authority,
        state_file: Arc::new(state_file),
        root,
        root_file: Arc::new(root_file),
        ui_prefix: PathBuf::new(),
        file_hashes: Arc::new(validated.file_hashes),
        expires: std::time::Instant::now() + std::time::Duration::from_secs(3600),
    };
    let mut found = None;
    for candidate in ["package.json", "source/package.json"] {
        let relative = FsPath::new(candidate);
        if grant.file_hashes.contains_key(relative) {
            let bytes = verified_ui_file(&grant, relative)
                .map_err(|error| ui_file_refusal(&plugin, candidate, error))?;
            found = Some((relative.to_path_buf(), bytes));
            break;
        }
    }
    let (manifest_path, bytes) = found.ok_or_else(|| ApiError::not_found(format!("plugin '{}' declares no dsh.client UI; package.json is absent from its staged bundle and source/ directory", plugin.name())).with_code("plugin_ui_not_declared"))?;
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|error| {
        ApiError::bad_request(format!(
            "plugin '{}' {} is not valid JSON: {error}",
            plugin.name(),
            ui_relative_display(&manifest_path)
        ))
    })?;
    if manifest
        .get("dsh")
        .and_then(|dsh| dsh.get("client"))
        .is_none_or(Value::is_null)
    {
        return Err(ApiError::not_found(format!("plugin '{}' declares no dsh.client UI in {}; add dsh.client and exports[\"./client\"] to expose client assets", plugin.name(), ui_relative_display(&manifest_path))).with_code("plugin_ui_not_declared"));
    }
    grant.ui_prefix = manifest_path
        .parent()
        .unwrap_or(FsPath::new(""))
        .to_path_buf();
    let raw = ui_client_entry(&manifest).unwrap_or_else(|| "client.js".into());
    let relative = ui_relative_path(&raw).map_err(|reason| {
        ApiError::bad_request(format!(
            "plugin '{}' dsh.client entry '{raw}' is invalid: {reason}",
            plugin.name()
        ))
    })?;
    verified_ui_file(&grant, &grant.ui_prefix.join(&relative))
        .map_err(|error| ui_file_refusal(&plugin, &ui_relative_display(&relative), error))?;
    admit_ui_grant(state, &grant)?;
    Ok((ui_relative_display(&relative), grant))
}

fn plugin_ui_base(
    state: &RuntimeApiState,
    selector: &str,
    headers: &HeaderMap,
    uri: &Uri,
) -> Result<String, ApiError> {
    let raw = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .or_else(|| uri.authority().map(Authority::as_str))
        .ok_or_else(|| {
            ApiError::bad_request(
                "plugin UI requires a loopback Host header so the sandboxed client can build an absolute asset base",
            )
        })?;
    if raw.is_empty()
        || raw.contains('@')
        || raw.contains('/')
        || raw.contains('?')
        || raw.contains('#')
        || raw.bytes().any(|byte| byte <= b' ' || byte == 0x7f)
    {
        return Err(ApiError::bad_request(
            "plugin UI Host must be a loopback authority with no credentials, path, query or fragment",
        ));
    }
    let authority: Authority = raw
        .parse()
        .map_err(|_| ApiError::bad_request("plugin UI Host is not a valid loopback authority"))?;
    if !super::host_header_is_loopback(raw) {
        return Err(ApiError::bad_request(format!(
            "plugin UI Host '{raw}' is not a loopback host; expected 127.0.0.1, [::1] or localhost"
        )));
    }
    let port_specified = if let Some(rest) = raw.strip_prefix('[') {
        match rest.split_once(']') {
            Some((_, "")) => false,
            Some((_, after)) if after.starts_with(':') => true,
            _ => {
                return Err(ApiError::bad_request(
                    "plugin UI Host must be a loopback authority with no credentials, path, query or fragment",
                ));
            }
        }
    } else {
        raw.contains(':')
    };
    let host_port = authority.port_u16().filter(|port| *port != 0);
    if port_specified && host_port.is_none() {
        return Err(ApiError::bad_request(
            "plugin UI Host must carry a numeric listener port",
        ));
    }
    let host = authority.host();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    let port = host_port
        .or_else(|| (state.bind_port != 0).then_some(state.bind_port))
        .ok_or_else(|| ApiError::bad_request("plugin UI Host is missing the listener port"))?;
    Ok(format!(
        "http://{host}:{port}/v1/apps/plugins/{}/ui/",
        urlencoding::encode(selector)
    ))
}

/// Execute an install/update/uninstall through the reviewed mutation
/// controller using the server's own config for network policy, then
/// invalidate the MCP pool so merged plugin servers reload on next use.
async fn run_plugin_mutation(
    state: &RuntimeApiState,
    request: PluginMutationRequest,
) -> Result<PluginMutationResponse, ApiError> {
    let network = {
        let config = state.config.read();
        config
            .network
            .clone()
            .map(codewhale_net::network_policy::NetworkPolicy::from)
            .unwrap_or_default()
    };
    let ctx = PluginMutationContext {
        network: &network,
        max_size: codewhale_plugins::install::DEFAULT_MAX_SIZE_BYTES,
    };
    let mut registry = (*registry_for_state(state)).clone();
    let receipt = codewhale_plugins::mutation::execute(request, &ctx, &mut registry)
        .await
        .map_err(|error| {
            if error
                .downcast_ref::<codewhale_plugins::install::PluginNameConflict>()
                .is_some()
                || error
                    .downcast_ref::<codewhale_plugins::install::PluginContentMismatch>()
                    .is_some()
                || error
                    .downcast_ref::<codewhale_plugins::install::PluginArchiveMismatch>()
                    .is_some()
            {
                ApiError::conflict(format!("plugin mutation failed: {error:#}"))
            } else {
                ApiError::internal(format!("plugin mutation failed: {error:#}"))
            }
        })?;

    // Policy outcomes are not server errors: report the blocked host with
    // the same wording the skill lifecycle API uses.
    let outcome = match &receipt.outcome {
        PluginMutationOutcome::NeedsApproval(host) => {
            return Err(ApiError::forbidden(format!(
                "network access to '{host}' requires explicit approval; \
                 approve the host in your network policy before installing this plugin"
            )));
        }
        PluginMutationOutcome::NetworkDenied(host) => {
            return Err(ApiError::forbidden(format!(
                "network access to '{host}' was denied by the active network policy"
            )));
        }
        PluginMutationOutcome::Installed => "installed",
        PluginMutationOutcome::Updated => "updated",
        PluginMutationOutcome::NoChange => "no_change",
        PluginMutationOutcome::Uninstalled => "uninstalled",
    };

    // Mutations can change merged plugin MCP servers. Advance the existing
    // shared generation so each captured workspace pool reloads on next use.
    state
        .workspace_scopes
        .mcp_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let plugin = registry_for_state(state)
        .get(receipt.name.as_str())
        .map(plugin_summary);
    let note = match receipt.outcome {
        PluginMutationOutcome::Installed => Some(
            "Installed disabled and untrusted. Open this plugin's detail \
             to review its capabilities, then trust and enable it.",
        ),
        PluginMutationOutcome::Updated => Some(
            "Installed content refreshed. Review its current capabilities \
             and trust it if needed before enabling.",
        ),
        _ => None,
    };
    Ok(PluginMutationResponse {
        outcome,
        name: receipt.name.clone(),
        path: receipt.path.as_ref().map(|p| p.display().to_string()),
        content_hash: receipt.installed_content_hash.or(receipt.content_hash),
        note,
        plugin,
    })
}

/// Run a registry state mutation (`trust`/`enable`/`restage`/`disable`/`revoke`)
/// against a fresh registry, then invalidate the MCP pool. Trust is the only
/// one with a precondition beyond the registry's own checks: the request
/// token must match the bundle's review token.
async fn run_registry_mutation(
    state: &RuntimeApiState,
    selector: &str,
    mutation: RegistryMutation<'_>,
) -> Result<PluginActionResponse, ApiError> {
    let registry = registry_for_state(state);
    if let RegistryMutation::Trust { token } = &mutation {
        let Some(plugin) = registry.get(selector) else {
            return Err(ApiError::not_found(format!(
                "plugin '{selector}' not found"
            )));
        };
        if token != &plugin.review_token() {
            return Err(ApiError::bad_request(
                "review token does not match this bundle's content and capability set; \
                 reload this plugin's detail and confirm the current review token",
            ));
        }
    }

    let action = match mutation {
        RegistryMutation::Trust { .. } => "trusted",
        RegistryMutation::Enable => "enabled",
        RegistryMutation::Restage => "restaged",
        RegistryMutation::Disable => "disabled",
        RegistryMutation::Revoke => "trust-revoked",
    };

    let mut registry = (*registry).clone();
    let result = match mutation {
        RegistryMutation::Trust { .. } => registry.trust(selector),
        RegistryMutation::Enable => registry.enable(selector),
        RegistryMutation::Restage => registry.restage(selector),
        RegistryMutation::Disable => registry.disable(selector),
        RegistryMutation::Revoke => registry.revoke_trust(selector),
    };
    result.map_err(|error| {
        ApiError::conflict(format!("{action} failed for '{selector}': {error}"))
    })?;

    state
        .workspace_scopes
        .mcp_generation
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    let fresh = registry_for_state(state);
    codewhale_engine::extension_host::plugins_changed(Arc::clone(&fresh));
    let Some(plugin) = fresh.get(selector) else {
        return Ok(PluginActionResponse {
            name: selector.to_string(),
            action,
            state: "removed",
            note: None,
        });
    };
    let note = match (action, plugin.state_label()) {
        ("enabled", "enabled-untrusted") => Some(
            "enabled-untrusted: the bundle is not trusted; open this plugin's \
             detail, review its capabilities and trust it first",
        ),
        ("enabled", _) => {
            let inactive = plugin.inventory.unsupported_labels();
            (!inactive.is_empty()).then_some(
                "supported declarative components are active; inventory-only \
                 capabilities stay inactive",
            )
        }
        _ => None,
    };
    Ok(PluginActionResponse {
        name: selector.to_string(),
        action,
        state: plugin.state_label(),
        note,
    })
}

enum RegistryMutation<'a> {
    Trust { token: &'a str },
    Enable,
    Restage,
    Disable,
    Revoke,
}

fn open_marketplace_store(state: &RuntimeApiState) -> Result<MarketplaceStore, ApiError> {
    MarketplaceStore::open(registry_for_state(state).state_path()).ok_or_else(|| {
        ApiError::internal(
            "this plugin registry has no persistence store; \
             marketplace catalogs cannot be saved",
        )
    })
}

fn load_marketplace_state(
    store: &MarketplaceStore,
) -> Result<codewhale_plugins::marketplace::store::MarketplaceState, ApiError> {
    store.load().map_err(|error| {
        ApiError::internal(format!(
            "marketplace state is fail-closed and will not be rewritten: {error}"
        ))
    })
}

// ---------------------------------------------------------------------------
// Marketplace DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(super) struct MarketplaceInstallPlanEntry {
    pub(super) installable: bool,
    pub(super) spec: Option<String>,
    pub(super) source_kind: Option<String>,
    pub(super) reason: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct MarketplaceCandidateEntry {
    pub(super) name: String,
    pub(super) display_name: Option<String>,
    pub(super) icon: Option<String>,
    pub(super) platforms: Vec<String>,
    pub(super) description: Option<String>,
    pub(super) version: Option<String>,
    pub(super) author: Option<String>,
    pub(super) homepage: Option<String>,
    pub(super) repository: Option<String>,
    pub(super) license: Option<String>,
    pub(super) keywords: Vec<String>,
    pub(super) categories: Vec<String>,
    pub(super) tier: String,
    pub(super) compatibility: Option<&'static str>,
    pub(super) install: MarketplaceInstallPlanEntry,
    /// Name occupancy, not an assertion that the catalog and local bytes match.
    pub(super) existing_plugin: Option<PluginSummaryEntry>,
    pub(super) diagnostics: Vec<PluginDiagnosticEntry>,
}

#[derive(Debug, Serialize)]
pub(super) struct MarketplaceCatalogEntry {
    pub(super) name: String,
    pub(super) display_name: Option<String>,
    pub(super) description: Option<String>,
    pub(super) format: &'static str,
    pub(super) tier: String,
    pub(super) added_at: String,
    pub(super) source_path: String,
    pub(super) candidate_count: usize,
    pub(super) warning_count: usize,
    pub(super) error_count: usize,
    pub(super) diagnostics: Vec<PluginDiagnosticEntry>,
    pub(super) candidates: Vec<MarketplaceCandidateEntry>,
}

#[derive(Debug, Serialize)]
pub(super) struct MarketplacesResponse {
    pub(super) marketplaces: Vec<MarketplaceCatalogEntry>,
}

#[derive(Debug, Serialize)]
pub(super) struct MarketplaceActionResponse {
    pub(super) name: String,
    pub(super) action: &'static str,
    pub(super) candidate_count: Option<usize>,
    pub(super) warning_count: Option<usize>,
}

fn marketplace_candidate_entry(
    entry: &codewhale_plugins::marketplace::store::StoredMarketplaceCatalog,
    candidate: &codewhale_plugins::marketplace::types::MarketplaceCandidate,
    registry: &codewhale_plugins::PluginRegistry,
) -> MarketplaceCandidateEntry {
    let mut existing_plugin = None;
    let install = match resolve_candidate_install(entry, candidate, registry) {
        CatalogInstallResolution::Supported { spec, source_kind } => MarketplaceInstallPlanEntry {
            installable: true,
            spec: Some(spec),
            source_kind: Some(source_kind),
            reason: None,
        },
        CatalogInstallResolution::AlreadyPresent { plugin, reason } => {
            existing_plugin = Some(plugin_summary(plugin));
            MarketplaceInstallPlanEntry {
                installable: false,
                spec: None,
                source_kind: None,
                reason: Some(reason),
            }
        }
        CatalogInstallResolution::Unsupported { reason } => MarketplaceInstallPlanEntry {
            installable: false,
            spec: None,
            source_kind: None,
            reason: Some(reason),
        },
        CatalogInstallResolution::HasErrors { diagnostics } => MarketplaceInstallPlanEntry {
            installable: false,
            spec: None,
            source_kind: None,
            reason: Some(format!("candidate has parse errors: {diagnostics}")),
        },
    };
    MarketplaceCandidateEntry {
        name: candidate.name.clone(),
        display_name: candidate.display_name.clone(),
        icon: candidate.icon.clone(),
        platforms: candidate
            .when
            .as_ref()
            .and_then(|when| when.os.clone())
            .unwrap_or_default(),
        description: candidate.description.clone(),
        version: candidate.version.clone(),
        author: candidate.author.clone(),
        homepage: candidate.homepage.clone(),
        repository: candidate.repository.clone(),
        license: candidate.license.clone(),
        keywords: candidate.keywords.clone(),
        categories: candidate.categories.clone(),
        tier: candidate.provenance.tier.to_string(),
        compatibility: candidate.compatibility.as_ref().map(|c| c.as_str()),
        install,
        existing_plugin,
        diagnostics: candidate
            .diagnostics
            .iter()
            .map(|d| PluginDiagnosticEntry {
                level: match d.level {
                    PluginDiagnosticLevel::Warning => "warning",
                    PluginDiagnosticLevel::Error => "error",
                },
                code: d.code.to_string(),
                message: d.message.clone(),
                path: None,
            })
            .collect(),
    }
}

fn marketplace_catalog_entry(
    name: &str,
    entry: &codewhale_plugins::marketplace::store::StoredMarketplaceCatalog,
    registry: &codewhale_plugins::PluginRegistry,
) -> MarketplaceCatalogEntry {
    MarketplaceCatalogEntry {
        name: name.to_string(),
        display_name: entry.catalog.display_name.clone(),
        description: entry.catalog.description.clone(),
        format: entry.catalog.format.as_str(),
        tier: entry.catalog.provenance.tier.to_string(),
        added_at: entry.added_at.clone(),
        source_path: entry.source_path.clone(),
        candidate_count: entry.catalog.total_candidates(),
        warning_count: entry.catalog.warning_count(),
        error_count: entry.catalog.error_count(),
        diagnostics: entry
            .catalog
            .diagnostics
            .iter()
            .map(|d| PluginDiagnosticEntry {
                level: match d.level {
                    PluginDiagnosticLevel::Warning => "warning",
                    PluginDiagnosticLevel::Error => "error",
                },
                code: d.code.to_string(),
                message: d.message.clone(),
                path: None,
            })
            .collect(),
        candidates: entry
            .catalog
            .candidates
            .iter()
            .map(|candidate| marketplace_candidate_entry(entry, candidate, registry))
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Handlers — plugins
// ---------------------------------------------------------------------------

/// `GET /v1/apps/plugins`
pub(super) async fn list_plugins(
    State(state): State<RuntimeApiState>,
) -> Result<Json<PluginsResponse>, ApiError> {
    let registry = registry_for_state(&state);
    Ok(Json(PluginsResponse {
        workspace: state.workspace.display().to_string(),
        plugins: registry.list().iter().map(|p| plugin_summary(p)).collect(),
        registry_diagnostics: registry
            .diagnostics()
            .iter()
            .map(diagnostic_entry)
            .collect(),
        validation_clean: registry.validation_is_clean(),
    }))
}

/// `GET /v1/apps/plugins/{selector}`
pub(super) async fn get_plugin(
    State(state): State<RuntimeApiState>,
    Path(selector): Path<String>,
) -> Result<Json<PluginDetailResponse>, ApiError> {
    Ok(Json(plugin_detail(&find_plugin(&state, &selector)?)))
}

pub(super) async fn get_plugin_ui(
    State(state): State<RuntimeApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(selector): Path<String>,
    Query(query): Query<PluginUiQuery>,
    headers: HeaderMap,
    uri: Uri,
) -> Result<Json<PluginUiSlotResponse>, ApiError> {
    if !peer.ip().is_loopback() {
        return Err(ApiError::forbidden(
            "plugin UI metadata is available only from loopback",
        ));
    }
    let slot = query.slot.ok_or_else(|| {
        ApiError::bad_request(
            "the 'slot' query parameter is required; supported slot: 'customize.plugins.detail'",
        )
    })?;
    if slot.as_str() != PLUGIN_UI_SLOT {
        return Err(ApiError::bad_request(format!(
            "slot '{slot}' is not supported; supported slot: 'customize.plugins.detail'"
        )));
    }
    let state_for_prepare = state.clone();
    let selector_for_prepare = selector.clone();
    let (entry, grant) = tokio::task::spawn_blocking(move || {
        prepare_plugin_ui(&state_for_prepare, &selector_for_prepare)
    })
    .await
    .map_err(|_| ApiError::internal("plugin UI preparation failed"))??;
    let base = plugin_ui_base(&state, &selector, &headers, &uri)?;
    let revision = state.plugin_ui_grants.insert(grant);
    Ok(Json(PluginUiSlotResponse {
        slot,
        base,
        entry,
        revision,
    }))
}

pub(super) async fn get_plugin_ui_asset(
    State(state): State<RuntimeApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((selector, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !peer.ip().is_loopback() {
        return Err(ApiError::forbidden(
            "plugin UI assets are available only from loopback",
        ));
    }
    let mut values = headers.get_all(header::IF_MATCH).iter();
    let first = values.next().ok_or_else(|| ApiError {
        status: StatusCode::PRECONDITION_REQUIRED,
        message: "plugin UI revision is required".into(),
        code: None,
    })?;
    if values.next().is_some() {
        return Err(ui_stale_revision());
    }
    let revision = first.to_str().map_err(|_| ui_stale_revision())?.to_string();
    let grant = state
        .plugin_ui_grants
        .get(&revision)
        .ok_or_else(ui_stale_revision)?;
    if grant.selector != selector || grant.authority.workspace != state.workspace {
        return Err(ui_stale_revision());
    }
    let relative = ui_relative_path(&path).map_err(|reason| {
        ApiError::bad_request(format!("plugin UI path '{path}' is invalid: {reason}"))
    })?;
    let content_type = ui_content_type(&relative);
    let state_for_asset = state.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        codewhale_plugins::registry::verify_plugin_authority(&grant.authority)
            .map_err(|_| ui_stale_revision())?;
        let bytes = verified_ui_file(&grant, &grant.ui_prefix.join(&relative)).map_err(
            |error| match error {
                UiFileError::Missing => {
                    ApiError::not_found("plugin UI file is not in the reviewed bundle")
                }
                UiFileError::TooLarge => {
                    ApiError::payload_too_large("plugin UI file exceeds the 8 MiB limit")
                }
                _ => ui_stale_revision(),
            },
        )?;
        admit_ui_grant(&state_for_asset, &grant)?;
        if state_for_asset.plugin_ui_grants.get(&revision).is_none() {
            return Err(ui_stale_revision());
        }
        Ok::<_, ApiError>(bytes)
    })
    .await
    .map_err(|_| ApiError::internal("plugin UI read failed"))??;
    Ok(ui_asset_response(bytes, content_type))
}

/// `POST /v1/apps/plugins/install`
pub(super) async fn install_plugin_api(
    State(state): State<RuntimeApiState>,
    Json(req): Json<InstallPluginRequest>,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiError> {
    let source =
        codewhale_plugins::install::PluginInstallSource::parse(&req.source).map_err(|error| {
            ApiError::bad_request(format!(
                "invalid plugin install source '{}': {error:#}; expected a local \
                 path, github:owner/repo, or an HTTPS tarball URL",
                req.source
            ))
        })?;
    let request = match req.expected_content_hash {
        Some(expected) => PluginMutationRequest::InstallExact {
            source,
            expected_content_hash: expected,
        },
        None => PluginMutationRequest::Install { source },
    };
    let response = run_plugin_mutation(&state, request).await?;
    Ok((StatusCode::CREATED, Json(response)))
}

/// `POST /v1/apps/plugins/import/dsh/preview`: convert a DeepSeek Harness
/// bundle package into scratch and return its receipt and review hash.
/// Nothing is installed; the install endpoint does that with the same hash.
pub(super) async fn preview_dsh_plugin_api(
    State(state): State<RuntimeApiState>,
    Json(req): Json<DshPreviewRequest>,
) -> Result<Json<DshPreviewResponse>, ApiError> {
    let requested = std::path::PathBuf::from(req.path.trim());
    let path = if requested.is_absolute() {
        requested
    } else {
        state.workspace.join(requested)
    };
    let preview = tokio::task::spawn_blocking(move || {
        let canonical = path
            .canonicalize()
            .map_err(|_| format!("DSH package not found at {}", path.display()))?;
        codewhale_plugins::install::preview_dsh(&canonical)
            .map(|(conversion, content_hash)| (canonical, conversion, content_hash))
            .map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| ApiError::internal(format!("DSH preview task failed: {error}")))?;
    let (canonical, conversion, content_hash) =
        preview.map_err(|error| ApiError::bad_request(format!("DSH import refused: {error}")))?;
    Ok(Json(DshPreviewResponse {
        install_source: format!("dsh:{}", canonical.display()),
        content_hash,
        conversion,
    }))
}

/// `POST /v1/apps/plugins/{selector}/update`
pub(super) async fn update_plugin_api(
    State(state): State<RuntimeApiState>,
    Path(selector): Path<String>,
) -> Result<Json<PluginMutationResponse>, ApiError> {
    find_plugin(&state, &selector)?;
    Ok(Json(
        run_plugin_mutation(
            &state,
            PluginMutationRequest::Update {
                selector: selector.clone(),
            },
        )
        .await?,
    ))
}

/// `DELETE /v1/apps/plugins/{selector}`
pub(super) async fn uninstall_plugin_api(
    State(state): State<RuntimeApiState>,
    Path(selector): Path<String>,
) -> Result<Json<PluginMutationResponse>, ApiError> {
    find_plugin(&state, &selector)?;
    Ok(Json(
        run_plugin_mutation(
            &state,
            PluginMutationRequest::Uninstall {
                selector: selector.clone(),
            },
        )
        .await?,
    ))
}

/// `POST /v1/apps/plugins/{selector}/trust`
pub(super) async fn trust_plugin_api(
    State(state): State<RuntimeApiState>,
    Path(selector): Path<String>,
    Json(req): Json<TrustPluginRequest>,
) -> Result<Json<PluginActionResponse>, ApiError> {
    Ok(Json(
        run_registry_mutation(
            &state,
            &selector,
            RegistryMutation::Trust { token: &req.token },
        )
        .await?,
    ))
}

/// `POST /v1/apps/plugins/{selector}/enable`
pub(super) async fn enable_plugin_api(
    State(state): State<RuntimeApiState>,
    Path(selector): Path<String>,
) -> Result<Json<PluginActionResponse>, ApiError> {
    Ok(Json(
        run_registry_mutation(&state, &selector, RegistryMutation::Enable).await?,
    ))
}

pub(super) async fn restage_plugin_api(
    State(state): State<RuntimeApiState>,
    Path(selector): Path<String>,
) -> Result<Json<PluginActionResponse>, ApiError> {
    Ok(Json(
        run_registry_mutation(&state, &selector, RegistryMutation::Restage).await?,
    ))
}

/// `POST /v1/apps/plugins/{selector}/disable`
pub(super) async fn disable_plugin_api(
    State(state): State<RuntimeApiState>,
    Path(selector): Path<String>,
) -> Result<Json<PluginActionResponse>, ApiError> {
    Ok(Json(
        run_registry_mutation(&state, &selector, RegistryMutation::Disable).await?,
    ))
}

/// `POST /v1/apps/plugins/{selector}/revoke`
pub(super) async fn revoke_plugin_api(
    State(state): State<RuntimeApiState>,
    Path(selector): Path<String>,
) -> Result<Json<PluginActionResponse>, ApiError> {
    Ok(Json(
        run_registry_mutation(&state, &selector, RegistryMutation::Revoke).await?,
    ))
}

// ---------------------------------------------------------------------------
// Handlers — marketplaces
// ---------------------------------------------------------------------------

/// `GET /v1/apps/marketplaces`
pub(super) async fn list_marketplaces(
    State(state): State<RuntimeApiState>,
) -> Result<Json<MarketplacesResponse>, ApiError> {
    let store = open_marketplace_store(&state)?;
    let marketplace_state = load_marketplace_state(&store)?;
    let registry = registry_for_state(&state);
    Ok(Json(MarketplacesResponse {
        marketplaces: marketplace_state
            .catalogs()
            .iter()
            .map(|(name, entry)| marketplace_catalog_entry(name, entry, &registry))
            .collect(),
    }))
}

/// `GET /v1/apps/marketplaces/{name}`
pub(super) async fn get_marketplace(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
) -> Result<Json<MarketplaceCatalogEntry>, ApiError> {
    let store = open_marketplace_store(&state)?;
    let marketplace_state = load_marketplace_state(&store)?;
    let entry = marketplace_state
        .get(&name)
        .ok_or_else(|| ApiError::not_found(format!("marketplace '{name}' not found")))?;
    Ok(Json(marketplace_catalog_entry(
        &name,
        entry,
        &registry_for_state(&state),
    )))
}

/// `POST /v1/apps/marketplaces`
pub(super) async fn add_marketplace(
    State(state): State<RuntimeApiState>,
    Json(req): Json<AddMarketplaceRequest>,
) -> Result<(StatusCode, Json<MarketplaceActionResponse>), ApiError> {
    let store = open_marketplace_store(&state)?;
    let loaded = load_catalog_document(&req.name, &state.workspace, &req.path)
        .map_err(ApiError::bad_request)?;
    store
        .add(&loaded.entry.catalog.id.clone(), loaded.entry)
        .map_err(ApiError::conflict)?;
    Ok((
        StatusCode::CREATED,
        Json(MarketplaceActionResponse {
            name: req.name,
            action: "added",
            candidate_count: Some(loaded.candidate_count),
            warning_count: Some(loaded.warning_count),
        }),
    ))
}

/// `DELETE /v1/apps/marketplaces/{name}`
pub(super) async fn remove_marketplace(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
) -> Result<Json<MarketplaceActionResponse>, ApiError> {
    let store = open_marketplace_store(&state)?;
    let removed = store
        .remove(&name)
        .map_err(|error| ApiError::internal(format!("remove marketplace: {error}")))?;
    if !removed {
        return Err(ApiError::not_found(format!(
            "marketplace '{name}' not found"
        )));
    }
    Ok(Json(MarketplaceActionResponse {
        name,
        action: "removed",
        candidate_count: None,
        warning_count: None,
    }))
}

/// `POST /v1/apps/marketplaces/{name}/install`
///
/// Resolves the stored candidate through the shared plan resolver, then
/// routes through the reviewed installer exactly like
/// `POST /v1/apps/plugins/install`.
pub(super) async fn install_marketplace_candidate_api(
    State(state): State<RuntimeApiState>,
    Path(name): Path<String>,
    Json(req): Json<InstallMarketplaceCandidateRequest>,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiError> {
    let store = open_marketplace_store(&state)?;
    let marketplace_state = load_marketplace_state(&store)?;
    let entry = marketplace_state
        .get(&name)
        .ok_or_else(|| ApiError::not_found(format!("marketplace '{name}' not found")))?;
    let candidate = entry
        .catalog
        .candidate_by_name(&req.candidate)
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "candidate '{}' not found in marketplace '{name}'",
                req.candidate
            ))
        })?;
    let registry = registry_for_state(&state);
    match resolve_candidate_install(entry, candidate, &registry) {
        CatalogInstallResolution::Supported { spec, .. } => {
            let response = run_plugin_mutation(
                &state,
                PluginMutationRequest::Install {
                    source: codewhale_plugins::install::PluginInstallSource::parse(&spec).map_err(
                        |error| {
                            ApiError::internal(format!(
                                "resolved install spec '{spec}' no longer parses: {error:#}"
                            ))
                        },
                    )?,
                },
            )
            .await?;
            Ok((StatusCode::CREATED, Json(response)))
        }
        CatalogInstallResolution::AlreadyPresent { reason, .. } => Err(ApiError::conflict(reason)),
        CatalogInstallResolution::Unsupported { reason } => Err(ApiError::conflict(format!(
            "candidate '{}' cannot be installed by Codewhale: {reason}",
            req.candidate
        ))),
        CatalogInstallResolution::HasErrors { diagnostics } => Err(ApiError::conflict(format!(
            "candidate '{}' has parse errors and cannot be installed: {diagnostics}",
            req.candidate
        ))),
    }
}

#[cfg(test)]
mod review_tests {
    use super::mcp_server_review;

    #[test]
    fn plugin_mcp_review_omits_url_credentials_without_changing_the_bundle() {
        for (raw, expected) in [
            (
                "https://review-user:review-password@mcp.example.invalid:8443/review-path?arbitrary=review-query#review-fragment",
                "https://mcp.example.invalid:8443",
            ),
            (
                "http://[::1]:9000/mcp?token=review-query",
                "http://[::1]:9000",
            ),
            (
                "https://mcp.example.invalid/mcp",
                "https://mcp.example.invalid",
            ),
            (
                "not a URL review-secret",
                "unparseable (configured value omitted)",
            ),
            (
                "data:text/plain,review-secret",
                "unparseable (configured value omitted)",
            ),
        ] {
            let cfg: codewhale_mcp::config::McpServerConfig =
                serde_json::from_value(serde_json::json!({
                    "url": raw,
                    "env": { "REVIEW_ENV": "review-env-value" },
                    "headers": { "Authorization": "review-header-value" }
                }))
                .unwrap();
            let review = mcp_server_review("demo", &cfg);
            assert_eq!(review.url.as_deref(), Some(expected));
            assert_eq!(review.kind, "remote");
            assert_eq!(review.env_keys, ["REVIEW_ENV"]);
            assert_eq!(review.header_keys, ["Authorization"]);
            let payload = serde_json::to_string(&review).unwrap();
            for secret in [
                "review-user",
                "review-password",
                "review-path",
                "review-query",
                "review-fragment",
                "review-secret",
                "review-env-value",
                "review-header-value",
            ] {
                assert!(!payload.contains(secret), "review exposed {secret}");
            }
            // Display redaction must not change the endpoint used at execution
            // or the manifest from which the trust receipt is derived.
            assert_eq!(cfg.url.as_deref(), Some(raw));
        }
        let stdio: codewhale_mcp::config::McpServerConfig =
            serde_json::from_value(serde_json::json!({
                "command": "npx", "args": ["demo-server"]
            }))
            .unwrap();
        let review = mcp_server_review("stdio", &stdio);
        assert_eq!(review.kind, "stdio");
        assert_eq!(review.url, None);
        assert_eq!(review.command, stdio.command);
        assert_eq!(review.args, stdio.args);
    }
}

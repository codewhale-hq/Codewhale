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

use codewhale_plugins::host_services::{PersonVia, PluginApprover, ReviewApproval};
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
const PLUGIN_UI_SEATS: [&str; 6] = [
    PLUGIN_UI_SLOT,
    "conversation.composer.dock",
    "conversation.chat.node",
    "workpane.panel",
    "sidebar.section",
    "command.row",
];
const DSH_BOOTSTRAP_ENTRY: &str = "__codewhale_dsh_bootstrap.js";
const DSH_FRAME_MODULES: [&str; 4] = [
    "react",
    "react-dom",
    "react-dom/client",
    "react/jsx-runtime",
];
const DSH_ROUTE_METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

enum DshSeat {
    Seat(&'static str),
    Unsupported(&'static str),
}

const DSH_SEAT_TABLE: [(&str, DshSeat); 30] = [
    ("plugins.bundle.config", DshSeat::Seat(PLUGIN_UI_SLOT)),
    ("plugins.row.config", DshSeat::Seat(PLUGIN_UI_SLOT)),
    ("plugins.tab", DshSeat::Seat(PLUGIN_UI_SLOT)),
    ("plugins.detail.section", DshSeat::Seat(PLUGIN_UI_SLOT)),
    ("settings.section", DshSeat::Seat(PLUGIN_UI_SLOT)),
    ("settings.plugin.item", DshSeat::Seat(PLUGIN_UI_SLOT)),
    ("settings.general.item", DshSeat::Seat(PLUGIN_UI_SLOT)),
    ("settings.plugins.tab", DshSeat::Seat(PLUGIN_UI_SLOT)),
    (
        "conversation.chat.node",
        DshSeat::Seat("conversation.chat.node"),
    ),
    (
        "conversation.chat.turnTail",
        DshSeat::Seat("conversation.chat.node"),
    ),
    (
        "conversation.chat.assistant-actions",
        DshSeat::Seat("conversation.chat.node"),
    ),
    (
        "conversation.chat.text-interaction",
        DshSeat::Seat("conversation.chat.node"),
    ),
    (
        "conversation.user.text.node",
        DshSeat::Seat("conversation.chat.node"),
    ),
    (
        "tool.call.toolview",
        DshSeat::Seat("conversation.chat.node"),
    ),
    ("conversation.view", DshSeat::Seat("conversation.chat.node")),
    (
        "conversation.input.dock",
        DshSeat::Seat("conversation.composer.dock"),
    ),
    (
        "conversation.input.overlay",
        DshSeat::Seat("conversation.composer.dock"),
    ),
    (
        "conversation.input.right",
        DshSeat::Seat("conversation.composer.dock"),
    ),
    (
        "conversation.composer.dock",
        DshSeat::Seat("conversation.composer.dock"),
    ),
    ("sidebar.right.pane.tab", DshSeat::Seat("workpane.panel")),
    ("main.conversation", DshSeat::Seat("workpane.panel")),
    ("main.page-menu", DshSeat::Seat("workpane.panel")),
    ("sidebar.panellist", DshSeat::Seat("sidebar.section")),
    ("sidebar.workspaces", DshSeat::Seat("sidebar.section")),
    ("sidebar.footer.action", DshSeat::Seat("sidebar.section")),
    (
        "sidebar.session.row.leading",
        DshSeat::Seat("sidebar.section"),
    ),
    (
        "sidebar.workspaces.session.row.action",
        DshSeat::Seat("sidebar.section"),
    ),
    (
        "conversation.session.header.actions",
        DshSeat::Unsupported("no ADE seat yet"),
    ),
    (
        "conversation.session.header.utilities",
        DshSeat::Unsupported("no ADE seat yet"),
    ),
    ("contextMenu.icon", DshSeat::Unsupported("no ADE seat yet")),
];

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
pub(super) struct PluginUiBridge {
    pub(super) kind: &'static str,
    pub(super) dsh_version: String,
    pub(super) compatibility_digest: String,
}

#[derive(Debug, Serialize)]
pub(super) struct PluginUiSlotResponse {
    pub(super) slot: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) dsh_slot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) bridge: Option<PluginUiBridge>,
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
    pub(super) dsh_slot: Option<String>,
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
    /// LOCAL catalog document path (kimi/claude/codex/dsh/codewhale format,
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
    dsh: Option<Arc<DshServed>>,
}

struct DshClosure {
    package: String,
    entry: String,
    files: BTreeMap<String, String>,
    host_provided: Vec<String>,
}

struct DshServed {
    closure: DshClosure,
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
        if entries.len() >= 256
            && let Some(oldest) = entries
                .iter()
                .min_by_key(|(_, value)| value.expires)
                .map(|(key, _)| key.clone())
        {
            entries.remove(&oldest);
        }
        loop {
            let revision = format!("\"{}\"", uuid::Uuid::new_v4());
            if !entries.contains_key(&revision) {
                entries.insert(revision.clone(), grant);
                return revision;
            }
        }
    }

    fn revoke_selector(&self, selector: &str) {
        self.entries
            .lock()
            .retain(|_, value| value.selector != selector);
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
        ))
        .with_code("plugin_ui_file_not_reviewed"),
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

fn valid_dsh_slot_key(key: &str) -> bool {
    let mut chars = key.chars();
    key.len() <= 128
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn ui_stale_revision() -> ApiError {
    ApiError {
        status: StatusCode::PRECONDITION_FAILED,
        message: "plugin UI authorization has expired or changed; reopen the plugin".into(),
        code: None,
    }
}

struct PreparedPluginUi {
    entry: String,
    dsh_slot: Option<String>,
    bridge: Option<PluginUiBridge>,
    grant: PluginUiGrant,
}

fn seat_not_admitted(plugin: &LoadedPlugin, seat: &str, dsh_slot: Option<&str>) -> ApiError {
    let key = dsh_slot
        .map(|key| format!(" key '{key}'"))
        .unwrap_or_default();
    ApiError::not_found(format!(
        "plugin '{}' has no admitted client for seat '{seat}'{key}",
        plugin.name()
    ))
    .with_code("plugin_ui_seat_not_admitted")
}

fn ui_unsupported(message: impl Into<String>) -> ApiError {
    ApiError::bad_request(message).with_code("plugin_ui_unsupported")
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn dsh_client_closure(conversion: &Value) -> Option<DshClosure> {
    let closure = conversion
        .get("client_closures")?
        .as_array()?
        .iter()
        .find(|closure| closure.get("root").and_then(Value::as_str) == Some(""))?;
    let files = closure
        .get("files")?
        .as_object()?
        .iter()
        .map(|(path, hash)| Some((path.clone(), hash.as_str()?.to_string())))
        .collect::<Option<BTreeMap<_, _>>>()?;
    let entry = closure.get("entry")?.as_str()?.to_string();
    if !files.contains_key(&entry) {
        return None;
    }
    Some(DshClosure {
        package: closure
            .get("package")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        entry,
        files,
        host_provided: closure
            .get("host_provided")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn verify_dsh_closure(
    grant: &PluginUiGrant,
    closure: &DshClosure,
) -> Result<Vec<(String, Vec<u8>)>, UiFileError> {
    let mut verified = Vec::with_capacity(closure.files.len());
    for (path, expected) in &closure.files {
        let relative = ui_relative_path(path)
            .map_err(|reason| UiFileError::Invalid(format!("closure file '{path}': {reason}")))?;
        let bytes = verified_ui_file(grant, &relative)?;
        if sha256_hex(&bytes) != *expected {
            return Err(UiFileError::Invalid(format!(
                "closure file '{path}' no longer matches its reviewed hash"
            )));
        }
        verified.push((path.clone(), bytes));
    }
    Ok(verified)
}

fn has_esm_syntax(text: &str) -> bool {
    text.contains("import.meta")
        || text.lines().any(|line| {
            let line = line.trim_start();
            [
                "import ", "import{", "import\"", "import'", "export ", "export{", "export*",
            ]
            .iter()
            .any(|prefix| line.starts_with(prefix))
        })
}

fn dsh_closure_unsupported(closure: &DshClosure, verified: &[(String, Vec<u8>)]) -> Option<String> {
    let missing: Vec<&str> = closure
        .host_provided
        .iter()
        .map(String::as_str)
        .filter(|module| !DSH_FRAME_MODULES.contains(module))
        .take(5)
        .collect();
    if !missing.is_empty() {
        return Some(format!(
            "this DSH client requires modules the plugin frame does not provide yet: {}",
            missing.join(", ")
        ));
    }
    for (path, bytes) in verified {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Some(format!("closure file '{path}' is not UTF-8 text"));
        };
        if path.ends_with(".json") {
            if serde_json::from_str::<Value>(text).is_err() {
                return Some(format!("closure file '{path}' is not valid JSON"));
            }
        } else if has_esm_syntax(text) {
            return Some(format!(
                "closure file '{path}' uses ES module syntax; only CommonJS closures are served"
            ));
        }
    }
    None
}

fn quoted_in(text: &str, key: &str) -> bool {
    ['"', '\'', '`']
        .iter()
        .any(|quote| text.contains(&format!("{quote}{key}{quote}")))
}

fn declared_dsh_keys(verified: &[(String, Vec<u8>)]) -> Vec<&'static (&'static str, DshSeat)> {
    let texts: Vec<String> = verified
        .iter()
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
        .collect();
    DSH_SEAT_TABLE
        .iter()
        .filter(|(key, _)| texts.iter().any(|text| quoted_in(text, key)))
        .collect()
}

fn admit_dsh_seat(
    plugin: &LoadedPlugin,
    seat: &str,
    requested: Option<&str>,
    declared: &[&'static (&'static str, DshSeat)],
) -> Result<String, ApiError> {
    if let Some(key) = requested {
        let Some((_, mapped)) = declared.iter().find(|(name, _)| *name == key) else {
            return Err(seat_not_admitted(plugin, seat, requested));
        };
        return match mapped {
            DshSeat::Unsupported(reason) => Err(ApiError {
                status: StatusCode::UNPROCESSABLE_ENTITY,
                message: format!(
                    "plugin '{}' declares '{key}', which has no Codewhale seat: {reason}",
                    plugin.name()
                ),
                code: Some("plugin_ui_seat_unsupported"),
            }),
            DshSeat::Seat(mapped_seat) if *mapped_seat == seat => Ok(key.to_string()),
            DshSeat::Seat(_) => Err(seat_not_admitted(plugin, seat, requested)),
        };
    }
    let mut on_seat = declared.iter().filter_map(|(name, mapped)| match mapped {
        DshSeat::Seat(mapped_seat) if *mapped_seat == seat => Some(*name),
        _ => None,
    });
    match (on_seat.next(), on_seat.next()) {
        (None, _) => Err(seat_not_admitted(plugin, seat, None)),
        (Some(key), None) => Ok(key.to_string()),
        (Some(_), Some(_)) => Err(ApiError::bad_request(format!(
            "plugin '{}' admits more than one client key on seat '{seat}'; pass dsh_slot",
            plugin.name()
        ))
        .with_code("plugin_ui_dsh_slot_required")),
    }
}

fn dsh_bridge() -> Result<PluginUiBridge, ApiError> {
    let receipt: Value = serde_json::from_slice(codewhale_plugins::activation::DSH_COMPATIBILITY)
        .map_err(|_| {
        ApiError::internal("the pinned DSH compatibility receipt is unreadable")
    })?;
    let version = receipt
        .get("target_dsh_version")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::internal("the pinned DSH compatibility receipt has no version"))?;
    Ok(PluginUiBridge {
        kind: "dsh-client",
        dsh_version: version.to_string(),
        compatibility_digest: sha256_hex(codewhale_plugins::activation::DSH_COMPATIBILITY),
    })
}

const DSH_BOOTSTRAP_HEAD: &str = r#""use strict";
(function () {
  var files = {
"#;

const DSH_BOOTSTRAP_TAIL: &str = r#"  var cache = {};
  var probes = ["", ".js", ".mjs", ".cjs", ".json", "/index.js", "/index.mjs", "/index.cjs"];
  var hostRequire = null;
  function normalize(path) {
    var out = [];
    path.split("/").forEach(function (part) {
      if (part === "" || part === ".") return;
      if (part === "..") out.pop();
      else out.push(part);
    });
    return out.join("/");
  }
  function resolve(from, id) {
    var base = normalize(from.slice(0, from.lastIndexOf("/")) + "/" + id);
    for (var i = 0; i < probes.length; i++) {
      if (Object.prototype.hasOwnProperty.call(files, base + probes[i])) return base + probes[i];
    }
    return null;
  }
  function load(path) {
    if (Object.prototype.hasOwnProperty.call(cache, path)) return cache[path].exports;
    var module = { exports: {} };
    cache[path] = module;
    files[path].call(module.exports, module, module.exports, makeRequire(path));
    return module.exports;
  }
  function makeRequire(from) {
    return function (id) {
      if (id.charAt(0) === ".") {
        var resolved = resolve(from, id);
        if (resolved === null) throw new Error("the plugin client requires a file outside its reviewed closure: " + id);
        return load(resolved);
      }
      return hostRequire(id);
    };
  }
  window.__ModuleLoader__.load({
    id: PLUGIN_ID,
    factory: function (require) {
      hostRequire = require;
      return load(ENTRY);
    }
  });
})();
"#;

fn dsh_bootstrap(
    selector: &str,
    closure: &DshClosure,
    verified: &[(String, Vec<u8>)],
) -> Result<Vec<u8>, UiFileError> {
    let quote = |text: &str| {
        serde_json::to_string(text).map_err(|error| UiFileError::Internal(error.to_string()))
    };
    let mut out = String::from(DSH_BOOTSTRAP_HEAD);
    for (path, bytes) in verified {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| UiFileError::Invalid(format!("closure file '{path}' is not UTF-8")))?;
        out.push_str("    ");
        out.push_str(&quote(path)?);
        out.push_str(": function (module, exports, require) {\n");
        if path.ends_with(".json") {
            out.push_str("module.exports = ");
            out.push_str(text);
            out.push(';');
        } else {
            out.push_str(text);
        }
        out.push_str("\n},\n");
    }
    out.push_str("  };\n");
    out.push_str(&format!(
        "  var PLUGIN_ID = {};\n  var ENTRY = {};\n",
        quote(if closure.package.is_empty() {
            selector
        } else {
            &closure.package
        })?,
        quote(&closure.entry)?
    ));
    out.push_str(DSH_BOOTSTRAP_TAIL);
    Ok(out.into_bytes())
}

fn prepare_plugin_ui(
    state: &RuntimeApiState,
    selector: &str,
    seat: &str,
    dsh_slot: Option<&str>,
) -> Result<PreparedPluginUi, ApiError> {
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
        dsh: None,
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
    let conversion_path = FsPath::new("CONVERSION.json");
    let mut closure = None;
    if grant.file_hashes.contains_key(conversion_path) {
        let bytes = verified_ui_file(&grant, conversion_path)
            .map_err(|error| ui_file_refusal(&plugin, "CONVERSION.json", error))?;
        let conversion: Value = serde_json::from_slice(&bytes).map_err(|_| {
            ApiError::bad_request("plugin UI conversion report is invalid")
                .with_code("plugin_ui_unsupported")
        })?;
        if conversion.get("schema").and_then(Value::as_str)
            == Some("codewhale.plugin-conversion.v1")
            && conversion
                .pointer("/source/dialect")
                .and_then(Value::as_str)
                == Some("dsh")
        {
            closure = Some(dsh_client_closure(&conversion).ok_or_else(|| {
                ui_unsupported(
                    "this DSH client UI was not converted and is not supported by Codewhale yet",
                )
            })?);
        }
    }
    grant.ui_prefix = manifest_path
        .parent()
        .unwrap_or(FsPath::new(""))
        .to_path_buf();
    if let Some(closure) = closure {
        let verified = verify_dsh_closure(&grant, &closure)
            .map_err(|error| ui_file_refusal(&plugin, "client closure", error))?;
        if let Some(reason) = dsh_closure_unsupported(&closure, &verified) {
            return Err(ui_unsupported(format!(
                "plugin '{}': {reason}",
                plugin.name()
            )));
        }
        let key = admit_dsh_seat(&plugin, seat, dsh_slot, &declared_dsh_keys(&verified))?;
        grant.dsh = Some(Arc::new(DshServed { closure }));
        admit_ui_grant(state, &grant)?;
        return Ok(PreparedPluginUi {
            entry: DSH_BOOTSTRAP_ENTRY.to_string(),
            dsh_slot: Some(key),
            bridge: Some(dsh_bridge()?),
            grant,
        });
    }
    if seat != PLUGIN_UI_SLOT || dsh_slot.is_some() {
        return Err(seat_not_admitted(&plugin, seat, dsh_slot));
    }
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
    Ok(PreparedPluginUi {
        entry: ui_relative_display(&relative),
        dsh_slot: None,
        bridge: None,
        grant,
    })
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

    state.plugin_ui_grants.revoke_selector(&receipt.name);

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
        RegistryMutation::Trust { .. } => registry.trust_reviewed(
            selector,
            ReviewApproval {
                approved_by: PluginApprover::person(PersonVia::App),
                host_services: None,
            },
        ),
        RegistryMutation::Enable => {
            registry.enable_by(selector, PluginApprover::person(PersonVia::App))
        }
        RegistryMutation::Restage => registry.restage(selector),
        RegistryMutation::Disable => registry.disable(selector),
        RegistryMutation::Revoke => registry.revoke_trust(selector),
    };
    result.map_err(|error| {
        ApiError::conflict(format!("{action} failed for '{selector}': {error}"))
    })?;

    state.plugin_ui_grants.revoke_selector(selector);

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
        ApiError::bad_request(format!(
            "the 'slot' query parameter is required; seats: {}",
            PLUGIN_UI_SEATS.join(", ")
        ))
        .with_code("plugin_ui_seat_unknown")
    })?;
    if !PLUGIN_UI_SEATS.contains(&slot.as_str()) {
        return Err(ApiError::bad_request(format!(
            "slot '{slot}' is not a plugin UI seat; seats: {}",
            PLUGIN_UI_SEATS.join(", ")
        ))
        .with_code("plugin_ui_seat_unknown"));
    }
    if let Some(key) = &query.dsh_slot
        && !valid_dsh_slot_key(key)
    {
        return Err(ApiError::bad_request(
            "dsh_slot must be 1 to 128 characters of letters, digits, '.', '_' or '-', starting with a letter or digit",
        )
        .with_code("plugin_ui_dsh_slot_invalid"));
    }
    let state_for_prepare = state.clone();
    let selector_for_prepare = selector.clone();
    let seat = slot.clone();
    let requested_key = query.dsh_slot.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        prepare_plugin_ui(
            &state_for_prepare,
            &selector_for_prepare,
            &seat,
            requested_key.as_deref(),
        )
    })
    .await
    .map_err(|_| ApiError::internal("plugin UI preparation failed"))??;
    let base = plugin_ui_base(&state, &selector, &headers, &uri)?;
    let revision = state.plugin_ui_grants.insert(prepared.grant);
    Ok(Json(PluginUiSlotResponse {
        slot,
        dsh_slot: prepared.dsh_slot,
        bridge: prepared.bridge,
        base,
        entry: prepared.entry,
        revision,
    }))
}

fn grant_for_request(
    state: &RuntimeApiState,
    selector: &str,
    headers: &HeaderMap,
) -> Result<(PluginUiGrant, String), ApiError> {
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
    Ok((grant, revision))
}

pub(super) async fn plugin_route_bridge(
    State(state): State<RuntimeApiState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path((selector, path)): Path<(String, String)>,
    method: axum::http::Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !peer.ip().is_loopback() {
        return Err(ApiError::forbidden(
            "plugin routes are available only from loopback",
        ));
    }
    let (grant, revision) = grant_for_request(&state, &selector, &headers)?;
    let state_for_route = state.clone();
    tokio::task::spawn_blocking(move || {
        codewhale_plugins::registry::verify_plugin_authority(&grant.authority)
            .map_err(|_| ui_stale_revision())?;
        admit_ui_grant(&state_for_route, &grant)?;
        if state_for_route.plugin_ui_grants.get(&revision).is_none() {
            return Err(ui_stale_revision());
        }
        if grant.dsh.is_none() {
            return Err(ApiError::forbidden(
                "this plugin UI grant has no DSH client and no plugin routes",
            )
            .with_code("plugin_route_not_admitted"));
        }
        Ok(())
    })
    .await
    .map_err(|_| ApiError::internal("plugin route check failed"))??;
    ui_relative_path(&path).map_err(|reason| {
        ApiError::bad_request(format!("plugin route path '{path}' is invalid: {reason}"))
    })?;
    if !DSH_ROUTE_METHODS.contains(&method.as_str()) {
        return Err(ApiError {
            status: StatusCode::METHOD_NOT_ALLOWED,
            message: format!("plugin routes do not accept {method}"),
            code: None,
        });
    }
    Err(ApiError::not_implemented(
        "this Engine has no plugin web server to forward the route to yet",
    )
    .with_code("plugin_route_not_served"))
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
    let (grant, revision) = grant_for_request(&state, &selector, &headers)?;
    let relative = ui_relative_path(&path).map_err(|reason| {
        ApiError::bad_request(format!("plugin UI path '{path}' is invalid: {reason}"))
    })?;
    let content_type = ui_content_type(&relative);
    if grant.dsh.is_some() && relative != FsPath::new(DSH_BOOTSTRAP_ENTRY) {
        return Err(
            ApiError::not_found("plugin UI file is not in the reviewed client closure")
                .with_code("plugin_ui_file_not_reviewed"),
        );
    }
    let state_for_asset = state.clone();
    let bytes = tokio::task::spawn_blocking(move || {
        codewhale_plugins::registry::verify_plugin_authority(&grant.authority)
            .map_err(|_| ui_stale_revision())?;
        let bytes = if let Some(dsh) = &grant.dsh {
            let verified =
                verify_dsh_closure(&grant, &dsh.closure).map_err(|_| ui_stale_revision())?;
            dsh_bootstrap(&grant.selector, &dsh.closure, &verified)
                .map_err(|_| ui_stale_revision())?
        } else {
            verified_ui_file(&grant, &grant.ui_prefix.join(&relative)).map_err(
                |error| match error {
                    UiFileError::Missing => {
                        ApiError::not_found("plugin UI file is not in the reviewed bundle")
                            .with_code("plugin_ui_file_not_reviewed")
                    }
                    UiFileError::TooLarge => {
                        ApiError::payload_too_large("plugin UI file exceeds the 8 MiB limit")
                    }
                    _ => ui_stale_revision(),
                },
            )?
        };
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

// ---------------------------------------------------------------------------
// Plugin catalog: built-in and experimental rows backed by `[features]`
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(super) struct CatalogRow {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) description: Option<String>,
    pub(super) icon: Option<String>,
    pub(super) experimental: bool,
    pub(super) enabled: bool,
    pub(super) available: bool,
    pub(super) unavailable_reason: Option<String>,
    pub(super) settings_schema: Option<Value>,
    pub(super) settings: Option<Value>,
    pub(super) restart_required: bool,
    pub(super) backing: &'static str,
    pub(super) dsh_package: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct CatalogResponse {
    pub(super) builtin: Vec<CatalogRow>,
    pub(super) experimental: Vec<CatalogRow>,
    pub(super) more: Vec<CatalogRow>,
}

#[derive(Debug, Serialize)]
pub(super) struct CatalogRowEnvelope {
    pub(super) row: CatalogRow,
}

const CATALOG_BUILTIN_KEYS: &[&str] = &["subagents", "web_search", "shell_tool"];
const AGENT_LOOP_ID: &str = "agent_loop";

fn catalog_title(key: &str) -> String {
    match key {
        "subagents" => "Subagent".to_string(),
        "web_search" => "Web search".to_string(),
        "shell_tool" => "Shell".to_string(),
        other => super::humanize_schema_key(other),
    }
}

fn catalog_description(key: &str) -> Option<String> {
    let text = match key {
        "subagents" => "Delegate bounded tasks to focused child agents with isolated context.",
        "web_search" => "Search and fetch live web pages while answering.",
        "shell_tool" => "Run commands in your workspace terminal with approval controls.",
        "apply_patch" => "Apply structured multi-file patches as one reviewed change.",
        "mcp" => "Connect Model Context Protocol servers and their tools.",
        "exec_policy" => "Check shell commands against your exec policy before they run.",
        "vision_model" => "Send images to a vision-capable model for analysis.",
        "verify_tool" => "Let the agent run an adversarial self-critique before finishing.",
        "code_mode" => "Compose MCP and plugin tools through one eager execute_tools call.",
        "extension_host" => {
            "Run reviewed plugins' native code in the TypeScript extension host. Changing this re-reviews every plugin after a restart."
        }
        "finance_host" => "Normalize finance data with the pinned TypeScript adapter.",
        "data_host" => "Present data validation results with the pinned TypeScript presenter.",
        "speech_host" => "Prepare speech input and output with the pinned TypeScript adapter.",
        "pdf_host" => "Orchestrate PDF outcomes with the pinned TypeScript adapter.",
        "ocr_host" => "Choose the OCR engine with the pinned TypeScript adapter.",
        "web_search_host" => "Run web queries through the pinned TypeScript provider adapters.",
        "web_extract_host" => "Select readable page regions with the pinned TypeScript adapter.",
        "github_host" => "Present GitHub and local reports with the pinned TypeScript adapter.",
        "review_host" => "Present captured reviews with the pinned TypeScript adapter.",
        _ => return None,
    };
    Some(text.to_string())
}

fn subagents_settings(config: &codewhale_runtime::config::Config) -> (Value, Value) {
    let max_concurrent = config
        .subagents
        .as_ref()
        .and_then(|subagents| subagents.max_concurrent)
        .map_or_else(
            || config.max_subagents(),
            |max| max.min(codewhale_runtime::config::MAX_SUBAGENTS),
        );
    let schema = serde_json::json!({
        "fields": [{
            "key": "max_concurrent",
            "label": "Maximum concurrent subagents",
            "kind": "number",
            "default": max_concurrent,
        }]
    });
    let settings = serde_json::json!({ "max_concurrent": max_concurrent });
    (schema, settings)
}

fn catalog_feature_row(
    config: &codewhale_runtime::config::Config,
    spec: &codewhale_runtime::features::FeatureSpec,
) -> CatalogRow {
    let (settings_schema, settings) = if spec.key == "subagents" {
        let (schema, settings) = subagents_settings(config);
        (Some(schema), Some(settings))
    } else {
        (None, None)
    };
    CatalogRow {
        id: spec.key.to_string(),
        title: catalog_title(spec.key),
        description: catalog_description(spec.key),
        icon: Some(spec.key.to_string()),
        experimental: spec.stage == codewhale_runtime::features::Stage::Experimental,
        enabled: config.features().enabled(spec.id),
        available: true,
        unavailable_reason: None,
        settings_schema,
        settings,
        restart_required: spec.key == "extension_host",
        backing: "feature",
        dsh_package: None,
    }
}

fn agent_loop_row() -> CatalogRow {
    CatalogRow {
        id: AGENT_LOOP_ID.to_string(),
        title: "Agent loop".to_string(),
        description: Some(
            "The planning, tool-use and verification loop every turn runs in. Always on."
                .to_string(),
        ),
        icon: Some(AGENT_LOOP_ID.to_string()),
        experimental: false,
        enabled: true,
        available: true,
        unavailable_reason: None,
        settings_schema: None,
        settings: None,
        restart_required: false,
        backing: "native",
        dsh_package: None,
    }
}

fn build_catalog(config: &codewhale_runtime::config::Config) -> CatalogResponse {
    let mut builtin = Vec::new();
    let mut experimental = Vec::new();
    let mut more = Vec::new();
    for spec in codewhale_runtime::features::FEATURES {
        let row = catalog_feature_row(config, spec);
        if CATALOG_BUILTIN_KEYS.contains(&spec.key) {
            builtin.push(row);
        } else if spec.stage == codewhale_runtime::features::Stage::Experimental {
            experimental.push(row);
        } else {
            more.push(row);
        }
    }
    builtin.sort_by_key(|row| {
        CATALOG_BUILTIN_KEYS
            .iter()
            .position(|key| *key == row.id)
            .unwrap_or(usize::MAX)
    });
    builtin.push(agent_loop_row());
    CatalogResponse {
        builtin,
        experimental,
        more,
    }
}

fn find_catalog_row(config: &codewhale_runtime::config::Config, id: &str) -> Option<CatalogRow> {
    let catalog = build_catalog(config);
    catalog
        .builtin
        .into_iter()
        .chain(catalog.experimental)
        .chain(catalog.more)
        .find(|row| row.id == id)
}

/// `GET /v1/apps/plugins/catalog`
pub(super) async fn list_plugin_catalog(
    State(state): State<RuntimeApiState>,
) -> Result<Json<CatalogResponse>, ApiError> {
    let config = state.config.read();
    Ok(Json(build_catalog(&config)))
}

async fn reload_after_catalog_write(state: RuntimeApiState) -> Result<(), ApiError> {
    tokio::spawn(async move {
        let _credential_change = state.provider_switches.lock().await;
        let mut reloaded = codewhale_plugins::providers::load_config(
            state.config_path.clone(),
            state.config_profile.as_deref(),
        )
        .map_err(|e| ApiError::internal(format!("Failed to reload config: {e}")))?;
        reloaded.preserve_runtime_credentials(&state.config.read());
        state
            .runtime_threads
            .reload_config(reloaded.clone())
            .await
            .map_err(|err| ApiError::bad_request(format!("Config reload rejected: {err}")))?;
        *state.config.write() = reloaded;
        Ok(())
    })
    .await
    .map_err(|_| ApiError::internal("config reload task failed"))?
}

async fn set_catalog_enabled(
    state: RuntimeApiState,
    id: String,
    on: bool,
) -> Result<Json<CatalogRowEnvelope>, ApiError> {
    if id == AGENT_LOOP_ID {
        if !on {
            return Err(ApiError::conflict(
                "The agent loop is always on; it cannot be turned off.",
            ));
        }
        return Ok(Json(CatalogRowEnvelope {
            row: agent_loop_row(),
        }));
    }
    let Some(spec) = codewhale_runtime::features::FEATURES
        .iter()
        .find(|spec| spec.key == id)
    else {
        return Err(ApiError::not_found(format!(
            "catalog plugin '{id}' not found"
        )));
    };
    codewhale_runtime::config_persistence::persist_table_bool_key(
        state.config_path.as_deref(),
        "features",
        spec.key,
        on,
    )
    .map_err(|e| ApiError::internal(format!("Failed to persist setting: {e}")))?;
    reload_after_catalog_write(state.clone()).await?;
    let config = state.config.read();
    let row = find_catalog_row(&config, &id).ok_or_else(|| {
        ApiError::internal(format!("catalog plugin '{id}' vanished after reload"))
    })?;
    Ok(Json(CatalogRowEnvelope { row }))
}

/// `POST /v1/apps/plugins/catalog/{id}/enable`
pub(super) async fn enable_catalog_plugin(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<CatalogRowEnvelope>, ApiError> {
    set_catalog_enabled(state, id, true).await
}

/// `POST /v1/apps/plugins/catalog/{id}/disable`
pub(super) async fn disable_catalog_plugin(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
) -> Result<Json<CatalogRowEnvelope>, ApiError> {
    set_catalog_enabled(state, id, false).await
}

#[derive(Debug, Deserialize)]
pub(super) struct CatalogSettingsRequest {
    settings: Value,
}

/// `PUT /v1/apps/plugins/catalog/{id}/settings`
pub(super) async fn put_catalog_settings(
    State(state): State<RuntimeApiState>,
    Path(id): Path<String>,
    Json(req): Json<CatalogSettingsRequest>,
) -> Result<Json<CatalogRowEnvelope>, ApiError> {
    {
        let config = state.config.read();
        if find_catalog_row(&config, &id).is_none() {
            return Err(ApiError::not_found(format!(
                "catalog plugin '{id}' not found"
            )));
        }
    }
    if id != "subagents" {
        return Err(ApiError::bad_request(format!(
            "catalog plugin '{id}' has no editable settings"
        )));
    }
    let Value::Object(settings) = &req.settings else {
        return Err(ApiError::bad_request("settings must be an object"));
    };
    for key in settings.keys() {
        if key != "max_concurrent" {
            return Err(ApiError::bad_request(format!(
                "unknown subagents setting '{key}'"
            )));
        }
    }
    if let Some(value) = settings.get("max_concurrent") {
        let Some(max) = value.as_u64() else {
            return Err(ApiError::bad_request(
                "max_concurrent must be a positive integer",
            ));
        };
        if max == 0 || max > codewhale_runtime::config::MAX_SUBAGENTS as u64 {
            return Err(ApiError::bad_request(format!(
                "max_concurrent must be between 1 and {}",
                codewhale_runtime::config::MAX_SUBAGENTS
            )));
        }
        codewhale_runtime::config_persistence::persist_subagents_integer_key(
            state.config_path.as_deref(),
            "max_concurrent",
            max,
        )
        .map_err(|e| ApiError::internal(format!("Failed to persist setting: {e}")))?;
        reload_after_catalog_write(state.clone()).await?;
    }
    let config = state.config.read();
    let row = find_catalog_row(&config, &id).ok_or_else(|| {
        ApiError::internal(format!("catalog plugin '{id}' vanished after reload"))
    })?;
    Ok(Json(CatalogRowEnvelope { row }))
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

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// On-disk schema for the `[hook_sinks]` table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookSinksToml {
    /// Unix domain socket path used by the app-server event sink.
    ///
    /// Known limitation: only `codewhale app-server` reads this table; the
    /// TUI and the Runtime API do not dispatch hook events to it.
    ///
    /// When unset, no Unix socket sink is registered. There is deliberately no
    /// shared `/tmp` default because socket ownership should be explicit.
    #[serde(default)]
    pub unix_socket_path: Option<PathBuf>,
}

/// On-disk schema for the `[lifecycle_outbox]` table.
///
/// Opt-in lifecycle event outbox: every emitted event is appended as one
/// JSONL line to `path` in the `RuntimeEventEnvelope` shape
/// (`schema_version, seq, event, kind, thread_id, turn_id, item_id,
/// timestamp, payload`), and optionally POSTed to `webhook_url`. An unset or
/// empty `path` disables the feature entirely — behavior is unchanged from a
/// release without the table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LifecycleOutboxToml {
    /// Path to the JSONL outbox file. Parent directories are created lazily
    /// on the first event. Unset or empty = feature OFF.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// Optional webhook URL. Events are POSTed as `{"at", "event"}` JSON
    /// only when this is set (in addition to, never instead of, `path`).
    /// Delivery is best-effort: failures are logged and dropped.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Optional bearer token sent as `Authorization: Bearer <token>` on
    /// webhook POSTs. Ignored when `webhook_url` is unset.
    #[serde(default)]
    pub webhook_token: Option<String>,
}

/// On-disk schema for the `[control_socket]` table.
///
/// Opt-in per-session control surface: when `enabled`, the interactive TUI
/// binds a unix domain socket at `<sessions-dir>/<session-id>/control.sock`
/// for the running session. The socket speaks newline-framed JSON-RPC with
/// the verbs `message`, `interrupt`, and `status`. An absent
/// table, or `enabled = false` (the default), disables the feature entirely —
/// behavior is unchanged from a release without the table.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ControlSocketToml {
    /// Bind the per-session control socket. Default: false (OFF).
    #[serde(default)]
    pub enabled: bool,
}

/// On-disk schema for the `[skills]` table (#140). See `config.example.toml`
/// for documentation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillsToml {
    /// Curated registry index URL. When unset, the TUI falls back to the
    /// bundled default (community-curated GitHub raw).
    #[serde(default)]
    pub registry_url: Option<String>,
    /// Per-skill maximum *uncompressed* size in bytes. When unset, the TUI
    /// uses 5 MiB.
    #[serde(default)]
    pub max_install_size_bytes: Option<u64>,
    /// Keys owned by the TUI runtime (for example `scan_codewhale_only`) or
    /// added by newer releases must survive dispatcher reads and typed saves.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

/// On-disk schema for the `[tools]` table (#2076).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ToolsToml {
    /// Native tool names to keep loaded outside the default core catalog.
    #[serde(default)]
    pub always_load: Vec<String>,
    /// Runtime-owned tool settings must survive dispatcher reads and saves.
    /// Their validation belongs to the runtime's ToolsConfig, not this facade.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

/// Existing `[workshop]` threshold configuration, retained for compatibility.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct WorkshopConfig {
    /// Token threshold above which results become handle-only evidence.
    #[serde(default)]
    pub large_output_threshold_tokens: Option<usize>,

    /// Per-tool threshold overrides (tool name → token limit). A tool whose
    /// name appears here uses this limit instead of
    /// `large_output_threshold_tokens`.
    #[serde(default)]
    pub per_tool_thresholds: Option<std::collections::HashMap<String, usize>>,

    /// Optional model-visible byte budget for a single `read` / `read_file`
    /// result. Absent keeps the compile-time default (#5367).
    #[serde(default)]
    pub read_result_max_bytes: Option<usize>,

    /// Optional model-visible byte budget for a generic tool result after
    /// spillover. Absent keeps the compile-time default (#5367).
    #[serde(default)]
    pub tool_result_max_bytes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PluginId(pub String);

impl PluginId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Persistable proof of the exact reviewed plugin authority attached to a
/// Skill or MCP server. Runtime contexts keep this receipt instead of a
/// pointer to mutable process-global discovery state, and revalidate it at
/// every side-effect boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginAuthority {
    pub plugin_id: PluginId,
    pub plugin_name: String,
    pub workspace: std::path::PathBuf,
    pub state_path: std::path::PathBuf,
    #[serde(default)]
    pub managed_policy_path: Option<std::path::PathBuf>,
    #[serde(skip)]
    pub managed_policy_source: Option<std::path::PathBuf>,
    pub source_manifest: std::path::PathBuf,
    pub staged_manifest: std::path::PathBuf,
    pub content_hash: String,
    pub capability_hash: String,
    /// Monotonic generation of this plugin's persisted authority. Any trust,
    /// enablement, disablement, or revocation transition invalidates receipts
    /// held by another process immediately, even when hashes are unchanged.
    pub state_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectHookAuthority {
    pub workspace: PathBuf,
    pub digest: String,
}

/// Identity of one activation. `generation` bumps on every (re)activation;
/// `owner_token` is 128+ random bits minted by the core and handed over only
/// in `ext/activate`. It catches bugs and stale fibers; it is not a boundary
/// against a malicious plugin in the same process (see the design, §4.4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OwnerRef {
    pub plugin_id: String,
    pub generation: u64,
    pub owner_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EntryRef {
    pub path: String,
    pub sha256: String,
}

impl std::fmt::Display for PluginId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Host uses the SDK by default; explicit Rust selection remains available.
/// A Host refusal never falls back to Rust.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpBackend {
    Rust,
    #[default]
    Host,
}

/// Return only the non-secret network authority of a configured URL.
///
/// Userinfo, path, query keys and values, and fragments are all omitted because
/// every one of those components can carry credentials. Parse failures also
/// omit the original input rather than echoing an attacker-controlled value.
#[must_use]
pub fn structural_url_authority(url: &str) -> String {
    let Some(parsed) = url::Url::parse(url).ok() else {
        return "unparseable (configured value omitted)".to_string();
    };
    let Some(host) = parsed.host_str() else {
        return "unparseable (configured value omitted)".to_string();
    };
    // Url::host_str already includes the brackets around an IPv6 address.
    let mut authority = format!("{}://{host}", parsed.scheme());
    if let Some(port) = parsed.port() {
        authority.push(':');
        authority.push_str(&port.to_string());
    }
    authority
}

/// Default snapshot retention window: 7 days.
pub const DEFAULT_SNAPSHOT_MAX_AGE: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Default workspace-size ceiling above which snapshots self-disable
/// on first use (2 GB of non-excluded content). Reports from users with
/// multi-hundred-GB project directories — datasets, model weights,
/// docker image dumps that fall outside the built-in excludes —
/// surfaced that `git add -A` on first init would hang the TUI for
/// minutes-to-hours while indexing the workspace. Snapshots are a
/// rollback safety net, not a backup tool; bailing out on workspaces
/// that big is the right tradeoff. Users with legitimate large
/// monorepos can raise `[snapshots] max_workspace_gb` (or set it to
/// `0` to disable the cap entirely).
pub const DEFAULT_MAX_WORKSPACE_BYTES_FOR_SNAPSHOT: u64 = 2 * 1024 * 1024 * 1024;

/// On-disk schema for the `[snapshots]` table (#137). See
/// `config.example.toml` for documentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotsToml {
    #[serde(default = "default_snapshots_enabled")]
    pub enabled: bool,
    #[serde(default = "default_snapshot_max_age_days")]
    pub max_age_days: u64,
    /// Keys owned by the TUI runtime (for example `max_workspace_gb`) or
    /// added by newer releases must survive dispatcher reads and typed saves.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

pub fn default_snapshots_enabled() -> bool {
    true
}

pub(crate) fn default_snapshot_max_age_days() -> u64 {
    DEFAULT_SNAPSHOT_MAX_AGE.as_secs() / (24 * 60 * 60)
}

impl Default for SnapshotsToml {
    fn default() -> Self {
        Self {
            enabled: default_snapshots_enabled(),
            max_age_days: default_snapshot_max_age_days(),
            extras: BTreeMap::new(),
        }
    }
}

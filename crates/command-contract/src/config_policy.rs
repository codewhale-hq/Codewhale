//! Policy/status observations only. Hosts own persistence, runtime services and I/O.
//! These values do not grant authority to edit configuration or operate a session.
use crate::types::{CommandApprovalMode, CommandCurrency, CommandMode};
use codewhale_protocol::cloud_facts::CloudFactsState;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPermissionAction {
    Allow,
    Ask,
    Deny,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPermissionsFileState {
    Missing,
    Empty,
    Present,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRule {
    pub action: CommandPermissionAction,
    pub tool: String,
    pub command: Option<String>,
    pub command_exact: bool,
    pub path: Option<String>,
    pub workspace: Option<String>,
    pub applies_here: bool,
    /// Opaque host-generated token, kept with the rule it confirms.
    pub removal_token: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionsView {
    pub path: PathBuf,
    pub file_state: CommandPermissionsFileState,
    pub rules: Vec<PermissionRule>,
    pub approval_mode: CommandApprovalMode,
    pub audit_path: Option<PathBuf>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedPermissionRule {
    pub action: CommandPermissionAction,
    pub tool: String,
}

pub trait CommandPermissionsContext {
    fn snapshot(&self) -> Result<PermissionsView, String>;
    /// The host must lock, re-read and verify this token before atomic removal.
    /// Index is zero-based; failure must not mutate the file.
    fn remove_rule(
        &mut self,
        index: usize,
        expected_token: &str,
    ) -> Result<RemovedPermissionRule, String>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSafety {
    ReadOnly {
        enforced: bool,
    },
    WorkspaceWrite {
        enforced: bool,
        network_access: bool,
    },
    FullAccess {
        no_new_privs: Option<bool>,
    },
    External,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusFleetDrift {
    pub name: String,
    pub ids: Vec<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusSnapshotScope {
    /// Snapshot-eligible content exceeds the configured workspace size cap.
    WorkspaceTooLarge,
    /// The entry ceiling is independent from the configurable size cap.
    TooManyFiles,
    /// Home/root locations remain refused regardless of the size setting.
    UnsafeLocation,
    /// Missing history was restarted; earlier restore points are gone.
    HistoryRepaired,
    /// A real git/disk failure; the notice limit field carries the error.
    Failing,
}
#[derive(Debug, Clone, PartialEq, Eq)]
/// Retained semantic observation shared by status and transient host notices.
pub struct StatusSnapshotNotice {
    pub workspace: String,
    pub scope: StatusSnapshotScope,
    pub limit: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusContextSource {
    Configured,
    UserDeclared,
    ConfiguredModel,
    ProviderReported,
    StaticKimiCodeSafeFloor,
    Catalog,
    NameSuffixHint,
    Fallback,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusWindowOverride {
    Provider(String),
    ActiveProvider,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusCatalogFreshness {
    Bundled,
    Live,
    Stale,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusCatalog {
    pub freshness: StatusCatalogFreshness,
    pub offering_count: usize,
    pub fetched_at: Option<u64>,
    pub last_error: Option<String>,
}
/// Same semantic inputs as the existing session metrics strip; no renderer dependency.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct StatusMetrics {
    pub turns: u64,
    pub steps: u64,
    pub llm_time: Duration,
    pub tool_time: Duration,
    pub ttft_avg: Option<Duration>,
    pub tokens_per_second: Option<f64>,
    pub cache_hit_percent: Option<u8>,
    pub input_tokens: u64,
}
impl StatusMetrics {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.turns == 0 && self.steps == 0 && self.input_tokens == 0
    }
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusToolOutputs {
    pub raw_large_count: usize,
    pub raw_large_chars: usize,
    pub receipt_count: usize,
    pub artifact_count: usize,
    pub artifact_bytes: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfigStatusView {
    pub version: String,
    pub provider: String,
    pub model: String,
    pub reasoning: String,
    pub workspace: PathBuf,
    pub home: Option<PathBuf>,
    pub project_docs: Vec<String>,
    pub mode: CommandMode,
    pub approval_mode: CommandApprovalMode,
    pub trusted: bool,
    pub allow_shell: bool,
    pub safety: StatusSafety,
    pub mcp_configured_count: usize,
    /// The raw missing pin, which may differ from the display model label.
    pub model_pin_drift: Option<String>,
    pub fleet_drift: Option<StatusFleetDrift>,
    pub snapshot_notice: Option<StatusSnapshotNotice>,
    pub context_used: usize,
    pub context_window: u32,
    pub context_source: StatusContextSource,
    pub window_override: Option<StatusWindowOverride>,
    pub catalog: StatusCatalog,
    pub cloud_facts: CloudFactsState,
    pub observed_at: u64,
    pub session_id: Option<String>,
    pub history_count: usize,
    pub message_count: usize,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
    pub cache_hit_tokens: u32,
    pub cache_miss_tokens: u32,
    pub cost: f64,
    pub currency: CommandCurrency,
    pub cost_label: Option<String>,
    pub metrics: StatusMetrics,
    pub ascii_safe: bool,
    pub tool_outputs: StatusToolOutputs,
}

/// Read-only observations; optional failures are represented by absent fields.
/// No config/session mutation, provider refresh or rendered-report callback.
pub trait CommandConfigStatusContext {
    fn snapshot(&self) -> ConfigStatusView;
}

/// The size-cap remedy is shared by the status report and host toast.
pub const SNAPSHOTS_CAP_CONFIG_KEY: &str = "[snapshots] max_workspace_gb";
impl StatusSnapshotNotice {
    pub fn render(&self, template: &str) -> String {
        codewhale_protocol::display::interpolate(
            template,
            &[
                ("{workspace}", &self.workspace),
                ("{limit}", &self.limit),
                ("{config_key}", SNAPSHOTS_CAP_CONFIG_KEY),
            ],
        )
    }
}

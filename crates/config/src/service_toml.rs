use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// On-disk schema for the `[workflow]` table (#4128 / Section 2.11).
///
/// Automatic Workflow launch, write/approval gates, child/isolation budgets,
/// and completed-activity persistence all read from this one model. When the
/// table is absent, consumers resolve [`WorkflowConfigToml::default`].
/// See `config.example.toml` for documentation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowConfigToml {
    /// Allow the parent agent to auto-launch Workflow for multi-agent work.
    /// Product default is on; set `false` to require explicit `/workflow`.
    #[serde(default = "default_workflow_automatic")]
    pub automatic: bool,
    /// When automatic launch is enabled, start read-only child plans without
    /// an approval card. Write/shell/network plans still consult
    /// [`Self::require_approval_for_writes`].
    #[serde(default = "default_workflow_auto_start_read_only")]
    pub auto_start_read_only: bool,
    /// Require an operator approval card before launching plans that write,
    /// elevate shell/network, or otherwise leave the read-only envelope.
    #[serde(default = "default_workflow_require_approval_for_writes")]
    pub require_approval_for_writes: bool,
    /// Operator-selected total child limit. Zero leaves total work uncapped.
    #[serde(default = "default_workflow_max_children")]
    pub max_children: u32,
    /// Maximum concurrently live agents inside one Workflow run (product: 16).
    #[serde(default = "default_workflow_max_concurrent")]
    pub max_concurrent: u32,
    /// Maximum structural nesting depth accepted for Workflow IR.
    ///
    /// Zero leaves structural depth uncapped. Runtime delegation is separate.
    #[serde(default = "default_workflow_max_depth")]
    pub max_depth: u32,
    /// Legacy declared token hint retained in Workflow receipts. The current
    /// runtime does not meter or enforce this value. Zero omits the hint.
    #[serde(default = "default_workflow_default_token_budget")]
    pub default_token_budget: u64,
}

pub(crate) fn default_workflow_automatic() -> bool {
    true
}

pub(crate) fn default_workflow_auto_start_read_only() -> bool {
    true
}

pub(crate) fn default_workflow_require_approval_for_writes() -> bool {
    true
}

pub(crate) fn default_workflow_max_children() -> u32 {
    0
}

pub(crate) fn default_workflow_max_concurrent() -> u32 {
    16
}

pub(crate) fn default_workflow_max_depth() -> u32 {
    0
}

pub(crate) fn default_workflow_default_token_budget() -> u64 {
    0
}

impl Default for WorkflowConfigToml {
    fn default() -> Self {
        Self {
            automatic: default_workflow_automatic(),
            auto_start_read_only: default_workflow_auto_start_read_only(),
            require_approval_for_writes: default_workflow_require_approval_for_writes(),
            max_children: default_workflow_max_children(),
            max_concurrent: default_workflow_max_concurrent(),
            max_depth: default_workflow_max_depth(),
            default_token_budget: default_workflow_default_token_budget(),
        }
    }
}

/// On-disk schema for `[verifier]`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifierConfigToml {
    /// Enable automatic verifier preview when the runtime wires a
    /// claim-of-done trigger. Manual `run_verifiers` remains available
    /// regardless.
    #[serde(default)]
    pub enabled: bool,
}

/// On-disk schema for `[advisor]` (#3982).
///
/// Advisor mode is **off by default**. When enabled, the engine spawns a
/// short-lived background reviewer after each turn that contained tool calls.
/// The reviewer reads a bounded slice of recent tool calls, makes a concise
/// LLM advisory call, and emits the note as an `AdvisoryNote` event without
/// blocking the parent turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdvisorConfigToml {
    /// Master on/off switch. `false` by default — no background reviewer is
    /// spawned until the user opts in via `[advisor] enabled = true` or
    /// `/advisor on`.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum number of recent tool-call/result pairs to include in each
    /// advisory review. Keeps the reviewer's context window bounded regardless
    /// of turn length. Defaults to 10; clamped to 1–50.
    #[serde(default = "advisor_default_max_tool_calls")]
    pub max_tool_calls: u32,
    /// Minimum wall-clock seconds between two consecutive advisor emissions.
    /// Prevents noise on rapid multi-turn sequences. Defaults to 60 seconds;
    /// clamped to 5–3600.
    #[serde(default = "advisor_default_rate_limit_secs")]
    pub rate_limit_secs: u64,
    /// Deduplication window in seconds. An advisory note whose content hash
    /// matches the previous note within this window is silently dropped.
    /// Defaults to 300 seconds (5 minutes).
    #[serde(default = "advisor_default_dedup_window_secs")]
    pub dedup_window_secs: u64,
    /// Optional model override for the advisor LLM call. When absent, the
    /// advisor reuses the session's current model.
    #[serde(default)]
    pub model: Option<String>,
}

pub(crate) fn advisor_default_max_tool_calls() -> u32 {
    10
}

pub(crate) fn advisor_default_rate_limit_secs() -> u64 {
    60
}

pub(crate) fn advisor_default_dedup_window_secs() -> u64 {
    300
}

impl Default for AdvisorConfigToml {
    fn default() -> Self {
        Self {
            enabled: false,
            max_tool_calls: advisor_default_max_tool_calls(),
            rate_limit_secs: advisor_default_rate_limit_secs(),
            dedup_window_secs: advisor_default_dedup_window_secs(),
            model: None,
        }
    }
}

/// On-disk schema for the `[network]` table (#135). See `config.example.toml`
/// for documentation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicyToml {
    /// Decision for hosts that are not in `allow` or `deny`. One of
    /// `"allow" | "deny" | "prompt"`. Defaults to `"prompt"`.
    #[serde(default = "default_network_decision")]
    pub default: String,
    /// Hosts that are always allowed. Subdomain rules: a leading dot
    /// (`.example.com`) matches subdomains but not the apex.
    #[serde(default)]
    pub allow: Vec<String>,
    /// Hosts that are always denied. Deny entries win over allow entries.
    #[serde(default)]
    pub deny: Vec<String>,
    /// Hostnames whose DNS may resolve to fake-IP/private proxy ranges in an
    /// explicitly trusted proxy setup. Literal IP URLs remain blocked.
    #[serde(default)]
    pub proxy: Vec<String>,
    /// Explicit fake-IP placeholder CIDRs for those proxy hosts. The runtime
    /// accepts only subnets contained by `198.18.0.0/15`.
    #[serde(default)]
    pub proxy_fake_ip_cidrs: Vec<String>,
    /// Whether to record one audit-log line per outbound network call.
    #[serde(default = "default_network_audit")]
    pub audit: bool,
    /// Keys owned by the TUI runtime or added by newer releases must survive
    /// dispatcher reads and typed saves.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}

pub(crate) fn default_network_decision() -> String {
    "prompt".to_string()
}

pub(crate) fn default_network_audit() -> bool {
    true
}

impl Default for NetworkPolicyToml {
    fn default() -> Self {
        Self {
            default: default_network_decision(),
            allow: Vec::new(),
            deny: Vec::new(),
            proxy: Vec::new(),
            proxy_fake_ip_cidrs: Vec::new(),
            audit: default_network_audit(),
            extras: BTreeMap::new(),
        }
    }
}

/// User-defined LSP server for one file extension (used inside
/// [`LspConfigToml::custom`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CustomLspDef {
    /// LSP `languageId` value used in `textDocument/didOpen`.
    pub language_id: String,
    /// Executable to spawn.
    pub command: String,
    /// Arguments passed to the executable.
    #[serde(default)]
    pub args: Vec<String>,
}

/// On-disk schema for the `[lsp]` table (#136). See `config.example.toml`
/// for documentation. All fields are optional so the TUI runtime can fall
/// back to its own defaults when keys are absent.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LspConfigToml {
    /// Master switch.
    pub enabled: Option<bool>,
    /// Maximum time to wait for diagnostics after an edit, in milliseconds.
    pub poll_after_edit_ms: Option<u64>,
    /// Cap on diagnostics surfaced per file.
    pub max_diagnostics_per_file: Option<usize>,
    /// When `true`, warnings (severity 2) are surfaced in addition to errors.
    pub include_warnings: Option<bool>,
    /// Optional override for the `language -> [cmd, ...args]` table.
    pub servers: Option<BTreeMap<String, Vec<String>>>,
    /// User-defined LSP servers for file extensions not in the built-in
    /// registry. Keyed by extension (e.g. `"php"`, `"rb"`).
    pub custom: Option<BTreeMap<String, CustomLspDef>>,
    /// Keys owned by the TUI runtime or added by newer releases must survive
    /// dispatcher reads and typed saves.
    #[serde(flatten)]
    pub extras: BTreeMap<String, toml::Value>,
}
